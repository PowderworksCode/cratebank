//! Transport, and the ledger of what has already been sent.
use std::io::Write;

use std::path::PathBuf;

use serde_json::Value;

use crate::cli::cargo_home;

/// POST one payload as a zstd blob.
///
/// The endpoint is our own Worker, which writes the body to R2 byte-for-byte
/// without decompressing or parsing it. So this is an *upload of a compressed
/// artifact*, not a compressed representation of a JSON request — hence
/// `content-type: application/zstd` and deliberately **no**
/// `content-encoding` header, which would invite an intermediary to decode the
/// body before the Worker ever sees it.
///
/// zstd because DuckDB decompresses it natively: the blob lands in R2 and is
/// queryable as-is, with no conversion step. brotli is not readable by DuckDB.
///
/// No negotiation and no fallback. If a send fails it simply is not recorded
/// as sent, so the session stays in the queue and goes out next time; a failed
/// upload costs a retry, not a contribution.
pub fn post(endpoint: &str, body: &Value) -> Result<String, String> {
    post_sized(endpoint, body).map(|(r, _)| r)
}

/// Returns the response and the bytes actually put on the wire.
pub fn post_sized(endpoint: &str, body: &Value) -> Result<(String, usize), String> {
    let raw = body.to_string();
    let blob = compress(raw.as_bytes()).ok_or("zstd failed")?;
    let n = blob.len();
    ureq::post(endpoint)
        .set("content-type", "application/zstd")
        .send_bytes(&blob)
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
        .map(|r| (r, n))
}

/// Level 19: the payload is small and sent once, so a slow setting costs
/// milliseconds. Unlike the Pipelines path, the ratio now genuinely matters --
/// the blob is stored as-is, so this is the on-disk size in R2 forever.
const ZSTD_LEVEL: i32 = 19;

pub fn compress(bytes: &[u8]) -> Option<Vec<u8>> {
    zstd::encode_all(bytes, ZSTD_LEVEL).ok()
}

pub fn decompress(bytes: &[u8]) -> Option<String> {
    String::from_utf8(zstd::decode_all(bytes).ok()?).ok()
}

/// Compressed and uncompressed size of a payload, for reporting.
pub fn sizes(body: &Value) -> (usize, usize) {
    let raw = body.to_string();
    let n = compress(raw.as_bytes())
        .map(|g| g.len())
        .unwrap_or(raw.len());
    (raw.len(), n)
}

pub fn state_file() -> PathBuf {
    cargo_home().join("cratebank").join("sent.txt")
}

pub fn already_sent(run_id: &str) -> bool {
    std::fs::read_to_string(state_file())
        .map(|s| s.lines().any(|l| l == run_id))
        .unwrap_or(false)
}

pub fn mark_sent(run_id: &str) {
    // Idempotent: `--session <id>` can deliberately resend an already-sent
    // session, and appending again each time would grow the ledger without
    // bound. already_sent() tolerates duplicates; the file should not have to.
    if already_sent(run_id) {
        return;
    }
    let p = state_file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{run_id}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn zstd_round_trips() {
        let payload = r#"[{"probe":"round-trip","n":1}]"#;
        let blob = super::compress(payload.as_bytes()).expect("compress");
        // zstd magic number -- a codec swap that still round-trips internally
        // but writes something DuckDB cannot read would pass without this.
        assert_eq!(&blob[..4], &[0x28, 0xb5, 0x2f, 0xfd], "not a zstd frame");
        assert_eq!(super::decompress(&blob).as_deref(), Some(payload));
    }

    /// Live check against a real stream. Ignored by default; run with
    /// `LIVE_EP=https://<stream>.ingest.cloudflare.com cargo test -- --ignored`
    #[test]
    #[ignore]
    fn posts_to_a_live_endpoint() {
        let ep = std::env::var("LIVE_EP").expect("set LIVE_EP");
        let body = serde_json::json!([{"probe":"client-zstd","via":"zstd"}]);
        let r = super::post(&ep, &body);
        println!("response: {r:?}");
        assert!(r.is_ok(), "post failed: {r:?}");
    }
}
