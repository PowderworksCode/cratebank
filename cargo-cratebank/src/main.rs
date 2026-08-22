//! `cargo cratebank` — sampled Rust builds for the cratebank census.
//!
//! `cargo cratebank build` runs stable `cargo build --timings` under samply,
//! parses both outputs, removes non-public units, and sends one combined
//! payload. Nothing wraps rustc, so configured compiler caches remain intact.
//!
//! Layout:
//!   cli       the command-line surface and process-wide helpers
//!   timings   Cargo timing-report parsing and privacy filtering
//!   sample    samply profile parsing and phase attribution
//!   payload   the combined upload envelope
//!   ship      compressed transport
//!   cmd/*     one file per subcommand
mod buildenv;
mod cli;
mod cmd;
mod load;
mod machine;
mod payload;
mod sample;
mod ship;
mod timings;

use cli::{Cli, Cmd};

fn main() {
    let cli = Cli::parse_from_cargo();
    let c = &cli.common;
    let code = match cli.cmd {
        Cmd::Build { ref args } => cmd::build::run(c, args),
        Cmd::Status => cmd::status::run(c),
        Cmd::Serve { port } => cmd::serve::run(c, port),
    };
    std::process::exit(code);
}
