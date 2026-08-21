//! What else the machine was doing.
//!
//! A build's wall clock is meaningless without it: sixteen units on a quiet
//! box and sixteen units fighting a test suite look identical in the log. The
//! statistics carry a per-observation contention term, and this is what feeds
//! it.
//!
//! Sampling only works while a build is running, so it happens in the two
//! commands that are present for one — `build` and `watch`. Sessions shipped
//! after the fact (`send`) report `null` rather than a load figure measured at
//! the wrong time.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// One-minute load average. Unix-only in substance: `sysinfo` reports zeros on
/// Windows, so a Windows contribution simply carries no load figure rather
/// than a fabricated one.
fn loadavg() -> f64 {
    sysinfo::System::load_average().one
}

/// Total stall time from the pressure-stall interface, in microseconds.
/// Linux only; the read simply fails elsewhere and the field is null.
/// Deltas across a build say how much of it was spent waiting for a resource
/// rather than using one.
fn psi_total(kind: &str) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/pressure/{kind}")).ok()?;
    let line = s.lines().find(|l| l.starts_with("some"))?;
    line.split("total=").nth(1)?.trim().parse().ok()
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<(u64, f64, f64)>>,
    psi0: [Option<u64>; 3],
}

const KINDS: [&str; 3] = ["cpu", "io", "memory"];

impl Sampler {
    pub fn start() -> Sampler {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            let (mut n, mut sum, mut max) = (0u64, 0.0f64, 0.0f64);
            while !s2.load(Ordering::Relaxed) {
                let l = loadavg();
                n += 1;
                sum += l;
                if l > max { max = l; }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            (n, sum, max)
        });
        Sampler {
            stop,
            handle: Some(handle),
            psi0: [psi_total(KINDS[0]), psi_total(KINDS[1]), psi_total(KINDS[2])],
        }
    }

    pub fn finish(mut self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        let (n, sum, max) = self.handle.take().and_then(|h| h.join().ok()).unwrap_or((0, 0.0, 0.0));
        let mut stall = serde_json::Map::new();
        for (i, k) in KINDS.iter().enumerate() {
            let d = match (self.psi0[i], psi_total(k)) {
                (Some(a), Some(b)) => json!((b.saturating_sub(a)) as f64 / 1e6),
                _ => Value::Null,
            };
            stall.insert(k.to_string(), d);
        }
        // Windows and macOS report no load average; send null rather than 0.0,
        // which would read as "idle machine" and bias every contention model.
        let usable = max > 0.0;
        json!({
            "loadavg_mean": if n > 0 && usable { json!(sum / n as f64) } else { Value::Null },
            "loadavg_max": if n > 0 && usable { json!(max) } else { Value::Null },
            "samples": n,
            "stall_seconds": stall,
        })
    }
}
