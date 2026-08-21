//! Transport, and the ledger of what has already been sent.
use std::io::Write;

use std::path::PathBuf;

use serde_json::Value;

use crate::cli::cargo_home;

/// POST one payload, brotli-compressed.
///
/// No negotiation and no fallback. The endpoint is assumed to be as dumb as
/// possible — it takes a blob and stores it, and anything that needs to read
/// the contents does so later. If a send fails it simply is not recorded as
/// sent, so the session stays in the queue and goes out next time; a failed
/// upload costs a retry, not a contribution.
pub fn post(endpoint: &str, body: &Value) -> Result<String, String> {
    post_sized(endpoint, body).map(|(r, _)| r)
}

/// Returns the response and the bytes actually put on the wire.
pub fn post_sized(endpoint: &str, body: &Value) -> Result<(String, usize), String> {
    let raw = body.to_string();
    let br = compress(raw.as_bytes()).ok_or("brotli failed")?;
    let n = br.len();
    ureq::post(endpoint)
        .set("content-type", "application/json")
        .set("content-encoding", "br")
        .send_bytes(&br)
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
        .map(|r| (r, n))
}

pub fn compress(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut out = Vec::new();
    {
        // quality 11, window 22: the payload is small and sent once, so the
        // slowest setting costs milliseconds and buys the best ratio
        let mut w = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
        w.write_all(bytes).ok()?;
    }
    Some(out)
}

pub fn decompress(bytes: &[u8]) -> Option<String> {
    use std::io::Read;
    let mut out = String::new();
    brotli::Decompressor::new(bytes, 4096).read_to_string(&mut out).ok()?;
    Some(out)
}

/// Compressed and uncompressed size of a payload, for reporting.
pub fn sizes(body: &Value) -> (usize, usize) {
    let raw = body.to_string();
    let n = compress(raw.as_bytes()).map(|g| g.len()).unwrap_or(raw.len());
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

