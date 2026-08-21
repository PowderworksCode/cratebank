//! Transport, and the ledger of what has already been sent.
use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;
use std::path::PathBuf;

use serde_json::Value;

use crate::cli::cargo_home;

/// POST one payload, gzipped.
///
/// Compression is worth ~9x on real sessions, for the cost of a header. But
/// inbound `Content-Encoding` is **undocumented** for the ingest endpoint, so a
/// rejection is treated as "this collector does not accept gzip" and the same
/// body is sent again uncompressed. A lost contribution is worse than a large
/// one.
pub fn post(endpoint: &str, body: &Value) -> Result<String, String> {
    post_sized(endpoint, body).map(|(r, _)| r)
}

/// Returns the response and the bytes actually put on the wire, so callers can
/// report what happened rather than what was hoped for.
pub fn post_sized(endpoint: &str, body: &Value) -> Result<(String, usize), String> {
    let raw = body.to_string();
    match gzip(raw.as_bytes()) {
        Some(gz) => match send(endpoint, &gz, true) {
            Ok(r) => Ok((r, gz.len())),
            Err(e) => {
                crate::cli::dbg(&format!("gzip rejected ({e}); retrying uncompressed"));
                send(endpoint, raw.as_bytes(), false).map(|r| (r, raw.len()))
            }
        },
        None => send(endpoint, raw.as_bytes(), false).map(|r| (r, raw.len())),
    }
}

fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut e = GzEncoder::new(Vec::new(), Compression::best());
    e.write_all(bytes).ok()?;
    e.finish().ok()
}

fn send(endpoint: &str, body: &[u8], gzipped: bool) -> Result<String, String> {
    let mut req = ureq::post(endpoint).set("content-type", "application/json");
    if gzipped {
        req = req.set("content-encoding", "gzip");
    }
    req.send_bytes(body)
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

/// Compressed and uncompressed size of a payload, for reporting.
pub fn sizes(body: &Value) -> (usize, usize) {
    let raw = body.to_string();
    let n = gzip(raw.as_bytes()).map(|g| g.len()).unwrap_or(raw.len());
    (raw.len(), n)
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

