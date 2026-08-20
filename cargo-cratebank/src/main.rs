//! `cargo cratebank` — opt-in sharing of the build timings you were already producing.
//!
//! Cargo's own `-Zbuild-analysis` writes one JSONL session log per invocation to
//! `$CARGO_HOME/log/`, and `-Zsection-timings` folds rustc's frontend/codegen
//! section boundaries into that same stream. This plugin does not instrument
//! anything itself: it reads those logs, redacts private identity, and POSTs the
//! events as JSON to a collector.
//!
//!   cargo cratebank enable               wire this project up for automatic sending
//!   cargo cratebank autosend             ship completed sessions (what automation calls)
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

// ---- automatic sending -------------------------------------------------
//
// Cargo has no post-build hook, so automation needs a trigger that runs during
// the build and a helper that outlives it. The trigger is a three-line build.rs
// (`cargo cratebank enable` writes it); the helper is `autosend --detach`,
// which re-spawns itself detached, waits for the session log to go quiet, and
// ships it. Nothing sits in the compile path and a failure never fails a build.

/// Nearest ancestor manifest declaring [workspace] (else the manifest dir).
fn workspace_root(from: &std::path::Path) -> PathBuf {
    let mut best = from.to_path_buf();
    let mut cur = Some(from.to_path_buf());
    while let Some(d) = cur {
        let f = d.join("Cargo.toml");
        if let Ok(txt) = std::fs::read_to_string(&f) {
            if txt.parse::<toml::Value>().map(|v| v.get("workspace").is_some()).unwrap_or(false) {
                best = d.clone();
            }
        }
        cur = d.parent().map(|x| x.to_path_buf());
    }
    best
}

/// The workspace a session log belongs to, read from the unredacted header.
fn session_workspace(path: &PathBuf) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let v: Value = serde_json::from_str(&line).ok()?;
        if v["reason"] == "build-started" {
            return v["workspace_root"].as_str().map(str::to_string);
        }
    }
    None
}

fn state_file() -> PathBuf { cargo_home().join("cratebank").join("sent.txt") }

fn already_sent(run_id: &str) -> bool {
    std::fs::read_to_string(state_file()).map(|s| s.lines().any(|l| l == run_id)).unwrap_or(false)
}

fn mark_sent(run_id: &str) {
    let p = state_file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{run_id}");
    }
}

/// Opt-in lives in the project's own manifest, and only a *primary* package can
/// opt in — so a dependency can never enrol its consumers.
fn opted_in(manifest_dir: &std::path::Path) -> bool {
    let mut dir = Some(manifest_dir.to_path_buf());
    while let Some(d) = dir {
        let f = d.join("Cargo.toml");
        if let Ok(txt) = std::fs::read_to_string(&f) {
            if let Ok(v) = txt.parse::<toml::Value>() {
                for path in [["package", "metadata"], ["workspace", "metadata"]] {
                    let share = v.get(path[0]).and_then(|x| x.get(path[1]))
                                 .and_then(|x| x.get("cratebank"))
                                 .and_then(|x| x.get("share"))
                                 .and_then(|x| x.as_bool());
                    if share == Some(true) { return true; }
                    if share == Some(false) { return false; }
                }
            }
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    false
}

/// The cargo process that ultimately spawned us, if we can find it. build.rs
/// runs EARLY in a build, so "the log stopped growing" is not a reliable
/// finish signal on its own -- a gap between slow units looks identical to a
/// finished build, and the helper would ship a partial session. Waiting for
/// the parent cargo to exit is exact. (Linux /proc; elsewhere we fall back to
/// quiescence alone.)
fn ancestor_cargo() -> Option<u32> {
    let mut pid = std::process::id();
    for _ in 0..12 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.rfind(')')?;
        let mut it = stat[close + 1..].split_whitespace();
        let _state = it.next()?;
        let ppid: u32 = it.next()?.parse().ok()?;
        if ppid <= 1 { return None; }
        let comm = std::fs::read_to_string(format!("/proc/{ppid}/comm")).unwrap_or_default();
        if comm.trim() == "cargo" { return Some(ppid); }
        pid = ppid;
    }
    None
}

fn wait_for_exit(pid: u32, max_ms: u64) {
    let mut waited = 0;
    while waited < max_ms && std::path::Path::new(&format!("/proc/{pid}")).exists() {
        std::thread::sleep(std::time::Duration::from_millis(200));
        waited += 200;
    }
}

/// A session is finished when its log stops growing. (Cargo emits no
/// build-finished event, so quiescence is the fallback signal.)
fn wait_quiet(path: &PathBuf, quiet_ms: u64, max_ms: u64) -> bool {
    let mut last = 0u64;
    let mut still = 0u64;
    let mut waited = 0u64;
    while waited < max_ms {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size == last && size > 0 {
            still += 250;
            if still >= quiet_ms { return true; }
        } else {
            still = 0;
            last = size;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        waited += 250;
    }
    false
}

/// CRATEBANK_DEBUG=1 explains why auto-send did or did not fire.
fn dbg(msg: &str) {
    if std::env::var("CRATEBANK_DEBUG").is_ok() { eprintln!("cratebank[autosend] {msg}"); }
}

fn cmd_autosend(o: &Opts) -> i32 {
    let detach = o.rest.iter().any(|a| a == "--detach");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());

    // Guard against a published crate enrolling its consumers. CARGO_PRIMARY_PACKAGE
    // is NOT available here -- cargo passes it to rustc, not to build-script
    // execution -- so the test is where the manifest lives: registry checkouts and
    // git checkouts live under CARGO_HOME, your own code does not.
    let home = cargo_home();
    if detach && manifest_dir.starts_with(&home) {
        dbg("manifest is inside CARGO_HOME (a dependency); refusing to enrol");
        return 0;
    }
    if !opted_in(&manifest_dir) {
        dbg(&format!("no opt-in found from {}", manifest_dir.display())); return 0;
    }

    if detach {
        // re-spawn ourselves, fully detached, and return immediately
        let exe = std::env::current_exe().unwrap_or_else(|_| "cargo-cratebank".into());
        let mut child = Command::new(exe);
        child.args(["cratebank", "autosend"])
            .env("CRATEBANK_MANIFEST_DIR", &manifest_dir);
        if let Some(pid) = ancestor_cargo() {
            child.env("CRATEBANK_WAIT_PID", pid.to_string());
        }
        let _ = child
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(if std::env::var("CRATEBANK_DEBUG").is_ok() {
                std::process::Stdio::inherit()   // debugging: let the child speak
            } else {
                std::process::Stdio::null()      // normal: never touch build output
            })
            .spawn();
        dbg("spawned detached helper");
        return 0;
    }

    // detached child: ship the newest session belonging to THIS workspace --
    // the log directory is global, so the newest log may be another project's
    let dir = std::env::var("CRATEBANK_MANIFEST_DIR").map(PathBuf::from).unwrap_or(manifest_dir);
    let root = workspace_root(&dir);
    let root_s = root.to_string_lossy().to_string();
    dbg(&format!("workspace root {root_s}; {} session log(s)", sessions().len()));
    let Some(newest) = sessions().into_iter()
        .find(|p| session_workspace(p).map(|w| w == root_s).unwrap_or(false))
    else {
        dbg("no session log belongs to this workspace (is build-analysis enabled?)");
        return 0;
    };
    dbg(&format!("session {}", newest.file_name().unwrap().to_string_lossy()));
    if let Ok(pid) = std::env::var("CRATEBANK_WAIT_PID").map(|v| v.parse::<u32>()) {
        if let Ok(pid) = pid {
            dbg(&format!("waiting for cargo pid {pid} to exit"));
            wait_for_exit(pid, 3_600_000);
        }
    }
    if !wait_quiet(&newest, 1500, 600_000) { dbg("session never went quiet"); return 0; }
    let Some((run_id, events, header)) = read_session(&newest, o.redact) else {
        dbg("could not parse session"); return 0;
    };
    if already_sent(&run_id) { dbg(&format!("{run_id} already sent")); return 0; }
    let body = payload(&run_id, events, &header, o.redact);
    match post(&o.endpoint, &body) {
        Ok(_) => { mark_sent(&run_id); dbg(&format!("sent {run_id} -> {}", o.endpoint)); }
        Err(e) => dbg(&format!("POST failed: {e}")),
    }
    0
}

/// Watch the log directory and ship every completed session from an opted-in
/// workspace. This is the reliable automatic path: cargo does not guarantee it
/// reruns a build script on every rebuild, so the build.rs trigger is
/// best-effort, while the watcher sees every session cargo writes.
fn cmd_watch(o: &Opts) -> i32 {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    eprintln!("cratebank: watching {} (every session from an opted-in workspace)", dir.display());
    loop {
        for path in sessions().into_iter().rev() {
            let Some(ws) = session_workspace(&path) else { continue };
            let run_id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if already_sent(&run_id) { continue; }
            if !opted_in(std::path::Path::new(&ws)) { mark_sent(&run_id); continue; }
            if !wait_quiet(&path, 2000, 3_600_000) { continue; }
            let Some((rid, events, header)) = read_session(&path, o.redact) else { continue };
            if already_sent(&rid) { continue; }
            let body = payload(&rid, events, &header, o.redact);
            match post(&o.endpoint, &body) {
                Ok(_) => {
                    mark_sent(&rid);
                    let c = &body["counts"];
                    eprintln!("sent {rid}: {} events, {} units ({} private), {} sections",
                              c["events"], c["units"], c["private_units"], c["sections"]);
                }
                Err(e) => eprintln!("cratebank: POST failed ({e}); will retry"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

const BUILD_RS_SNIPPET: &str = r#"fn main() {
    // cratebank: ship this build's timings once it finishes (opt-in via
    // [package.metadata.cratebank] share = true). No-op if not installed.
    //
    // Deliberately no `cargo:rerun-if-changed` line: the default is to rerun
    // whenever any file in this package changes, which is exactly "this
    // package was rebuilt" -- the moment worth reporting.
    let _ = std::process::Command::new("cargo-cratebank")
        .args(["cratebank", "autosend", "--detach"])
        .status();
}
"#;

/// The two unstable flags must be enabled at the WORKSPACE root, or a build
/// started from the root never sees them.
fn write_cargo_config(root: &std::path::Path, o: &Opts, changed: &mut Vec<&'static str>) {
    let cfg = root.join(".cargo").join("config.toml");
    let want = "[unstable]\nbuild-analysis = true\nsection-timings = true\n\n[build.analysis]\nenabled = true\n";
    let have = std::fs::read_to_string(&cfg).unwrap_or_default();
    if have.contains("build-analysis") { return; }
    if o.dry_run {
        println!("--- {} ---\n{want}", cfg.display());
    } else {
        let _ = std::fs::create_dir_all(cfg.parent().unwrap());
        let _ = std::fs::write(&cfg, format!("{}{}{want}", have.trim_end(),
                                             if have.trim().is_empty() { "" } else { "\n\n" }));
    }
    changed.push(".cargo/config.toml (workspace root): build-analysis + section-timings");
}

fn cmd_enable(o: &Opts) -> i32 {
    let dir = std::env::current_dir().unwrap_or_default();
    let manifest = dir.join("Cargo.toml");
    let Ok(txt) = std::fs::read_to_string(&manifest) else {
        eprintln!("cratebank: no Cargo.toml here"); return 1;
    };
    let mut changed = vec![];
    // A virtual workspace manifest has no [package]; writing package.metadata
    // there yields "missing field `package.name`" and breaks the manifest.
    let parsed = txt.parse::<toml::Value>().ok();
    let is_package = parsed.as_ref().map(|v| v.get("package").is_some()).unwrap_or(false);
    let is_virtual_ws = !is_package
        && parsed.as_ref().map(|v| v.get("workspace").is_some()).unwrap_or(false);

    if !opted_in(&dir) {
        let table = if is_package { "package" } else { "workspace" };
        let add = format!("\n[{table}.metadata.cratebank]\nshare = true\n");
        if o.dry_run { println!("--- append to Cargo.toml ---{add}"); }
        else { std::fs::write(&manifest, format!("{}{add}", txt.trim_end())).ok(); }
        changed.push(if is_package { "Cargo.toml: [package.metadata.cratebank] share = true" }
                     else { "Cargo.toml: [workspace.metadata.cratebank] share = true" });
    }

    write_cargo_config(&workspace_root(&dir), o, &mut changed);

    if is_virtual_ws {
        // The opt-in is inherited by members (opted_in walks up), but the
        // trigger must live in a package that actually builds.
        for c in &changed { println!("  + {c}"); }
        println!("\nThis is a virtual workspace: the opt-in now covers all members.");
        println!("Run `cargo cratebank enable` inside one member to add the build.rs trigger,");
        println!("or use `cargo cratebank build` / a CI step instead.");
        return 0;
    }

    let build_rs = dir.join("build.rs");
    if !build_rs.exists() {
        if o.dry_run { println!("--- build.rs ---
{BUILD_RS_SNIPPET}"); }
        else { std::fs::write(&build_rs, BUILD_RS_SNIPPET).ok(); }
        changed.push("build.rs: created (spawns autosend --detach)");
    } else if !std::fs::read_to_string(&build_rs).unwrap_or_default().contains("cargo-cratebank") {
        println!("build.rs already exists — add these three lines to its main():
");
        println!("    let _ = std::process::Command::new(\"cargo-cratebank\")");
        println!("        .args([\"cratebank\", \"autosend\", \"--detach\"])");
        println!("        .status();\n");
    }

    if o.dry_run { eprintln!("\ncratebank: dry run, nothing written"); return 0; }
    if changed.is_empty() { println!("cratebank: already enabled here."); }
    else { for c in &changed { println!("  + {c}"); } }
    println!("\nEvery `cargo build` on a nightly toolchain will now ship its session log to\n{}\nafter the build finishes. Disable any time with  share = false.", o.endpoint);
    0
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
    let here = std::env::current_dir().unwrap_or_default();
    println!("auto-send     {}", if opted_in(&here) { "ON for this project (share = true)" }
                                 else { "off here — run: cargo cratebank enable" });
    let sent = std::fs::read_to_string(state_file()).map(|s| s.lines().count()).unwrap_or(0);
    println!("sessions sent {sent}");
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
        "enable" => cmd_enable(&o),
        "autosend" => cmd_autosend(&o),
        "watch" => cmd_watch(&o),
        "send" => cmd_send(&o),
        "status" => cmd_status(&o),
        "serve" => cmd_serve(&o),
        _ => {
            eprintln!("cargo cratebank — share the build timings you were already producing\n");
            eprintln!("  cargo cratebank enable               opt this project in to automatic sending");
            eprintln!("  cargo cratebank build [cargo args…]   build with analysis flags, then send");
            eprintln!("  cargo cratebank send [--all|--session ID|--since N]");
            eprintln!("  cargo cratebank status");
            eprintln!("  cargo cratebank watch                ship every completed session (reliable auto path)");
            eprintln!("  cargo cratebank serve [--port 8787]   echo collector for testing\n");
            eprintln!("  --dry-run        print the exact payload, send nothing");
            eprintln!("  --endpoint URL   default {DEFAULT_ENDPOINT}\n                   (testing: cargo cratebank serve, then --endpoint http://127.0.0.1:8787/ingest)");
            eprintln!("  --keep-private   do not redact private units (own collector only)");
            2
        }
    })
}
