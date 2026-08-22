# cargo-cratebank

`cargo-cratebank` captures one Rust build with two required stable inputs:

- `cargo build --timings` for unit wall time, compiler sections, graph edges,
  concurrency, and whole-build CPU; and
- samply for CPU-weighted compiler phases attributed to rustc processes.

Both outputs come from the same build and are sent in one payload.

## Requirements

- a stable Rust toolchain;
- `cargo-cratebank`; and
- samply.

```sh
cargo install cargo-cratebank
cargo install samply
```

Samply is mandatory. A missing sampler, failed sampled build, missing timing
report, or parsing failure stops the command and sends nothing.

## Commands

```sh
cargo cratebank build
cargo cratebank build --release --all-features
cargo cratebank --dry-run build
cargo cratebank status
cargo cratebank serve --port 8787
```

| command | behavior |
| --- | --- |
| `build` | run the Cargo build under samply, parse both outputs, filter private units, and upload |
| `status` | check samply and show the active endpoint |
| `serve` | run a local collector that decodes and summarizes an uploaded payload |

Set `CRATEBANK_ENDPOINT` or pass global `--endpoint` before the subcommand to
use another collector:

```sh
cargo cratebank --endpoint http://127.0.0.1:8787/ingest build
```

## Capture flow

1. Stable `cargo metadata --no-deps` locates the workspace and target
   directory without fetching packages for unrelated platforms.
2. The client records the existing timing-report filenames.
3. Samply runs `cargo build --timings` with the supplied Cargo arguments.
4. The client selects the new timestamped report from
   `target/cargo-timings/`.
5. Cargo timing JSON and the samply profile plus symbol sidecar are parsed.
6. Cargo metadata and `Cargo.lock` remove non-public units from both inputs.
7. Machine, load, and whitelisted build context are attached.
8. The schema-2 JSON payload is zstd-compressed and posted.

Nothing sets `RUSTC_WRAPPER`; samply wraps the Cargo process and attributes
samples by rustc process id. A configured `sccache` remains in place.

## Cargo timing data

Cargo embeds three structured values in its HTML report:

- `UNIT_DATA`: package, version, mode, features, start, duration, section
  spans, and units unblocked;
- `CONCURRENCY_DATA`: active, waiting, and inactive units over build time; and
- `CPU_USAGE`: whole-machine CPU percentage over build time.

The client extracts the structured values directly. The timestamp, profile,
compiler version, host, and job count come from the same report summary.

## Samply data

Samply records the complete Cargo process tree with rustc arguments included.
Each rustc process becomes one compilation-unit observation. Symbols map CPU
samples into:

- `macro_expand`
- `resolve`
- `coherence`
- `type_check`
- `borrowck`
- `monomorphize`
- `metadata_encode`
- `codegen`
- `blocked`
- `unattributed`

Serial rustc threads and parallel codegen-unit threads remain separate.
Sample counts are CPU-weighted observations; Cargo section durations are wall
clock. They are published separately and must not be treated as the same
quantity.

The samply command line also supplies resolved compilation settings such as
opt-level, debuginfo, codegen units, panic strategy, LTO, edition, target,
features, and whether incremental compilation is active. Filesystem paths are
not retained.

## Privacy

Only public compilation units leave the machine.

| source | uploaded |
| --- | --- |
| crates.io | yes |
| public Git dependency | yes |
| workspace package | only with explicit `public = true` |
| private registry | no |
| local path dependency | no |

Workspace publication is explicit:

```toml
[package.metadata.cratebank] # or [workspace.metadata.cratebank]
public = true
repository = "https://github.com/you/project"
```

For Cargo timings, package name and version are classified against
`Cargo.lock`. For samply units, the package directory or rustc target name is
matched against the same public set. Ambiguous matches are withheld. Edges to
withheld timing units are removed.

The payload never includes source code, manifest paths, target paths,
usernames, hostnames, or the full environment. Build configuration is read
from a fixed allowlist, and path-shaped values are dropped or reduced to a
basename.

## Machine identity

The default machine id is random, generated once, and stored in
`$CARGO_HOME/cratebank/machine-id`. It joins observations from the same machine
without reading a hostname or user account.

Override it with `CRATEBANK_MACHINE_ID` or
`[package.metadata.cratebank] machine_id`. Set it to `none` to omit it.

The machine profile includes CPU model, core count, memory, kernel, operating
system, architecture, virtualization hint, Cargo version, and CI presence.

## Payload

The upload is schema version 2:

```json
{
  "cratebank_schema": 2,
  "client": "cargo-cratebank 0.1.0",
  "run_id": "20260822T100652933Z-62713dc30536de9e",
  "trust": "anonymous",
  "env": {
    "timestamp": "2026-08-22T10:06:52.93381Z",
    "profile": "dev",
    "rustc_version": "rustc 1.96.1",
    "host": "aarch64-apple-darwin",
    "jobs": 12,
    "ci": false
  },
  "timings": {
    "unit_data": [],
    "concurrency_data": [],
    "cpu_usage": []
  },
  "phases": {
    "sampler": "samply",
    "rate_hz": 4999,
    "units": []
  },
  "counts": {
    "units": 0,
    "units_withheld": 0,
    "sections": 0,
    "phase_units": 0
  }
}
```

`--dry-run` prints the complete payload. Normal sends encode the same JSON as
zstd level 19 with `Content-Type: application/zstd`; the ingest Worker stores
those exact bytes.

## Source layout

| path | responsibility |
| --- | --- |
| `cmd/build.rs` | required sampled build and combined-send orchestration |
| `timings.rs` | Cargo metadata, timing-report discovery, parsing, and privacy filtering |
| `sample.rs` | samply invocation, profile parsing, and phase attribution |
| `payload.rs` | schema-2 envelope |
| `buildenv.rs` | whitelisted configuration snapshot |
| `machine.rs` | machine id and hardware profile |
| `load.rs` | load, CPU utilization, and pressure during the build |
| `ship.rs` | zstd encoding and HTTP transport |
| `cmd/serve.rs` | local reference collector |
