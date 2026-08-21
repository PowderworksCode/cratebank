# Schema

## The individual: a compilation class

```
class_id = hash(package, version, source_id, resolved_features,
                cone (recursively, by class_id), profile, rustflags, target)
```

Deliberately **excludes the toolchain**. Identity is the specimen; *when* it was
measured is separate. So a measurement keys on `(class_id, observed_at)`,
observations join on `class_id`, and "same class, new nightly, what moved?" is
one join across every class in the ecosystem.

The cone (transitive dependency closure) is part of identity because a unit's
compilation genuinely depends on it: every unit deserializes its cone's rmeta,
and in dev profiles `share-generics` means what upstream already instantiated
changes what this unit must emit.

## Tables

**classes** — one row per class_id. Identity, provenance, `dep_class_ids`
(direct edges: the DAG), `cone_class_ids` (precomputed closure: summability),
source pins.

**measurements** — one row per `(class_id, observation)`. Terminal
responses (CPU, wall, RSS, faults), phase decomposition, intermediate
responses (mono items, artifact bytes), conservation verdict, capture context
(machine, load, schedule position, harness versions). Replicates are rows,
never averaged at capture.

**projects** — one row per project view: `unit_class_ids`, unit graph,
link and build-script measurements, whole-build envelope.

**machines** — one row per contributing machine profile (see
`contributed-builds.md`), including its calibration factor.

**environments** — toolchain, policy attestations, client version: what a
measurement was taken under.

## Summability

Aggregates are one `unnest` + join, never a recursive CTE:

```sql
-- tokio and its whole cone
SELECT sum(m.cpu_s)
FROM classes c, unnest(c.cone_class_ids) AS t(id)
JOIN measurements m ON m.class_id = t.id
WHERE c.name = 'tokio';
```

Total cost decomposes exactly:

```
Total(project) = Σ intrinsic(class) + link + build-scripts
```

Costs a dependency *causes* land inside its consumers' class rows (generic
instantiations, proc-macro execution, trait obligations) and are attributable
because mono items and obligations carry their defining crate in the def-path.
Linking — and LTO where enabled — is the explicitly non-additive remainder.

## Format

Parquet is canonical: frozen spec, every engine reads it, HTTP range reads
work, and new data is an append. Anything that wants a single-file database
can build one from the parquet; canonical data never lives only there. Contributed builds land as parquet directly (see `ingest.md`).

```
cratebank/
  raw/year=/month=/day=/*.parquet    # what arrived, verbatim
```

Not published: compiled artifacts, and sources — crates.io and pinned repos
are the source archive; cratebank stores pins and patches only.
