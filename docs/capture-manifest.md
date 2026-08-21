# Capture Manifest — everything worth recording about a build

The archival round is only one-time if nothing forces a redo, so this list errs
exhaustive. Archive classes: **MUST** (analysis depends on it), **RIDE** (near
free alongside an instrumented build; capture speculatively), **TGT** (costs a
extra pass; run targeted, on implicated classes only), **DRV** (re-derivable
forever from archived source; cache, never archive-critical).

Granularity: `class` = compilation-unit class (the individual), `proj` =
project view, `era` = one archival round.

## A. Identity & provenance (class) — all MUST

| field | source |
| --- | --- |
| package name, version, source id (crates.io / git+rev / path) | cargo metadata |
| resolved feature set | unit graph |
| dependency edges as class ids (the cone) | unit graph |
| resolved profile: opt-level, debuginfo, codegen-units, panic, overflow-checks, lto, incremental=false | unit graph / cargo |
| full rustc argv, verbatim, plus filtered env | the profile's processName (`samply --include-args`) |
| target triple, edition, toolchain (rustc -vV: commit hash, LLVM version) | rustc |
| source hash: crates.io checksum, or workspace commit + dirty patch | lockfile / git |
| inducing projects' Cargo.lock hashes | lockfile |
| harness versions: whyslow, crategen, extractor git revs | tools |

## B. Terminal responses (class)

| field | source | class |
| --- | --- | --- |
| CPU per rustc invocation, split by compiler phase | **the sampler** (below) — samples carry a pid and every unit is its own process, so per-class CPU becomes first-class rather than derived from wall sections | MUST |
| wall, start/end timestamps (schedule reconstruction), rmeta-emit time (pipelining point) | --timings + shim | MUST |
| max RSS, minor/major faults, voluntary/involuntary context switches, fs in/out | shim rusage | MUST |
| exit code, warning count, full stderr on failure (exclusions are data) | shim | MUST |
| jobs in flight at start, -j, core pinning mask, shard id | harness | MUST |

## C. Phase decomposition (class)

| field | source | class |
| --- | --- | --- |
| frontend/codegen sections | --timings | MUST (the conserved backbone) |
| coarse passes | -Ztime-passes-format=json | RIDE |
| full query profile: self-time, counts, cache hits/misses, blocked, incremental | -Zself-profile raw .mm_profdata, archived compressed, summaries beside | RIDE (raw is the point: our parsing improves, measurement doesn't) |
| conservation verdict: Σ decomposition vs shim CPU, tolerance, pass/fail | computed at capture | MUST |

## D. Intermediate responses (class) — the mechanistic layer

| field | source | class |
| --- | --- | --- |
| mono items: def path (→ origin crate), instantiation count, size estimate | -Zdump-mono-stats json | MUST (induced-cost attribution) |
| full mono item list with CGU assignment + linkage | -Zprint-mono-items | RIDE (exact duplication/ICF/partitioning analysis) |
| AST/HIR node counts (post-expansion volume) | -Zhir-stats | RIDE (moves "expansion volume" into the no-rebuild zone) |
| trait obligations: evaluate_obligation counts/time | self-profile (C) | RIDE |
| proc-macro expansion time per macro | self-profile activity args, best effort | RIDE |
| CGU count and per-CGU size | print-mono-items / self-profile module names | RIDE |
| LLVM pass statistics | -Cllvm-args=-stats (stderr text) | TGT (perturbs; targeted) |
| LLVM IR lines per item | cargo llvm-lines (separate emit build) | TGT |
| artifact bytes: rmeta, rlib, .o total, section sizes (.text/.data/.rodata), symbol count, debuginfo bytes | ls + size/nm on artifacts before target cleanup | MUST (cheap, excellent responses) |
| dep-info file lists (what source was actually read) | .d files | RIDE |

## E. Source variables (class) — all DRV

whyslow-metrics census + shape metrics, LOC/bytes/files, at a recorded
extractor version. Recomputed at will from archived source; never load-bearing
in the archive.

## F. Project view (proj)

| field | source | class |
| --- | --- | --- |
| unit graph json (the DAG over class ids) | cargo --unit-graph | MUST |
| cargo metadata json (features, members, licenses) | cargo metadata | MUST |
| link step: rusage, wall, linker identity+version, argv, LTO mode | shim wraps linker too | MUST |
| build-script executions: per-script rusage, stdout (cargo:: directives), rerun-if declarations | shim + cargo out dirs | MUST |
| final binary: size, per-crate bloat attribution, section sizes | cargo-bloat-style scan | RIDE |
| ICF potential | lld --print-icf-sections | TGT |
| whole-build envelope: process-tree CPU/wall/RSS + reconciliation vs Σ classes | whyslow measure | MUST |
| schedule Gantt + realized parallelism + critical path | derived from B timestamps | DRV |

## G. Environment (once per measurement run)

Machine identity (CPU model, cores, memory, kernel, governor), load+PSI
timeline for the whole run, toolchain hashes, sccache-disabled and
incremental-off attestations, filtered env snapshot, corpus list with pins,
randomization seeds and shard plan, tool git revs.

## H. Replicates

Classes deliberately built more than once (across shards or repeated by
design) are kept as rows, never averaged at capture — they are the noise
floor. The schedule position of each build (what else was running) rides
along from B.

## I. Publication

Measurements are published as parquet on object storage, partitioned by date,
with raw instrument outputs alongside. Zero-egress storage makes the query
surface free to serve, so the public interface is `ATTACH` plus SQL rather than
an API.

- **Publish**: measurements, raw instrument outputs, metadata. At top-1000
  scale raw self-profiles dominate: O(1-5 MB) x O(20k classes).
- **Do not publish**: compiled artifacts (huge, pointless, a supply-chain
  liability). Sources are assumed available — crates.io and pinned repos are the
  archive; we store pins and patches only.
- **Keying**: `class_id` is the fingerprint hash WITHOUT the toolchain, so
  identity is the specimen and the toolchain is an observed condition. Two
  measurements of one class under different nightlies join on `class_id`, which
  is what makes "same class, new compiler, what moved?" a query rather than a
  project.

How and when that data is aggregated, compacted or released is deliberately not
specified here. It depends on what the data turns out to look like, and
committing to a shape now would only constrain a decision better made later.

## The one new instrument this demands

Everything above is existing flags plus one instrument: **a sampling profiler
around the whole build**. It upgrades per-class phase cost from "derived from
wall sections" to measured, and it is how B/C conservation is checked per class
rather than per project.

A rustc wrapper was the intended instrument and was abandoned after measuring
it. `RUSTC_WRAPPER` overrides a contributor's `build.rustc-wrapper` instead of
stacking, so it silently evicts `sccache` and doubles the build — the study's
TRAP 1, reappearing as the fix for itself. The sampler stays out of the compile
path entirely: every compilation unit is its own process and every sample
carries a pid, so attribution is exact without changing how anything compiles.

What it cannot reach: `-Ztime-passes`' 57 named passes and `-Zself-profile`'s
per-query detail are nightly-only. Sampling recovers the phase decomposition on
stable to within about a point (validated against `-Ztime-passes` at
`-Ccodegen-units=1`), but not the leaf-level query breakdown.
