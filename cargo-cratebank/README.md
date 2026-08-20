# cargo-cratebank

Opt-in sharing of the build timings you were already producing.

Cargo (nightly) already records everything: `-Zbuild-analysis` writes one JSONL
session log per invocation to `$CARGO_HOME/log/`, and `-Zsection-timings` folds
rustc's frontend/codegen section boundaries into the same stream. cratebank does
not instrument anything itself — no wrapper in the compile path, no extra
builds, no conflict with sccache. It reads those logs, redacts private
identity, and POSTs them.

```
cargo cratebank build [cargo args…]   build with both flags on, then send
cargo cratebank send [--all|--session ID|--since N]
cargo cratebank status                is everything wired up?
cargo cratebank serve [--port 8787]   echo collector, for testing
```

Flags: `--dry-run` (print the exact payload, send nothing), `--endpoint URL`,
`--keep-private` (skip redaction — for your own collector only).

To record every build, put this in `.cargo/config.toml`:

```toml
[unstable]
build-analysis = true
section-timings = true

[build.analysis]
enabled = true
```

On a stable toolchain these only emit an unknown-config warning, so they are
safe to leave in place.

## Privacy

Redaction is **per unit, not per project** — a closed-source workspace still
contributes every public dependency measurement in its graph while disclosing
nothing about itself.

- units from crates.io or public git remotes: sent with full identity;
- units from local paths or private registries: `package_id` replaced with a
  stable hash, target name replaced with `<private>`, flagged `private: true`;
- `cwd`, `workspace_root`, `target_dir`, `manifest_path`: dropped entirely;
- the command line is reduced to flag names, with every value replaced by
  `<arg>`;
- environment variables are never read or sent.

`--dry-run` prints byte-for-byte what would be transmitted.

## Payload

One session, one POST. Events pass through verbatim (after redaction) under a
small header, because cargo's log schema is explicitly unstable — normalising
in the client would bake in today's shape. Capture broadly, model server-side.

```json
{ "cratebank_schema": 1, "client": "cargo-cratebank 0.1.0",
  "run_id": "…", "redacted": true,
  "env": {"host", "profile", "jobs", "num_cpus", "rustc_version", "ci", …},
  "counts": {"events", "units", "sections", "private_units"},
  "events": [ … cargo's JSONL, verbatim … ] }
```
