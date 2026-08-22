# Public schema

Each successful `cargo cratebank build` produces one schema-2 payload that
contains parsed stable Cargo timings and parsed samply output from the same
build. Daily compaction publishes five Parquet tables at fixed URLs under
`https://data.cratebank.io/`.

The machine-readable schema is
`https://data.cratebank.io/schema/v2/tables.json`.

## `sessions.parquet`

One row per sampled build. It contains the run id, payload and client versions,
Cargo report summary, machine and load context, completeness, and retained and
withheld unit counts.

## `units.parquet`

One row per public Cargo timing unit. It contains package name and version,
mode, target, features, start and finish, duration, frontend/codegen/link wall
spans, and the number of units unblocked.

## `phases.parquet`

One row per public samply unit, thread class, and compiler phase. `samples` is
a CPU-weighted sample count. `wall_s` is the rustc process duration. Serial and
parallel codegen threads stay separate.

## `timeline.parquet`

One row per Cargo concurrency or CPU sample. Cargo records those series on
different clocks, so a row contains either `active`/`waiting`/`inactive` or
`cpu_pct`.

## `unit_flags.parquet`

One row per public samply unit and scrubbed compilation setting. Paths and full
command lines are absent; features are comma-separated and incremental is only
a boolean.

All tables join through `run_id`. Cargo section values in `units.parquet` are
wall-clock durations. Samply phase values in `phases.parquet` are CPU-weighted
counts and must not be treated as the same quantity.

Raw schema-2 payloads remain stored as zstd JSON under date-partitioned
`sessions/` keys. Source code, private unit identity, and filesystem paths are
not uploaded.
