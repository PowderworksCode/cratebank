//! `cargo cratebank watch` — the reliable automatic path.
//!
//! Cargo does not guarantee it reruns a build script on every rebuild, so the
//! `build.rs` trigger is best-effort. This watches the log directory instead
//! and sees every session cargo writes.

use crate::cli::Common;
use crate::project::{opted_in, wait_quiet};
use crate::session::{log_dir, payload, read_session, session_workspace, sessions};
use crate::ship::{already_sent, mark_sent, post};

/// Watch the log directory and ship every completed session from an opted-in
/// workspace. This is the reliable automatic path: cargo does not guarantee it
/// reruns a build script on every rebuild, so the build.rs trigger is
/// best-effort, while the watcher sees every session cargo writes.
pub fn run(o: &Common) -> i32 {
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
            let Some(s) = read_session(&path, o.public_only()) else { continue };
            let rid = s.run_id.clone();
            if already_sent(&rid) { continue; }
            let body = payload(&s.run_id, s.events, &s.header, o.public_only(), s.withheld);
            match post(&o.endpoint, &body) {
                Ok(_) => {
                    mark_sent(&rid);
                    let c = &body["counts"];
                    eprintln!("sent {rid}: {} events, {} units ({} withheld), {} sections",
                              c["events"], c["units"], c["units_withheld"], c["sections"]);
                }
                Err(e) => eprintln!("cratebank: POST failed ({e}); will retry"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

