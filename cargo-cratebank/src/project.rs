//! Reading the project: where the workspace root is, whether it opted in,
//! and when its build has actually finished.
use std::path::PathBuf;

/// Nearest ancestor manifest declaring [workspace] (else the manifest dir).
pub fn workspace_root(from: &std::path::Path) -> PathBuf {
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
/// Opt-in lives in the project's own manifest, and only a *primary* package can
/// opt in — so a dependency can never enrol its consumers.
pub fn opted_in(manifest_dir: &std::path::Path) -> bool {
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
/// The cargo process that ultimately spawned us, if we can find it. build.rs
/// runs EARLY in a build, so "the log stopped growing" is not a reliable
/// finish signal on its own -- a gap between slow units looks identical to a
/// finished build, and the helper would ship a partial session. Waiting for
/// the parent cargo to exit is exact. (Linux /proc; elsewhere we fall back to
/// quiescence alone.)
pub fn ancestor_cargo() -> Option<u32> {
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

pub fn wait_for_exit(pid: u32, max_ms: u64) {
    let mut waited = 0;
    while waited < max_ms && std::path::Path::new(&format!("/proc/{pid}")).exists() {
        std::thread::sleep(std::time::Duration::from_millis(200));
        waited += 200;
    }
}

/// A session is finished when its log stops growing. (Cargo emits no
/// build-finished event, so quiescence is the fallback signal.)
/// A session is finished when its log stops growing. (Cargo emits no
/// build-finished event, so quiescence is the fallback signal.)
pub fn wait_quiet(path: &PathBuf, quiet_ms: u64, max_ms: u64) -> bool {
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

