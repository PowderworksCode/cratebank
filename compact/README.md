# compact

Turns stored payloads into tables worth querying, and publishes them.

```sh
python compact/compact.py                      # R2 -> R2, from the environment
python compact/compact.py --local DIR --out D  # develop against .ndjson on disk
python compact/compact.py --dry-run            # build and report, write nothing
```

Runs hourly in CI (`.github/workflows/compact.yml`), or on demand.

## Two buckets

| bucket | contents | lifecycle |
| --- | --- | --- |
| `cratebank` | `raw/` — what arrived, JSON intact | append-only, never rewritten |
| `cratebank-published` | the tables below | replaced wholesale each run, safe to delete |

That separation is the point. Ingest interprets nothing, so a reading that turns
out to be wrong is **rerun over everything ever collected** rather than being a
permanent property of the data. A transform baked into the pipeline could only
ever have applied going forward.

## What it publishes

| table | grain | why |
| --- | --- | --- |
| `sessions` | one build | machine, toolchain, profile, load, counts |
| `units` | one compilation unit | identity, features, cpu, rss, dependency edges |
| `unit_sections` | one section | what `-Zsection-timings` reported: `codegen`, `link` |
| `unit_timeline` | one unit | the raw marks: started, codegen start/end, rmeta, finished |
| **`unit_costs`** | one unit | **start here** — units joined to sessions with the split derived |

Each is written as zstd parquet, plus `cratebank.duckdb` containing all five.

Two files means two ways in, and neither needs a server:

```sql
-- 1. the whole thing, one attach
ATTACH 'https://data.cratebank.io/cratebank.duckdb' AS cb (READ_ONLY);
SELECT * FROM cb.unit_costs LIMIT 10;

-- 2. or just the table you want, straight over HTTP
SELECT name, avg(cpu_s), avg(codegen_s)
FROM 'https://data.cratebank.io/unit_costs.parquet'
GROUP BY 1 ORDER BY 2 DESC;
```

`unit_costs` is materialised rather than left as a view, so a single downloaded
file is a usable table rather than a definition referring to four others.

## Two things the SQL has to get right

**The join Pipelines cannot do.** `unit-registered` carries identity,
`unit-finished` carries elapsed time, `unit-section-finished` carries the
codegen split — correlated only by `index`. Streaming SQL has no joins, which is
why this step exists at all.

**There is no frontend section.** `-Zsection-timings` reports `codegen` and
`link`. The frontend is what happens before codegen starts, so `frontend_s` is
derived from the timeline (`codegen_start − unit_started`), falling back to the
whole unit where codegen never begins — a `cargo check`, or a fresh unit. That
is the right answer rather than a null.

## The all-null audit

Every run reports columns that are null in *every* row, because that means the
reading is wrong rather than the data absent — a renamed field upstream shows up
here as a column that quietly stopped working, and cargo's log schema is still
moving.

Nulls in *some* rows are ordinary and expected: `cpu_s` only exists when the
rustc shim was in use, `load` only when sampled during a build, `repository`
only for projects that declared themselves public.

## Cost

DuckDB reads and writes R2 over its S3 interface directly, so there is no
download step and no local staging beyond the DuckDB file itself. Zero egress
means the published bucket can be served publicly without the query surface
becoming a bill.
