//! The single upload envelope: Cargo timings and samply phases from one build.

use serde_json::{json, Value};

pub const SCHEMA: u32 = 2;

pub fn build(
    project: &crate::timings::Project,
    capture: crate::timings::Capture,
    phases: Value,
    load: Value,
) -> Value {
    let units = capture.timings["unit_data"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let sections = capture.timings["unit_data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|unit| unit["sections"].as_array().map(Vec::len).unwrap_or(0))
        .sum::<usize>();
    let phase_units = phases["units"].as_array().map(Vec::len).unwrap_or(0);

    json!({
        "cratebank_schema": SCHEMA,
        "client": concat!("cargo-cratebank ", env!("CARGO_PKG_VERSION")),
        "run_id": capture.run_id,
        "trust": "anonymous",
        "env": capture.env,
        "repository": project.repository,
        "machine": crate::machine::snapshot(Some(&project.workspace_root)),
        "load": load,
        "build_env": crate::buildenv::snapshot(&project.workspace_root),
        "timings": capture.timings,
        "phases": phases,
        "complete": true,
        "counts": {
            "units": units,
            "units_withheld": capture.withheld,
            "sections": sections,
            "phase_units": phase_units,
        },
    })
}
