# Ingest and storage

The ingest path is deliberately small:

```text
cargo-cratebank --POST zstd blob--> Worker --put()--> r2://cratebank/sessions/year=/month=/day=/*.json.zst
                                                    |
                                                    +--> daily compactor --> public parquet
```

The Worker does not decompress, parse, validate, or interpret the body. The
exact bytes sent by the client are stored in R2. Parsing and modelling happen
during compaction, where they can be corrected and rerun over all raw data.

## Endpoint

The client sends one build session per request:

```http
POST https://ingest.cratebank.io/v1/sessions
Content-Type: application/zstd
Content-Length: <bytes>

<zstd-compressed session JSON>
```

Do not set `Content-Encoding`; the zstd payload is the stored object rather
than an HTTP representation that an intermediary may decode.

The endpoint requires `Content-Length`, rejects empty bodies, and streams the
request directly to the R2 `BUCKET` binding. A successful response is:

```json
{"success":true,"key":"sessions/year=2026/month=08/day=22/<uuid>.json.zst"}
```

The command succeeds only after a successful response. A failed upload exits
with an error.

`GET` and `HEAD` requests redirect to `https://cratebank.io/`. `/v1/sessions`
is the version boundary for the payload contract.

## Authentication and trust

`v1` is unauthenticated. Contributors need no account, token, or signup. Every
submission is tagged `trust: anonymous` so authenticated tiers can remain
distinct if they are added.

This has direct analytical consequences:

- submission counts are not trustworthy;
- `machine_id` and attribution are self-declared;
- estimates must use robust per-class statistics rather than raw submission
  totals; and
- unwanted contributors are excluded by their recorded attributes after
  ingestion.

Do not put an account-level Cloudflare or R2 credential in the public client.

## Stored payload

Each request becomes one immutable object:

```text
cratebank/
  sessions/year=2026/month=08/day=22/<uuid>.json.zst
```

The Hive-style date partitions let credentialed S3 clients prune raw objects
by date. Listing raw objects requires R2 credentials, even though known object
URLs are publicly readable through `data.cratebank.io`.

The client sends schema-2 JSON compressed with zstd level 19. DuckDB reads
zstd JSON natively, so the raw object is queryable without a conversion step.
The payload contains the parsed Cargo timing report, parsed samply phase data,
and their shared build context. The client does not batch or negotiate codecs.

The request-size ceiling is the Cloudflare zone's request-body limit: 100 MB
on Free and Pro, 200 MB on Business, and 500 MB on Enterprise. The Worker
streams into R2 and does not buffer the full body in its 128 MB memory limit.

## Public data

Raw blobs are the ground truth. A Worker scheduled for `0 5 * * *` rebuilds
the public parquet tables at fixed URLs:

| file | grain |
| --- | --- |
| `sessions.parquet` | one row per build session |
| `units.parquet` | one row per compilation unit |
| `phases.parquet` | sampled compiler phase, thread class, and unit |
| `timeline.parquet` | build concurrency and CPU point |
| `unit_flags.parquet` | resolved setting and compilation unit |

The schema is published at
`https://data.cratebank.io/schema/v2/tables.json`. Dated copies are stored
under `snapshots/`.

The fixed public URLs require no R2 credentials:

```sql
INSTALL httpfs;
LOAD httpfs;

SELECT package, phase, sum(samples)
FROM 'https://data.cratebank.io/phases.parquet'
WHERE thread = 'serial'
GROUP BY 1, 2
ORDER BY 3 DESC;
```

The compactor maps Cargo timing units, samply phases, and Cargo timeline points
into public tables, coerces input fields at the table boundary, and writes the
schema alongside the tables. Workers use the bundled `fzstd` decoder and
`hyparquet-writer`.

Compaction is a full rebuild and holds its output rows in memory. Monitor the
`objects` and `bytes_in` cron metrics against the Worker's 128 MB limit.

## Cost and abuse controls

Storage traffic has no egress charge. Variable cost is primarily R2 storage,
one Class A `PutObject` per submission, and the Class B reads performed by the
daily full rebuild. Operations scale with the number of sessions rather than
their compressed size.

The public endpoint relies on Cloudflare WAF rate limiting by IP. Treat all
anonymous observations as untrusted input: keep estimates robust, retain
machine and client metadata for filtering, and never execute or deserialize a
payload outside the constrained compaction parser.

## Client contract

The client compresses one session with zstd and posts it to
`https://ingest.cratebank.io/v1/sessions`. `--dry-run` prints the payload, and
`cargo cratebank serve` is the reference collector. The reference collector
detects zstd from its magic bytes so its behavior matches stored-data
readability rather than relying on an HTTP header.
