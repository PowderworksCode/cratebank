//! `cargo cratebank build` — run the build with cargo's analysis flags on,
//! then send the session it produced.
use std::process::Command;

use crate::cli::{Common, SendArgs};
use crate::session::sessions;

pub fn run(o: &Common, args: &[String]) -> i32 {
    let before = sessions().len();
    let mut c = Command::new("cargo");
    c.arg("build")
        .arg("-Zbuild-analysis")
        .arg("-Zsection-timings")
        .arg("--config").arg("build.analysis.enabled=true")
        .args(args);
    eprintln!("cratebank: cargo build -Zbuild-analysis -Zsection-timings {}", args.join(" "));
    let sampler = crate::load::Sampler::start();
    let st = c.status();
    let _ = sampler;
    match st {
        Ok(s) if !s.success() => { eprintln!("cratebank: build failed; sending nothing"); return 1 }
        Err(e) => { eprintln!("cratebank: cannot run cargo: {e}"); return 1 }
        _ => {}
    }
    if sessions().len() == before {
        eprintln!("cratebank: no new session log appeared — is this a nightly toolchain?");
        return 1;
    }
    crate::cmd::send::run(o, &SendArgs { since: 1, ..Default::default() })
}

