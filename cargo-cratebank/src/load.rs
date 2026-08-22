//! What else the machine was doing.
//!
//! A build's wall clock is meaningless without it: sixteen units on a quiet
//! box and sixteen units fighting a test suite look identical in the log. The
//! statistics carry a per-observation contention term, and this is what feeds
//! it.
//!
//! Sampling runs for the same interval as the Cargo and samply capture so the
//! load context belongs to the measured build.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// One-minute load average — a unix concept. Windows has none, and no crate
/// invents one honestly (systemstat's Windows implementation returns
/// "Not supported"; sysinfo returns zeros), so it stays null off unix.
fn loadavg() -> f64 {
    sysinfo::System::load_average().one
}

/// Machine-wide CPU utilisation, 0-100. This is the portable contention
/// signal: every platform has it, and it says more directly than load average
/// how busy the machine was while the build ran. (Load average also counts
/// uninterruptible sleepers on Linux, so a disk-bound neighbour inflates it
/// without competing for CPU.)
fn cpu_busy(sys: &mut sysinfo::System) -> f32 {
    sys.refresh_cpu_usage();
    sys.global_cpu_usage()
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

/// What one sampling run observed.
#[derive(Default, Clone, Copy)]
struct Samples {
    n: u64,
    load_sum: f64,
    load_max: f64,
    cpu_sum: f64,
    cpu_max: f64,
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Samples>>,
    psi0: [Option<u64>; 3],
}

const KINDS: [&str; 3] = ["cpu", "io", "memory"];

impl Sampler {
    pub fn start() -> Sampler {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            let mut acc = Samples::default();
            // first CPU sample is meaningless: usage is a delta between refreshes
            cpu_busy(&mut sys);
            while !s2.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let l = loadavg();
                let c = cpu_busy(&mut sys) as f64;
                acc.n += 1;
                acc.load_sum += l;
                acc.cpu_sum += c;
                if l > acc.load_max {
                    acc.load_max = l;
                }
                if c > acc.cpu_max {
                    acc.cpu_max = c;
                }
            }
            acc
        });
        Sampler {
            stop,
            handle: Some(handle),
            psi0: [
                psi_total(KINDS[0]),
                psi_total(KINDS[1]),
                psi_total(KINDS[2]),
            ],
        }
    }

    pub fn finish(mut self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        let s = self
            .handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        let (n, sum, max, csum, cmax) = (s.n, s.load_sum, s.load_max, s.cpu_sum, s.cpu_max);
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
            "cpu_busy_mean": if n > 0 { json!(csum / n as f64) } else { Value::Null },
            "cpu_busy_max": if n > 0 { json!(cmax) } else { Value::Null },
            "samples": n,
            "stall_seconds": stall,
        })
    }
}
