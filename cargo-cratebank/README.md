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
| environment variables | never read (except the build-config whitelist below) |
| machine id | **yes** — random by default, configurable, or `none` |
| hostname, username, network identity | never |
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

### Machine id — stated plainly

**Every payload carries a machine id.** It is the one field here that enables
linkage: with it, sessions from one machine join together, which is what makes
within-machine comparisons possible — *on this same box, did serde 1.0.200
compile slower than 1.0.199?* — and equally what makes a build timeline
reconstructable. That is the trade, said out loud. If you do not want it, do not
send the data.

The id is yours to set, and a chosen value is often the better one:

| source | precedence | typical use |
| --- | --- | --- |
| `CRATEBANK_MACHINE_ID` | first | CI, where `$CARGO_HOME` is ephemeral |
| `[package.metadata.cratebank] machine_id` | second | one id for a project or an org |
| `$CARGO_HOME/cratebank/machine-id` | third | a plain file — edit it freely |
| random, generated once | fallback | a personal machine |

```toml
[workspace.metadata.cratebank]
share = true
machine_id = "acme-ci"        # attribution: these runs are ours
```

`acme-ci` credits a company's builds to that company, which is attribution
rather than tracking — and on ephemeral CI a random id would be a fresh
meaningless value every job anyway. Set it to `none` (or empty) and no id is
sent at all.

`cargo cratebank enable` says all of this at the moment you opt in — before any
data moves, not after somebody finds an id in a payload:

```
  Who these builds are from
  ------------------------
  A random machine id was generated for you:  d9fdc164f7f16f8b4393…
  It links your own sessions together — which is what allows
  comparisons like "did this crate get slower on the same machine?"

  If you would rather take credit for the work, name yourself:

      [package.metadata.cratebank]   # or [workspace.metadata…]
      machine_id = "your-org"

  On CI, set CRATEBANK_MACHINE_ID instead — a fresh runner would
  otherwise invent a new random id on every job.
  Send no id at all with  machine_id = "none".
```

If you have already named yourself it says so instead, and
`cargo cratebank status` always shows the current id and where it came from.

Alongside it, a *profile*: CPU model, cores, memory to the nearest GB, kernel,
OS/arch, virtualization, cargo version, and whether `CI` is set — each shared by
millions of machines. Hostname, user and network identity are never read.

### Build configuration from the environment

`RUSTFLAGS`, the linker and any compiler wrapper move compile time, and cargo's
log does not record them — so `build_env` collects them from a **whitelist** of
environment variables and cargo's resolved config. Never the environment as a
whole, and every value is classified before it is allowed out:

| shape | treatment |
| --- | --- |
| `-C opt-level=2`, `-C target-cpu=native`, `-Zthreads=8` | kept verbatim |
| `-C linker=/usr/bin/clang`, `RUSTC_WRAPPER=/usr/local/bin/sccache` | basename only → `clang`, `sccache` |
| any value containing a path separator | value replaced with `<path>` |
| `--remap-path-prefix=…`, `-L /home/…` | dropped entirely |
| unrecognised flag with a value | name kept, value replaced |

Flag names and non-path values are build configuration and are the point of
collecting this; anything path-shaped is somebody's filesystem and never leaves
the machine. Covered by unit tests.

A session log has no build-finished event, so a build that died half way looks
exactly like one that completed. Every payload therefore carries a `complete`
flag: registered units versus finished units. Truncated sessions are data, but
they are not the same data.

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

**If the project already has a `build.rs`, cratebank does not touch it.** That
is somebody's build logic and a trigger is not worth the risk of mangling it.
Enable says so, prints the three lines to add if you want them, and — this is
the part that matters — does *not* claim sending is active when it is not:

```
This project is opted in, but nothing is sending yet:

  You already have a build.rs, and cratebank will not edit it.

  Either run the watcher (no edits needed, and it sees every build):

      cargo cratebank watch

  or add these three lines to its main(): …
```

If that build.rs declares `rerun-if` directives, enable adds a note: cargo then
reruns it only when those inputs change, so a trigger inside it would fire
rarely, and the watcher is the better answer. Sessions are recorded either way —
`cargo cratebank send` ships them whenever you like.

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

## CPU time, load and artifact bytes

Cargo's log reports **wall clock** per unit, which on a `-j16` build is largely
a statement about contention. Three additions make the data answer cost
questions rather than scheduling ones.

**Per-unit compiler phases** — `cargo cratebank build` runs the build under
[samply](https://github.com/mstange/samply) and reports where each crate's
compile time went:

```
emit          serial 11476  parallel  226   borrowck=4165 type_check=3549 macro_expand=1240
serde_derive  serial  4968  parallel 2238   borrowck=1390 codegen=1173   macro_expand=816
```

Nothing wraps rustc. The sampler runs once around the whole build; every
compilation unit is its own rustc process and every sample carries a pid, so
attribution is exact however many run in parallel — and your `sccache` is
untouched, because no `RUSTC_WRAPPER` is involved.

`serial` and `parallel` are separated because rustc codegens on a thread per
codegen unit: on a large build a third of all compile CPU is there, and mixing
it into one number gives something comparable to neither wall clock nor CPU.

This needs `cargo install samply`. If samply is missing or fails, the build is
re-run without it and the session is still sent — with `phases: null`, never a
zero, so an analysis cannot mistake "not measured" for "took no time".

Validated against nightly `-Ztime-passes` on a real crate at
`-Ccodegen-units=1`: every phase within about one point.

**Machine load** — sampled *during* the build (`build` and `watch`, the two
commands present for one): mean and max **CPU utilisation**, mean and max load
average, and pressure-stall deltas for cpu/io/memory.

CPU utilisation is the portable measure and the one to model on: every platform
has it, whereas load average is a unix concept that no crate shims honestly
(systemstat's Windows implementation returns "Not supported"; sysinfo returns
zeros). It is also the better signal even on Linux, where load average counts
uninterruptible sleepers — a disk-bound neighbour inflates it without competing
for CPU at all.

A session shipped after the fact with `send` reports `load: null` rather than a
figure measured at the wrong time.

**Artifact bytes** — rmeta, rlib and object bytes per crate, scanned from the
target directory when the build ends. rmeta tracks what the frontend had to
describe and object bytes what the backend had to emit, so both are good
responses and both are free. Sizes are reported **only for units being sent**:
a file name in a target directory contains a crate name, so measuring a
withheld unit would leak the identity the filter just removed.

## Platforms

Built and checked for unix and Windows. There is no portable way to ask for a
child process's CPU time, so that is the one platform-specific piece —
`wait4` on unix, `GetProcessTimes` on Windows — and hardware detection goes
through `sysinfo` rather than parsing `/proc`.

What differs, and is reported as absent rather than faked:

| | unix | Windows |
| --- | --- | --- |
| per-unit CPU | user + sys | user + kernel |
| peak RSS | yes | not from `GetProcessTimes` — `null` |
| CPU utilisation | yes | yes — the portable contention signal |
| load average | yes | none exists — `null`, never `0.0` |
| pressure stalls (PSI) | Linux only | `null` |
| virtualization hint | Linux only | `null` |

Reporting `0.0` for a load average that does not exist would read as "idle
machine" and bias every contention model built on the data, so those fields are
null instead.

## Compression

Submissions are zstd-compressed at level 19, and the compressed bytes are what
gets stored — the ingest Worker writes the body to R2 without decoding it. So
the codec is not a transport detail; it is the on-disk format, and it decides
whether the data is queryable.

**zstd because DuckDB reads it natively.** `read_json_auto()` decompresses
zstd (and gzip) transparently, so an uploaded session is queryable the moment it
lands, with no conversion step. DuckDB cannot read brotli, so brotli blobs
would require a separate rewrite before querying.

```
sent …: 91 events, 43 units (11 withheld), 2 KB zstd from 25 KB -> …
```

The `zstd` crate binds C. `ring` — via `ureq` → `rustls` for TLS — also requires
a C toolchain, so zstd adds no separate toolchain requirement. A TLS stack
without a C dependency would make zstd the remaining blocker because there is
no pure-Rust zstd encoder.

No negotiation and no fallback. A send that fails is simply not recorded as
sent, so the session stays queued and goes out next time — a failed upload
costs a retry, not a contribution. `cargo cratebank serve` decompresses too, so
the reference collector behaves like a real one — it sniffs the zstd magic
number rather than trusting a header, since the header says nothing about
whether the stored bytes will be readable.

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
  "repository": null,
  "env": {
    "host": "x86_64-unknown-linux-gnu",
    "profile": "dev", "jobs": 16, "num_cpus": 16, "ci": false,
    "rustc_version": "1.99.0-nightly",
    "rustc_version_verbose": "… commit-hash, commit-date, LLVM version …"
  },
  "complete": true,
  "machine": {
    "machine_id": "acme-ci",
    "cpu_model": "AMD EPYC 9554P 64-Core Processor",
    "cpu_cores": 16, "mem_gb": 63, "kernel": "6.12.93",
    "os": "linux", "arch": "x86_64", "virt": "kvm", "ci": false,
    "cargo_version": "cargo 1.96.1 (356927216 2026-06-26)"
  },
  "build_env": {
    "env": {"RUSTFLAGS": ["-C target-cpu=native", "-C lto=thin", "-C linker=clang"]},
    "config": {"build.rustc-wrapper": "sccache", "build.incremental": "false"}
  },
  "phases": {"sampler": "samply", "rate_hz": 4999, "units": [
      {"crate": "syn", "crate_type": "lib",
       "serial": {"borrowck": 1237, "type_check": 1038, "metadata_encode": 634},
       "parallel": {"codegen": 1875}}]},
  "load": {"cpu_busy_mean": 99.5, "cpu_busy_max": 99.9,
           "loadavg_mean": 8.49, "loadavg_max": 8.49, "samples": 7,
           "stall_seconds": {"cpu": 0.31, "io": 0.02, "memory": 0.0}},
  "artifacts": {"total": {"rmeta": 29190392, "rlib": 89460474, "obj": 0},
                "per_crate": {"aho_corasick": {"rmeta": 1871852, "rlib": 10809004, "obj": 0}}},
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
