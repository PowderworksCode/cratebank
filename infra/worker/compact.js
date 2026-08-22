// cratebank compaction: turn the raw session blobs into public parquet.
//
// The raw uploads are zstd-compressed JSON, one object per build, and they stay
// the ground truth -- nothing here is authoritative. This job produces the
// *convenient* form: two flat parquet tables at a stable public URL, so anyone
// can query the census with one line and no credentials:
//
//   SELECT * FROM 'https://data.cratebank.io/units.parquet';
//
// That indirection is the point. A reading done here can be corrected and
// re-run over everything already collected, which is exactly what could not be
// done if the client had flattened the payload before sending it.
//
// Both dependencies are pure JavaScript on purpose: Workers' DecompressionStream
// supports only gzip/deflate/deflate-raw, so zstd needs a library, and a WASM
// parquet encoder would cost megabytes of script budget for no benefit.

import { decompress } from 'fzstd'
import { parquetWriteBuffer } from 'hyparquet-writer'

const SESSIONS = 'sessions/'

// cargo package ids look like `registry+https://…/index#serde@1.0.0` or
// `workspace#bun_alloc@0.0.0`. The bare crate name is the useful thing --
// without it, every build script is called `build-script-build` and the obvious
// `ORDER BY duration` query says nothing about which package is slow.
function packageName(id) {
  if (!id) return null
  const hash = id.lastIndexOf('#')
  if (hash === -1) return null
  const frag = id.slice(hash + 1)
  const at = frag.indexOf('@') // versions may contain '@'-free '+' build metadata
  return at === -1 ? frag || null : frag.slice(0, at) || null
}

// Deliberately unbatched: hold every row in memory, write one file. At the
// current scale that is free, and the failure mode is loud rather than subtle
// -- the Worker's 128 MB ceiling produces an error, not a silently truncated
// dataset. The escape hatch when it arrives is per-day compaction into
// daily/YYYY-MM-DD.parquet, merging only the days not already merged, which the
// hive-partitioned key layout already supports. `stats` below is what tells you
// it is coming; watch objects and bytes in the cron logs.

/** Every object under sessions/, following the 1000-per-page cursor. */
async function listAll(bucket) {
  const keys = []
  let cursor
  do {
    const page = await bucket.list({ prefix: SESSIONS, cursor, limit: 1000 })
    for (const o of page.objects) keys.push(o.key)
    cursor = page.truncated ? page.cursor : undefined
  } while (cursor)
  return keys
}

/** One session blob -> rows for both tables. */
function flatten(session, key) {
  const s = session
  // `elapsed` on a cargo event is a TIMESTAMP -- seconds since the build
  // started -- not a duration. Every event for a unit carries a larger value
  // than the last. Publishing it as a duration silently answers "which crates
  // finished last" when the reader asked "which crates were slowest", and the
  // two rankings are completely different: on a real build the top finishers
  // took 50-90ms each, while the genuinely slow units were build scripts at
  // over a second.
  //
  // So durations are differences, computed here, and the raw timestamps are
  // published too since finish order is what critical-path analysis needs.
  const units = new Map()
  const sectionStart = new Map() // `${index}:${section}` -> timestamp
  for (const e of s.events ?? []) {
    if (e.reason === 'unit-registered') {
      units.set(e.index, {
        crate: e.target?.name ?? null,
        mode: e.mode ?? null,
        package_id: e.package_id ?? null,
        package: packageName(e.package_id),
        started: null,
        finished: null,
        duration: null,
        frontend: null,
        codegen: null,
        link: null,
        sectioned: false,
        unblocked: null,
      })
    } else if (e.reason === 'unit-started') {
      const u = units.get(e.index)
      if (u) u.started = e.elapsed ?? null
    } else if (e.reason === 'unit-finished') {
      const u = units.get(e.index)
      if (u) {
        u.finished = e.elapsed ?? null
        if (u.started != null && u.finished != null) u.duration = u.finished - u.started
        u.unblocked = Array.isArray(e.unblocked) ? e.unblocked.length : null
      }
    } else if (e.reason === 'unit-section-started') {
      if (e.section && e.elapsed != null) sectionStart.set(`${e.index}:${e.section}`, e.elapsed)
    } else if (e.reason === 'unit-section-finished') {
      const u = units.get(e.index)
      const t0 = sectionStart.get(`${e.index}:${e.section}`)
      // Sections can repeat for a unit, so accumulate durations rather than
      // overwrite. Only known section names get a column; cargo currently
      // emits codegen and link, and `frontend` stays null until it emits one.
      if (u && e.section && e.elapsed != null && t0 != null && e.section in u) {
        u[e.section] = (u[e.section] ?? 0) + (e.elapsed - t0)
        u.sectioned = true
      }
    }
  }

  // `frontend` is DERIVED, not measured. cargo emits sections for `codegen`
  // and `link` only, so the parsing/type-checking/borrow-checking time is
  // whatever is left of the unit's duration. Without it the section columns
  // answer "where did the time go" with a hole exactly where the answer
  // usually is -- plenty of units show codegen 0.0 / link 0.0 and a
  // sub-second duration, all of which is frontend.
  //
  // Only computed for units that actually reported sections; null elsewhere
  // (build scripts, for instance, report none). It can come out slightly
  // negative when section and unit timings overlap at the edges -- that is
  // left as measured rather than clamped, because a clamp would hide it.
  for (const u of units.values()) {
    if (u.sectioned && u.duration != null) {
      u.frontend = u.duration - (u.codegen ?? 0) - (u.link ?? 0)
    }
    delete u.sectioned
  }

  const unitRows = [...units.entries()].map(([index, u]) => ({
    run_id: s.run_id ?? null,
    index,
    ...u,
  }))

  const sessionRow = {
    run_id: s.run_id ?? null,
    object_key: key,
    client: s.client ?? null,
    schema: s.cratebank_schema ?? null,
    timestamp: s.env?.timestamp ?? null,
    rustc_version: s.env?.rustc_version ?? null,
    profile: s.env?.profile ?? null,
    jobs: s.env?.jobs ?? null,
    ci: s.env?.ci ?? null,
    complete: s.complete ?? null,
    events: s.counts?.events ?? null,
    units: s.counts?.units ?? null,
    units_withheld: s.counts?.units_withheld ?? null,
    sections: s.counts?.sections ?? null,
    machine_id: s.machine?.machine_id ?? null,
    os: s.machine?.os ?? null,
    arch: s.machine?.arch ?? null,
    cpu_model: s.machine?.cpu_model ?? null,
    cpu_cores: s.machine?.cpu_cores ?? null,
    mem_gb: s.machine?.mem_gb ?? null,
    loadavg_mean: s.load?.loadavg_mean ?? null,
    cpu_busy_mean: s.load?.cpu_busy_mean ?? null,
  }

  // Sampled compiler phases, if the build was sampled. A long table rather
  // than columns: the marker set will keep changing as the phase mapping
  // improves, and a long table accepts new phases without changing columns.
  // `thread` separates the serial frontend from the per-CGU codegen
  // threads -- blending them gives a number comparable to neither wall clock
  // nor CPU.
  //
  // Deliberately NOT reusing the `frontend`/`codegen` column names from the
  // unit table: those come from artifact/rmeta boundaries (wall), these come
  // from sampling (CPU). Same words, different measurements.
  const phaseRows = []
  for (const pu of s.phases?.units ?? []) {
    for (const [thread, counts] of [['serial', pu.serial], ['parallel', pu.parallel]]) {
      for (const [phase, samples] of Object.entries(counts ?? {})) {
        phaseRows.push({
          run_id: s.run_id ?? null,
          crate: pu.crate ?? null,
          package: pu.package ?? null,
          crate_type: pu.crate_type ?? null,
          thread,
          phase,
          samples,
          wall_s: pu.wall_s ?? null,
          rate_hz: s.phases?.rate_hz ?? null,
          sampler: s.phases?.sampler ?? null,
        })
      }
    }
  }

  // cargo's own build timeline. Two series that cargo samples on *different*
  // clocks -- concurrency at 0.33s, CPU at 0.51s on the same build -- so they
  // cannot be joined by index and a row carries one or the other, never both.
  // Sparse columns cost almost nothing in parquet and the alternative is two
  // tables describing the same axis.
  //
  // `waiting` is the reason this exists: it is cargo's count of units ready to
  // build but blocked on a dependency, and nothing else collected can
  // distinguish a build that is dependency-bound from one that is CPU-bound.
  const timelineRows = []
  for (const c of s.timings?.concurrency_data ?? []) {
    timelineRows.push({
      run_id: s.run_id ?? null, t: c.t ?? null,
      active: c.active ?? null, waiting: c.waiting ?? null, inactive: c.inactive ?? null,
      cpu_pct: null,
    })
  }
  for (const [t, pct] of s.timings?.cpu_usage ?? []) {
    timelineRows.push({
      run_id: s.run_id ?? null, t: t ?? null,
      active: null, waiting: null, inactive: null, cpu_pct: pct ?? null,
    })
  }

  // Resolved compilation settings per unit, long. Long because the set is
  // open -- rustc gains flags, cargo profiles differ -- and because most are
  // uniform across a build while `features` is not, so columns would be mostly
  // repetition with one exception. These decide what a measurement *means*:
  // an opt-level 0 unit and an opt-level 3 unit are not the same specimen.
  const flagRows = []
  for (const pu of s.phases?.units ?? []) {
    for (const [flag, value] of Object.entries(pu.flags ?? {})) {
      flagRows.push({
        run_id: s.run_id ?? null,
        crate: pu.crate ?? null,
        package: pu.package ?? null,
        crate_type: pu.crate_type ?? null,
        flag,
        value: String(value),
      })
    }
  }

  return { unitRows, sessionRow, phaseRows, timelineRows, flagRows }
}

// Column builders. Two hard-won details:
//
// 1. INT64 needs real BigInt -- hyparquet-writer throws on plain numbers, and
//    it throws at write time, after every blob has already been decompressed.
// 2. They coerce rather than trust. cargo's log format is explicitly still
//    moving, which is the premise this whole project is built on, so a field
//    that changes type must not kill the nightly job for everyone. The raw
//    blobs remain the ground truth if a coercion here is ever wrong.
//    (`cratebank_schema` is a number, not a string -- found exactly this way.)
const num = v => {
  const n = Number(v)
  return Number.isFinite(n) ? n : null
}
const str = (rows, k) => ({
  name: k,
  data: rows.map(r => (r[k] == null ? null : String(r[k]))),
  type: 'STRING',
})
const dbl = (rows, k) => ({
  name: k,
  data: rows.map(r => (r[k] == null ? null : num(r[k]))),
  type: 'DOUBLE',
})
const i64 = (rows, k) => ({
  name: k,
  data: rows.map(r => {
    const n = r[k] == null ? null : num(r[k])
    return n == null ? null : BigInt(Math.trunc(n))
  }),
  type: 'INT64',
})
const bool = (rows, k) => ({
  name: k,
  data: rows.map(r => (r[k] == null ? null : Boolean(r[k]))),
  type: 'BOOLEAN',
})

async function compact(env) {
  const started = Date.now()
  const keys = await listAll(env.BUCKET)

  const unitRows = []
  const sessionRows = []
  const phaseRows = []
  const timelineRows = []
  const flagRows = []
  let bytesIn = 0
  let failed = 0

  for (const key of keys) {
    try {
      const obj = await env.BUCKET.get(key)
      if (!obj) continue
      const raw = new Uint8Array(await obj.arrayBuffer())
      bytesIn += raw.byteLength
      const json = new TextDecoder().decode(decompress(raw))
      const parsed = JSON.parse(json)
      const { unitRows: u, sessionRow, phaseRows: ph, timelineRows: tl, flagRows: fl } =
        flatten(parsed, key)
      unitRows.push(...u)
      sessionRows.push(sessionRow)
      phaseRows.push(...ph)
      timelineRows.push(...tl)
      flagRows.push(...fl)
    } catch (e) {
      // One malformed blob must not cost the whole run. Count it and move on;
      // the raw object is still there to inspect.
      failed++
      console.error(`compact: skipping ${key}: ${e}`)
    }
  }

  if (!sessionRows.length) {
    return { ok: false, error: 'no sessions found', keys: keys.length }
  }

  const unitsBuf = parquetWriteBuffer({
    columnData: [
      str(unitRows, 'run_id'), i64(unitRows, 'index'),
      str(unitRows, 'crate'), str(unitRows, 'package'), str(unitRows, 'mode'),
      str(unitRows, 'package_id'),
      dbl(unitRows, 'duration'), dbl(unitRows, 'started'), dbl(unitRows, 'finished'),
      dbl(unitRows, 'frontend'), dbl(unitRows, 'codegen'), dbl(unitRows, 'link'),
      i64(unitRows, 'unblocked'),
    ],
    compressed: true,
  })

  const sessionsBuf = parquetWriteBuffer({
    columnData: [
      str(sessionRows, 'run_id'), str(sessionRows, 'object_key'),
      str(sessionRows, 'client'), i64(sessionRows, 'schema'),
      str(sessionRows, 'timestamp'), str(sessionRows, 'rustc_version'),
      str(sessionRows, 'profile'), i64(sessionRows, 'jobs'), bool(sessionRows, 'ci'),
      bool(sessionRows, 'complete'),
      i64(sessionRows, 'events'), i64(sessionRows, 'units'),
      i64(sessionRows, 'units_withheld'), i64(sessionRows, 'sections'),
      str(sessionRows, 'machine_id'), str(sessionRows, 'os'), str(sessionRows, 'arch'),
      str(sessionRows, 'cpu_model'), i64(sessionRows, 'cpu_cores'), i64(sessionRows, 'mem_gb'),
      dbl(sessionRows, 'loadavg_mean'), dbl(sessionRows, 'cpu_busy_mean'),
    ],
    compressed: true,
  })

  const phasesBuf = phaseRows.length
    ? parquetWriteBuffer({
        columnData: [
          str(phaseRows, 'run_id'), str(phaseRows, 'crate'), str(phaseRows, 'package'),
          str(phaseRows, 'crate_type'),
          str(phaseRows, 'thread'), str(phaseRows, 'phase'), i64(phaseRows, 'samples'),
          dbl(phaseRows, 'wall_s'), i64(phaseRows, 'rate_hz'), str(phaseRows, 'sampler'),
        ],
        compressed: true,
      })
    : null

  const timelineBuf = timelineRows.length
    ? parquetWriteBuffer({
        columnData: [
          str(timelineRows, 'run_id'), dbl(timelineRows, 't'),
          i64(timelineRows, 'active'), i64(timelineRows, 'waiting'),
          i64(timelineRows, 'inactive'), dbl(timelineRows, 'cpu_pct'),
        ],
        compressed: true,
      })
    : null

  const flagsBuf = flagRows.length
    ? parquetWriteBuffer({
        columnData: [
          str(flagRows, 'run_id'), str(flagRows, 'crate'), str(flagRows, 'package'),
          str(flagRows, 'crate_type'), str(flagRows, 'flag'), str(flagRows, 'value'),
        ],
        compressed: true,
      })
    : null

  const day = new Date().toISOString().slice(0, 10)
  const put = (k, b) =>
    env.BUCKET.put(k, b, { httpMetadata: { contentType: 'application/vnd.apache.parquet' } })

  // Stable names are the public interface; the dated copies are an audit trail,
  // so a bad run can be diagnosed against the file it actually produced.
  // The schema travels with the data. Published beside the parquet so anyone
  // holding a copy can interpret it without this repository. Versioning keeps
  // every snapshot tied to its schema. The schema is generated from the same
  // column lists that build the files, so it cannot drift from them.
  const schema = {
    version: 1,
    generated: new Date().toISOString(),
    note: 'Raw sessions under sessions/ are the ground truth; these tables are derived and rebuilt nightly.',
    tables: {
      'units.parquet': {
        grain: 'one row per compilation unit',
        source: 'cargo build-analysis session log',
        columns: {
          run_id: 'session id; joins to sessions.parquet',
          index: 'cargo unit index within the session',
          crate: "crate name; every build script is called 'build-script-build'",
          package: 'package name parsed from package_id -- the useful one',
          mode: 'build | run-custom-build',
          package_id: 'cargo package id, verbatim',
          duration: 'seconds; finished - started',
          started: 'seconds since build start (timestamp, not a duration)',
          finished: 'seconds since build start',
          frontend: 'seconds to rmeta; WALL, from artifact boundaries',
          codegen: 'seconds after rmeta; WALL',
          link: 'seconds; WALL',
          unblocked: 'count of units this one released',
        },
      },
      'sessions.parquet': {
        grain: 'one row per build',
        columns: {
          run_id: 'session id', object_key: 'the raw blob this was derived from',
          client: 'cargo-cratebank version', schema: 'payload schema version',
          timestamp: 'build start, RFC3339', rustc_version: '', profile: 'dev | release',
          jobs: '-j value', ci: 'CI env var was set', complete: 'every registered unit finished',
          events: '', units: '', units_withheld: 'private units omitted; the graph is partial',
          sections: '', machine_id: 'self-declared, unverified', os: '', arch: '',
          cpu_model: '', cpu_cores: '', mem_gb: '',
          loadavg_mean: 'machine load during the build', cpu_busy_mean: 'percent',
        },
      },
      'unit_flags.parquet': {
        grain: 'one row per unit per compilation setting',
        source: 'the rustc command line, from the profile',
        caution: 'paths are deliberately absent -- every one would name the '
          + "builder's machine. `incremental` is true/false only, for the same "
          + 'reason, though its presence shifts every timing in the session.',
        columns: {
          run_id: 'joins to sessions.parquet', crate: '', package: '', crate_type: '',
          flag: 'opt-level | debuginfo | codegen-units | panic | lto | edition | '
            + 'target | features | incremental | ...',
          value: 'always a string; features is comma-separated',
        },
      },
      'timeline.parquet': {
        grain: 'one row per timeline sample',
        source: 'cargo build --timings',
        caution: 'cargo samples concurrency and CPU on different clocks, so a '
          + 'row carries the concurrency triple OR cpu_pct, never both. '
          + 'Filter on the column you want being non-null.',
        columns: {
          run_id: 'joins to sessions.parquet',
          t: 'seconds since build start',
          active: 'units compiling',
          waiting: 'units ready but blocked on a dependency -- the reason this '
            + 'table exists; high waiting means dependency-bound, not slow',
          inactive: 'units not yet runnable',
          cpu_pct: 'whole-machine CPU percent',
        },
      },
      'phases.parquet': {
        grain: 'one row per unit per thread-class per phase',
        source: 'sampling profiler around the whole build',
        caution: 'CPU-weighted sample counts, not wall time. Not comparable to '
          + "units.parquet's frontend/codegen, which are wall boundaries.",
        columns: {
          run_id: 'joins to sessions.parquet', crate: '',
          package: "package this unit belongs to; distinguishes the many "
            + "build scripts, which are all named build_script_build",
          crate_type: '',
          thread: "serial (main rustc) | parallel (per-codegen-unit threads); "
            + 'do not sum them into one number',
          phase: 'macro_expand | resolve | type_check | borrowck | coherence | '
            + 'monomorphize | metadata_encode | codegen | unattributed',
          samples: 'sample count, not seconds',
          wall_s: 'wall seconds for the whole unit, from the profiler',
          rate_hz: 'sampling rate, needed to turn samples into seconds',
          sampler: 'which profiler produced this',
        },
      },
    },
  }
  const schemaBuf = new TextEncoder().encode(JSON.stringify(schema, null, 2))

  await Promise.all([
    put('units.parquet', unitsBuf),
    env.BUCKET.put('schema/v1/tables.json', schemaBuf, {
      httpMetadata: { contentType: 'application/json' },
    }),
    ...(phasesBuf
      ? [put('phases.parquet', phasesBuf), put(`snapshots/phases-${day}.parquet`, phasesBuf)]
      : []),
    ...(timelineBuf
      ? [put('timeline.parquet', timelineBuf), put(`snapshots/timeline-${day}.parquet`, timelineBuf)]
      : []),
    ...(flagsBuf
      ? [put('unit_flags.parquet', flagsBuf), put(`snapshots/unit_flags-${day}.parquet`, flagsBuf)]
      : []),
    put('sessions.parquet', sessionsBuf),
    put(`snapshots/units-${day}.parquet`, unitsBuf),
    put(`snapshots/sessions-${day}.parquet`, sessionsBuf),
  ])

  const stats = {
    ok: true,
    sessions: sessionRows.length,
    units: unitRows.length,
    objects: keys.length,
    failed,
    bytes_in: bytesIn,
    units_parquet: unitsBuf.byteLength,
    phase_rows: phaseRows.length,
    timeline_rows: timelineRows.length,
    flag_rows: flagRows.length,
    phases_parquet: phasesBuf ? phasesBuf.byteLength : 0,
    sessions_parquet: sessionsBuf.byteLength,
    ms: Date.now() - started,
  }
  console.log(`compact: ${JSON.stringify(stats)}`)
  return stats
}

export default {
  async scheduled(_event, env, ctx) {
    ctx.waitUntil(compact(env))
  },

  // Manual trigger, because debugging a cron-only Worker is miserable. Guarded
  // by a secret so it is not a free way to burn our CPU budget.
  async fetch(request, env) {
    const url = new URL(request.url)
    if (url.pathname !== '/compact') {
      return new Response('POST /compact', { status: 404 })
    }
    if (!env.COMPACT_SECRET || request.headers.get('authorization') !== `Bearer ${env.COMPACT_SECRET}`) {
      return new Response('unauthorized', { status: 401 })
    }
    const stats = await compact(env)
    return Response.json(stats, { status: stats.ok ? 200 : 500 })
  },
}
