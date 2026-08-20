# cratebank

**A continuous census of Rust compilation as it actually happens.**

Nobody can currently answer basic questions about how Rust is really built.
What fraction of builds use `lld`, or `mold`, or the default linker? What
opt-levels and LTO settings do people actually use, as opposed to what the
documentation recommends? What does `tokio` cost to compile on a real laptop
versus a CI runner? Today's answers come from a self-reported annual survey, a
synthetic benchmark suite, or download counts — none of which measure
compilation as it happens.

cratebank measures it, by collecting the build timings people are **already
producing**. Nothing extra is compiled; the work was happening anyway.

Collection and analysis are deliberately separate:

- **cratebank collects.** Capture generously, model nothing at ingest.
- **Studies model.** Cost models, causal experiments, regression detection are
  built *on top of* the data and can be redone as understanding improves,
  without re-collecting anything.

## Contributing a build

Requires a nightly toolchain (cargo's build-analysis flags are unstable).

```sh
cargo install cargo-cratebank      # not yet published; see cargo-cratebank/
cd your-project
cargo cratebank enable             # writes the opt-in and cargo's analysis flags
cargo cratebank status             # confirm it is wired up
```

From then on, an ordinary `cargo build` produces a session log that gets
shipped. Run `cargo cratebank watch` in the background if you want *every*
build captured (see [the client README](cargo-cratebank/README.md) for why the
`build.rs` trigger alone is best-effort).

Before trusting it with anything, look at exactly what it would send:

```sh
cargo cratebank send --dry-run     # byte-for-byte payload, sends nothing
cargo cratebank serve              # run your own collector on localhost
```

Opting out is `share = false`, or deleting the metadata key.

## What is collected

Per build, a **build-environment snapshot** — toolchain (with commit hash and
LLVM version), target, profile, jobs and cores, CI or local — plus **per-unit
timings** for every compilation unit in the graph, each with its resolved
features, platform and mode, including rustc's frontend/codegen section
boundaries.

Not captured yet: `RUSTFLAGS` and the linker in use. Both matter for a build
census and both live in environment variables and config that can contain local
paths, so they need a deliberate design rather than a blanket read.

Every unit is keyed by a **compilation class**: a
`(package, version, features, cone, profile, flags, target)` fingerprint. The
same dependency built by thousands of people is recognisably the same
individual, which is what makes cross-machine and cross-time comparison work at
all. The class id deliberately excludes the toolchain, so each release also
becomes an ecosystem-scale compiler-regression instrument: *same class, new
nightly, what moved?*

### Only public units are uploaded

Everything else is dropped **entirely** — not a name, not a hash, not a timing,
not a graph edge. Dropping a unit also removes every event that referenced it
and prunes every dependency edge pointing at it, so no orphaned indices remain.

| | uploaded? |
| --- | --- |
| public dependencies (crates.io, public git remotes) | yes, full identity |
| your own workspace crates | **no**, unless the project declares itself public |
| private registries, local paths | never |
| paths, env vars, command-line values, source | never |

A local path is indistinguishable from private code, so a project opts in
explicitly to publish its own crates:

```toml
[package.metadata.cratebank]   # or [workspace.metadata.cratebank]
share = true
public = true
repository = "https://github.com/you/project"
```

Those units are then linked as `workspace#name@version` — by repository, never
by the path they were built from.

The only trace of withheld code is a `units_withheld` count, kept so a partial
graph is visibly partial rather than silently truncated.

A closed-source shop can therefore contribute the bulk of what matters — what
tokio, serde and diesel actually cost in their environment — while disclosing
nothing about their own code.

## How it works

The client instruments nothing. Cargo already does it: `-Zbuild-analysis`
writes a JSONL session log per invocation to `$CARGO_HOME/log/`, and
`-Zsection-timings` folds rustc's frontend/codegen section boundaries into the
same stream. `cargo cratebank` reads those logs, drops everything non-public,
and POSTs the rest.

Consequences worth stating: nothing sits in the compile path, there is no
conflict with `sccache` or any other `RUSTC_WRAPPER`, and no build is ever run
on your behalf. Cache hits are recorded as cache events with no timing claim; a
miss is a genuine compile and measured as one.

Cargo's log schema is explicitly still evolving, so events pass through
verbatim under a versioned envelope rather than being normalised client-side.

## The family

**cratebank.io** — the census is public: the data, the queries, and the client.

| repo | role |
| --- | --- |
| **cratebank** (this) | the census: schema, collection design, the `cargo-cratebank` client, ingest, publication |
| **whyslow** | `cargo whyslow` — measures one build in depth and explains why it was slow; the diagnostic counterpart |
| **crategen** | synthetic Rust workspaces with controlled characteristics — the causal complement to observational data |

## Docs

- [`docs/collection.md`](docs/collection.md) — capture tiers, privacy, the
  build-environment census, contamination flags, and honest limits
  (observational not causal; self-selected)
- [`docs/schema.md`](docs/schema.md) — the compilation class, the keying, and
  the id-array columns that make totals summable with one `unnest`
- [`docs/capture-manifest.md`](docs/capture-manifest.md) — everything worth
  recording about a build, tagged MUST / RIDE / TGT / DRV
- [`cargo-cratebank/`](cargo-cratebank/) — the client

## Status

Early. The design docs and the client exist and are tested end to end against a
local collector; the client is not yet published to crates.io.

Not yet built: the ingest service, the stratum builder that compacts
contributed data into immutable monthly releases, and publication to object
storage (parquet, queryable directly over HTTP with DuckDB — no server).
