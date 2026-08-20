# cratebank

**A continuous census of Rust compilation as it actually happens.**

People opt in to sharing the build timings they were already producing. Their
build environment is snapshotted alongside per-dependency timings and shipped
to ClickHouse like any other log stream. Nothing extra is compiled; the work
was happening anyway.

The result is something that does not currently exist: an ongoing,
representative record of how Rust is really built — which toolchains, targets,
linkers, profiles, feature sets, and hardware, and what each dependency
actually costs in the wild, across CI and laptops, over time.

Collection and analysis are deliberately separate concerns:

- **cratebank collects.** Capture generously, log broadly, model nothing at
  ingest. Schema-on-read.
- **Studies model.** Statistical work — cost models, causal experiments,
  regression detection — is built *on top of* the data as separate projects,
  and can be redone as understanding improves without re-collecting anything.

## What is collected

Per build: a **build-environment snapshot** (toolchain, target, profile,
linker, jobs, hardware, wrapper/cache state, CI or local) plus **per-unit
timings** for every compilation unit in the graph.

Every unit is keyed by a **compilation class** — a
(package, version, features, cone, profile, flags, target) fingerprint — so
the same dependency built by thousands of people is recognisably the same
individual, which is what makes cross-machine and cross-time comparison work.

**Public projects** are linked as themselves: repository, workspace members,
their own compile costs.

**Private projects** send their public dependency measurements and no
top-level identity — no project name, no workspace crate names, no paths.
Nearly all of the value is in the dependency graph, and that part is public
code regardless of who is compiling it.

## Two collection tiers

| tier | how | gets |
| --- | --- | --- |
| **harvest** | reads `target/cargo-timings/` artifacts cargo already writes | zero configuration, nothing in the compile path; per-unit **wall** time |
| **shim** | `RUSTC_WRAPPER=whyslow-shim`, chaining any existing wrapper | per-invocation CPU/RSS via `wait4`; comparable to controlled measurements |

Caches are handled honestly: a wrapper hit is recorded as a cache event with
no timing claim, a miss is a genuine compile and measured as one.

**cratebank.io** — the census is public: data, queries, and the client.

| repo | role |
| --- | --- |
| **cratebank** (this) | the census: schema, collection design, ingest, publication |
| **whyslow** | the client: `cargo whyslow` measures a build and explains why it was slow; contribution is a flag on a tool people already want |
| **crategen** | synthetic workspaces with controlled characteristics — the causal complement to observational data |

## Docs

- [`docs/collection.md`](docs/collection.md) — what is captured, privacy, tiers, contamination flags
- [`docs/schema.md`](docs/schema.md) — the event log and the class fingerprint
- [`docs/capture-manifest.md`](docs/capture-manifest.md) — the exhaustive field list, including deep-instrumentation fields for controlled runs

## Client

[`cargo-cratebank/`](cargo-cratebank/) — the plugin. `cargo cratebank build`
runs your build with cargo's own `-Zbuild-analysis` and `-Zsection-timings`
enabled and ships the resulting session log; `cargo cratebank send` ships logs
from builds that already happened. Nothing instruments the compile path.
