# Ingest and storage

**A ~60-line Worker takes a compressed blob and puts it in R2. That is the
entire ingest.**

```
cargo-cratebank ──POST zstd blob──▶ Worker ──put()──▶ r2://cratebank/sessions/year=/month=/day=/*.json.zst
                                                              │
                                                              ▼
                                                   DuckDB reads it directly
```

The Worker does not decompress, parse, validate or interpret the body. The bytes
the client compresses are the bytes stored in R2 are the bytes a query engine
reads. *Capture generously, model nothing at ingest* — enforced by there being
no code that could do otherwise.

## Why not Pipelines

This document previously specified Cloudflare Pipelines, on the grounds that it
gave us JSON-over-HTTP, SQL transforms, and parquet on R2 as pure configuration
with no code to own. That was a good trade on paper. It was built, deployed,
measured against, and abandoned the same day, for reasons only a live account
could reveal:

| | Pipelines | Worker |
| --- | --- | --- |
| Max request | 5 MB, **decompressed** | 100 MB (Free/Pro plan limit) |
| Max single message | **1 MB** — undocumented | n/a |
| Compression accepted | gzip only | anything; we never decode it |
| Failure mode | accepts, then **silently drops** | `put()` succeeds or throws |
| Time to queryable | 300s sink roll | immediate |

The binding constraint was the 1 MB per-message cap, which appears on no
documentation page and only exists as error code `1018`. The client sends one
object per session, so an entire build counted as a single message — capping
submissions at roughly a third of what this document projects a large build to
produce. Escaping that meant writing a client-side batching layer, which is a
codebase, a deploy story and a schema-migration story of its own. The Worker is
smaller than the batching layer would have been.

The "no server code" property was real and worth wanting. It cost 60 lines to
give up, and it bought a 100× headroom increase and the removal of an entire
class of silent failure.

## The endpoint

The client posts to **our** hostname, never Cloudflare's:

```
POST https://ingest.cratebank.io/v1/sessions
Content-Type: application/zstd

<zstd-compressed session JSON>
```

One session, one request, one object in R2. The body is a compressed blob, not
a compressed *representation* of a JSON request — hence `content-type:
application/zstd` and deliberately **no** `Content-Encoding` header, which would
invite an intermediary to decode the body before the Worker sees it.

A Workers custom domain binds `ingest.cratebank.io` to the script and manages
its own DNS record. The indirection still matters for the reason it always did:
a released client hardcodes this name, contributors upgrade slowly, and the name
they were shipped with has to keep working however the implementation behind it
moves. `/v1/` is the version boundary — the Worker owns the path, so a `/v2/`
with a different payload shape can run alongside it.

Responses are JSON. `200 {"success":true,"key":"sessions/year=…"}` on success;
the key is returned so a contributor can see exactly what was stored.

Authentication is off in v1: a contributor needs no account, no token, no
signup.

## Authentication

Nothing that ships inside a public binary can be an account-level credential:
it would hand every contributor a key to the account, and revoking it would
break every client at once. That rules out the obvious answers and leaves the
question of what the Worker should check.

| option | code | credential | revocable | self-serve | notes |
| --- | --- | --- | --- | --- | --- |
| none (public endpoint) | none | — | — | — | submission counts untrustworthy; **today** |
| **Access service tokens** | **none** | client id + secret, per consumer | per token | no — we issue them | config only; ideal for organisations |
| **our tokens, checked in the Worker** | ~20 lines | our token, per contributor | per token | yes | most flexible; see below |
| GitHub device flow in the Worker | more | GitHub identity | per user | yes | strongest identity, most machinery |
| mTLS / API Shield | none | client cert | per cert | no | too heavy for volunteers |

Two are worth taking seriously.

**Cloudflare Access service tokens** are pure configuration: a Service Auth
policy on the ingest hostname, and callers present `CF-Access-Client-Id` and
`CF-Access-Client-Secret`. Per-consumer, individually revocable, no code at all.
The catch is that *we* issue every token, so it fits organisations ("here is
Acme's credential") and not a volunteer who wants to contribute this afternoon.

**Checking a token in the Worker** is the general answer, and it now costs
almost nothing: the Worker already exists and already reaches R2 through a
binding — `await env.BUCKET.put(...)` — so there is no shippable secret anywhere
in the request path. Authentication is one `if` before the `put`.

Tokens can be stateless: `token = id.HMAC(id, secret)`, verified with one
secret in the Worker, no database. Revocation is a small KV denylist.
Registration can be instant and anonymous (`cargo cratebank register`), which
makes a token less an identity than a **handle** — something to rate-limit
against and revoke, which an IP is not.

### The Worker is already there

This section used to argue that a Worker would be worth building *if* we ever
needed auth. That argument is settled — the Worker exists for unrelated reasons,
so adding auth is now a change to a file we already own rather than a new
component. Concretely, it means:

1. **Verification costs a few lines, not an architecture.** Checking an HMAC
   header before `env.BUCKET.put()` is a small edit to `infra/worker/ingest.js`.
2. **No shippable secret.** The Worker reaches R2 through a binding, so nothing
   in the request path needs a credential that could be extracted from a public
   binary — which was the objection that ruled out every other option above.
3. **A version boundary already exists.** `/v1/sessions` is a path the Worker
   controls; auth can land on `/v2/` while `/v1/` keeps accepting anonymous
   submissions from older clients.

### Decision: start authless

**v1 ships with no authentication.** A public Worker endpoint, a WAF rate limit,
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

1. **Every submission is tagged with how it arrived** (`trust: anonymous` today)
   from the first object stored. Adding tiers later is then a filter, not a migration,
   and the pre-auth data does not have to be thrown away or silently mixed in.
2. **The endpoint is not advertised** until auth exists. Authless is fine while
   contributors are people we asked directly; it stops being fine the moment a
   README says "run this and share your builds".

When it is time, the options are above: Access service tokens for organisations
that want a credential they can rotate and audit, or a Worker issuing
self-serve keys so the long tail is not gated on us answering email.

## One blob per build, stored verbatim

An earlier draft of this document had the client flatten each build into
`sessions` and `units` rows, on the theory that a nested array of 300 events is
not a columnar row. That was wrong twice over.

**It needs a join, at the edge, where there is none.** Our events are
heterogeneous — `unit-registered` carries the package id and features,
`unit-finished` carries elapsed time, `unit-section-finished` carries the
frontend/codegen split — all correlated by `index`. One row per unit is a
self-join. That was impossible in Pipelines SQL, and in a Worker it would be
possible but wrong: it is real parsing logic, running once, at the only moment
that cannot be re-run.

**Nor should it.** Declaring a shape today freezes today's field names, while
cargo's log format is explicitly still moving and every field it gains would be
silently dropped. That is exactly the mistake this project's collection
principle exists to prevent: measurement happens once, parsing happens forever.

So v1 stores the payload byte-for-byte and interprets nothing:

```
cargo-cratebank ──POST zstd blob──▶ [Worker: put()] ──▶ r2://cratebank/sessions/
```

Nothing is interpreted at ingest, so nothing can be lost by interpreting it
wrongly. This is not a compromise forced by a limitation — it is the same
conclusion the Pipelines design reached, arrived at with fewer moving parts.

Anything that wants rows rather than payloads reads them out later, with a real
query engine that has joins. Correlating `unit-registered` with `unit-finished`
by index is trivial there and impossible in streaming SQL — and, unlike a
transform baked into the pipeline, a reading done later can be corrected and
rerun over everything already collected.

## Limits

The Worker's ceiling is the **request body size for the zone's plan**: 100 MB on
Free and Pro, 200 MB on Business, 500 MB on Enterprise. Nothing else in the path
binds — the body streams to R2 without being buffered, so the Worker's 128 MB
memory limit is not a second ceiling, and CPU time is irrelevant for a handler
that never decodes the payload.

Measured on a real 115-unit build: 1.6 KB per unit uncompressed, so a
1,000-unit build projects to **≈1.6 MB** in one request before compression, and
~115 KB after it. Against 100 MB neither figure is a constraint, so **the client
does not batch** — it compresses one session and posts it.

For contrast, the same projection against the Pipelines limits this document
used to specify: 1.6 MB sat under the 5 MB request cap but was **well over the
undocumented 1 MB per-message cap**, and the client sends one object per
session, so a large build would have been rejected outright. Batching existed as
a requirement solely to work around that, and the Worker deleted the
requirement rather than satisfying it.

What to watch instead, at scale, is R2 Class A operations: one `PutObject` per
submission, priced at $4.50/million with the first million each month free. See
*Cost*.

## Payload size

Measured on a real session — cratebank building itself, 957 events, 115 units,
204 sections:

| | bytes | vs compact |
| --- | --- | --- |
| compact JSON | 184,690 | — |
| **zstd -19 — what the client sends** | **13,325** | **13.9x** |

An earlier, much smaller session (91 events, 43 units) gave the comparison
across codecs:

| | bytes | vs compact |
| --- | --- | --- |
| compact JSON | 25,928 | — |
| gzip -9 | 2,912 | 8.9x |
| brotli q11 | 2,455 | 10.6x |
| slimmed JSON (no repeated `run_id`, delta timestamps) | 18,829 | 1.4x |
| slimmed **and** gzipped | 2,539 | 10.2x |

zstd beats every one of those ratios on the larger session, which is what should
be expected: the bigger the build, the more repeated structure there is for the
window to find. Note also that the per-event cost falls with size — 193 bytes
per event here against the ~398 this document projects from the small sample —
so extrapolations from short builds overestimate.

Three conclusions, in order of how much they matter.

Three conclusions, in order of how much they matter.

**Compress the body, with zstd, because DuckDB can read it.** The Worker stores
the blob byte-for-byte, so the codec is not a transport detail — it is the
on-disk format in R2 forever, and it decides whether the data is queryable
without a conversion step:

| codec | `read_json_auto()` |
| --- | --- |
| none | reads |
| gzip | reads |
| **zstd** | **reads** |
| brotli | **fails** — `Invalid Input Error: Malformed JSON` |

That table is why the client does not send brotli, despite brotli winning on
ratio. A brotli blob in R2 is opaque: querying it would require a decompress-
and-rewrite pass, which is exactly the server-side processing this design exists
to avoid. zstd gets brotli-class ratios and stays directly readable, so a
contribution is queryable the moment it lands.

Two historical notes, since both cost real time. Brotli was chosen originally
for ratio and for being pure Rust; **Pipelines rejected it outright** (`1003
Must be valid UTF-8 JSON`, along with `zstd` and `deflate` — it decompressed
gzip and nothing else), so every submission from that client would have failed.
And the zstd crate *is* a C dependency, which is acceptable only because `ring`
— pulled in by ureq → rustls for TLS — already requires a C toolchain to build
this crate at all. If the TLS provider is ever swapped for a pure-Rust one, zstd
becomes the blocker: there is no pure-Rust zstd **encoder**, only decoders.

**No negotiation and no fallback.** The endpoint is treated as the dumbest thing
that could work: it takes a blob and stores it, and anything that needs to read
the contents does so later. If a send fails it is not recorded as sent, so the
session stays queued and goes out next time — a failed upload costs a retry, not
a contribution, which is what makes the simplicity affordable.

The Worker never inspects `Content-Encoding` at all — it stores what it is
given — so the only thing that matters is whether a query engine can read the
result. That is a property of the file, not of the transport, which is why the
table above is about DuckDB rather than about HTTP.

**Do not slim the payload.** Dropping the `run_id` repeated on every event and
delta-encoding timestamps removes 27% of the raw bytes — those two fields are a
third of the payload — but almost nothing once compressed, because repeated
strings are exactly what LZ already collapses. That is a poor trade for reintroducing
client-side decisions about the payload's shape, which this design just removed.
It becomes worth doing only if compression turns out to be unavailable.

**Do not send parquet from the client.** It would be smaller again — columnar,
dictionary-encoded, zstd. The old objection (the endpoint only takes JSON) no
longer applies, since the Worker stores whatever bytes arrive and would accept
parquet without noticing. The remaining objection is the one that actually
mattered: parquet is a *schema*, and putting it in the client freezes cargo's
still-moving field names into a released binary. Columnarising is a re-runnable
step over data already collected; doing it at capture time is the one place the
decision cannot be revised.

### Why those Pipelines limits are recorded here

Kept because they were expensive to learn and are invisible from the
documentation. All measured against a live stream:

| probe | result |
| --- | --- |
| 9 MB payload, gzipped to 8,818 B on the wire | `413 Body must not exceed 5 MB` |
| 1 message, 900 KB | `200 committed:1` |
| 1 message, 1.1 MB | `400 1018` — message exceeds 1 MB |
| 6 messages x 700 KB (4.2 MB) | `200 committed:6` |

Eight kilobytes on the wire, refused for exceeding 5 MB: **the limit counted
decompressed bytes**, so compression bought nothing against it. Combined with
the undocumented 1 MB per-message cap, that is what made the Worker the smaller
option.

### `committed` was an ingestion receipt, not a delivery receipt

The trap that cost most of a day, recorded so nobody re-derives it. Pipelines
answered `200 {"success":true,"result":{"committed":N}}` for events it then
**silently discarded** on delivery to the sink — documented Cloudflare behaviour
for events that do not match the stream schema. Every probe reported success
while the bucket stayed empty, with no error on any surface. The count was
visible only via the `pipelinesUserErrorsAdaptiveGroups` GraphQL dataset, which
needs **Account Analytics · Read** on the API token: 18 events, `missing_field`.

The Worker has no equivalent failure mode. `env.BUCKET.put()` either succeeds or
throws, the handler returns 500 on a throw, and the client only marks a session
sent on a 2xx — so a failure leaves the session queued for the next run.

## Storage layout

The Worker builds the key; there is no sink configuration and no roll interval,
because there is no buffering — an object appears the moment the request
succeeds.

```
cratebank/
  sessions/year=2026/month=08/day=21/<uuid>.json.zst   # what arrived, verbatim
```

Hive-style partitioning is what every engine expects, so date-filtered queries
prune partitions automatically:

Hive partitioning means a credentialed reader prunes by date for free:

```sql
-- needs R2 credentials: the S3 API lists objects, which is what expands a glob
SELECT run_id, counts.events
FROM read_json_auto('r2://cratebank/sessions/**/*.json.zst', union_by_name=true);
```

That works because zstd is one of the codecs DuckDB decompresses natively — the
uploaded bytes are the queried bytes, with no conversion step between
contribution and query. Verified end to end: a blob written by
`cargo-cratebank` through the live endpoint reads back with its structs and
arrays intact.

**A glob cannot be served publicly.** Expanding `**` requires listing, listing
is an API call, and the API call is what needs credentials — so
`https://…/sessions/**/*.json.zst` does not work, and DuckDB says so rather
than failing quietly:

```
Consider `SET allow_asterisks_in_http_paths = true;` to allow this behaviour
```

That flag is a trap: it passes the asterisks through *literally* and requests a
file named `**`, returning 404. The same reasoning rules out R2 Data Catalog,
whose Iceberg REST endpoint is a service and therefore authenticated: "Iceberg
clients must authenticate to the catalog with an R2 API token". The public
surface has to be a *static file at a fixed URL* — see *Compaction*.

One object per submission, rather than the rolled-up files a sink produced. That
is the trade for immediacy, and it moves the scaling question from bytes to
Class A operations — see *Cost*. If small-file count ever becomes a problem, a
scheduled compaction job into parquet is a separate, re-runnable step, which is
precisely the property this design keeps insisting on.

## Compaction: the public surface

The raw blobs are the ground truth, but they are not a usable public interface:
reading them needs credentials, because listing needs credentials. So a nightly
Worker (`infra/worker/compact.js`, cron `0 5 * * *`) rebuilds two flat parquet
tables at fixed URLs:

| file | one row per | columns |
| --- | --- | --- |
| `units.parquet` | compilation unit | `run_id`, `crate`, `mode`, `package_id`, `elapsed`, `frontend`, `codegen`, `link`, `unblocked` |
| `sessions.parquet` | build | machine profile, load, rustc, counts, `complete` |

```sql
SELECT crate, round(elapsed, 3) AS secs
FROM 'https://data.cratebank.io/units.parquet'
ORDER BY elapsed DESC LIMIT 10;
```

No credentials, no glob, no extension beyond `httpfs`, and DuckDB range-requests
only the columns it reads. Dated copies land in `snapshots/` so a bad run can be
diagnosed against the file it actually produced.

**The unit table is the join already done.** Correlating `unit-registered` with
`unit-finished` by `index` is the self-join the ingest design deliberately
defers; doing it here rather than in the client is what makes it correctable —
a flattening bug is fixed by fixing this Worker and waiting a day, not by
re-collecting data that no longer exists.

Two implementation notes, both found by running it:

- **Workers cannot decompress zstd.** `DecompressionStream` supports only
  gzip/deflate/deflate-raw, so the Worker bundles `fzstd` (80 KB, pure JS).
  With `hyparquet-writer` the whole bundle is 66 KB minified — no WASM, which
  is what makes parquet-in-a-Worker reasonable at all.
- **The column builders coerce rather than trust.** `cratebank_schema` is a
  number where a string was assumed, and it threw at write time after every
  blob had been decompressed. Since cargo's log format is explicitly still
  moving, one drifted field must not cost everyone the nightly rebuild.

It is a full rebuild, not an incremental merge, and it holds every row in
memory. That is free at this scale and will not be: the Worker's ceiling is
128 MB, so roughly 10k sessions. The escape hatch is per-day compaction into
`daily/YYYY-MM-DD.parquet` merging only unmerged days, which the hive layout
already supports. The `objects` and `bytes_in` counters in the cron log are what
tell you it is coming.

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

No sink fees, because there is no sink. What remains:

| | price | free tier |
| --- | --- | --- |
| Workers Paid | $5/month | 10M requests included |
| R2 storage | $0.015/GB-month | 10 GB-month |
| R2 Class A (one `PutObject` per submission) | $4.50/million | 1M/month |
| R2 egress | **free** | — |

At ~3 KB compressed per session, a million contributed builds a month is roughly
3 GB of storage and 1M Class A operations — which is to say, inside the free
tier on both, on top of the $5 that was already being spent. The public query
surface costs nothing to serve, which is the property that makes "the census is
public" affordable rather than aspirational.

Operations, not bytes, are the thing to watch: one object per submission means
cost scales with *number of builds*, not their size.

Compaction adds one full read of the bucket per night — N Class B operations
plus four Class A writes — so at a million sessions it is ~30M Class B a month,
around $11. That is the first real bill this design produces, and it is also the
point at which per-day compaction stops being optional.

## What has to change in the client

**Nothing.** This section previously called for a batching layer; the Worker's
100 MB ceiling removed the requirement before it was written.

The client compresses one session with zstd and posts it to
`https://ingest.cratebank.io/v1/sessions`. `--dry-run` prints exactly what would
be sent, and `cargo cratebank serve` remains the reference collector — it now
sniffs the zstd magic number rather than trusting a header, because the header
says nothing about whether the bytes in R2 will be readable.
