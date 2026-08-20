//! Command-line surface.
//!
//! Invoked as `cargo cratebank …`, so cargo passes "cratebank" as argv[1];
//! `Cli::parse_from_cargo` drops it.
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub const DEFAULT_ENDPOINT: &str = "https://ingest.cratebank.io/v1/sessions";

#[derive(Parser, Debug)]
#[command(name = "cargo-cratebank", version, about = "Share the Rust build timings you were already producing")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug, Clone)]
pub struct Common {
    /// Where to POST sessions
    #[arg(long, global = true, env = "CRATEBANK_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,
    /// Print the exact payload and send nothing
    #[arg(long, global = true)]
    pub dry_run: bool,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Opt this project in to automatic sending
    Enable,
    /// Ship every completed session from any opted-in workspace (background)
    Watch,
    /// Build with cargo's analysis flags enabled, then send
    Build {
        /// Arguments passed through to `cargo build`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send session logs from builds that already happened
    Send(SendArgs),
    /// Show whether everything is wired up
    Status,
    /// Run a local echo collector (for testing)
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// Internal: used by the build.rs trigger
    #[command(hide = true)]
    Autosend {
        /// Re-spawn detached and return immediately
        #[arg(long)]
        detach: bool,
    },
}

#[derive(Args, Debug, Default)]
pub struct SendArgs {
    /// Send every unsent session
    #[arg(long)]
    pub all: bool,
    /// Send one session by id (or a unique fragment of it)
    #[arg(long)]
    pub session: Option<String>,
    /// Send the N most recent sessions
    #[arg(long, default_value_t = 1)]
    pub since: usize,
}

impl Cli {
    pub fn parse_from_cargo() -> Self {
        let mut args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("cratebank") {
            args.remove(1);
        }
        Cli::parse_from(args)
    }
}

/// `$CARGO_HOME`, falling back to `~/.cargo`.
pub fn cargo_home() -> PathBuf {
    std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cargo")
    })
}

/// CRATEBANK_DEBUG=1 explains why a send did or did not happen.
pub fn dbg(msg: &str) {
    if std::env::var("CRATEBANK_DEBUG").is_ok() {
        eprintln!("cratebank[autosend] {msg}");
    }
}

