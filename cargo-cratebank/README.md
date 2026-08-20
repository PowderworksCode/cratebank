# cargo-cratebank

Opt-in sharing of the build timings you were already producing, for
[cratebank](../README.md) — a public census of Rust compilation.

This plugin **instruments nothing**. Cargo already records everything on
nightly: `-Zbuild-analysis` writes one JSONL session log per invocation to
`$CARGO_HOME/log/`, and `-Zsection-timings` folds rustc's frontend/codegen
section boundaries into that same stream. `cargo-cratebank` reads those logs,
drops everything non-public, and POSTs the rest.

So: nothing in the compile path, no conflict with `sccache` or any other
`RUSTC_WRAPPER`, no extra builds, and no build ever run on your behalf.

## Requirements

A nightly toolchain — cargo's build-analysis flags are unstable. The config
keys it writes only emit an unknown-config warning on stable, so they are safe
to leave in place if you switch back and forth.

## Quickstart

```sh
cargo install cargo-cratebank        # not yet published; build from this directory
cd your-project
cargo cratebank enable               # opt in + turn on cargo's analysis flags
cargo cratebank status               # confirm everything is wired up
```

Then look at exactly what would be sent before you send anything:

```sh
cargo cratebank send --dry-run       # byte-for-byte payload, sends nothing
cargo cratebank serve                # your own collector, on localhost
CRATEBANK_ENDPOINT=http://127.0.0.1:8787/ingest cargo cratebank send
```

## Commands

| command | what it does |
| --- | --- |
| `enable` | writes the opt-in, cargo's analysis flags at the workspace root, and a `build.rs` trigger |
| `watch` | background: ships every completed session from any opted-in workspace, and clears backlog |
| `build [cargo args…]` | runs `cargo build` with both flags on, then sends |
| `send [--all \| --session ID \| --since N]` | ships logs from builds that already happened |
| `status` | log directory, session count, nightly availability, endpoint, opt-in state |
| `serve [--port 8787]` | echo collector for testing — prints what it receives |

Flags: `--dry-run` (print the payload, send nothing) and `--endpoint URL` (or
`CRATEBANK_ENDPOINT`).

There is deliberately **no flag to include non-public units**. Filtering is
unconditional, so no invocation, misconfiguration or future edit can turn it
off — a code path that could send private data is a liability even when nobody
invokes it.

## Privacy

**Only public units are uploaded.** Everything else is dropped entirely — not a
name, not a hash, not a timing, not a graph edge. Dropping a unit also removes
every event that referenced it and prunes every dependency edge pointing at it,
so the payload never carries an orphaned index.

| | uploaded? |
| --- | --- |
| public dependencies (crates.io, public git remotes) | yes, full identity |
| your own workspace crates | **no**, unless you declare the project public |
| private registries, local paths | never |
| paths (`cwd`, `workspace_root`, `target_dir`, `manifest_path`) | never |
| environment variables | never read |
| command-line values | replaced with `<arg>`; flag names kept (see below) |
| source code | never |

A local path is indistinguishable from private code, so publishing your own
crates is an explicit choice:

```toml
[package.metadata.cratebank]   # or [workspace.metadata.cratebank]
share = true
public = true
repository = "https://github.com/you/project"
```

They are then linked as `workspace#name@version` — by repository, never by the
path they were built from.

Scrubbing the command line costs nothing, because cargo already records the
same information in structured form — and in better form, since features are
recorded **as resolved** rather than as requested:

| dropped from the command line | already in the log |
| --- | --- |
| `-j N` | `jobs`, alongside `num_cpus` |
| `--target <triple>` | `platform`, per unit |
| `--features` / `--all-features` | `features`, per unit, resolved |
| `--release` / `--profile` | `profile` |
| `build` / `check` / `test` | `mode`, per unit |
| `-Z…` | kept — flag names survive scrubbing |

What is genuinely *not* captured today, by cargo or by us: `RUSTFLAGS` and the
linker in use. Both affect compile time and both belong in a build-environment
census, but they live in environment variables and config that can contain
local paths, so capturing them needs a deliberate design rather than a blanket
read. Tracked as future work.

The only trace of withheld code is a `units_withheld` count, kept so a partial
graph is visibly partial rather than silently truncated. Measured on ripgrep:
43 public dependency units and 68 section timings uploaded, its 11 workspace
crates withheld, zero orphaned events, zero dangling edges, no paths anywhere.

Opting out is `share = false`, or deleting the metadata key.

Two structural safety properties: the opt-in must live in **your own**
manifest, and a manifest inside `CARGO_HOME` (a downloaded dependency) can
never trigger a send — a published crate cannot enrol its consumers.

## Automatic sending

`cargo cratebank enable` writes three things: the opt-in, the two unstable
flags in the **workspace root's** `.cargo/config.toml`, and a small `build.rs`
trigger. After that an ordinary `cargo build` ships its session log — nothing
cratebank-shaped in the command.

| path | reliability |
| --- | --- |
| **`cargo cratebank watch`** (recommended) | sees **every** build, and clears any backlog |
| **`build.rs` trigger** | **best-effort** — cargo does not guarantee it reruns a build script on every rebuild |

The `build.rs` helper spawns detached and waits for the parent cargo process to
exit before reading the log. `build.rs` runs *early* in a build, so quiescence
alone ("the log stopped growing") ships a partial session during a gap between
slow units — measured as 27 events where the complete session had 61.

If a send does not happen, `CRATEBANK_DEBUG=1` says why: no opt-in found, not a
primary package, no session for this workspace, already sent, or a failed POST.

What `enable` writes to `.cargo/config.toml`, if you would rather do it by hand:

```toml
[unstable]
build-analysis = true
section-timings = true

[build.analysis]
enabled = true
```

## Layout

| file | role |
| --- | --- |
| `cli.rs` | the clap command-line surface, `$CARGO_HOME`, debug logging |
| `session.rs` | finding, reading and filtering cargo's session logs — the privacy rules live here |
| `project.rs` | manifests, opt-in, workspace roots, and detecting that a build has finished |
| `ship.rs` | transport and the ledger of what has already been sent |
| `cmd/*.rs` | one file per subcommand |

## Payload

One session, one POST. Events pass through **verbatim** under a small header:
cargo's log schema is explicitly still evolving, so normalising in the client
would bake in today's shape and break on every churn. Capture broadly, model
server-side.

```json
{
  "cratebank_schema": 1,
  "client": "cargo-cratebank 0.1.0",
  "run_id": "20260820T215558080Z-2437f0c648fa6cb1",
  "public_only": true,          // a constant, not a mode
  "repository": null,
  "env": {
    "host": "x86_64-unknown-linux-gnu",
    "profile": "dev", "jobs": 16, "num_cpus": 16, "ci": false,
    "rustc_version": "1.99.0-nightly",
    "rustc_version_verbose": "… commit-hash, commit-date, LLVM version …"
  },
  "counts": {"events": 338, "units": 43, "sections": 68, "units_withheld": 11},
  "events": [
    {"reason": "build-started", "command": ["cargo", "<arg>", "-Zbuild-analysis", …], …},
    {"reason": "unit-registered", "package_id": "registry+…#aho-corasick@1.1.5",
     "target": {"name": "aho-corasick", "kind": "lib"}, "dependencies": [12], …},
    {"reason": "unit-section-finished", "index": 5, "section": "codegen", "elapsed": 0.213},
    {"reason": "unit-finished", "index": 5, "elapsed": 0.418}
  ]
}
```
