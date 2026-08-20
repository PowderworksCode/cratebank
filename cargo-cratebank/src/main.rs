//! `cargo cratebank` — opt-in sharing of the build timings you were already producing.
//!
//! Cargo's own `-Zbuild-analysis` writes one JSONL session log per invocation to
//! `$CARGO_HOME/log/`, and `-Zsection-timings` folds rustc's frontend/codegen
//! section boundaries into that same stream. This plugin does not instrument
//! anything itself: it reads those logs, redacts private identity, and POSTs the
//! events as JSON to a collector.
//!
//!   cargo cratebank build [cargo args…]   build with both flags on, then send
//!   cargo cratebank send [--all|--session ID|--since N]
//!   cargo cratebank status                is everything wired up?
//!   cargo cratebank serve [--port N]      echo collector, for testing
//!
//! Flags: --dry-run (print the exact payload, send nothing), --endpoint URL,
//!        --keep-private (do NOT redact; only for your own collector).
//!
//! Privacy: units from crates.io and public git remotes are sent with identity.
//! Units from local paths or private registries are sent as opaque stable
//! hashes with no name — their timings still contribute, their identity never
//! leaves the machine. Workspace paths, cwd, and target dirs are dropped.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Map, Value};

const SCHEMA: u32 = 1;
const DEFAULT_ENDPOINT: &str = "https://ingest.cratebank.io/v1/sessions";

struct Opts {
    endpoint: String,
    dry_run: bool,
    redact: bool,
    session: Option<String>,
    all: bool,
    since: usize,
    rest: Vec<String>,
}

fn parse(mut args: Vec<String>) -> Opts {
    let mut o = Opts { endpoint: std::env::var("CRATEBANK_ENDPOINT")
                           .unwrap_or_else(|_| DEFAULT_ENDPOINT.into()),
                       dry_run: false, redact: true, session: None, all: false,
                       since: 1, rest: vec![] };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => o.dry_run = true,
            "--keep-private" => o.redact = false,
            "--all" => o.all = true,
            "--endpoint" => { o.endpoint = args.get(i + 1).cloned().unwrap_or_default(); i += 1 }
            "--session" => { o.session = args.get(i + 1).cloned(); i += 1 }
            "--since" => { o.since = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1); i += 1 }
            _ => o.rest.push(std::mem::take(&mut args[i])),
        }
        i += 1;
    }
    o
}

fn cargo_home() -> PathBuf {
    std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cargo")
    })
}

fn log_dir() -> PathBuf { cargo_home().join("log") }

/// Session logs, newest first.
fn sessions() -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(log_dir())
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                    .collect())
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

fn stable_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("private:{:016x}", h.finish())
}

/// crates.io and public git remotes keep their identity; everything else does not.
fn is_public(package_id: &str) -> bool {
    package_id.starts_with("registry+https://github.com/rust-lang/crates.io-index")
        || package_id.starts_with("sparse+https://index.crates.io")
        || (package_id.starts_with("git+http") && !package_id.contains("@"))
}

/// Redaction is per-unit, not per-project: a private workspace still contributes
/// every public dependency measurement in its graph.
fn redact(mut ev: Map<String, Value>) -> Map<String, Value> {
    for k in ["cwd", "workspace_root", "target_dir", "manifest_path"] {
        ev.remove(k);
    }
    if let Some(Value::Array(cmd)) = ev.get("command") {
        // keep the shape of the invocation, not its paths or config values
        let kept: Vec<Value> = cmd.iter().filter_map(|a| a.as_str()).enumerate()
            .map(|(i, a)| if i == 0 { "cargo".to_string() }
                 else if a.starts_with('-') { a.to_string() }
                 else { "<arg>".to_string() })
            .map(Value::from).collect();
        ev.insert("command".into(), Value::Array(kept));
    }
    if let Some(pid) = ev.get("package_id").and_then(|v| v.as_str()).map(str::to_string) {
        if !is_public(&pid) {
            ev.insert("package_id".into(), Value::from(stable_hash(&pid)));
            ev.insert("private".into(), Value::Bool(true));
            if let Some(Value::Object(t)) = ev.get_mut("target") {
                t.insert("name".into(), Value::from("<private>"));
            }
        }
    }
    ev
}

fn read_session(path: &PathBuf, redact_on: bool) -> Option<(String, Vec<Value>, Map<String, Value>)> {
    let f = std::fs::File::open(path).ok()?;
    let mut events = vec![];
    let mut header = Map::new();
    let mut run_id = String::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let v: Value = serde_json::from_str(&line).ok()?;
        let mut obj = v.as_object()?.clone();
        if run_id.is_empty() {
            run_id = obj.get("run_id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        }
        if obj.get("reason").and_then(|v| v.as_str()) == Some("build-started") {
            header = obj.clone();
        }
        if redact_on { obj = redact(obj); }
        events.push(Value::Object(obj));
    }
    if events.is_empty() { return None; }
    Some((run_id, events, header))
}

/// One session -> one payload. Raw events pass through verbatim (minus
/// redaction): cargo's schema is explicitly unstable, so normalising here would
/// bake in today's shape. Model server-side, log broadly.
fn payload(run_id: &str, events: Vec<Value>, header: &Map<String, Value>, redacted: bool) -> Value {
    let get = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
    let sections = events.iter()
        .filter(|e| e["reason"] == "unit-section-finished").count();
    let units = events.iter().filter(|e| e["reason"] == "unit-registered").count();
    let private = events.iter().filter(|e| e["private"] == Value::Bool(true)).count();
    json!({
        "cratebank_schema": SCHEMA,
        "client": concat!("cargo-cratebank ", env!("CARGO_PKG_VERSION")),
        "run_id": run_id,
        "redacted": redacted,
        "env": {
            "host": get("host"), "profile": get("profile"),
            "jobs": get("jobs"), "num_cpus": get("num_cpus"),
            "rustc_version": get("rustc_version"),
            "rustc_version_verbose": get("rustc_version_verbose"),
            "timestamp": get("timestamp"),
            "ci": std::env::var("CI").is_ok(),
        },
        "counts": {"events": events.len(), "units": units,
                   "sections": sections, "private_units": private},
        "events": events,
    })
}

fn post(endpoint: &str, body: &Value) -> Result<String, String> {
    ureq::post(endpoint)
        .set("content-type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

fn cmd_send(o: &Opts) -> i32 {
    let mut list = sessions();
    if let Some(id) = &o.session {
        list.retain(|p| p.file_name().map(|f| f.to_string_lossy().contains(id)).unwrap_or(false));
    } else if !o.all {
        list.truncate(o.since);
    }
    if list.is_empty() {
        eprintln!("cratebank: no session logs in {}", log_dir().display());
        eprintln!("  enable them:  cargo cratebank status");
        return 1;
    }
    let mut sent = 0;
    for p in &list {
        let Some((run_id, events, header)) = read_session(p, o.redact) else { continue };
        let body = payload(&run_id, events, &header, o.redact);
        if o.dry_run {
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
            continue;
        }
        match post(&o.endpoint, &body) {
            Ok(resp) => {
                sent += 1;
                let c = &body["counts"];
                println!("sent {run_id}: {} events, {} units ({} private), {} sections -> {} [{}]",
                         c["events"], c["units"], c["private_units"], c["sections"],
                         o.endpoint, resp.trim());
            }
            Err(e) => eprintln!("cratebank: POST {} failed: {e}", o.endpoint),
        }
    }
    if o.dry_run { eprintln!("cratebank: dry run, nothing sent ({} session(s))", list.len()); }
    if !o.dry_run && sent == 0 { 1 } else { 0 }
}

fn cmd_build(o: &Opts) -> i32 {
    let before = sessions().len();
    let mut c = Command::new("cargo");
    c.arg("build")
        .arg("-Zbuild-analysis")
        .arg("-Zsection-timings")
        .arg("--config").arg("build.analysis.enabled=true")
        .args(&o.rest);
    eprintln!("cratebank: cargo build -Zbuild-analysis -Zsection-timings {}", o.rest.join(" "));
    let st = c.status();
    match st {
        Ok(s) if !s.success() => { eprintln!("cratebank: build failed; sending nothing"); return 1 }
        Err(e) => { eprintln!("cratebank: cannot run cargo: {e}"); return 1 }
        _ => {}
    }
    if sessions().len() == before {
        eprintln!("cratebank: no new session log appeared — is this a nightly toolchain?");
        return 1;
    }
    cmd_send(&Opts { since: 1, all: false, session: None, ..clone_opts(o) })
}

fn clone_opts(o: &Opts) -> Opts {
    Opts { endpoint: o.endpoint.clone(), dry_run: o.dry_run, redact: o.redact,
           session: o.session.clone(), all: o.all, since: o.since, rest: vec![] }
}

fn cmd_status(o: &Opts) -> i32 {
    let nightly = Command::new("cargo").arg("-Zhelp").output()
        .map(|x| String::from_utf8_lossy(&x.stdout).contains("build-analysis")).unwrap_or(false);
    let s = sessions();
    println!("cargo home    {}", cargo_home().display());
    println!("log dir       {}  ({} session log(s))", log_dir().display(), s.len());
    println!("nightly flags {}", if nightly { "available" } else { "NOT available (need nightly)" });
    println!("endpoint      {}", o.endpoint);
    println!("redaction     {}", if o.redact { "on (private units hashed)" } else { "OFF" });
    if let Some(p) = s.first() { println!("latest        {}", p.file_name().unwrap().to_string_lossy()); }
    println!("\nto record every build, add to .cargo/config.toml:\n\
              \n  [unstable]\n  build-analysis = true\n  section-timings = true\n\
              \n  [build.analysis]\n  enabled = true\n\
              \n(on a stable toolchain these only warn, so they are safe to leave in place)");
    if !nightly { 1 } else { 0 }
}

/// Minimal echo collector so the whole path is testable with no infrastructure.
fn cmd_serve(o: &Opts) -> i32 {
    let port: u16 = o.rest.iter().position(|a| a == "--port")
        .and_then(|i| o.rest.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(8787);
    let l = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => { eprintln!("cratebank serve: {e}"); return 1 }
    };
    eprintln!("cratebank: echo collector on http://127.0.0.1:{port}/ingest");
    for stream in l.incoming() {
        let Ok(mut s) = stream else { continue };
        let mut buf = Vec::new();
        let mut tmp = [0u8; 8192];
        let mut len = 0usize;
        loop {
            match s.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if len == 0 {
                        if let Some(p) = String::from_utf8_lossy(&buf).find("\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                            len = head.lines().find_map(|l| l.strip_prefix("content-length:"))
                                      .and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                            let body = buf.len() - (p + 4);
                            if body >= len { break }
                        }
                    } else if let Some(p) = String::from_utf8_lossy(&buf).find("\r\n\r\n") {
                        if buf.len() - (p + 4) >= len { break }
                    }
                }
                Err(_) => break,
            }
        }
        let txt = String::from_utf8_lossy(&buf).to_string();
        let body = txt.splitn(2, "\r\n\r\n").nth(1).unwrap_or("").to_string();
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => eprintln!("[ingest] run {} · {} events · {} units ({} private) · {} sections · {} · rustc {}",
                v["run_id"].as_str().unwrap_or("?"),
                v["counts"]["events"], v["counts"]["units"], v["counts"]["private_units"],
                v["counts"]["sections"], v["env"]["host"].as_str().unwrap_or("?"),
                v["env"]["rustc_version"].as_str().unwrap_or("?")),
            Err(e) => eprintln!("[ingest] {} bytes, not json ({e})", body.len()),
        }
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\r\nok");
    }
    0
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("cratebank") { args.remove(0); }
    let sub = args.first().cloned().unwrap_or_default();
    let o = parse(args.into_iter().skip(1).collect());
    std::process::exit(match sub.as_str() {
        "build" => cmd_build(&o),
        "send" => cmd_send(&o),
        "status" => cmd_status(&o),
        "serve" => cmd_serve(&o),
        _ => {
            eprintln!("cargo cratebank — share the build timings you were already producing\n");
            eprintln!("  cargo cratebank build [cargo args…]   build with analysis flags, then send");
            eprintln!("  cargo cratebank send [--all|--session ID|--since N]");
            eprintln!("  cargo cratebank status");
            eprintln!("  cargo cratebank serve [--port 8787]   echo collector for testing\n");
            eprintln!("  --dry-run        print the exact payload, send nothing");
            eprintln!("  --endpoint URL   default {DEFAULT_ENDPOINT}\n                   (testing: cargo cratebank serve, then --endpoint http://127.0.0.1:8787/ingest)");
            eprintln!("  --keep-private   do not redact private units (own collector only)");
            2
        }
    })
}
