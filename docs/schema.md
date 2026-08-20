# Schema

## The individual: a compilation class

```
class_id = hash(package, version, source_id, resolved_features,
                cone (recursively, by class_id), profile, rustflags, target)
```

Deliberately **excludes the toolchain**. Identity is the specimen; *when* it
was measured is the era. So `measurements` keys on `(class_id, era)`, strata
join on `class_id`, and "same class, new nightly, what moved?" is one join
across every class in the ecosystem.

The cone (transitive dependency closure) is part of identity because a unit's
compilation genuinely depends on it: every unit deserializes its cone's rmeta,
and in dev profiles `share-generics` means what upstream already instantiated
changes what this unit must emit.

## Tables

**classes** — one row per class_id. Identity, provenance, `dep_class_ids`
(direct edges: the DAG), `cone_class_ids` (precomputed closure: summability),
source pins.

**measurements** — one row per `(class_id, era, observation)`. Terminal
responses (CPU, wall, RSS, faults), phase decomposition, intermediate
responses (mono items, artifact bytes), conservation verdict, capture context
(machine, load, schedule position, harness versions). Replicates are rows,
never averaged at capture.

**projects** — one row per project view per era: `unit_class_ids`, unit graph,
link and build-script measurements, whole-build envelope.

**machines** — one row per contributing machine profile (see
`contributed-builds.md`), including its calibration factor.

**eras** — one row per stratum: toolchain, corpus list, policy attestations,
seeds, tool revisions, schema version.

## Summability

Aggregates are one `unnest` + join, never a recursive CTE:

```sql
-- tokio and its whole cone
SELECT sum(m.cpu_s)
FROM classes c, unnest(c.cone_class_ids) AS t(id)
JOIN measurements m ON m.class_id = t.id AND m.era = '2026-08'
WHERE c.name = 'tokio' AND c.era = '2026-08';
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
work, a stratum is an append. A `.duckdb` file ships alongside as convenience
(single-file ATTACH, predefined views), regenerated from the parquet; canonical
data never lives only there. Contributed builds land in ClickHouse first (a
high-rate append stream) and are compacted into each stratum's parquet at
release.

```
cratebank/2026-08/
  tables/          classes, measurements, projects, machines, eras  (parquet)
  cratebank.duckdb convenience database + views
  raw/{class}/     self-profile.zst, mono.json.zst, timings.html.zst, stderr
  manifest.json    era record, schema version, checksums
```

Not published: compiled artifacts, and sources — crates.io and pinned repos
are the source archive; cratebank stores pins and patches only.
