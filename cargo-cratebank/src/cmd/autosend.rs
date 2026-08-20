//! `cargo cratebank autosend` — what the `build.rs` trigger calls.
//!
//! Cargo has no post-build hook, so automation needs a trigger that runs during
//! the build and a helper that outlives it. `--detach` re-spawns this command
//! detached; the detached copy waits for the parent cargo to exit, then ships.
//! A failure here never fails a build.
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::cli::{cargo_home, dbg, Common};
use crate::project::{ancestor_cargo, opted_in, wait_for_exit, wait_quiet, workspace_root};
use crate::session::{payload, read_session, session_workspace, sessions};
use crate::ship::{already_sent, mark_sent, post};

pub fn run(o: &Common, detach: bool) -> i32 {
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
    if let Some(pid) = std::env::var("CRATEBANK_WAIT_PID").ok().and_then(|v| v.parse::<u32>().ok()) {
        dbg(&format!("waiting for cargo pid {pid} to exit"));
        wait_for_exit(pid, 3_600_000);
    }
    if !wait_quiet(&newest, 1500, 600_000) { dbg("session never went quiet"); return 0; }
    let Some(s) = read_session(&newest) else {
        dbg("could not parse session"); return 0;
    };
    let mut s = s;
    let run_id = s.run_id.clone();
    if already_sent(&run_id) { dbg(&format!("{run_id} already sent")); return 0; }
    let env = crate::buildenv::snapshot(&s.dir);
    let body = payload(&mut s, env, Value::Null);
    match post(&o.endpoint, &body) {
        Ok(_) => { mark_sent(&run_id); dbg(&format!("sent {run_id} -> {}", o.endpoint)); }
        Err(e) => dbg(&format!("POST failed: {e}")),
    }
    0
}

