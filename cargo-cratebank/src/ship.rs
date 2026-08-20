//! Transport, and the ledger of what has already been sent.
use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;

use crate::cli::cargo_home;

pub fn post(endpoint: &str, body: &Value) -> Result<String, String> {
    ureq::post(endpoint)
        .set("content-type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

pub fn state_file() -> PathBuf { cargo_home().join("cratebank").join("sent.txt") }

pub fn already_sent(run_id: &str) -> bool {
    std::fs::read_to_string(state_file()).map(|s| s.lines().any(|l| l == run_id)).unwrap_or(false)
}

pub fn mark_sent(run_id: &str) {
    let p = state_file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{run_id}");
    }
}

