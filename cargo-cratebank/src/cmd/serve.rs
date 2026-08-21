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
        let Some(head_end) = find_header_end(&buf) else { continue };
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
        Err(e) => { eprintln!("cratebank serve: {e}"); return 1 }
    };
    eprintln!("cratebank: echo collector on http://127.0.0.1:{port}/ingest");
    for stream in l.incoming() {
        let Ok(mut s) = stream else { continue };
        let buf = read_request(&mut s);
        let txt = String::from_utf8_lossy(&buf).to_string();
        let head_len = find_header_end(&buf).unwrap_or(0);
        let compressed = txt[..head_len.min(txt.len())].to_lowercase().contains("content-encoding: br");
        let body = if compressed {
            crate::ship::decompress(&buf[head_len..]).unwrap_or_default()
        } else {
            txt.splitn(2, "\r\n\r\n").nth(1).unwrap_or("").to_string()
        };
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => eprintln!("[ingest]{} run {} · {} events · {} units ({} withheld) · {} sections · {} · rustc {}",
                if compressed { " br" } else { "" },
                v["run_id"].as_str().unwrap_or("?"),
                v["counts"]["events"], v["counts"]["units"], v["counts"]["units_withheld"],
                v["counts"]["sections"], v["env"]["host"].as_str().unwrap_or("?"),
                v["env"]["rustc_version"].as_str().unwrap_or("?")),
            Err(e) => eprintln!("[ingest] {} bytes, not json ({e})", body.len()),
        }
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\r\nok");
    }
    0
}

