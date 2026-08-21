# infra

The whole ingest stack as Terraform: one R2 bucket, one Worker, one custom
domain. The Worker is ~60 lines in `worker/ingest.js` and does one thing — it
puts the request body in R2 without decoding it.

## Apply it

```sh
cp terraform.tfvars.example prod.tfvars   # gitignored; fill it in
tofu init
tofu plan  -var-file=prod.tfvars          # <- the real check, see below
tofu apply -var-file=prod.tfvars
```

Leave `zone_id` empty on a first apply. The custom domain is skipped and the
Worker is reachable at its `workers.dev` subdomain, so the whole path can be
tested before the domain exists.

## OpenTofu, not Terraform

`required_version >= 1.6` cannot be met by Homebrew's `terraform`, which is
frozen at 1.5.7 and marked deprecated because the next release changed to BUSL.
Use `brew install opentofu` and run `tofu`. The config is plain HCL with nothing
HashiCorp-specific in it, so either binary works if you have a recent one.

### Credentials

Two separate things, which is the usual place to get stuck:

| | where | permissions |
| --- | --- | --- |
| `cloudflare_api_token` | dash → profile → API Tokens | Account · Workers Scripts · Edit; Account · Workers R2 Storage · Edit; Zone · Workers Routes · Edit; Zone · DNS · Edit |
| `r2_access_key_id` / `r2_secret_access_key` | dash → R2 object storage → **Account Details** panel → **Manage** next to **API Tokens** | Object Read & Write |

Adding any Zone-level row makes a **Zone Resources** selector appear below
Account Resources. It defaults to *All zones* — set it to *Include →
cratebank.io* unless you want a token in a `.env` that can rewrite DNS for every
domain on the account.

The R2 keys are not used by the Worker, which reaches the bucket through a
binding and needs no credential at all. They exist for out-of-band access:
`aws s3 ls --endpoint-url https://<account>.r2.cloudflarestorage.com`.

If the R2 token screen is unreachable, the keys can be derived from any token
with R2 permissions: `access_key_id` is the token's **ID**, and
`secret_access_key` is `printf '%s' "$TOKEN" | shasum -a 256 | cut -d" " -f1`.
Use `printf`, not `echo` — a trailing newline changes the hash and fails
opaquely much later.

## `plan` is the real check, not `validate`

`tofu validate` passes against the provider schema, so resource types and
top-level argument names are checked. It is **much weaker than it looks for
nested blocks**:

> An earlier version of this file put `time_pattern` and `interval_seconds` at
> the top of a sink `config` instead of inside `partitioning` and
> `rolling_policy`. `validate` said "Success!". Adding a nested key called
> `bogus_key = "nonsense"` also passes.

So run `plan` against a real account before believing any of it.

## What it creates

| resource | why it looks like that |
| --- | --- |
| `cloudflare_r2_bucket` | storage; zero egress is what makes a public query surface affordable |
| `cloudflare_workers_script.ingest` | the ingest Worker, with an R2 binding named `BUCKET` |
| `cloudflare_workers_custom_domain.ingest` | `ingest.cratebank.io` → the Worker; skipped if `zone_id` is empty |
| `cloudflare_workers_script.compact` | nightly compaction to public parquet (`compact.tf`) |
| `cloudflare_workers_cron_trigger.compact` | `0 5 * * *` |
| `cloudflare_workers_script_subdomain.compact` | workers.dev route for the manual `POST /compact` trigger |
| `cloudflare_r2_custom_domain.data` | `data.cratebank.io` → the bucket, **public** |

**The Worker never decodes the body.** It streams `request.body` straight to
`env.BUCKET.put()`. It requires `content-length` and returns 411 without it, so
a 100 MB upload never has to be held in the Worker's 128 MB of memory.

**Do not add a `cloudflare_dns_record` for `ingest` or `data`.** A Workers
custom domain and an R2 custom domain each create and manage their own DNS
record; declaring one alongside makes them fight over the same hostname.

**`data.cratebank.io` makes the entire bucket world-readable**, raw session
blobs included. That is intended — the census is public — but it is not
reversible for anything already fetched or indexed.

## The compaction Worker

`worker/compact.js` reads every session blob nightly and writes
`units.parquet` and `sessions.parquet` to the bucket root, plus dated copies
under `snapshots/`. Those are the public interface; the raw blobs are the
ground truth.

`worker/dist/compact.js` is the esbuild bundle Terraform actually deploys, and
it **is committed** so a clone without a node toolchain can still apply. CI
rebuilds it and fails on drift. Two gitignore lines are needed to keep it
tracked, because a global gitignore may exclude `dist/` and git will not
descend into an excluded directory to find a re-included file.

Rebuild after editing `compact.js`:

```sh
cd worker && npm install && npm run build
```

Trigger it by hand rather than waiting for 05:00 UTC:

```sh
curl -X POST "https://cratebank-compact.<subdomain>.workers.dev/compact" \
  -H "authorization: Bearer $compact_secret"
```

It is a full rebuild holding every row in memory, which is free now and will
not be — the ceiling is the Worker's 128 MB, roughly 10k sessions. Watch
`objects` and `bytes_in` in the cron logs; the fix when it arrives is per-day
compaction, which the hive key layout already supports.

## Two traps, both the same shape

Terraform reads a server-populated field as a field you deleted, and plans a
change forever. Both were caught by reading a plan that claimed to modify
something nobody had touched.

**Optional+computed blocks must be declared in full.** `observability = {
enabled = true }` looks complete, but the server fills in `head_sampling_rate`,
`logs` and `traces`. Terraform then reads the difference as a removal and
re-uploads the script on **every plan**. Declare every sub-field, or expect
permanent drift.

**The same bug bites harder on resources that cannot be updated in place.** The
now-removed Pipelines stream had a server-generated schema; the config did not
declare it, so every plan forced a *replacement*, minting a new stream id and
endpoint each time. It needed `lifecycle { ignore_changes = [schema] }`. If a
plan proposes replacing something you did not touch, read the diff before
applying it.

## What was here before

A Cloudflare Pipelines stream, sink and pipeline, deleted once the Worker
existed. `docs/ingest.md` records why in detail; the short version is a 1 MB
per-message cap that appears in no documentation, and an ingest that answered
`200 committed:N` for events it then silently discarded.

Two findings worth keeping if anyone reconsiders Pipelines:

- **`format.unstructured = true` is the only way to get an unstructured
  stream.** Declaring `schema.fields = [{name="value", type="json", required=true}]`
  instead produces a *structured* stream, and every event without a literal
  `value` key — which is every event this client sends — is accepted and then
  dropped before reaching R2.
- **Deleting a stream requires destroying the pipeline first**, and Terraform
  will not work that out if the pipeline's SQL string is unchanged. It fails
  mid-apply with `422 Stream still in use and cannot be deleted`; force it with
  `tofu apply -replace=cloudflare_pipeline.<name>`.

## Pulumi

`pulumi-cloudflare` (v6.19.0) is bridged from this same Terraform provider, so
the resources and arguments are identical modulo naming convention:

```sh
pulumi convert --from terraform --language typescript --out ../infra-pulumi
```

That is the honest recommendation over hand-writing it — the bridge means the
Terraform is the source of truth for what the provider accepts either way, and
`convert` will not invent an argument the provider does not have.
