-- Turn stored payloads into tables worth querying.
--
-- Ingest keeps bytes: one row per build, JSON intact, nothing interpreted.
-- This is where interpretation happens, and it happens here precisely because
-- it can be rewritten and rerun over everything ever collected. A reading baked
-- into the pipeline could only ever apply going forward.
--
-- Expects: a view `raw` with a `value` column of session JSON.
-- Produces: sessions, units, unit_sections, and the analysis view `unit_costs`.

-- ── one row per build ────────────────────────────────────────────────────────
CREATE OR REPLACE TABLE sessions AS
SELECT
  value->>'run_id'                                  AS run_id,
  (value->'events'->0->>'timestamp')::TIMESTAMP     AS started_at,
  value->>'repository'                              AS repository,
  (value->>'complete')::BOOLEAN                     AS complete,

  value->'machine'->>'machine_id'                   AS machine_id,
  value->'machine'->>'cpu_model'                    AS cpu_model,
  (value->'machine'->>'cpu_cores')::INTEGER         AS cpu_cores,
  (value->'machine'->>'mem_gb')::INTEGER            AS mem_gb,
  value->'machine'->>'os'                           AS os,
  value->'machine'->>'arch'                         AS arch,
  value->'machine'->>'kernel'                       AS kernel,
  value->'machine'->>'os_version'                   AS os_version,
  value->'machine'->>'virt'                         AS virt,
  (value->'machine'->>'ci')::BOOLEAN                AS ci,
  value->'machine'->>'cargo_version'                AS cargo_version,

  value->'env'->>'profile'                          AS profile,
  value->'env'->>'host'                             AS host,
  (value->'env'->>'jobs')::INTEGER                  AS jobs,
  (value->'env'->>'num_cpus')::INTEGER              AS num_cpus,
  value->'env'->>'rustc_version'                    AS rustc_version,
  -- the commit hash is the part that actually identifies a nightly
  regexp_extract(value->'env'->>'rustc_version_verbose', 'commit-hash: (\w+)', 1) AS rustc_commit,
  regexp_extract(value->'env'->>'rustc_version_verbose', 'LLVM version: ([\d.]+)', 1) AS llvm_version,

  value->'build_env'->'config'->>'build.rustc-wrapper' AS rustc_wrapper,
  value->'build_env'->'env'->'RUSTFLAGS'               AS rustflags,

  (value->'load'->>'cpu_busy_mean')::DOUBLE         AS cpu_busy_mean,
  (value->'load'->>'cpu_busy_max')::DOUBLE          AS cpu_busy_max,
  (value->'load'->>'loadavg_mean')::DOUBLE          AS loadavg_mean,
  (value->'load'->'stall_seconds'->>'cpu')::DOUBLE  AS cpu_stall_s,

  (value->'counts'->>'units')::INTEGER              AS units,
  (value->'counts'->>'units_withheld')::INTEGER     AS units_withheld,
  (value->'counts'->>'sections')::INTEGER           AS sections,
  (value->'cpu_coverage'->>'matched')::INTEGER      AS cpu_matched,
  (value->'artifacts'->'total'->>'rmeta')::BIGINT   AS rmeta_bytes,
  (value->'artifacts'->'total'->>'rlib')::BIGINT    AS rlib_bytes,

  'anonymous'                                       AS trust,
  value->>'client'                                  AS client
FROM raw;

-- ── one row per compilation unit ─────────────────────────────────────────────
--
-- This is the join Pipelines SQL cannot do: unit-registered carries identity,
-- unit-finished carries elapsed, unit-section-finished carries the
-- frontend/codegen split, and they are correlated only by `index`.
CREATE OR REPLACE TABLE units AS
WITH ev AS (
  SELECT value->>'run_id' AS run_id, unnest(from_json(value->'events', '["JSON"]')) AS e
  FROM raw
),
registered AS (
  SELECT run_id,
         (e->>'index')::INTEGER   AS idx,
         e->>'package_id'         AS package_id,
         e->'target'->>'name'     AS target_name,
         e->'target'->>'kind'     AS target_kind,
         e->>'mode'               AS mode,
         e->>'platform'           AS platform,
         from_json(e->'features', '["VARCHAR"]')     AS features,
         from_json(e->'dependencies', '["INTEGER"]') AS dep_indices,
         (e->>'cpu_s')::DOUBLE    AS cpu_s,
         (e->>'max_rss_kb')::BIGINT AS max_rss_kb
  FROM ev WHERE e->>'reason' = 'unit-registered'
),
finished AS (
  SELECT run_id, (e->>'index')::INTEGER AS idx, max((e->>'elapsed')::DOUBLE) AS wall_s
  FROM ev WHERE e->>'reason' = 'unit-finished' GROUP BY 1, 2
),
rmeta AS (
  SELECT run_id, (e->>'index')::INTEGER AS idx, max((e->>'elapsed')::DOUBLE) AS rmeta_at_s
  FROM ev WHERE e->>'reason' = 'unit-rmeta-finished' GROUP BY 1, 2
),
fresh AS (
  SELECT run_id, (e->>'index')::INTEGER AS idx, any_value(e->>'status') AS fingerprint
  FROM ev WHERE e->>'reason' = 'unit-fingerprint' GROUP BY 1, 2
)
SELECT r.*,
       f.wall_s,
       m.rmeta_at_s,
       fr.fingerprint,
       -- crates.io ids carry name@version; keep them as columns worth grouping on
       regexp_extract(r.package_id, '#([^@]+)@(.+)$', 1) AS name,
       regexp_extract(r.package_id, '#([^@]+)@(.+)$', 2) AS version,
       CASE WHEN r.package_id LIKE 'registry+%' THEN 'crates.io'
            WHEN r.package_id LIKE 'git+%'      THEN 'git'
            WHEN r.package_id LIKE 'workspace#%' THEN 'workspace'
            ELSE 'other' END                            AS source
FROM registered r
LEFT JOIN finished f USING (run_id, idx)
LEFT JOIN rmeta    m USING (run_id, idx)
LEFT JOIN fresh    fr USING (run_id, idx);

-- ── the sections cargo actually reports, one row each ────────────────────────
--
-- `-Zsection-timings` emits `codegen` and `link`. There is deliberately no
-- `frontend` section: the frontend is what happens before codegen starts, so it
-- is derived below rather than read off.
CREATE OR REPLACE TABLE unit_sections AS
WITH ev AS (
  SELECT value->>'run_id' AS run_id, unnest(from_json(value->'events', '["JSON"]')) AS e
  FROM raw
),
bounds AS (
  SELECT run_id,
         (e->>'index')::INTEGER AS idx,
         e->>'section'          AS section,
         e->>'reason'           AS reason,
         (e->>'elapsed')::DOUBLE AS at_s
  FROM ev WHERE e->>'reason' IN ('unit-section-started', 'unit-section-finished')
)
SELECT run_id, idx, section,
       max(at_s) FILTER (WHERE reason = 'unit-section-finished')
     - max(at_s) FILTER (WHERE reason = 'unit-section-started') AS seconds
FROM bounds GROUP BY 1, 2, 3;

-- ── per-unit timeline, from which the frontend falls out ─────────────────────
CREATE OR REPLACE TABLE unit_timeline AS
WITH ev AS (
  SELECT value->>'run_id' AS run_id, unnest(from_json(value->'events', '["JSON"]')) AS e
  FROM raw
),
marks AS (
  SELECT run_id,
         (e->>'index')::INTEGER AS idx,
         e->>'reason'            AS reason,
         e->>'section'           AS section,
         (e->>'elapsed')::DOUBLE AS at_s
  FROM ev
  WHERE e->>'reason' IN ('unit-started', 'unit-finished', 'unit-rmeta-finished',
                         'unit-section-started', 'unit-section-finished')
)
SELECT run_id, idx,
       max(at_s) FILTER (WHERE reason = 'unit-started')                                AS started_s,
       max(at_s) FILTER (WHERE reason = 'unit-section-started'  AND section = 'codegen') AS codegen_start_s,
       max(at_s) FILTER (WHERE reason = 'unit-section-finished' AND section = 'codegen') AS codegen_end_s,
       max(at_s) FILTER (WHERE reason = 'unit-section-started'  AND section = 'link')    AS link_start_s,
       max(at_s) FILTER (WHERE reason = 'unit-section-finished' AND section = 'link')    AS link_end_s,
       max(at_s) FILTER (WHERE reason = 'unit-rmeta-finished')                          AS rmeta_s,
       max(at_s) FILTER (WHERE reason = 'unit-finished')                                AS finished_s
FROM marks GROUP BY 1, 2;

-- ── the view most questions actually want ────────────────────────────────────
--
-- frontend_s is derived, not measured: everything before codegen begins. Where
-- a unit never reaches codegen (a `cargo check`, a fresh unit) it is the whole
-- unit, which is the right answer rather than a null.
CREATE OR REPLACE VIEW unit_costs_v AS
SELECT u.run_id, u.idx, u.name, u.version, u.source, u.target_kind, u.mode, u.platform,
       u.features, u.fingerprint,
       u.cpu_s, u.wall_s, u.max_rss_kb,
       coalesce(t.codegen_start_s, t.finished_s) - t.started_s        AS frontend_s,
       t.codegen_end_s - t.codegen_start_s                            AS codegen_s,
       t.link_end_s - t.link_start_s                                  AS link_s,
       t.rmeta_s - t.started_s                                        AS rmeta_s,
       ses.profile, ses.rustc_version, ses.rustc_commit, ses.llvm_version,
       ses.cpu_model, ses.cpu_cores, ses.ci, ses.machine_id,
       ses.cpu_busy_mean, ses.started_at
FROM units u
LEFT JOIN unit_timeline t USING (run_id, idx)
JOIN sessions ses USING (run_id);
