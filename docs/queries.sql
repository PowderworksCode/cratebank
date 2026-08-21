-- Poking at cratebank data with DuckDB.
--
-- ---------------------------------------------------------------------------
-- The easy way: public parquet, no credentials, no setup at all.
-- ---------------------------------------------------------------------------
--
--   INSTALL httpfs; LOAD httpfs;
--
--   SELECT crate, round(duration, 3) AS secs
--   FROM 'https://data.cratebank.io/units.parquet'
--   ORDER BY duration DESC LIMIT 10;
--
-- Order by `duration`, not `finished`. cargo's `elapsed` field is a TIMESTAMP
-- (seconds since the build began), so sorting by it answers "which crates
-- finished last", which is a different and usually less interesting question:
-- on a real build the last finishers took ~90ms each while the slowest units
-- were build scripts taking over a second. `units.parquet` carries `started`,
-- `finished` and `duration` separately so both questions are answerable.
--
--   SELECT * FROM 'https://data.cratebank.io/sessions.parquet';
--
-- Those are rebuilt nightly from the raw blobs. A glob over https:// will NOT
-- work -- expanding `**` needs object listing, and listing needs credentials.
-- Everything below is the credentialed path against the raw sessions, which is
-- the ground truth if you need something the parquet tables do not carry.
--
--
-- Sessions are stored as zstd-compressed JSON, one object per build, and DuckDB
-- decompresses zstd natively -- so these read the uploaded bytes directly. There
-- is no conversion step and nothing to load first.
--
--   duckdb -init docs/queries.sql
--
-- ---------------------------------------------------------------------------
-- Setup: point DuckDB at R2. Fill in from .env, or export them and use
-- getenv() as below.
-- ---------------------------------------------------------------------------

INSTALL httpfs; LOAD httpfs;

CREATE OR REPLACE SECRET r2 (
    TYPE r2,
    KEY_ID     getenv('R2_ACCESS_KEY_ID'),
    SECRET     getenv('R2_SECRET_ACCESS_KEY'),
    ACCOUNT_ID getenv('R2_ACCOUNT_ID')
);

-- Every session. `union_by_name` keeps this working as the payload gains
-- fields, which it will -- cargo's log format is still moving.
CREATE OR REPLACE VIEW sessions AS
SELECT * FROM read_json_auto(
    'r2://cratebank/sessions/**/*.json.zst',
    union_by_name = true
);

-- One row per event, with its session's run_id alongside. This is the view
-- worth starting from: everything interesting is an aggregate over it.
CREATE OR REPLACE VIEW events AS
SELECT s.run_id AS session_run_id, e.*
FROM sessions s, UNNEST(s.events) AS t(e);

-- ---------------------------------------------------------------------------
-- Queries. Run them one at a time; none of them are run by -init.
-- ---------------------------------------------------------------------------

-- What have we got?
--   SELECT run_id, counts.units AS units, counts.events AS events,
--          complete, env.rustc_version AS rustc, machine.cpu_model AS cpu,
--          env.timestamp AS at
--   FROM sessions ORDER BY at DESC;

-- Slowest crates in a build, from the raw events. This is the self-join the
-- ingest design is built around: the crate name is on `unit-registered`, the
-- timings are on `unit-started`/`unit-finished`, and they are correlated only
-- by `index`. Doing it here rather than at capture time is what makes it
-- re-runnable and correctable.
--
-- Note the subtraction. `elapsed` is a timestamp, not a duration.
--   WITH reg AS (SELECT session_run_id, index, target.name AS crate, mode
--                FROM events WHERE reason = 'unit-registered'),
--        beg AS (SELECT session_run_id, index, elapsed AS started
--                FROM events WHERE reason = 'unit-started'),
--        fin AS (SELECT session_run_id, index, elapsed AS finished
--                FROM events WHERE reason = 'unit-finished')
--   SELECT reg.crate, reg.mode, round(fin.finished - beg.started, 4) AS secs
--   FROM reg JOIN beg USING (session_run_id, index)
--            JOIN fin USING (session_run_id, index)
--   ORDER BY secs DESC LIMIT 20;

-- Where does the time actually go -- frontend, codegen, or link?
--   WITH beg AS (SELECT session_run_id, index, section, elapsed AS t0
--                FROM events WHERE reason = 'unit-section-started'),
--        fin AS (SELECT session_run_id, index, section, elapsed AS t1
--                FROM events WHERE reason = 'unit-section-finished')
--   SELECT section, count(*) AS units, round(sum(t1 - t0), 3) AS total_secs
--   FROM beg JOIN fin USING (session_run_id, index, section)
--   GROUP BY 1 ORDER BY total_secs DESC;
-- (cargo emits `codegen` and `link` sections; there is no `frontend` today.)

-- Critical path proxy: which units unblocked the most others?
-- Note this needs the join too -- `unit-finished` carries `unblocked` but not
-- the crate name, which only `unit-registered` has. Selecting target.name off
-- a unit-finished row silently yields NULL.
--   WITH reg AS (SELECT session_run_id, index, target.name AS crate
--                FROM events WHERE reason = 'unit-registered'),
--        fin AS (SELECT session_run_id, index, len(unblocked) AS n
--                FROM events WHERE reason = 'unit-finished' AND unblocked IS NOT NULL)
--   SELECT reg.crate, fin.n AS unblocked_count
--   FROM reg JOIN fin USING (session_run_id, index)
--   ORDER BY unblocked_count DESC LIMIT 20;

-- Was the machine actually busy, or waiting? A build that is slow on an idle
-- box is a different problem from one that is slow on a saturated one.
--   SELECT run_id,
--          round(load.loadavg_mean, 2) AS load_mean,
--          round(load.cpu_busy_mean, 1) AS cpu_busy_pct,
--          machine.cpu_cores AS cores,
--          env.jobs AS jobs
--   FROM sessions;

-- Same crate across sessions -- the whole point, once there is more than one
-- contributor. Needs several sessions to say anything.
--   WITH reg AS (SELECT session_run_id, index, target.name AS crate
--                FROM events WHERE reason = 'unit-registered'),
--        fin AS (SELECT session_run_id, index, elapsed
--                FROM events WHERE reason = 'unit-finished')
--   SELECT crate, count(*) AS observations,
--          round(median(elapsed), 3) AS median_secs,
--          round(min(elapsed), 3) AS best, round(max(elapsed), 3) AS worst
--   FROM reg JOIN fin USING (session_run_id, index)
--   GROUP BY crate HAVING count(*) > 1
--   ORDER BY median_secs DESC LIMIT 20;

-- A local file works identically -- swap the path and drop the secret:
--   SELECT * FROM read_json_auto('/tmp/build.json.zst');
