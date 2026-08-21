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

The client posts to **our** hostname, never Cloudflare's:

```
POST https://ingest.cratebank.io
Content-Type: application/json

[ {...}, {...} ]          # a JSON array, not a single object
```

`ingest.cratebank.io` is a CNAME to the stream's
`{stream-id}.ingest.cloudflare.com`. That indirection is worth having from the
first request: the stream id is an implementation detail, streams cannot be
altered after creation so we will eventually need to move to a new one, and a
released client that hardcodes a Cloudflare hostname can never be redirected.
Contributors upgrade slowly; the name they were shipped with has to remain
correct.

Authentication is **optional per stream**, and v1 runs with it off: a
contributor needs no account, no token, no signup. (Pipelines' own auth uses a
Cloudflare API token scoped `Workers Pipeline Send` — an account-level
credential that could not be shipped inside a public binary anyway, which is why
the eventual answer is a Worker rather than stream auth.)

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

### Decision: start authless

**v1 ships with no authentication.** A public Pipelines stream, a WAF rate limit,
and data flowing this week. The reasons are practical rather than principled:
nothing downstream exists yet, so there is nothing to protect; the credential
design above is only worth building once the shape of the data has settled; and
an ingest that works end to end is the fastest way to find out whether any of
this is right.

What that costs, stated plainly:

- **submission counts are not trustworthy.** Any statistic weighted by number of
  submissions is manipulable. Estimates must be per-class and robust — medians
  and trimmed means over distinct compilation classes, never sums over rows;
- **attribution is a claim, not a fact.** The `machine_id` in a payload is
  self-declared, so "these builds are Acme's" is only as good as nobody having a
  reason to lie yet;
- **a bad contributor is a filter predicate, not an incident** — every row
  carries its machine profile and client version — but we cannot revoke anyone,
  only exclude after the fact.

Two things make this reversible rather than a trap:

1. **Every row is tagged with how it arrived** (`trust: anonymous` today) from
   the first row stored. Adding tiers later is then a filter, not a migration,
   and the pre-auth data does not have to be thrown away or silently mixed in.
2. **The endpoint is not advertised** until auth exists. Authless is fine while
   contributors are people we asked directly; it stops being fine the moment a
   README says "run this and share your builds".

When it is time, the options are above: Access service tokens for organisations
that want a credential they can rotate and audit, or a Worker issuing
self-serve keys so the long tail is not gated on us answering email.

## One stream, stored verbatim

An earlier draft of this document had the client flatten each build into
`sessions` and `units` rows, on the theory that a nested array of 300 events is
not a columnar row. That was wrong twice over.

**Pipelines cannot do it.** Its SQL has `WITH`, `SELECT`, `WHERE` and `UNNEST`,
but no joins and no aggregation. `UNNEST(events)` produces one row per *event*,
and our events are heterogeneous — `unit-registered` carries the package id and
features, `unit-finished` carries elapsed time, `unit-section-finished` carries
the frontend/codegen split — all correlated by `index`. One row per unit is a
self-join, which is not on the menu.

**Nor should it.** Streams can be **unstructured**: a single `value` column
holding any valid JSON, no validation. And structured streams have a property
that settles the argument — *"schema modifications are not supported after
stream creation"*. Declaring a schema today freezes today's field names into
infrastructure we cannot alter, while cargo's log format is explicitly still
moving and every field it gains would be silently dropped. That is exactly the
mistake this project's collection principle exists to prevent: measurement
happens once, parsing happens forever.

So v1 is one unstructured stream, one R2 sink, and no transform SQL at all:

```
cargo-cratebank ──POST──▶ [stream: unstructured] ──▶ [sink: R2 parquet, zstd] ──▶ r2://cratebank/raw/
```

The payload lands as one row per build with its JSON intact. Nothing is
interpreted at ingest, so nothing can be lost by interpreting it wrongly.

Anything that wants rows rather than payloads reads them out later, with a real
query engine that has joins. Correlating `unit-registered` with `unit-finished`
by index is trivial there and impossible in streaming SQL — and, unlike a
transform baked into the pipeline, a reading done later can be corrected and
rerun over everything already collected.

## Limits, and the one that binds

Open-beta limits: **5 MB per request**, 5 MB/s per stream, 20 streams / 20
pipelines / 20 sinks per account.

Measured against real payloads: ~398 bytes per event, so a 1,000-unit build
projects to **≈3.0 MB in one request** — under the limit, but not by much, and
the fleet's largest projects are that size. Two consequences:

The client must **batch** if the limit counts decompressed bytes: split
oversized builds across several requests, with the session header on each chunk
so partial delivery stays interpretable. If it counts compressed bytes, the
largest builds are ~260 KB and batching is unnecessary — see *Payload size*
below.

The 5 MB/s per-stream ingest rate is the one to watch at scale: it is roughly
150 medium builds per second, which is a long way off, but it is a per-stream
cap and the fix is more streams (20 allowed) or a limit-increase request.

## Payload size

Measured on a real session (91 events, 43 units):

| | bytes | vs compact |
| --- | --- | --- |
| compact JSON | 25,928 | — |
| gzip -9 | 2,912 | 8.9x |
| **brotli q11 — what the client sends** | **2,455** | **10.6x** |
| slimmed JSON (no repeated `run_id`, delta timestamps) | 18,829 | 1.4x |
| slimmed **and** gzipped | 2,539 | 10.2x |

Three conclusions, in order of how much they matter.

**Compress the request body — done.** The client sends brotli q11 with
`Content-Encoding: br`, measured at 10.6x on a real session (25 KB → 2.4 KB) and
16% better than gzip. The `brotli` crate is pure Rust, so nothing needs a C
toolchain — which matters on Windows, where the one dependency that does
(`ring`, via TLS) is already the hard part of cross-building.

**No negotiation and no fallback.** The endpoint is treated as the dumbest thing
that could work: it takes a blob and stores it, and anything that needs to read
the contents does so later. If a send fails it is not recorded as sent, so the
session stays queued and goes out next time — a failed upload costs a retry, not
a contribution, which is what makes the simplicity affordable.

Inbound `Content-Encoding` remains undocumented for Pipelines (the docs and
launch blog cover compression only for sink *output*; the one GZIP-on-ingest
reference belongs to the pre-Arroyo API). If it turns out the endpoint stores
the compressed bytes rather than decompressing them, that is equally fine —
processing happens later either way.

**Do not slim the payload.** Dropping the `run_id` repeated on every event and
delta-encoding timestamps removes 27% of the raw bytes — those two fields are a
third of the payload — but almost nothing once compressed, because repeated
strings are exactly what LZ already collapses. That is a poor trade for reintroducing
client-side decisions about the payload's shape, which this design just removed.
It becomes worth doing only if compression turns out to be unavailable.

**Do not send parquet from the client.** It would be smaller again — columnar,
dictionary-encoded, zstd — but the ingest endpoint takes JSON, so parquet means
writing to R2 directly, which needs credentials, which needs a Worker. That
trades the no-code ingest for a marginal gain over gzip, and it puts schema
decisions back in the client where a released binary freezes them. The place
parquet belongs is the sink, where it already is.

### One open question

Whether the 5 MB request limit counts compressed or decompressed bytes. The
limits page states "Maximum payload size per ingestion request: 5 MB" and says
nothing about compression state — the word does not appear on the page.

It decides real work: if compressed, the largest builds land near 260 KB and the
client needs **no batching at all** for v1; if decompressed, batching stays as
designed. One request against the live endpoint settles it, and it should be the
first thing tried once an account exists.

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
  raw/year=2026/month=09/day=03/*.parquet     # what arrived, verbatim
```

Hive-style partitioning is what every engine expects, so
`SELECT … FROM 'https://data.cratebank.io/raw/**/*.parquet'` works in DuckDB
directly, and partition pruning happens automatically for date-filtered
queries. R2's zero egress is what makes serving that publicly sane.

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

**Batching, and nothing else.** The payload shape stays as it is, because the
stream stores it verbatim. Split builds larger than ~4 MB across several
requests, keep the session header on each chunk, and that is the whole change.

`--dry-run` keeps printing exactly what would be sent, and
`cargo cratebank serve` remains the reference collector.
