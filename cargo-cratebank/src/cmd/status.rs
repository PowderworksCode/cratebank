//! `cargo cratebank status` — is everything wired up?
use std::process::Command;

use crate::cli::{cargo_home, Common};
use crate::project::opted_in;
use crate::session::{log_dir, sessions};
use crate::ship::state_file;

pub fn run(o: &Common) -> i32 {
    let nightly = Command::new("cargo").arg("-Zhelp").output()
        .map(|x| String::from_utf8_lossy(&x.stdout).contains("build-analysis")).unwrap_or(false);
    let s = sessions();
    println!("cargo home    {}", cargo_home().display());
    println!("log dir       {}  ({} session log(s))", log_dir().display(), s.len());
    println!("nightly flags {}", if nightly { "available" } else { "NOT available (need nightly)" });
    println!("endpoint      {}", o.endpoint);
    println!("privacy       public units only (non-public units are never sent)");
    let here = std::env::current_dir().unwrap_or_default();
    let (id, src) = if let Ok(v) = std::env::var("CRATEBANK_MACHINE_ID") {
        (v, "CRATEBANK_MACHINE_ID")
    } else {
        match crate::machine::machine_id(Some(&here)) {
            Some(v) => (v, "Cargo.toml or $CARGO_HOME/cratebank/machine-id"),
            None => ("(none — no id is sent)".into(), "configured off"),
        }
    };
    println!("machine id    {id}\n              from {src}");
    let here = std::env::current_dir().unwrap_or_default();
    println!("auto-send     {}", if opted_in(&here) { "ON for this project (share = true)" }
                                 else { "off here — run: cargo cratebank enable" });
    let sent = std::fs::read_to_string(state_file()).map(|s| s.lines().count()).unwrap_or(0);
    println!("sessions sent {sent}");
    if let Some(p) = s.first() { println!("latest        {}", p.file_name().unwrap().to_string_lossy()); }
    println!("\nto record every build, add to .cargo/config.toml:\n\
              \n  [unstable]\n  build-analysis = true\n  section-timings = true\n\
              \n  [build.analysis]\n  enabled = true\n\
              \n(on a stable toolchain these only warn, so they are safe to leave in place)");
    if !nightly { 1 } else { 0 }
}

