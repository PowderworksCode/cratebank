# Registration and contributor identity

> **Not built yet.** v1 ingest is authless — see the decision in
> [`ingest.md`](ingest.md). This is the design to reach for when the endpoint is
> ready to be advertised publicly, which is the point at which authless stops
> being reasonable.

The goal is a credential you can get in one command, with no account, no email
and no waiting:

```sh
$ cargo cratebank register --id acme
registered as  acme
key written to ~/.cargo/cratebank/key
```

and if the name is taken, it still succeeds:

```sh
$ cargo cratebank register --id acme
`acme` is taken; registered as  acme-7f3a
```

Always returning a usable credential matters more than it looks: a CLI that can
fail on name collision is a CLI that needs retry logic, prompts and a second
round trip, and every one of those is a place a contributor gives up.

## Keys, not secrets

Two ways to do the credential:

| | server-issued token | client-generated keypair |
| --- | --- | --- |
| what crosses the wire | the secret itself, once | a public key |
| what we store | a hash | a public key |
| breach of our store | hashes — bad, not fatal | nothing of value |
| proving identity later | present the secret again | sign a challenge |
| client work | store a string | Ed25519 sign per request |

**Recommendation: the keypair.** The client generates Ed25519 locally, sends
only the public key, and signs each submission. We never hold anything worth
stealing, and a contributor can prove they are `acme` at any point in the future
by signing a challenge — which is exactly what the verification path below
needs. Workers verify Ed25519 through WebCrypto; the cost is a signing call in
the client and a verify in the Worker.

## The endpoint

```
POST /v1/register
{ "desired_id": "acme", "public_key": "<base64 ed25519>", "client": "cargo-cratebank 0.1.0" }

200 { "id": "acme",      "granted": true  }
200 { "id": "acme-7f3a", "granted": false, "reason": "taken" }
```

Storage is one KV entry per id: public key, created-at, revoked flag, verified
flag. No database.

Submissions then carry the id and a signature over the payload:

```
Authorization: Cratebank id="acme", sig="<base64>"
```

## Squatting, which is the real problem

A public namespace that anyone can claim instantly is a namespace where someone
claims `tokio`, `google` or `rust-lang` — and since the id appears in published
data as *who contributed this*, that is not a naming annoyance, it is
impersonation. Bad numbers attributed to a real company is the failure mode.

Three defences, all cheap:

1. **Unverified ids are visibly unverified.** The id is stored with
   `verified: false` and published that way, so a consumer can tell
   self-claimed names from proven ones. Data is still usable; the claim is not
   dressed up as more than it is.
2. **A reserved list.** Names matching well-known crates, organisations and the
   Rust project itself are not granted on request — they return a suffixed id
   like any collision, and the real owner can claim them through verification.
3. **Verification upgrades an id.** Prove control of a domain (a DNS TXT
   record) or a GitHub organisation, sign the challenge with the key already
   registered, and `verified` flips to true. Nothing about the credential
   changes — only the claim's standing.

The suffixing behaviour does double duty here: collisions and refusals both
produce a working credential, so the client never needs a distinct error path.

## The authenticated id is authoritative

The client already sends a self-declared `machine_id`, which is a label a
contributor chooses freely. Once submissions are authenticated, those two must
not be confused:

- the **authenticated id** — proven by signature — is what attribution and any
  published credit is based on;
- the **declared `machine_id`** is kept as a hint, useful for distinguishing a
  contributor's own machines from each other, and never trusted as identity.

Without that rule, attribution is spoofable by simply typing someone else's name
into a manifest.

## Registering is rate-limited, not gated

Instant anonymous registration means an attacker can mint many ids. That is
acceptable and normal — it is the API-key model — provided:

- registration is rate-limited per IP at the edge;
- ids are individually revocable, so abuse is a filter predicate rather than an
  incident;
- estimates are per-class and robust, so no contributor moves a number much
  regardless of how many ids they hold.

What registration buys is not a wall. It is a **handle**: something to
rate-limit against, revoke, and attribute to — none of which an IP address can
do.

## Trust tiers

Every stored row records how it arrived, so analyses can weight or exclude by
provenance rather than trusting everything equally:

| tier | meaning |
| --- | --- |
| `verified` | signed by a key whose id proved domain or org control |
| `registered` | signed by a registered key, name self-claimed |
| `service` | a Cloudflare Access service token, issued by us to an organisation |
| `anonymous` | no credential — only if an open tier is ever offered |
