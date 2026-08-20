# cratebank

A public cohort of Rust compilations: what it costs to compile the ecosystem,
measured, versioned, and published for anyone to query.

Each **stratum** is one immutable monthly release — a pinned nightly, the top
~1000 crates and projects, every compilation unit measured once, published as
parquet + raw instrument outputs on object storage. You analyse it with
`ATTACH` and SQL; there is no server.

Two collection paths feed the same schema:

- **the archival round** — one controlled machine, uniform policy, deep
  instrumentation. The calibrated backbone.
- **contributed builds** — volunteers run `cargo whyslow` on their own
  projects and opt in to sending measurements. Coverage, real configurations,
  real hardware, at a scale no single machine reaches.

The unit of analysis is the **compilation class**, not the project: a
(package, version, features, cone, profile, flags, target) fingerprint. Two
projects compiling the same class the same way are measuring one individual
twice — which is what makes deduplication, cross-machine calibration and
cross-stratum comparison all work.

| repo | role |
| --- | --- |
| **cratebank** (this) | the dataset: schema, capture manifest, stratum builder, publication |
| **whyslow** | the instrument: `cargo whyslow` measures a build and explains why it was slow; also the contribution client |
| **crategen** | synthetic workspaces with controlled characteristics; also cratebank's calibration standard |

Statistical methodology for the *study* that uses this data lives in the
research repository. cratebank is the bank, not the analysis.

## Docs

- [`docs/schema.md`](docs/schema.md) — the data model and why it is keyed this way
- [`docs/capture-manifest.md`](docs/capture-manifest.md) — everything recorded about a build
- [`docs/contributed-builds.md`](docs/contributed-builds.md) — the volunteer path: design, privacy, calibration
