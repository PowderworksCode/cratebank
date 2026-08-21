# Ingest and storage

**Cloudflare Pipelines does the ingest, and we write no server code.**

Pipelines accepts JSON over HTTP, transforms it with SQL, and writes Parquet to
R2 with exactly-once delivery. That is the whole pipeline this project needed,
minus the part where we operate it.

```
cargo-cratebank ──POST JSON──▶ Pipelines stream ──SQL──▶ R2 (zstd parquet, date-partitioned)
                                                              │
                                                              ▼
                                              DuckDB / R2 SQL / anything reading parquet
```

## Why not a Worker, a queue, or a server

The obvious alternative — a Worker that validates and writes to R2 — costs us a
codebase, a deploy story, a schema-migration story, and a batching layer to
avoid writing one tiny object per build. Pipelines has all of that as
configuration: `wrangler pipelines setup`, a roll interval, a partition pattern.
The only thing we lose is arbitrary validation at the edge, which we do not want
at the edge anyway — the collection design is *capture generously, model
nothing at ingest*.

## The endpoint

```
POST https://{stream-id}.ingest.cloudflare.com
Content-Type: application/json

[ {...}, {...} ]          # a JSON array of rows, not a single object
```

Authentication is **optional per stream**, which is what makes a public census
possible: a contributor needs no account, no token, no signup. (Pipelines' own
auth uses a Cloudflare API token scoped `Workers Pipeline Send` — an
account-level credential that could not be shipped inside a public binary
anyway.)

## Authentication

Pipelines' own stream auth uses a Cloudflare API token scoped `Workers Pipeline
Send`. That is an **account-level credential**: shipping it inside a public
binary would hand every contributor a key to the account, and revoking it would
break every client at once. So stream auth is not the mechanism for a public
census — the options are what sits in front.

| option | code | credential | revocable | self-serve | notes |
| --- | --- | --- | --- | --- | --- |
| none (public stream) | none | — | — | — | submission counts untrustworthy |
| **Access service tokens** | **none** | client id + secret, per consumer | per token | no — we issue them | config only; ideal for organisations |
| **Worker + our tokens** | ~100 lines | our token, per contributor | per token | yes | most flexible; see below |
| Worker + GitHub device flow | more | GitHub identity | per user | yes | strongest identity, most machinery |
| mTLS / API Shield | none | client cert | per cert | no | too heavy for volunteers |

Two are worth taking seriously.

**Cloudflare Access service tokens** are pure configuration: a Service Auth
policy on the ingest hostname, and callers present `CF-Access-Client-Id` and
`CF-Access-Client-Secret`. Per-consumer, individually revocable, no code at all.
The catch is that *we* issue every token, so it fits organisations ("here is
Acme's credential") and not a volunteer who wants to contribute this afternoon.

**A Worker in front of Pipelines** is the general answer, and it costs less than
it sounds: Worker bindings need **no API token** at all — `await
env.STREAM.send(events)` — so the Worker authenticates the contributor and
forwards over a binding that carries no shippable secret.

Tokens can be stateless: `token = id.HMAC(id, secret)`, verified with one
secret in the Worker, no database. Revocation is a small KV denylist.
Registration can be instant and anonymous (`cargo cratebank register`), which
makes a token less an identity than a **handle** — something to rate-limit
against and revoke, which an IP is not.

### The Worker earns its keep beyond auth

Auth is the reason to add it, but three other things fall out, and together they
are worth more:

1. **Flattening moves server-side.** The client can keep sending one session
   object — the shape it already produces — and the Worker splits it into
   `sessions` and `units` rows. The client stays dumb and the row schema can
   change without a client release, which matters because contributors upgrade
   slowly and cargo's log schema is still moving.
2. **Batching moves server-side** too, so the 5 MB request limit stops being the
   client's problem.
3. **A version boundary.** Payload schema, stream layout and Pipelines config
   can all change behind one stable endpoint.

That reverses the client work this document originally called for: with a Worker,
points 1–3 of *What has to change in the client* become the Worker's job, and the
client only gains a token header.

Registration — how a contributor gets that token in one command, without an
account or an email — is specified in [`registration.md`](registration.md).

### Recommendation

Ship both, for different contributors:

- **Access service tokens** immediately, for named organisations. Zero code, and
  it is the credential an enterprise contributor will expect to be able to
  rotate and audit.
- **A Worker with stateless HMAC tokens** as the general path, self-serve so the
  long tail is not gated on us answering email.

And tag every row with how it arrived — `trust: service | token | anonymous` —
so an analysis can weight or exclude by provenance rather than trusting all
submissions equally. If an anonymous tier is ever opened, that tag is what keeps
it from contaminating everything else.

The cost is honest: it is no longer a no-code ingest. The no-code path remains
available and is the right way to *start* — a public stream, a WAF rate limit,
and real data flowing this week — with the Worker added before the endpoint is
advertised anywhere public.

## Two streams, because rows should be rows

The client currently sends **one object per build**, with the events nested
inside it. That shape is wrong for columnar storage: the interesting unit of
analysis is a compilation unit, and a nested array of 300 events is neither
queryable nor compressible as one.

| stream | one row per | sink | contents |
| --- | --- | --- | --- |
| `sessions` | build | parquet | run id, machine profile, build env, load, counts, `complete` |
| `units` | compilation unit | parquet | run id, package id, features, platform, mode, timings, section splits, cpu, rss, artifact bytes |
| `raw` | build | JSON | the verbatim event log, for reprocessing |

`sessions` and `units` are the analytics tables and map directly onto
`docs/schema.md`. `raw` exists because cargo's log schema is explicitly still
evolving and our *extraction* will improve after the fact — the collection
principle is that measurement happens once and parsing happens forever, so the
bytes we parsed must remain. Pipelines supports a JSON sink, so this costs a
stream rather than a service.

Duplicating session columns onto every unit row is deliberate: parquet
dictionary-encodes them to near nothing, and it removes a join from every
query anyone will ever write.

## Limits, and the one that binds

Open-beta limits: **5 MB per request**, 5 MB/s per stream, 20 streams / 20
pipelines / 20 sinks per account.

Measured against real payloads: ~398 bytes per event, so a 1,000-unit build
projects to **≈3.0 MB in one request** — under the limit, but not by much, and
the fleet's largest projects are that size. Two consequences:

1. the client must **batch**: split rows into arrays under ~4 MB and POST them
   in sequence. Arrays make this trivial and rows are independent;
2. the flattened `units` shape helps here too — it is far more compact than the
   raw event stream it replaces.

The 5 MB/s per-stream ingest rate is the one to watch at scale: it is roughly
150 medium builds per second, which is a long way off, but it is a per-stream
cap and the fix is more streams (20 allowed) or a limit-increase request.

## Storage layout

R2 sink configuration, all of it flags rather than code:

```
--format parquet --compression zstd
--partition-pattern "year=%Y/month=%m/day=%d"
--roll-interval 300 --roll-size 100
--path cratebank/units
```

Which produces exactly the layout the schema doc assumed:

```
cratebank/
  units/year=2026/month=09/day=03/*.parquet
  sessions/year=2026/month=09/day=03/*.parquet
  raw/year=2026/month=09/day=03/*.json
```

Hive-style partitioning is what every engine expects, so
`SELECT … FROM 'https://data.cratebank.io/units/**/*.parquet'` works in DuckDB
directly, and partition pruning happens automatically for date-filtered
queries. R2's zero egress is what makes serving that publicly sane.

## Strata

Daily partitions are the raw feed. A **stratum** is a monthly immutable
release: the day partitions compacted into larger files, deduplicated,
schema-normalised, and published under `cratebank/2026-09/`. That is a batch
job — the first piece of this system that genuinely needs code — and it is
deliberately *after* ingest, so the pipeline works before the release process
exists.

## Abuse, honestly

An unauthenticated public endpoint accepts anything. The mitigations that do
not require a server: Cloudflare WAF rate limiting by IP, and the fact that a
poisoned row is a row, not a compromise — every record carries its machine
profile and client version, so a bad contributor is a filter predicate rather
than an incident. Common classes accumulate thousands of observations, so no
single contributor moves an estimate much.

What we should not pretend: without auth, submission counts are not
trustworthy, and any statistic that weights by *number of submissions* rather
than by distinct class is manipulable. Estimates should be robust
(medians, trimmed means) and per-class rather than per-submission.

## Cost

Sink pricing is $0.03/GB for JSON output and $0.06/GB for parquet, plus R2
storage at ~$0.015/GB/month and **zero egress**. At 35 KB per session, a
million contributed builds a month is ~35 GB — a few dollars of sink cost and
under a dollar of storage. The public query surface costs nothing to serve,
which is the property that makes "the census is public" affordable rather than
aspirational.

Requires a Workers Paid plan ($5/month).

## What has to change in the client

If ingest goes straight to Pipelines: emit rows rather than one nested object,
batch under 5 MB, and take an endpoint per stream.

**If a Worker fronts it — the recommendation above — almost none of that.** The
client keeps its current payload, gains an `Authorization` header, and the
Worker does the flattening and batching. That is the better division of labour:
schema changes ship at our deploy cadence rather than at the rate contributors
upgrade a CLI.

Either way `--dry-run` keeps printing exactly what would be sent, and
`cargo cratebank serve` remains the reference collector.
