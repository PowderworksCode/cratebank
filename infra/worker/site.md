# cratebank

A public census of where Rust build time actually goes.

## Why

Everyone has a theory about what makes Rust builds slow — generics, macros,
LLVM, linking. Very little of it is measured, and almost none of it is measured
across the enormous range of machines and crate graphs that real builds happen
on.

cratebank collects the timings your builds already produce, plus a sampled
breakdown of what the compiler was doing, and publishes the result so anyone
can query it. The same dependencies get compiled on thousands of machines;
that overlap is what makes the comparison work.

## Getting started

```sh
cargo install cargo-cratebank
cargo install samply          # the profiler that measures compiler phases

cargo cratebank build         # builds, measures, and sends
```

> Needs a nightly toolchain for cargo's build-analysis logs. Without samply it
> still works — you get the build and the timings, just no phase breakdown.

## What gets sent

- **Public crates only.** Anything not from crates.io or a public git remote is
  dropped entirely — not the name, not a hash, not a timing. Only a count of
  how many units were withheld survives, so the graph is honestly marked as
  partial.
- **No paths, ever.** Working directory, target directory and manifest path are
  stripped; compiler flags are kept but their path values are not.
- **Your own crates are private by default.** Publishing them takes an explicit
  opt-in: `[package.metadata.cratebank] public = true`.

> Nothing sits in your compile path, so there is no conflict with `sccache` or
> any other `RUSTC_WRAPPER`, and no build is ever run on your behalf.

## Using the data

Everything is public parquet on R2 — no account, no API key, no egress cost.
Query it straight from [DuckDB](https://duckdb.org):

```sql
INSTALL httpfs;
LOAD httpfs;

SELECT package, phase, sum(samples) AS samples
FROM 'https://data.cratebank.io/phases.parquet'
WHERE thread = 'serial'
GROUP BY 1, 2
ORDER BY samples DESC;
```

Five tables, described by a machine-readable schema:

- `sessions.parquet` — one row per build
- `units.parquet` — one row per compilation unit
- `phases.parquet` — sampled compiler phases per unit
- `timeline.parquet` — build concurrency and CPU over time
- `unit_flags.parquet` — the settings each unit was built with

[schema/v1/tables.json](https://data.cratebank.io/schema/v1/tables.json)
describes every column, and carries the warnings that matter — chiefly that
sampled phases are *CPU* while the section boundaries in `units` are *wall
clock*, and the two are not interchangeable.

---

[Source on GitHub](https://github.com/PowderworksCode/cratebank) · opt-in ·
public domain data
