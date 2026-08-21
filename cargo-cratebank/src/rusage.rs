//! Per-invocation CPU, via an optional rustc wrapper.
//!
//! Cargo's log reports `elapsed` — wall clock — which on a `-j16` build is
//! mostly a statement about contention. CPU time is what the compiler actually
//! spent, and it is additive across units, so it is the quantity worth
//! modelling. Nothing in cargo records it.
//!
//! `RUSTC_WRAPPER=cargo-cratebank … rustc-shim` fills the gap: it execs the
//! real rustc, reaps it with `wait4`, and appends the rusage to a sidecar file
//! that `send` merges into the session. It is opt-in because it does sit in
//! the compile path — although all it adds is a fork and a write.
//!
//! It **chains** rather than competes: if a wrapper was already configured, we
//! run that instead of rustc directly, so `sccache` keeps working and a cache
//! hit is simply a very cheap invocation.
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

use crate::cli::cargo_home;

pub fn sidecar_dir() -> PathBuf {
    cargo_home().join("cratebank").join("rusage")
}

/// Reap a child and report `(exit code, user CPU, system CPU, peak RSS kB)`.
///
/// There is no portable way to ask for a child's CPU time, so this is the one
/// place the client is platform-specific: `wait4` on unix, `GetProcessTimes`
/// on Windows. Both give exact accounting for the process that just exited —
/// far better than sampling, which misses short-lived compilations entirely.
#[cfg(unix)]
fn wait_rusage(child: &mut std::process::Child) -> (i32, f64, f64, i64) {
    let pid = child.id() as i32;
    let mut status: libc::c_int = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    let code = if r < 0 {
        -1
    } else if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    };
    (code, tv(ru.ru_utime), tv(ru.ru_stime), ru.ru_maxrss)
}

#[cfg(windows)]
fn wait_rusage(child: &mut std::process::Child) -> (i32, f64, f64, i64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{FILETIME, HANDLE};
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    // Read the times before waiting: after wait() the handle is closed.
    let handle = child.as_raw_handle() as HANDLE;
    let status = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let mut zero = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let ok = unsafe {
        GetProcessTimes(
            handle,
            &mut zero[0],
            &mut zero[1],
            &mut zero[2],
            &mut zero[3],
        )
    };
    // FILETIME counts 100-nanosecond intervals.
    let secs =
        |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 / 1e7;
    if ok == 0 {
        return (status, 0.0, 0.0, 0);
    }
    (status, secs(zero[3]), secs(zero[2]), 0) // user, kernel; no RSS from this call
}

/// Find a `-C key=value` among the codegen flags (they repeat, so the first
/// `-C` is rarely the one wanted).
fn codegen_value(args: &[String], key: &str) -> Option<String> {
    let want = format!("{key}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let candidate = if a == "-C" {
            it.next()?
        } else if let Some(v) = a.strip_prefix("-C") {
            v
        } else {
            continue;
        };
        if let Some(v) = candidate.strip_prefix(&want) {
            return Some(v.to_string());
        }
    }
    None
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
    }
    None
}

/// The wrapper entry point. Must stay cheap and must never break a build:
/// any failure here still runs the compiler and still returns its status.
pub fn shim(args: &[String]) -> i32 {
    if args.is_empty() {
        return 1;
    }
    // chain: honour a wrapper that was already configured
    let (program, rest) = match std::env::var("CRATEBANK_INNER_WRAPPER") {
        Ok(w) if !w.is_empty() => (w, args.to_vec()),
        _ => (args[0].clone(), args[1..].to_vec()),
    };
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut child = match Command::new(&program).args(&rest).spawn() {
        Ok(c) => c,
        Err(_) => return 1,
    };
    let (code, user, sys, maxrss) = wait_rusage(&mut child);
    #[cfg(unix)]
    std::mem::forget(child); // wait4 already reaped it

    // record, best effort: a failure to write must not disturb the build
    if let Some(crate_name) = arg_value(args, "--crate-name") {
        let rec = json!({
            "crate_name": crate_name,
            "extra_filename": codegen_value(args, "extra-filename"),
            "crate_type": arg_value(args, "--crate-type"),
            "started": started,
            "cpu_user_s": user, "cpu_sys_s": sys, "max_rss_kb": maxrss,
            "rc": code,
        });
        let dir = sidecar_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            let f = dir.join(format!("{}.jsonl", std::process::id() / 1000));
            if let Ok(mut fh) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(f)
            {
                let _ = writeln!(fh, "{rec}");
            }
        }
    }
    code
}

/// Merge sidecar rusage into a session's units by crate name and time order.
///
/// The sidecar cannot know cargo's unit indices, so matching is by crate name;
/// where a name compiled more than once (lib, bin, build script) the records
/// are assigned in start order. Every unit says whether it was matched, so an
/// analysis can drop unmatched ones rather than silently treat a wall time as
/// a CPU time.
pub fn merge(events: &mut [Value], window: (f64, f64)) -> (usize, usize) {
    let mut recs: Vec<Value> = vec![];
    if let Ok(rd) = std::fs::read_dir(sidecar_dir()) {
        for e in rd.flatten() {
            let Ok(txt) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            for line in txt.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    let t = v["started"].as_f64().unwrap_or(0.0);
                    if t >= window.0 && t <= window.1 {
                        recs.push(v);
                    }
                }
            }
        }
    }
    recs.sort_by(|a, b| {
        a["started"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&b["started"].as_f64().unwrap_or(0.0))
            .unwrap()
    });

    let mut matched = 0;
    let mut total = 0;
    for ev in events.iter_mut() {
        if ev["reason"] != "unit-registered" {
            continue;
        }
        // A build script *execution* runs no compiler, so it can never have
        // rustc CPU. Counting it as a miss would understate coverage. (Its own
        // cost is real and material -- measured at ~9% of a fleet's CPU -- but
        // reaching it needs a different wrapper than RUSTC_WRAPPER.)
        if ev["mode"] == "run-custom-build" {
            continue;
        }
        total += 1;
        let Some(name) = ev["target"]["name"].as_str().map(|s| s.replace('-', "_")) else {
            continue;
        };
        // A crate's lib and its build script share a --crate-name, so name
        // alone can hand a lib unit its build script's numbers. cargo's target
        // kind says which is which: a build script compiles as a bin.
        let kind = ev["target"]["kind"].as_str().unwrap_or("lib");
        let want_bin = matches!(kind, "bin" | "custom-build" | "build-script");
        let same_name =
            |r: &Value| r["crate_name"].as_str().map(|c| c.replace('-', "_")) == Some(name.clone());
        let kind_ok = |r: &Value| {
            let ct = r["crate_type"].as_str().unwrap_or("");
            if want_bin {
                ct == "bin"
            } else {
                ct != "bin"
            }
        };
        let pos = recs
            .iter()
            .position(|r| same_name(r) && kind_ok(r))
            .or_else(|| recs.iter().position(same_name));
        if let Some(pos) = pos {
            let r = recs.remove(pos);
            let (u, s) = (
                r["cpu_user_s"].as_f64().unwrap_or(0.0),
                r["cpu_sys_s"].as_f64().unwrap_or(0.0),
            );
            if let Some(o) = ev.as_object_mut() {
                o.insert("cpu_s".into(), json!(u + s));
                o.insert("max_rss_kb".into(), r["max_rss_kb"].clone());
            }
            matched += 1;
        }
    }
    (matched, total)
}

/// Remove sidecar files older than a day, so the directory cannot grow forever.
pub fn prune() {
    let Ok(rd) = std::fs::read_dir(sidecar_dir()) else {
        return;
    };
    let day = std::time::Duration::from_secs(86_400);
    for e in rd.flatten() {
        if let Ok(md) = e.metadata() {
            if md
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|d| d > day)
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}
