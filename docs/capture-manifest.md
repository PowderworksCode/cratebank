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
| full rustc argv, verbatim, plus filtered env | the rustc shim (below) |
| target triple, edition, toolchain (rustc -vV: commit hash, LLVM version) | rustc |
| source hash: crates.io checksum, or workspace commit + dirty patch | lockfile / git |
| inducing projects' Cargo.lock hashes | lockfile |
| harness versions: whyslow, crategen, extractor git revs | tools |

## B. Terminal responses (class)

| field | source | class |
| --- | --- | --- |
| CPU user+sys per rustc invocation | **the rustc shim** — a whyslow wrapper that records argv + wait4 rusage per invocation; the instrument that makes per-class CPU first-class (today we have per-unit *wall* sections + project-tree CPU only) | MUST |
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

## G. Era (once per stratum)

Machine identity (CPU model, cores, memory, kernel, governor), load+PSI
timeline for the whole run, toolchain hashes, sccache-disabled and
incremental-off attestations, filtered env snapshot, stratum id and date,
corpus list with pins, randomization seeds and shard plan, tool git revs.

## H. Replicates

Classes deliberately built more than once (across shards or repeated by
design) are kept as rows, never averaged at capture — they are the noise
floor. The schedule position of each build (what else was running) rides
along from B.

## I. Publication (the R2 idea)

Object storage layout, one prefix per stratum, immutable:

```
cratebank/2026-08/
  tables/          classes, measurements, projects, fits — parquet (canonical)
  cratebank.duckdb convenience database: same tables + predefined views
  raw/{class}/     self-profile.zst, mono.json.zst, timings.html.zst, stderr
  manifest.json    era record (G), schema version, checksums
```

- **Parquet is the canonical archive format**: frozen spec, every engine reads
  it (DuckDB, polars, pandas, DataFusion), HTTP range reads work, and a
  stratum is an append, never a rewrite. The **.duckdb file is a convenience
  layer** regenerated from the parquet — single-file `ATTACH`, plus shipped
  views/macros so the standard queries (below) are one-liners. Canonical data
  never lives only in the .duckdb.
- **Summability is a schema feature, not a query exercise.** Every class row
  carries array columns: `dep_class_ids` (direct edges — the graph) and
  `cone_class_ids` (precomputed transitive closure). Every project row carries
  `unit_class_ids` (its view). Any aggregate is one unnest + join, no
  recursive CTEs:

  ```sql
  -- what does tokio-with-its-whole-cone cost to compile?
  SELECT sum(m.cpu_s)
  FROM classes c, unnest(c.cone_class_ids) AS t(id)
  JOIN measurements m ON m.class_id = t.id AND m.era = '2026-08'
  WHERE c.name = 'tokio' AND c.era = '2026-08';
  ```

- **Keying: `class_id` is the fingerprint hash WITHOUT the toolchain; the
  measurement key is `(class_id, era)`.** Identity = the specimen; era = when
  it was measured. Consequence: strata join on `class_id`, so every monthly
  release is automatically a compiler-regression instrument at ecosystem
  scale — "same class, new nightly, what moved" is one join. (rustc-perf's
  question, answered over 20k real compilation classes instead of a fixed
  benchmark suite.)
- **Publish**: measurements, raw instrument outputs, metadata, tables.
  Ballpark per stratum at top-1000 scale: raw self-profiles dominate,
  O(1–5 MB) × O(20k classes) ≈ 20–100 GB compressed; tables are GBs.
- **Do not publish**: compiled artifacts (huge, pointless, supply-chain
  liability). Sources are assumed available (crates.io + pinned repos);
  the archive stores pins and patches, nothing more.
- R2's zero egress makes "others examine it" free; the public interface is
  `ATTACH` + SQL, no server.

## The one new instrument this demands

Everything above is existing flags plus one build: **the whyslow rustc shim**
(`RUSTC_WRAPPER=whyslow-shim`) recording per-invocation argv/rusage/timestamps
— it upgrades per-class CPU from "derived from wall sections" to measured, it
wraps the linker for F, and it is how B/C conservation is checked per class
rather than per project. Ironic and satisfying: the study's TRAP 1 was a
rustc-wrapper; its definitive instrument is one.
