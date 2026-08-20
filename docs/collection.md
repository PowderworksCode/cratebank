# Collection

Volunteers contribute the builds they are **already running** — CI pipelines,
local `cargo build`, `cargo check` on save. No extra compilation, no separate
invocation, no benchmark to run. The marginal cost to a contributor is
approximately zero, because the work was happening anyway.

This is the folding@home shape with a twist: folding@home asks for spare
cycles, cratebank asks only for the *measurements of cycles already spent*.

## What a passively captured build actually is

Real builds are not our archival round. The distribution is roughly:

| setting | what it produces | value to cratebank |
| --- | --- | --- |
| **CI, cold runner** | clean full-graph compiles, every dependency built from scratch, on platforms and configurations we do not own | **the jackpot**: these are archival-round observations, for free, at scale, on Windows/macOS/ARM |
| **CI, warm cache** | mostly cache hits, plus genuine cold compiles for anything that changed | the misses are valid class observations; the hits are absence, not error |
| **local dev, incremental** | partial rebuilds of workspace crates, dirty-state dependent | a *different phenomenon* — tagged separately, and the seed corpus for the incremental study v1 deferred |
| **local dev, first build** | full dependency graph compiles | archival-comparable |

The key structural fact: **a cache miss is a clean class compile.** Whenever
any unit is compiled for the first time in any environment — cold CI, a new
dependency, a version bump — that invocation is exactly the individual our
schema is keyed on. Incrementality affects *workspace* crates; the dependency
cone below them is compiled fresh or not at all.

So passive capture yields archive-comparable data as a byproduct, and the
messy remainder is not noise to be filtered but the raw material for the
question we deliberately postponed (what does an edit cost?).

Every observation is therefore tagged `clean | cached | incremental` and they
are never pooled.

## Two capture tiers

**Tier 1 — harvest (zero configuration).** Cargo already writes
`target/cargo-timings/*.html` when `--timings` is on. `cargo whyslow harvest`
scans for those artifacts, extracts per-unit data, and uploads. No wrapper, no
build-time overhead, nothing in the compile path. Limitation: `--timings`
reports **wall clock per unit**, not CPU, so tier-1 observations are noisier
and carry a contention term. Fine in bulk; the crossed model absorbs it.

**Tier 2 — shim (opt-in, richer).** `RUSTC_WRAPPER=whyslow-shim` records
per-invocation argv, `wait4` rusage (real CPU, RSS, faults), and timestamps.
This is the same instrument the archival round uses, so tier-2 contributions
are directly comparable to it.

On the sccache collision — the study's original trap, now a design
constraint: the shim **chains** rather than competes. If a wrapper is already
configured, whyslow-shim execs it and records the outcome, including whether
sccache reported a hit or a miss. Hits are recorded as cache events with no
timing claim; misses are real compiles and measured normally. Nothing is ever
attributed compile cost that came from a cache.

## What makes this statistically sound

Heterogeneous machines would normally destroy a measurement study. The class
schema rescues it, because **the same classes recur everywhere**: nearly every
contributor compiles `serde 1.x` with the same features. That overlap is
exactly what separates the effects:

```
log cost(class i, machine j) = intrinsic_i + speed_j + schedule_ij + noise
```

A crossed random-effects model (item-response theory, price indices, chess
ratings — same shape) identifies `intrinsic_i` and `speed_j` jointly *because*
many machines share many classes. Rare classes are still measurable, since the
machines that built them were calibrated by the common ones.

Three consequences:

1. machine heterogeneity becomes a parameter, not a confound;
2. the archival round is the **calibration anchor** — one machine, uniform
   policy, deep instrumentation, pinning the scale;
3. `schedule_ij` is itself a finding: it measures what contention costs real
   developers, which the controlled arm deliberately excludes.

### The standard candle

Contributors can optionally build a fixed **crategen** workspace — same
source, same fingerprint, everywhere — as a direct probe of `speed_j`,
independent of which crates that machine happens to compile, re-measurable as
hardware and thermal conditions drift. This is the one place we ask for spare
cycles, it is strictly optional, and it takes seconds. Machines without a
recent candle reading are still usable (they calibrate through shared classes)
but weight lower.

## Privacy

**Only public units are uploaded. Nothing else leaves the machine.**

- **public units** (crates.io, public git remotes) upload with full identity.
  This is the dependency graph, and it is public code no matter who compiles it.
- **workspace units** are withheld by default — a local path cannot be
  distinguished from private code by source alone. A project declares itself
  public to include them, and they are then linked by repository
  (`workspace#name@version`), never by the local path they were built from.
- **everything else is dropped entirely**: no name, no hash, no timing, no
  edge. Every event referencing a withheld unit is removed and every dependency
  edge pointing at one is pruned, so the payload carries no orphaned indices.
  A single `units_withheld` count remains, so the receiver knows the graph is
  partial rather than silently truncated.

A closed-source shop therefore contributes the bulk of what matters — what
tokio, serde and diesel actually cost in this environment — while disclosing
nothing about its own code.

Never captured, in any tier: source, file paths, environment variables (CI
environments hold secrets), command lines beyond a whitelist of compiler flags,
repository identity for undeclared projects, usernames, IP-derived location.

Contribution is opt-in per project or per machine, announced on first use, and
inspectable: `--dry-run` prints the exact payload. A contributor can request
deletion of everything under their anonymous-but-stable machine id.

## Contamination controls

Passive data is observational and carries its own health flags. Ingest
rejects, tags, or down-weights on:

| flag | handling |
| --- | --- |
| wrapper/cache state, per invocation | cache hits recorded as events, never as timings |
| `incremental` on/off, and dirty-state | tagged; never pooled with clean |
| jobs in flight, load average, PSI | the `schedule` term; heavy contention down-weights |
| CPU model, cores, governor, virtualization, OS | machine identity for `speed_j` |
| thermal/frequency where available | candle drift |
| toolchain, client version | era bucketing |
| CI vs local | strong covariate: CI is cold and uniform, local is warm and varied |

Poisoning is handled as any open cohort does: stable anonymous machine ids,
robust estimators, and the candle as consistency check. No single contributor
moves a common class's estimate, because common classes accumulate thousands
of observations.

## Ingest

ClickHouse: an append-only firehose of build events — cheap columnar
aggregation, and the right home for a stream nobody wants to rewrite. At each
stratum release, contributed data is compacted into that stratum's parquet
alongside the archival round, tagged `source = archival | contributed` and
`capture = clean | cached | incremental`, with fitted `speed_j` retained so
anyone can re-derive normalised costs.

## The build-environment census

Per-unit timings are half the product. The other half is the **environment
snapshot** attached to every build, which by itself answers questions nobody
can currently answer about the Rust ecosystem. Some of these need capture that
does not exist yet — the linker and `RUSTFLAGS` are not recorded by cargo's log
and are noted below as future work:

- what fraction of real builds use lld, mold, or the default linker?
- what opt-levels, debuginfo settings, codegen-units, LTO modes are actually
  in use — as opposed to what documentation recommends?
- how many jobs do real machines run, on how many cores, and how contended?
- what is sccache adoption, and what hit rates do people actually get?
- which targets matter by volume; how fast do toolchain versions propagate?
- how do these distributions differ between CI and developer laptops?

Existing sources are self-reported (the annual survey), synthetic
(rustc-perf), or about consumption rather than compilation (crates.io download
counts). A measured, ongoing census of build environments is new, useful to
the Rust project itself, and falls out of collection for free.

## What this buys that a controlled fleet cannot

- **Platform and configuration coverage**: Windows, macOS, ARM, real feature
  combinations, real profiles — a single Linux box will never see them.
- **Scale**: rare crates and rare feature sets get measured because someone
  actually uses them.
- **Realised developer cost**: what builds actually cost people, contention
  and caching included, alongside controlled CPU numbers.
- **A regression tripwire**: a nightly that slows a common class shows up
  within hours, across thousands of machines, before anyone files an issue.
- **The incremental corpus**: the phenomenon v1 postpones arrives as a
  byproduct, at a scale no controlled study could fund.

## Honest limits

- **Observational, not causal.** This data can say what correlates with cost
  in the wild; it cannot say what *causes* it. Causal claims come from
  designed synthetic experiments (crategen) and are checked against this
  cohort's residuals — the model-organism arrangement, not a substitute for
  it.
- **Self-selected.** Contributors will skew toward CI-heavy, larger, and
  more tooling-aware projects, and toward developer-grade hardware.
  Population statements need weighting and honest caveats; per-class cost
  estimates are far more robust, since they lean on overlap rather than
  representativeness.
- **Tier-1 data is wall clock**, so it carries a contention term; tier-2 shim
  data is real CPU. Both are usable, but never pooled without the tier flag.
- **A controlled run is still worth having** as a calibration anchor and for
  deep instrumentation (self-profile, mono stats) that is too heavy to ask of
  contributors. It is now optional and secondary rather than the backbone —
  common dependencies compiled on thousands of machines are themselves a
  strong anchor.
