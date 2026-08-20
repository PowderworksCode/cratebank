//! Cargo's build-analysis session logs: finding them, reading them, and
//! deciding what may leave the machine.
//!
//! Nothing here instruments a build. `-Zbuild-analysis` writes one JSONL log
//! per cargo invocation to `$CARGO_HOME/log/`; `-Zsection-timings` adds
//! frontend/codegen section events to the same stream. We read what cargo
//! already wrote.
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::cli::cargo_home;

pub const SCHEMA: u32 = 1;

/// The directory cargo writes session logs to.
pub fn log_dir() -> PathBuf { cargo_home().join("log") }

/// Session logs, newest first.
pub fn sessions() -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(log_dir())
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                    .collect())
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

/// The workspace a session log belongs to, read from the unredacted header.
pub fn session_workspace(path: &PathBuf) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let v: Value = serde_json::from_str(&line).ok()?;
        if v["reason"] == "build-started" {
            return v["workspace_root"].as_str().map(str::to_string);
        }
    }
    None
}

/// crates.io and public git remotes keep their identity; everything else does not.
pub fn is_public(package_id: &str) -> bool {
    package_id.starts_with("registry+https://github.com/rust-lang/crates.io-index")
        || package_id.starts_with("sparse+https://index.crates.io")
        || (package_id.starts_with("git+http") && !package_id.contains("@"))
}

/// Strip identifying fields that are never useful and never safe.
/// Strip identifying fields that are never useful and never safe.
pub fn scrub_header(ev: &mut Map<String, Value>) {
    for k in ["cwd", "workspace_root", "target_dir", "manifest_path"] {
        ev.remove(k);
    }
    if let Some(Value::Array(cmd)) = ev.get("command") {
        let kept: Vec<Value> = cmd.iter().filter_map(|a| a.as_str()).enumerate()
            .map(|(i, a)| if i == 0 { "cargo".to_string() }
                 else if a.starts_with('-') { a.to_string() }
                 else { "<arg>".to_string() })
            .map(Value::from).collect();
        ev.insert("command".into(), Value::Array(kept));
    }
}

/// Is this project itself public? Workspace units live at `path+…` and are
/// indistinguishable from private code by source alone, so publishing them
/// requires saying so: `[package.metadata.cratebank] public = true`.
/// Is this project itself public? Workspace units live at `path+…` and are
/// indistinguishable from private code by source alone, so publishing them
/// requires saying so: `[package.metadata.cratebank] public = true`.
pub fn declared_public(dir: &std::path::Path) -> Option<String> {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        if let Ok(txt) = std::fs::read_to_string(d.join("Cargo.toml")) {
            if let Ok(v) = txt.parse::<toml::Value>() {
                for t in ["package", "workspace"] {
                    let cb = v.get(t).and_then(|x| x.get("metadata")).and_then(|x| x.get("cratebank"));
                    if cb.and_then(|x| x.get("public")).and_then(|x| x.as_bool()) == Some(true) {
                        // link it properly: prefer the declared repository
                        let repo = cb.and_then(|x| x.get("repository")).and_then(|x| x.as_str())
                            .or_else(|| v.get("package").and_then(|p| p.get("repository"))
                                         .and_then(|x| x.as_str()))
                            .unwrap_or("").to_string();
                        return Some(repo);
                    }
                }
            }
        }
        cur = d.parent().map(|x| x.to_path_buf());
    }
    None
}

/// Drop every private unit and every event that refers to one.
///
/// Nothing about non-public code leaves the machine: not a name, not a hash,
/// not a timing, not an edge. Public dependencies are unaffected, which is
/// where nearly all of the value is. The only trace is a count of how many
/// units were withheld, kept so the receiver knows the graph is partial.
/// Drop every private unit and every event that refers to one.
///
/// Nothing about non-public code leaves the machine: not a name, not a hash,
/// not a timing, not an edge. Public dependencies are unaffected, which is
/// where nearly all of the value is. The only trace is a count of how many
/// units were withheld, kept so the receiver knows the graph is partial.
pub fn filter_private(events: Vec<Value>, project_public: bool) -> (Vec<Value>, usize) {
    let mut private_ix: std::collections::BTreeSet<i64> = Default::default();
    for e in &events {
        if e["reason"] == "unit-registered" {
            let pid = e["package_id"].as_str().unwrap_or("");
            let public = is_public(pid) || (project_public && pid.starts_with("path+"));
            if !public {
                if let Some(i) = e["index"].as_i64() { private_ix.insert(i); }
            }
        }
    }
    let keep_ix = |v: &Value| v.as_i64().map(|i| !private_ix.contains(&i)).unwrap_or(true);
    // A public project's own units still carry the builder's local path
    // (`path+file:///home/me/code/…`). Identity should be the crate in the
    // repository, never the directory layout of the machine that built it.
    let canon = |pid: &str| -> String {
        let Some(rest) = pid.strip_prefix("path+") else { return pid.to_string() };
        let Some((path, tail)) = rest.split_once('#') else { return "workspace".into() };
        // cargo writes `#name@version`, or just `#version` when the crate name
        // equals its directory -- recover the name so identity survives, then
        // discard the path entirely
        if tail.contains('@') {
            format!("workspace#{tail}")
        } else {
            let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("crate");
            format!("workspace#{name}@{tail}")
        }
    };
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let mut o = match e { Value::Object(o) => o, _ => continue };
        if let Some(i) = o.get("index") {
            if !keep_ix(i) { continue; }   // the unit itself is private
        }
        // edges may point at private units; prune rather than leak the index
        for arr in ["dependencies", "unblocked"] {
            if let Some(Value::Array(a)) = o.get(arr) {
                let pruned: Vec<Value> = a.iter().filter(|x| keep_ix(x)).cloned().collect();
                o.insert(arr.into(), Value::Array(pruned));
            }
        }
        if let Some(pid) = o.get("package_id").and_then(|v| v.as_str()).map(str::to_string) {
            if pid.starts_with("path+") { o.insert("package_id".into(), Value::from(canon(&pid))); }
        }
        if o.get("reason").and_then(|v| v.as_str()) == Some("build-started") {
            scrub_header(&mut o);
        }
        out.push(Value::Object(o));
    }
    (out, private_ix.len())
}

/// One parsed session: what to send, and how much was withheld.
pub struct Session {
    /// The workspace this session was built in — where its cargo config lives.
    pub dir: PathBuf,
    pub run_id: String,
    pub events: Vec<Value>,
    pub header: Map<String, Value>,
    pub withheld: usize,
}

/// Read one session log, keeping only what may leave the machine.
///
/// There is no parameter for this and no flag to override it: non-public units
/// are dropped, always. A code path that could send them is a liability even
/// when nobody invokes it, so it does not exist.
pub fn read_session(path: &PathBuf) -> Option<Session> {
    let f = std::fs::File::open(path).ok()?;
    let mut events = vec![];
    let mut header: Map<String, Value> = Map::new();
    let mut run_id = String::new();
    let mut ws = String::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let v: Value = serde_json::from_str(&line).ok()?;
        let obj = v.as_object()?.clone();
        if run_id.is_empty() {
            run_id = obj.get("run_id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        }
        if obj.get("reason").and_then(|v| v.as_str()) == Some("build-started") {
            header = obj.clone();
            ws = obj.get("workspace_root").and_then(|v| v.as_str()).unwrap_or("").to_string();
        }
        events.push(Value::Object(obj));
    }
    if events.is_empty() { return None; }
    let repo = declared_public(std::path::Path::new(&ws));
    let (mut events, withheld) = filter_private(events, repo.is_some());
    if let (Some(r), Some(Value::Object(h))) = (repo.filter(|r| !r.is_empty()), events.first_mut()) {
        h.insert("repository".into(), Value::from(r));   // link a public project properly
    }
    scrub_header(&mut header);
    Some(Session { dir: PathBuf::from(&ws), run_id, events, header, withheld })
}

/// One session -> one payload. Raw events pass through verbatim (minus
/// redaction): cargo's schema is explicitly unstable, so normalising here would
/// bake in today's shape. Model server-side, log broadly.
/// One session -> one payload. Raw events pass through verbatim (minus
/// redaction): cargo's schema is explicitly unstable, so normalising here would
/// bake in today's shape. Model server-side, log broadly.
pub fn payload(run_id: &str, events: Vec<Value>, header: &Map<String, Value>,
           withheld: usize, build_env: Value) -> Value {
    let get = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
    let sections = events.iter()
        .filter(|e| e["reason"] == "unit-section-finished").count();
    let units = events.iter().filter(|e| e["reason"] == "unit-registered").count();
    let repository = events.first().and_then(|e| e.get("repository")).cloned().unwrap_or(Value::Null);
    json!({
        "cratebank_schema": SCHEMA,
        "client": concat!("cargo-cratebank ", env!("CARGO_PKG_VERSION")),
        "run_id": run_id,
        "env": {
            "host": get("host"), "profile": get("profile"),
            "jobs": get("jobs"), "num_cpus": get("num_cpus"),
            "rustc_version": get("rustc_version"),
            "rustc_version_verbose": get("rustc_version_verbose"),
            "timestamp": get("timestamp"),
            "ci": std::env::var("CI").is_ok(),
        },
        "repository": repository,
        "machine": crate::machine::snapshot(),
        "build_env": build_env,
        // A session log has no build-finished event, so a build that failed
        // half way looks exactly like one that completed. Every registered
        // unit that never finished is either still running (impossible, we
        // waited) or died with the build -- so this is the completeness flag.
        "complete": events.iter().filter(|e| e["reason"] == "unit-finished").count()
                  == events.iter().filter(|e| e["reason"] == "unit-registered").count(),
        "counts": {"events": events.len(), "units": units, "sections": sections,
                   // identity, timings and edges of withheld units are absent
                   // entirely; only this count records that they existed
                   "units_withheld": withheld},
        "events": events,
    })
}

