import assert from 'node:assert/strict'
import test from 'node:test'

import { flatten } from './compact.js'

test('schema 2 derives units and timelines from Cargo timings', () => {
  const result = flatten({
    cratebank_schema: 2,
    run_id: 'run-2',
    counts: { units: 1, units_withheld: 2, sections: 3 },
    env: { profile: 'release', rustc_version: 'rustc 1.96.1' },
    timings: {
      unit_data: [{
        i: 7,
        name: 'serde',
        version: '1.0.228',
        mode: 'todo',
        target: '',
        features: ['derive', 'std'],
        start: 1.5,
        duration: 2.0,
        unblocked_units: [8, 9],
        sections: [
          ['frontend', { start: 0, end: 1.25 }],
          ['codegen', { start: 1.25, end: 1.8 }],
          ['link', { start: 1.8, end: 2.0 }],
        ],
      }],
      concurrency_data: [{ t: 1, active: 2, waiting: 1, inactive: 3 }],
      cpu_usage: [[1.1, 87.5]],
    },
    phases: {
      sampler: 'samply',
      rate_hz: 4999,
      units: [{
        crate: 'serde', package: 'serde-1.0.228', crate_type: 'lib', wall_s: 2,
        flags: { 'opt-level': '3' },
        serial: { type_check: 100 }, parallel: { codegen: 40 },
      }],
    },
  }, 'sessions/run-2.json.zst')

  assert.deepEqual(result.unitRows[0], {
    run_id: 'run-2', index: 7, crate: 'serde', package: 'serde',
    version: '1.0.228', mode: 'todo', target: null, features: 'derive,std',
    started: 1.5, finished: 3.5, duration: 2,
    frontend: 1.25, codegen: 0.55, link: 0.19999999999999996, unblocked: 2,
  })
  assert.equal(result.phaseRows.length, 2)
  assert.equal(result.timelineRows.length, 2)
  assert.equal(result.flagRows.length, 1)
})

test('rejects payloads other than the current combined schema', () => {
  assert.throws(
    () => flatten({ cratebank_schema: 1 }, 'sessions/old.json.zst'),
    /unsupported payload schema/,
  )
})
