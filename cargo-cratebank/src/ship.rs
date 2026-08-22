//! Compressed transport.

use serde_json::Value;

/// POST one payload as a zstd blob.
///
/// The endpoint is our own Worker, which writes the body to R2 byte-for-byte
/// without decompressing or parsing it. So this is an *upload of a compressed
/// payload*, not a compressed representation of a JSON request — hence
/// `content-type: application/zstd` and deliberately **no**
/// `content-encoding` header, which would invite an intermediary to decode the
/// body before the Worker sees it.
///
/// DuckDB decompresses zstd natively, so the blob is queryable as-is.
///
/// Every payload uses the same queryable on-disk representation.
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
/// milliseconds. The blob is stored as-is, so this is the on-disk size in R2.
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
        let r = super::post_sized(&ep, &body);
        println!("response: {r:?}");
        assert!(r.is_ok(), "post failed: {r:?}");
    }
}
