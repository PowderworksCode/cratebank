# Collection

Each contribution is one explicit `cargo cratebank build`. The client runs
stable `cargo build --timings` under samply, parses both results, applies the
same privacy boundary to each, and sends one combined observation.

A contribution exists only when the sampled build and both parsers succeed.

## The two measurements

Cargo timings provide wall-clock structure:

- package, version, mode, target, and features per compilation unit;
- unit start and duration;
- frontend, codegen, and link section spans;
- dependency-unblocking edges;
- active, waiting, and inactive unit counts over time; and
- whole-machine CPU utilization over time.

Samply provides CPU-weighted compiler behavior:

- samples attributed to each rustc process;
- compiler phases derived from rustc symbols;
- serial rustc threads separated from parallel codegen-unit threads;
- rustc process wall time; and
- resolved compiler settings from the scrubbed rustc command line.

Cargo section durations and samply phase counts are different quantities. The
former are wall-clock spans; the latter are CPU-weighted samples. They remain
separate in the payload and public tables.

## One build, one observation

The client records the timing-report directory before invoking samply. Samply
then starts Cargo with `--timings`. After the command succeeds, the client
selects the new timestamped Cargo report and parses the samply profile and
symbol sidecar from that same process tree.

The run id comes from the Cargo report filename and joins timings, phases,
machine context, and public tables. A cached build is valid: Cargo records
fresh units in its timing data, while samply naturally records no rustc process
for a cache hit.

## Privacy boundary

Only public units are uploaded.

- Crates.io packages are public.
- Git dependencies are public only when their lockfile source is a public HTTP
  URL without embedded credentials.
- Private registries and local path dependencies are withheld.
- Workspace packages are withheld unless their package or workspace metadata
  declares `cratebank.public = true`.

Cargo timing units are classified by package name and version against
`Cargo.lock` and workspace metadata. Samply units are classified by their
package directory or rustc target name against the same set. Any ambiguous
classification is withheld.

Edges pointing to withheld timing units are pruned. The payload carries only a
`units_withheld` count so consumers know the graph is partial.

Never uploaded:

- source code;
- manifest, workspace, target, or source paths;
- usernames or hostnames;
- the full environment;
- private package identity; or
- compiler arguments outside the explicit settings allowlist.

`--dry-run` prints the exact payload after filtering.

## Build context

Each observation includes the compiler and host reported by Cargo, profile,
job count, CI presence, machine hardware, machine id, load and CPU utilization
during the build, and whitelisted build configuration.

Missing platform concepts are null rather than zero. For example, Windows has
no Unix load average, and non-Linux systems have no Linux pressure-stall data.

The machine id is locally generated and stable by default. Contributors can
set `CRATEBANK_MACHINE_ID` to an organization label or `none` to omit it.

## Statistical use

Every observation must be analyzed as observational data. Machines,
concurrency, cache state, compiler version, profile, features, and target are
conditions rather than noise to discard.

The same public packages recur across many machines. That overlap supports a
crossed model of compilation class and machine speed:

```text
log cost(class i, machine j) = intrinsic_i + speed_j + schedule_ij + noise
```

Use robust per-class estimates. The ingest endpoint is anonymous, so raw
submission counts and self-declared attribution are not trustworthy weights.

## Published form

Raw zstd payloads are the ground truth. Daily compaction publishes Cargo unit
rows, samply phase rows, Cargo timeline rows, unit settings, and session
context as parquet at fixed public URLs. See `docs/ingest.md` and
`docs/schema.md`.
