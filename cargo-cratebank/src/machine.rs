//! Who built it, and on what.
//!
//! The census separates "this crate is expensive" from "this machine is slow"
//! with a crossed random-effects model — `log cost = intrinsic(class) +
//! speed(machine) + …` — so it needs to know what a build ran on.
//!
//! Two things are sent: a **machine id** and a machine **profile**.
//!
//! The id is stated plainly rather than hidden, because it is the one field
//! here that enables linkage — with it, the sessions from one machine can be
//! joined together, which is what makes within-machine comparisons possible
//! ("on this same box, did serde 1.0.200 compile slower than 1.0.199?") and
//! equally what makes a build timeline reconstructable. Anyone who does not
//! want that should not send the data.
//!
//! It is yours to set:
//!
//! | source | precedence | typical use |
//! | --- | --- | --- |
//! | `CRATEBANK_MACHINE_ID` | first | CI, where `$CARGO_HOME` is ephemeral |
//! | `[package.metadata.cratebank] machine_id` | second | one id for a project or org |
//! | `$CARGO_HOME/cratebank/machine-id` | third | a plain file — edit it freely |
//! | random, generated once | fallback | a personal machine |
//!
//! A non-random value is often the *better* choice: `acme-ci` attributes a
//! company's runs to that company, which is attribution rather than tracking,
//! and on ephemeral CI a random id would be a fresh meaningless value every
//! job. Setting it to `none` or an empty string omits the field entirely.
//!
//! The profile — CPU model, cores, memory, kernel, virtualization — is the kind
//! of detail a hardware review prints, each value shared by millions of
//! machines. Hostname, user and network identity are never read.
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::cargo_home;

fn id_file() -> PathBuf { cargo_home().join("cratebank").join("machine-id") }

/// Resolve the machine id, generating and announcing one on first use.
/// `None` means the user asked for no id at all.
pub fn machine_id(project_dir: Option<&std::path::Path>) -> Option<String> {
    machine_id_with(project_dir, true)
}

/// `announce = false` for callers that explain the id themselves (enable does,
/// at more length and better placed than a one-line notice).
pub fn machine_id_with(project_dir: Option<&std::path::Path>, announce: bool) -> Option<String> {
    let normalise = |v: String| {
        let v = v.trim().to_string();
        if v.is_empty() || v == "none" { None } else { Some(v) }
    };
    if let Ok(v) = std::env::var("CRATEBANK_MACHINE_ID") {
        return normalise(v);
    }
    if let Some(v) = project_dir.and_then(configured_id) {
        return normalise(v);
    }
    if let Ok(v) = std::fs::read_to_string(id_file()) {
        if !v.trim().is_empty() { return normalise(v); }
    }
    // first use on this machine: generate, store, and say so
    let id = random_id();
    let p = id_file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if std::fs::write(&p, &id).is_ok() && announce {
        eprintln!("cratebank: generated machine id {id}\n  stored in {}\n  \
                   edit that file, or set CRATEBANK_MACHINE_ID, to use your own \
                   (e.g. your org's name); set it to `none` to send no id.",
                  p.display());
    }
    Some(id)
}

/// `[package.metadata.cratebank] machine_id = "acme-ci"`, searched upward.
pub fn configured_id(dir: &std::path::Path) -> Option<String> {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        if let Ok(txt) = std::fs::read_to_string(d.join("Cargo.toml")) {
            if let Ok(v) = txt.parse::<toml::Value>() {
                for t in ["package", "workspace"] {
                    if let Some(x) = v.get(t).and_then(|x| x.get("metadata"))
                        .and_then(|x| x.get("cratebank")).and_then(|x| x.get("machine_id"))
                        .and_then(|x| x.as_str())
                    {
                        return Some(x.to_string());
                    }
                }
            }
        }
        cur = d.parent().map(|x| x.to_path_buf());
    }
    None
}

fn random_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos()).unwrap_or(0);
    now().hash(&mut h);
    std::process::id().hash(&mut h);
    (&h as *const _ as usize).hash(&mut h);
    let a = h.finish();
    std::thread::yield_now();
    now().hash(&mut h);
    format!("{a:016x}{:016x}", h.finish())
}

/// Hardware and OS, via `sysinfo` — one cross-platform source instead of
/// `/proc` parsing that silently returns nothing everywhere else.
fn hardware() -> (Option<String>, Option<usize>, Option<u64>, Option<String>, Option<String>) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let cpu = sys.cpus().first().map(|c| c.brand().trim().to_string()).filter(|s| !s.is_empty());
    let cores = std::thread::available_parallelism().map(|p| p.get()).ok();
    // nearest GB: a size, not a fingerprint
    let mem = Some((sys.total_memory() + 512 * 1024 * 1024) / (1024 * 1024 * 1024));
    let kernel = System::kernel_version();
    let os = System::long_os_version();
    (cpu, cores, mem, kernel, os)
}

/// Cheap virtualization hint — a VM's timings are not a laptop's. Linux only;
/// elsewhere the CI flag and the CPU brand carry most of the same signal.
fn virt() -> Option<String> {
    let out = std::process::Command::new("systemd-detect-virt").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() || v == "none" { None } else { Some(v) }
}

pub fn cargo_version() -> Option<String> {
    let out = std::process::Command::new("cargo").arg("-V").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Specs a hardware review would print. No hostname, user or network identity.
pub fn snapshot(project_dir: Option<&std::path::Path>) -> Value {
    let (cpu_model, cpu_cores, mem_gb, kernel, os_version) = hardware();
    json!({
        "machine_id": machine_id(project_dir),
        "cpu_model": cpu_model,
        "cpu_cores": cpu_cores,
        "mem_gb": mem_gb,
        "kernel": kernel,
        "os": std::env::consts::OS,
        "os_version": os_version,
        "arch": std::env::consts::ARCH,
        "virt": virt(),
        "cargo_version": cargo_version(),
        "ci": std::env::var("CI").is_ok(),
    })
}
