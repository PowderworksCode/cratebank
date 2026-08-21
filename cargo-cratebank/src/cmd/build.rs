//! `cargo cratebank build` — run the build under the sampler, then send the
//! session it produced along with per-unit compiler phases.
//!
//! The sampler wraps the whole `cargo build`, once. It must not wrap each
//! rustc: samply costs a flat ~1s per invocation regardless of what it
//! profiles, so per-unit sampling would pay that for every crate. Wrapping
//! once is also what makes attribution work, since samples carry a pid and
//! every unit is its own process.
//!
//! Sampling is the only phase mechanism; there is no second one to fall back
//! to. But the *build* always has to work. samply can take a build down with
//! it -- on macOS it injects a preload dylib into every child, and if dyld
//! rejects it (a GitHub arm64e runner, for instance) every build script dies
//! with SIGABRT and the failure looks like the project's fault. So a sampler
//! that fails is loud and then gets out of the way, and the build is re-run
//! plainly.
use std::process::Command;

use crate::cli::{Common, SendArgs};
use crate::session::sessions;

/// samply's flat startup cost dominates on small builds and vanishes on large
/// ones, while each sample costs ~116us. So a low rate saves almost nothing
/// and starves short units of samples; a high one is close to free.
const RATE_HZ: u32 = 4999;

/// cargo's own flags still matter: the session log is what identifies the
/// units, and the sampler only says how their time was spent.
fn cargo_args(args: &[String]) -> Vec<String> {
    // Includes the subcommand: both the sampled and plain paths run the very
    // same argv, so they cannot drift apart.
    let mut v: Vec<String> = vec![
        "build".into(),
        "-Zbuild-analysis".into(),
        "-Zsection-timings".into(),
        "--config".into(),
        "build.analysis.enabled=true".into(),
    ];
    v.extend_from_slice(args);
    v
}

fn warn_unsampled(reason: &str) {
    eprintln!();
    eprintln!("cratebank: ⚠ NOT SAMPLED — {reason}");
    eprintln!("cratebank:   the build will run normally and the session is still sent,");
    eprintln!("cratebank:   but it carries no compiler phase data.");
    eprintln!();
}

/// Run the build without the sampler. Used when sampling is unavailable or
/// broke, so a contributor's build never depends on the profiler working.
fn plain_build(args: &[String]) -> Result<(), i32> {
    let st = Command::new("cargo").args(cargo_args(args)).status();
    match st {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => {
            eprintln!("cratebank: build failed; sending nothing");
            Err(1)
        }
        Err(e) => {
            eprintln!("cratebank: cannot run cargo: {e}");
            Err(1)
        }
    }
}

pub fn run(o: &Common, args: &[String]) -> i32 {
    let before = sessions().len();
    let tmp = std::env::temp_dir().join(format!("cratebank-{}", std::process::id()));

    let sampler = crate::load::Sampler::start();

    let phases = if !crate::sample::samply_available() {
        warn_unsampled("samply is not installed (`cargo install samply`)");
        if let Err(c) = plain_build(args) {
            return c;
        }
        serde_json::Value::Null
    } else {
        eprintln!(
            "cratebank: sampling at {RATE_HZ} Hz — samply record -- cargo build {}",
            args.join(" ")
        );
        match crate::sample::record(&cargo_args(args), &tmp, RATE_HZ) {
            Ok((prof, syms)) => match crate::sample::attribute(&prof, &syms) {
                Ok(units) if !units.is_empty() => {
                    let total: u64 = units.iter().map(|u| u.total()).sum();
                    eprintln!("cratebank: {} units sampled, {total} samples", units.len());
                    crate::sample::to_json(&units, RATE_HZ)
                }
                Ok(_) => {
                    warn_unsampled("the profile attributed no samples to any compilation unit");
                    serde_json::Value::Null
                }
                Err(e) => {
                    warn_unsampled(&format!("could not read the profile: {e}"));
                    serde_json::Value::Null
                }
            },
            Err(e) => {
                // Either the build genuinely failed, or samply took it down.
                // Both look identical from here, so re-run plainly: if the
                // code is broken it fails again and we report that honestly,
                // and if samply was the problem the contributor still gets
                // their build.
                warn_unsampled(&format!("{e} — retrying without the sampler"));
                if let Err(c) = plain_build(args) {
                    let _ = std::fs::remove_dir_all(&tmp);
                    return c;
                }
                serde_json::Value::Null
            }
        }
    };

    let load = sampler.finish();
    let _ = std::fs::remove_dir_all(&tmp);

    if sessions().len() == before {
        eprintln!("cratebank: no new session log appeared — is this a nightly toolchain?");
        return 1;
    }

    crate::cmd::send::run_with_load(
        o,
        &SendArgs {
            since: 1,
            ..Default::default()
        },
        load,
        phases,
    )
}
