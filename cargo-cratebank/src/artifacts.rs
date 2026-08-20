//! How much output a build produced.
//!
//! Artifact size is one of the better responses available: rmeta bytes track
//! what the frontend had to describe, object and rlib bytes track what the
//! backend had to emit, and both are free to measure — the files are sitting
//! in the target directory when the build ends.
//!
//! Sizes are reported per crate name and only for units that are actually
//! being sent. A file name in a target directory contains the crate name, so
//! reporting sizes for a withheld unit would leak exactly the identity the
//! filter just removed.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

#[derive(Default, Clone, Copy)]
pub struct Bytes {
    pub rmeta: u64,
    pub rlib: u64,
    pub obj: u64,
}

/// crate name (underscored) -> bytes, from `<target>/<profile>/deps`.
pub fn scan(target_dir: &Path) -> BTreeMap<String, Bytes> {
    let mut out: BTreeMap<String, Bytes> = BTreeMap::new();
    for profile in ["debug", "release"] {
        let deps = target_dir.join(profile).join("deps");
        let Ok(rd) = std::fs::read_dir(&deps) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Some(name) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(md) = e.metadata() else { continue };
            let size = md.len();
            // libfoo_bar-9a8b7c6d.rlib  ->  foo_bar
            let stem = name.strip_prefix("lib").unwrap_or(name);
            let Some((crate_name, _hash)) = stem.rsplit_once('-') else { continue };
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            let slot = out.entry(crate_name.to_string()).or_default();
            match ext {
                "rmeta" => slot.rmeta += size,
                "rlib" => slot.rlib += size,
                "o" => slot.obj += size,
                _ => {}
            }
        }
    }
    out
}

/// Keep only the crates we are sending, and total what is kept.
pub fn report(target_dir: &Path, public_crates: &[String]) -> Value {
    let all = scan(target_dir);
    let mut per = Map::new();
    let (mut rmeta, mut rlib, mut obj) = (0u64, 0u64, 0u64);
    for name in public_crates {
        let key = name.replace('-', "_");
        if let Some(b) = all.get(&key) {
            rmeta += b.rmeta;
            rlib += b.rlib;
            obj += b.obj;
            per.insert(key, json!({"rmeta": b.rmeta, "rlib": b.rlib, "obj": b.obj}));
        }
    }
    if per.is_empty() {
        return Value::Null;
    }
    json!({
        "total": {"rmeta": rmeta, "rlib": rlib, "obj": obj},
        "per_crate": Value::Object(per),
    })
}
