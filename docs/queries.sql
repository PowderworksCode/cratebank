-- Recent contributed builds and their compiler/machine context.
SELECT
    run_id,
    timestamp,
    rustc_version,
    cpu_model,
    units,
    complete
FROM sessions
ORDER BY timestamp DESC;

-- The slowest individual Cargo compilation units by wall time.
SELECT
    package,
    crate,
    mode,
    round(duration, 3) AS seconds
FROM units
ORDER BY duration DESC
LIMIT 20;

-- CPU-weighted compiler phase share on serial rustc threads.
SELECT
    phase,
    sum(samples) AS samples,
    round(100.0 * sum(samples) / sum(sum(samples)) OVER (), 1) AS share_pct
FROM phases
WHERE thread = 'serial'
GROUP BY phase
ORDER BY samples DESC;

-- Cargo's wall-clock frontend, codegen, and link spans by package.
SELECT
    package,
    round(sum(frontend), 3) AS frontend_seconds,
    round(sum(codegen), 3) AS codegen_seconds,
    round(sum(link), 3) AS link_seconds
FROM units
GROUP BY package
ORDER BY frontend_seconds + codegen_seconds + link_seconds DESC
LIMIT 20;

-- Packages whose completion released the most downstream units.
SELECT
    package,
    sum(unblocked) AS units_unblocked
FROM units
GROUP BY package
ORDER BY units_unblocked DESC
LIMIT 20;

-- Mean whole-machine CPU utilization for each sampled build.
SELECT
    run_id,
    round(avg(cpu_pct), 1) AS mean_cpu_pct
FROM timeline
WHERE cpu_pct IS NOT NULL
GROUP BY run_id
ORDER BY mean_cpu_pct DESC;

-- Compilation settings observed across public units.
SELECT
    flag,
    value,
    count(*) AS units
FROM unit_flags
GROUP BY flag, value
ORDER BY flag, units DESC;
