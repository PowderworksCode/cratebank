# cratebank

**A continuous census of Rust compilation as it actually happens.**

cratebank measures real Rust builds across projects, machines, profiles, and
targets. Cargo supplies wall-clock unit and concurrency measurements; samply
supplies the CPU-weighted compiler-phase breakdown. The client parses both
outputs from the same build and uploads one privacy-filtered payload.

The public data supports questions such as which compiler phases dominate,
which dependencies are consistently expensive, how build settings differ in
practice, and how the same compilation class behaves across machines and
compiler releases.

## Contributing a build

Use a stable Rust toolchain and install both required commands:

```sh
cargo install cargo-cratebank
cargo install samply

cd your-project
cargo cratebank build
```

`cargo cratebank build` runs `cargo build --timings` under samply. If Cargo,
samply, timing-report parsing, or profile parsing fails, cratebank fails and
sends nothing.

Arguments after `build` pass through to Cargo:

```sh
cargo cratebank build --release --all-features
```

Inspect the complete payload without uploading it:

```sh
cargo cratebank --dry-run build
```

`cargo cratebank status` checks that samply is installed and prints the active
endpoint. `cargo cratebank serve` runs a local reference collector.

## What is collected

Every payload combines:

- Cargo `--timings` unit records, section timings, dependency-unblocking edges,
  concurrency, and CPU timeline;
- samply phase samples attributed to each rustc process and separated into
  serial and parallel-codegen thread classes;
- compiler, target, profile, feature, machine, load, and build-configuration
  context; and
- one run id joining every part of the observation.

Nothing wraps rustc, so cratebank does not replace `RUSTC_WRAPPER` or interfere
with a configured compiler cache.

### Only public units are uploaded

Cargo metadata and `Cargo.lock` classify every timing unit before upload.
Crates.io packages and public Git dependencies are retained. Private
registries, local path dependencies, and workspace packages are removed.
Dependency edges pointing at removed units are pruned.

A project can explicitly publish its own workspace units:

```toml
[package.metadata.cratebank] # or [workspace.metadata.cratebank]
public = true
repository = "https://github.com/you/project"
```

The payload contains only a `units_withheld` count for removed units. It never
contains source, workspace paths, usernames, hostnames, or the full process
environment. Path-shaped configuration values are removed or reduced to a
program basename before upload.

The machine id is generated locally on first use. Set
`CRATEBANK_MACHINE_ID=your-org` to choose one or `CRATEBANK_MACHINE_ID=none` to
omit it.

## Public data

A daily compaction Worker turns the raw payloads into five public parquet
tables:

- `sessions.parquet` — one row per build;
- `units.parquet` — Cargo wall-clock measurements per compilation unit;
- `phases.parquet` — samply phase counts per compilation unit;
- `timeline.parquet` — Cargo concurrency and CPU over time; and
- `unit_flags.parquet` — resolved compilation settings.

Install persistent DuckDB views directly from the permanent GitHub URL, then
use the sample queries:

```sh
curl -fsSL https://raw.githubusercontent.com/PowderworksCode/cratebank/main/docs/install.sql | duckdb cratebank.duckdb
duckdb cratebank.duckdb
```

```sql
SELECT package, phase, sum(samples)
FROM phases
WHERE thread = 'serial'
GROUP BY 1, 2
ORDER BY 3 DESC;
```

The files live at `https://data.cratebank.io/` and need no account or API key.
The machine-readable table schema is
`https://data.cratebank.io/schema/v2/tables.json`.

[Download the Python starter notebook](https://raw.githubusercontent.com/PowderworksCode/cratebank/main/docs/cratebank-analysis.ipynb)
for a guided analysis of sessions, package wall time, compiler phases, build
settings, and one build timeline.

## Repository map

- [`cargo-cratebank/`](cargo-cratebank/) — the stable Cargo and samply client
- [`docs/collection.md`](docs/collection.md) — capture and privacy model
- [`docs/capture-manifest.md`](docs/capture-manifest.md) — fields captured from
  Cargo timings and samply
- [`docs/ingest.md`](docs/ingest.md) — upload, storage, and compaction
- [`docs/install.sql`](docs/install.sql) — DuckDB view installation
- [`docs/queries.sql`](docs/queries.sql) — copy-and-paste queries
- [`docs/cratebank-analysis.ipynb`](docs/cratebank-analysis.ipynb) — Python and DuckDB starter analysis
- [`infra/`](infra/) — Cloudflare infrastructure and deployment

The landing page is `https://cratebank.io/`; ingest is
`https://ingest.cratebank.io/v1/sessions`.
