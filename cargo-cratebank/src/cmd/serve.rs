//! `cargo cratebank serve` — a minimal echo collector, so the whole path is
//! testable with no infrastructure.
use std::io::{Read, Write};

use serde_json::Value;

use crate::cli::Common;

/// Minimal echo collector so the whole path is testable with no infrastructure.
/// Read one request: headers, then `content-length` bytes of body.
fn read_request(s: &mut std::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let Some(head_end) = find_header_end(&buf) else {
            continue;
        };
        if buf.len() - head_end >= content_length(&buf[..head_end]) {
            break;
        }
    }
    buf
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(buf).find("\r\n\r\n").map(|p| p + 4)
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .to_lowercase()
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

pub fn run(_o: &Common, port: u16) -> i32 {
    let l = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cratebank serve: {e}");
            return 1;
        }
    };
    eprintln!("cratebank: echo collector on http://127.0.0.1:{port}/ingest");
    for stream in l.incoming() {
        let Ok(mut s) = stream else { continue };
        let buf = read_request(&mut s);
        let txt = String::from_utf8_lossy(&buf).to_string();
        let head_len = find_header_end(&buf).unwrap_or(0);
        // The real endpoint is our Worker, which stores the body byte-for-byte
        // and never decodes it. So the only thing that identifies a blob here
        // is content-type, and decoding is this collector's own convenience --
        // it decompresses purely so it can print a human summary.
        //
        // Sniff the zstd magic number rather than trusting the header: that is
        // what actually determines whether the bytes in R2 are readable, and
        // a client that set the right header while writing the wrong codec is
        // exactly the regression worth catching.
        let raw = &buf[head_len..];
        let compressed = raw.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]);
        let body = if compressed {
            crate::ship::decompress(raw).unwrap_or_default()
        } else {
            txt.split_once("\r\n\r\n")
                .map(|x| x.1)
                .unwrap_or("")
                .to_string()
        };
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => eprintln!("[ingest]{} run {} · {} timing units ({} withheld) · {} sampled units · {} · rustc {}",
                if compressed { " zstd" } else { "" },
                v["run_id"].as_str().unwrap_or("?"),
                v["counts"]["units"], v["counts"]["units_withheld"],
                v["counts"]["phase_units"], v["env"]["host"].as_str().unwrap_or("?"),
                v["env"]["rustc_version"].as_str().unwrap_or("?")),
            Err(e) => eprintln!("[ingest] {} bytes, not json ({e})", body.len()),
        }
        let _ = s.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\r\nok",
        );
    }
    0
}
