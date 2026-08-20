//! Who built it, and on what.
//!
//! The census separates "this crate is expensive" from "this machine is slow"
//! with a crossed random-effects model — `log cost = intrinsic(class) +
//! speed(machine) + …` — which is identifiable only because thousands of
//! machines compile the same classes. That requires a stable machine label and
//! enough hardware detail to interpret it. Without them the timings pool into
//! mush.
//!
//! The label is a **random** id generated once and stored in
//! `$CARGO_HOME/cratebank/machine-id`. It is deliberately not derived from a
//! hostname, MAC address, username or anything else about you: it groups your
//! own builds together and says nothing else. Delete the file and you are a
//! new machine.
//!
//! The specs are the kind a hardware review would print — CPU model, core
//! count, memory size, kernel — each shared by millions of machines. Hostname,
//! user, and network identity are never read.
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::cargo_home;

fn id_file() -> PathBuf { cargo_home().join("cratebank").join("machine-id") }

/// A stable, random, meaningless label for this machine.
pub fn machine_id() -> String {
    if let Ok(s) = std::fs::read_to_string(id_file()) {
        let s = s.trim().to_string();
        if !s.is_empty() { return s; }
    }
    // random, not derived: nanotime + pid + a stack address, hashed
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    std::process::id().hash(&mut h);
    (&h as *const _ as usize).hash(&mut h);
    let a = h.finish();
    std::thread::yield_now();
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    let id = format!("{a:016x}{:016x}", h.finish());
    let p = id_file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(&p, &id);
    id
}

fn first_value(path: &str, key: &str) -> Option<String> {
    let txt = std::fs::read_to_string(path).ok()?;
    for line in txt.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key { return Some(v.trim().to_string()); }
        }
    }
    None
}

fn mem_gb() -> Option<u64> {
    let kb: u64 = first_value("/proc/meminfo", "MemTotal")?
        .split_whitespace().next()?.parse().ok()?;
    Some((kb + 512 * 1024) / (1024 * 1024))   // nearest GB; not a fingerprint
}

/// Cheap virtualization hint — a VM's timings are not a laptop's.
fn virt() -> Option<String> {
    let out = std::process::Command::new("systemd-detect-virt").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() || v == "none" { None } else { Some(v) }
}

fn kernel() -> Option<String> {
    let out = std::process::Command::new("uname").arg("-r").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

pub fn cargo_version() -> Option<String> {
    let out = std::process::Command::new("cargo").arg("-V").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Specs a hardware review would print. No hostname, user or network identity.
pub fn snapshot() -> Value {
    json!({
        "machine_id": machine_id(),
        "cpu_model": first_value("/proc/cpuinfo", "model name"),
        "cpu_cores": std::thread::available_parallelism().map(|p| p.get()).ok(),
        "mem_gb": mem_gb(),
        "kernel": kernel(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "virt": virt(),
        "cargo_version": cargo_version(),
        "ci": std::env::var("CI").is_ok(),
    })
}
