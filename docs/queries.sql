SELECT
    run_id,
    timestamp,
    rustc_version,
    cpu_model,
    units,
    complete
FROM sessions
ORDER BY timestamp DESC;

SELECT
    package,
    crate,
    mode,
    round(duration, 3) AS seconds
FROM units
ORDER BY duration DESC
LIMIT 20;

SELECT
    phase,
    sum(samples) AS samples,
    round(100.0 * sum(samples) / sum(sum(samples)) OVER (), 1) AS share_pct
FROM phases
WHERE thread = 'serial'
GROUP BY phase
ORDER BY samples DESC;

SELECT
    package,
    round(sum(frontend), 3) AS frontend_seconds,
    round(sum(codegen), 3) AS codegen_seconds,
    round(sum(link), 3) AS link_seconds
FROM units
GROUP BY package
ORDER BY frontend_seconds + codegen_seconds + link_seconds DESC
LIMIT 20;

SELECT
    package,
    sum(unblocked) AS units_unblocked
FROM units
GROUP BY package
ORDER BY units_unblocked DESC
LIMIT 20;

SELECT
    run_id,
    round(avg(cpu_pct), 1) AS mean_cpu_pct
FROM timeline
WHERE cpu_pct IS NOT NULL
GROUP BY run_id
ORDER BY mean_cpu_pct DESC;

SELECT
    flag,
    value,
    count(*) AS units
FROM unit_flags
GROUP BY flag, value
ORDER BY flag, units DESC;
