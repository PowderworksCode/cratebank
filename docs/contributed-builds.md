# Contributed builds

Volunteers run `cargo whyslow` on their own projects and opt in to sending
measurements. Folding@home for compile time.

The client must be **useful before it is generous**: `cargo whyslow explain`
already tells you why *your* build is slow. Contribution is a flag on a tool
people install for their own reasons. Nobody runs a client that only feeds a
dataset.

## What makes this statistically sound

Crowdsourcing normally destroys measurement studies: heterogeneous CPUs,
thermal states, background load, wildly different configurations. Everything
this study previously did depended on holding the machine fixed.

The class schema rescues it, because **the same classes recur on every
machine**. Nearly every contributor compiles `serde 1.x` with the same
features. That creates massive overlap between machines and classes, which is
exactly the structure that separates the two effects:

```
log cpu(class i, machine j) = intrinsic_i + speed_j + schedule_ij + noise
```

A crossed random-effects model (the same shape as item-response theory, price
indices, or chess ratings) identifies `intrinsic_i` and `speed_j` jointly,
*because* many machines share many classes. Common classes are the linking
items; rare classes are still measurable because the machines that built them
are already calibrated by the common ones.

Three consequences:

1. **Machine heterogeneity becomes a parameter, not a confound.**
2. **The archival round becomes the calibration anchor** — one machine with
   deep instrumentation, uniform policy, and known conditions, pinning the
   scale that contributed builds are estimated against.
3. **The residual `schedule_ij` term is itself interesting**: it measures what
   real-world contention costs real developers, which the controlled arm
   deliberately excludes.

## The standard candle

Every client ships a fixed **crategen** workspace and builds it periodically.
Same source, same knobs, same fingerprint, everywhere — a synthetic class with
no ecosystem drift, no feature variation, no version churn.

That is a standard candle: it measures `speed_j` directly, independent of
which real crates that machine happens to compile, and it re-measures it over
time (thermal, kernel, hardware changes). Contributed measurements without a
recent candle reading are accepted but down-weighted. This is crategen's
second job, and the reason the generator belongs in the same family as the
bank.

## What is uploaded

**Only classes whose source is public.** The `source_id` says whether a unit
came from crates.io, a public git remote, or a local path. Public units upload
normally; everything else uploads **nothing** — no names, no hashes, no
metrics. A private workspace crate is invisible to cratebank, and its
existence is inferable only as an unattributed gap in the project envelope.

Per accepted unit: class identity, the measurements in the capture manifest's
MUST tier (CPU/wall/RSS, phase split, mono summary, artifact bytes), and
capture context. Never: source, paths, environment variables, binary
artifacts, project names for non-public projects.

Contribution is opt-in per invocation or per project (`whyslow.contribute` in
config), always announced, always `--dry-run`-inspectable: the client can
print the exact JSON it would send.

## Contamination controls

Contributed data is observational and must carry its own health flags. Every
submission records, and the ingest rejects or down-weights on:

| flag | why |
| --- | --- |
| `rustc_wrapper` state (sccache et al.) | **the study's original trap**: a wrapped build measures a cache. Rejected outright, not down-weighted. |
| `incremental` on/off | different phenomenon; kept, but modeled separately |
| jobs in flight, load average, PSI | schedule term; heavy contention down-weights |
| CPU model, cores, governor, virtualization | machine identity for `speed_j` |
| thermal/frequency where available | candle drift |
| candle recency | weight |
| client version, toolchain | era bucketing |

Poisoning is handled the way any open cohort handles it: per-machine
identities (anonymous but stable), robust estimators, and the candle as a
consistency check — a machine reporting implausible speed on the standard
workload is quarantined, and no single contributor can move a class's
estimate much because common classes have thousands of observations.

## Ingest

ClickHouse: an append-only firehose of build events, cheap columnar
aggregation, and the natural home for a stream nobody wants to rewrite. At
each stratum release, contributed data is compacted into that stratum's
parquet alongside the archival round, with `source = archival | contributed`
and the fitted `speed_j` retained so anyone can re-derive normalised costs.

## What this buys that the archival round cannot

- **Configuration coverage**: real feature combinations, real profiles, real
  targets — including Windows and macOS, which one Linux box will never see.
- **Scale**: rare crates and rare feature sets get measured because *someone*
  uses them.
- **Real-world wall-clock**: what builds actually cost people, contention
  included, alongside the controlled CPU numbers.
- **A regression tripwire**: a nightly that slows a common class shows up
  within hours across thousands of machines.

It does not replace the archival round. Contributed builds are shallow
(MUST-tier only, no self-profile raw), heterogeneous, and self-selected toward
developer-grade hardware. The archival round remains the calibrated,
deeply-instrumented, uniform-policy backbone; contribution is breadth on top
of it.
