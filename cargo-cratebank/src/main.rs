//! `cargo cratebank` — opt-in sharing of the build timings you were already
//! producing, for the cratebank census.
//!
//! This plugin instruments nothing. Cargo already records everything on
//! nightly: `-Zbuild-analysis` writes one JSONL session log per invocation to
//! `$CARGO_HOME/log/`, and `-Zsection-timings` folds rustc's frontend/codegen
//! section boundaries into that same stream. We read those logs, drop
//! everything non-public, and POST the rest. Nothing sits in the compile path,
//! nothing conflicts with sccache, no build is ever run on your behalf.
//!
//! Layout:
//!   cli       the command-line surface and process-wide helpers
//!   session   finding, reading and filtering cargo's session logs
//!   project   manifests, opt-in, workspace roots, build liveness
//!   ship      transport and the ledger of what has already been sent
//!   cmd/*     one file per subcommand
mod artifacts;
mod buildenv;
mod cli;
mod cmd;
mod load;
mod machine;
mod project;
mod rusage;
mod session;
mod ship;

use cli::{Cli, Cmd};

fn main() {
    // RUSTC_WRAPPER must name a bare executable, and cargo execs it with the
    // real rustc as argv[1]. Detect that shape and go straight to the shim,
    // before clap sees a command line full of rustc flags.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv
        .first()
        .map(|a| {
            let f = a.rsplit(['/', '\\']).next().unwrap_or(a);
            f == "rustc" || f == "rustc.exe"
        })
        .unwrap_or(false)
    {
        std::process::exit(rusage::shim(&argv));
    }

    let cli = Cli::parse_from_cargo();
    let c = &cli.common;
    let code = match cli.cmd {
        Cmd::Enable => cmd::enable::run(c),
        Cmd::Watch => cmd::watch::run(c),
        Cmd::Build { ref args } => cmd::build::run(c, args),
        Cmd::Send(ref a) => cmd::send::run(c, a),
        Cmd::Status => cmd::status::run(c),
        Cmd::Serve { port } => cmd::serve::run(c, port),
        Cmd::Autosend { detach } => cmd::autosend::run(c, detach),
        Cmd::RustcShim { ref args } => rusage::shim(args),
    };
    std::process::exit(code);
}
