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

1. **Emit rows, not one nested object** — a `sessions` row, N `units` rows, and
   the raw log, each posted to its own stream.
2. **Batch** to stay under 5 MB per request.
3. **Endpoint config per stream**, replacing the single `--endpoint`.
4. Keep `--dry-run` printing exactly what would be posted, per stream.

Until those land, `cargo cratebank serve` remains the reference collector and
the payload shape is unchanged.
