# Capture Manifest — everything worth recording about a build

**Scope: everything obtainable from two instruments — a sampling profiler
around the whole build, and `cargo build --timings`.** Both work on stable, on
any rustc version, and neither sits in the compile path, so neither collides
with a contributor's `sccache`. Fields that need a nightly `-Z` flag, a rustc
wrapper, or an extra build are listed at the end as out of scope, with what
each would have bought — deleting them would only invite someone to re-derive
the same dead ends.

The archival round is only one-time if nothing forces a redo, so within that
scope this list errs exhaustive. Archive classes: **MUST** (analysis depends on
it), **RIDE** (near free alongside an instrumented build; capture
speculatively), **TGT** (costs an extra pass; run targeted, on implicated
classes only), **DRV** (re-derivable forever from archived source; cache, never
archive-critical).

Granularity: `class` = compilation-unit class (the individual), `proj` =
project view, `era` = one archival round.

## A. Identity & provenance (class) — all MUST

| field | source |
| --- | --- |
| package name, version, source id (crates.io / git+rev / path) | rustc argv source path + `--timings` `name`/`version` |
| resolved feature set | rustc argv `--cfg feature=`, and `--timings` `features` |
| dependency edges as class ids (the cone) | `--timings` `unblocked_units` |
| resolved profile: opt-level, debuginfo, codegen-units, panic, overflow-checks, lto, incremental | rustc argv `-C` flags, verbatim |
| full rustc argv, verbatim, plus filtered env | the profile's processName (`samply --include-args`) |
| target triple, edition | rustc argv `--target`, `--edition` |
| toolchain (rustc -vV: commit hash, LLVM version) | `rustc -vV`, one call per build |
| source hash: crates.io checksum, or workspace commit + dirty patch | lockfile / git |
| unit key (disambiguates the many `build_script_build`) | rustc argv `-C metadata=` |

## B. Terminal responses (class)

| field | source | class |
| --- | --- | --- |
| CPU per rustc invocation, split by compiler phase | **sampler** — samples carry a pid and every unit is its own process, so per-class CPU is first-class rather than derived from wall sections | MUST |
| per-unit wall, start/end timestamps (schedule reconstruction) | **sampler** (process start/exit) and **--timings** (`start`, `duration`) — two independent sources, so they cross-check | MUST |
| rmeta-emit time (the pipelining point) | **--timings** `sections` | MUST |
| units active / waiting / inactive over the build | **--timings** `CONCURRENCY_DATA` — the only source for "was this build dependency-bound or CPU-bound" | MUST |
| whole-build CPU curve | **--timings** `CPU_USAGE` | MUST |
| blocked time per unit (thread parked, burning no CPU) | **sampler** — leaf frame in a wait syscall; a bucket of its own, never folded into a phase | MUST |
| `-j`, jobs in flight | **--timings** concurrency + rustc argv | MUST |
| max RSS, faults, context switches, fs in/out | ~~shim rusage~~ **not captured** — needs `wait4` around each rustc, which is the wrapper this design removed | — |
| exit code, warning count, stderr on failure | **not captured** — see *What these two instruments cannot give* | — |

## C. Phase decomposition (class)

| field | source | class |
| --- | --- | --- |
| frontend/codegen sections (WALL) | **--timings** `sections` | MUST (the conserved backbone) |
| phase decomposition (CPU): macro_expand, resolve, type_check, coherence, borrowck, monomorphize, metadata_encode, codegen | **sampler** — symbol prefix recovers the phase, because rustc's crate structure is its phase structure | MUST |
| serial vs per-CGU-thread split | **sampler** — thread name; rustc codegens on a thread per CGU and a blended number is comparable to neither wall nor CPU | MUST |
| conservation verdict: Σ sampled phases vs unit wall, tolerance, pass/fail | computed at capture | MUST |

The two rows above measure different things and must never be compared
directly. `--timings` sections are **wall boundaries**; sampled phases are
**CPU**. On a 16-CGU crate they disagree by up to ten points purely because
codegen runs on many threads at once. That disagreement is itself a signal —
the ratio is realized codegen parallelism — but only if the two are kept
apart.

## D. Intermediate responses (class) — the mechanistic layer

| field | source | class |
| --- | --- | --- |
| CGU count, and time per codegen unit | **sampler** — one named thread per CGU (`opt cgu.NN`), so both fall out of thread attribution | MUST |
| trait-solving cost | **sampler** — `rustc_trait_selection` is ~24% of compile CPU on real crates and has no span of its own, so no phase-based tool reports it at all | MUST |
| artifact bytes: rmeta, rlib, .o total | reading files cargo already wrote, before target cleanup | MUST (needs no instrument) |
| artifact detail: section sizes (.text/.data/.rodata), symbol count, debuginfo bytes | `size`/`nm` on the same artifacts | RIDE (cheap, no instrument) |
| dep-info file lists (what source was actually read) | `.d` files cargo already wrote | RIDE (needs no instrument) |
| mono items, HIR node counts, LLVM pass statistics, per-macro expansion time | **out of scope** — every one needs a nightly `-Z` flag; see below |

## E. Source variables (class) — all DRV

whyslow-metrics census + shape metrics, LOC/bytes/files, at a recorded
extractor version. Recomputed at will from archived source; never load-bearing
in the archive.

## F. Project view (proj)

| field | source | class |
| --- | --- | --- |
| the DAG over class ids | **--timings** `unblocked_units` / `unblocked_rmeta_units` | MUST |
| resolved features per unit | **--timings** `features`, and rustc argv `--cfg feature=` | MUST |
| package name, version, target kind | **--timings**, and rustc argv | MUST |
| build-script executions: which ran, and for how long | **sampler** — a build script is a process in the tree; its *compilation* also appears as a unit | MUST |
| link step: wall, linker identity, argv | **sampler** — the linker is a child process like any other | RIDE (untested) |
| whole-build envelope: process-tree wall + reconciliation vs Σ classes | **sampler** — the profile spans the entire tree (140 processes on a bun build) | MUST |
| schedule Gantt + realized parallelism + critical path | derived from B timestamps and the DAG | DRV |
| licenses, workspace members | **not captured** — needs `cargo metadata` | — |
| final binary bloat, ICF potential | **out of scope** — separate builds and tools | — |

## G. Environment (once per measurement run)

Machine identity (CPU model, cores, memory, kernel, governor), load+PSI
timeline for the whole run, toolchain hashes, filtered env snapshot, corpus
list with pins, randomization seeds and shard plan, tool git revs.

Two attestations are load-bearing rather than hygiene, because both make a unit
look fast for a reason that has nothing to do with the compiler:

- **cache state.** An `sccache` hit compiles nothing, so it spawns no rustc,
  collects no samples, and still appears as a unit in the build graph. It must
  be recorded as a cache event with no timing claim, never as a fast compile.
  On a developer machine hits are the majority case, not an edge case.
- **incremental on/off.** A cached incremental build shifts every phase
  boundary. Sessions with it on are not comparable to sessions without.

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

- **Publish**: measurements, raw instrument outputs, metadata. Sampled phase
  counts are tiny (aggregated per unit, not per sample), so the raw session
  blobs dominate at O(10-20 KB) x O(classes) — three orders of magnitude below
  what raw self-profiles would have cost.
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

## The two instruments

**A sampling profiler around the whole build.** Not around each rustc: the
profiler costs a flat ~1s per invocation regardless of what it profiles, so
per-unit sampling would pay that for every crate. Wrapping once is also what
makes attribution work — samples carry a pid, every compilation unit is its own
process, and `--include-args` puts the rustc command line in the profile, so a
unit's identity is already in the data. Attribution is exact however many units
compile in parallel; a time-window join would be hopeless on `-j12`.

Validated against nightly `-Ztime-passes` at `-Ccodegen-units=1`: every phase
within about one point.

**`cargo build --timings`.** Stable since 1.60. The HTML embeds `UNIT_DATA`
(durations, features, versions, the DAG, frontend/codegen sections),
`CONCURRENCY_DATA` and `CPU_USAGE`. Most of it duplicates what the sampler and
the session log already give, which is the point: two independent measurements
of per-unit wall that can be cross-checked. What only it provides is
`CONCURRENCY_DATA` — how many units were ready-but-blocked versus running.

A rustc wrapper was the intended instrument for years of this document and was
abandoned after measuring it. `RUSTC_WRAPPER` **overrides**
`build.rustc-wrapper` rather than stacking with it, so it silently evicts a
contributor's `sccache` and doubles their build — the study's TRAP 1,
reappearing as its own proposed fix.

## What these two instruments cannot give

Recorded so the same ground is not re-covered. Each was measured before being
ruled out.

| wanted | needs | why not |
| --- | --- | --- |
| 57 named passes | `-Ztime-passes` | nightly; hard error on stable |
| per-query profile, cache hits/misses | `-Zself-profile` | nightly, and **14 MB compressed for a 3-crate build** |
| mono items, CGU assignment, HIR node counts | `-Zdump-mono-stats`, `-Zprint-mono-items`, `-Zhir-stats` | nightly |
| per-macro expansion time | `-Zmacro-stats` / self-profile | nightly |
| max RSS, faults, context switches | `wait4` around each rustc | requires the wrapper, which evicts sccache |
| exit code, warning count, stderr | wrapping rustc, or parsing cargo's JSON | wrapper; cargo's `--message-format=json` could reach the warnings |
| LLVM pass statistics, IR lines | `-Cllvm-args=-stats`, `cargo llvm-lines` | perturbs, or needs a separate build |
| licenses, workspace members | `cargo metadata` | cheap, just not one of these two instruments |

Two near-misses worth knowing about, both measured:

- **`RUSTC_LOG` works on release rustc**, but `max_level_info` compiles DEBUG
  and TRACE out, so the phase spans do not exist. 51,727 span records on one
  crate, **zero** with a non-zero duration, at +72% build time. The counts
  (37,242 normalizations, 3,200 coercions) are real and machine-independent,
  and are the one thing here worth revisiting.
- **Artifact mtimes** give parse/expand, analysis and codegen boundaries for
  free on every platform, by timestamping the files rustc writes. They cannot
  split the frontend, which is 80% of compile time on some crates. Extra
  `--emit` kinds (+119%) and `-Csave-temps` (+83%) buy backend detail that is
  not worth it; both look free measured on a single unit, and that measurement
  is wrong.
