//! Who built it, and on what.
//!
//! The census separates "this crate is expensive" from "this machine is slow"
//! with a crossed random-effects model — `log cost = intrinsic(class) +
//! speed(machine) + …` — so it needs to know what a build ran on.
//!
//! It does **not** need to know *which* machine, and there is deliberately no
//! machine id. On CI an id would be useless at best: runners are ephemeral, so
//! every job looks like a new machine. At worst it would be wrong — a cached
//! `$CARGO_HOME` carries the id across genuinely different physical runners,
//! grouping unrelated hardware under one label. And on a laptop a persistent
//! id is exactly the sort of durable per-user handle a build tool has no
//! business minting.
//!
//! What is sent instead is a machine *profile*: CPU model, cores, memory,
//! kernel, virtualization. The grouping the model wants is "a 4-core Linux
//! CI runner" or "an M-series laptop", not one ephemeral VM, and a profile
//! expresses that directly while being shared by millions of machines.
//! Hostname, user and network identity are never read.
use serde_json::{json, Value};

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
