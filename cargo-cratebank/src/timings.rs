//! `cargo build --timings`: the build's own view of itself.
//!
//! Cargo writes an HTML report whose script block embeds the data it used to
//! draw the charts. We read the data, not the chart.
//!
//! Most of it duplicates the session log -- durations, features, the DAG,
//! frontend/codegen sections are all in `-Zbuild-analysis` events already --
//! and it is captured anyway, for two reasons. `CONCURRENCY_DATA` exists
//! nowhere else: it is cargo's own count of how many units were *ready but
//! blocked on dependencies* versus running, which is the difference between a
//! build that is dependency-bound and one that is CPU-bound. And `--timings`
//! is **stable**, where the session log needs nightly, so this is the
//! substitute if stable-only collection is ever wanted rather than a
//! supplement to it.
//!
//! The duplication is deliberate but must not be mistaken for corroboration:
//! these numbers and the session log's have one source between them, so they
//! agreeing proves nothing. The sampler's per-unit wall is the independent
//! measurement.
use std::path::Path;

use serde_json::{Map, Value};

/// The blocks worth having. `UNIT_DATA` is per-unit records; the other two are
/// whole-build time series.
const BLOCKS: [&str; 3] = ["UNIT_DATA", "CONCURRENCY_DATA", "CPU_USAGE"];

/// Pull `const NAME = <json>;` out of the report.
///
/// Brace/bracket matching rather than a regex: the payload is JSON containing
/// strings that contain brackets (feature names, target names), so a
/// shortest-match regex silently truncates the array and a greedy one swallows
/// the rest of the script.
fn extract(html: &str, name: &str) -> Option<Value> {
    let needle = format!("const {name} = ");
    let start = html.find(&needle)? + needle.len();
    let bytes = html.as_bytes();
    let open = *bytes.get(start)?;
    let close = match open {
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    for (i, &c) in bytes[start..].iter().enumerate() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&html[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Cargo stamps each report `cargo-timing-YYYYMMDDTHHMMSSZ-<hash>.html` and
/// never cleans them up, so a directory accumulates every run ever made.
/// Match the session's own `build-started` timestamp against that stamp rather
/// than taking the newest file: a session shipped later by `cratebank send`
/// would otherwise silently attach a *different* build's report, which is the
/// same failure as attributing samples to the wrong unit -- plausible-looking
/// data about something else entirely.
///
/// Both are truncated to the minute. Cargo names the file when the build ends
/// and the header is stamped when it begins, so they differ by the build's
/// duration; a minute of slack matches short builds without matching the run
/// before or after.
fn stamp_prefix(started: &str) -> Option<String> {
    // `2026-08-22T09:16:06.131984Z` -> `cargo-timing-20260822T0916`
    let (date, rest) = started.split_once('T')?;
    let hhmm: String = rest.chars().filter(char::is_ascii_digit).take(4).collect();
    if hhmm.len() < 4 {
        return None;
    }
    Some(format!("cargo-timing-{}T{}", date.replace('-', ""), hhmm))
}

/// Newest `cargo-timing-*.html` under `<target>/cargo-timings/`.
///
/// Cargo also writes an unsuffixed `cargo-timing.html`, but it is a copy of
/// the newest run and gives no way to tell a stale one from a fresh one --
/// so match on the timestamped name and take the most recent by mtime.
fn report(target_dir: &Path) -> Option<std::path::PathBuf> {
    let dir = target_dir.join("cargo-timings");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let is_report = p
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.starts_with("cargo-timing-") && f.ends_with(".html"));
        if !is_report {
            continue;
        }
        let Ok(t) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Read the report cargo just wrote, or Null if there is nothing to read.
///
/// Never an error: a missing report means less data, not a failed build, and
/// the session is still worth sending.
pub fn capture(target_dir: &Path, header: &Map<String, Value>) -> Value {
    let Some(path) = report(target_dir) else {
        return Value::Null;
    };
    // Refuse a report that is not this build's. Without a usable timestamp the
    // safe answer is no data, not a guess.
    let Some(started) = header.get("timestamp").and_then(Value::as_str) else {
        return Value::Null;
    };
    let Some(prefix) = stamp_prefix(started) else {
        return Value::Null;
    };
    let matches = path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.starts_with(&prefix));
    if !matches {
        return Value::Null;
    }
    let Ok(html) = std::fs::read_to_string(&path) else {
        return Value::Null;
    };
    let mut out = serde_json::Map::new();
    for b in BLOCKS {
        if let Some(v) = extract(&html, b) {
            out.insert(b.to_lowercase(), v);
        }
    }
    if out.is_empty() {
        return Value::Null;
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_block_whose_strings_contain_brackets() {
        // A feature or target name containing a bracket breaks naive matching:
        // a lazy regex stops at the first `]` inside the string, a greedy one
        // runs past the end of the array.
        let html = r#"<script>
const UNIT_DATA = [{"name":"a","features":["x]y","z"]},{"name":"b","features":[]}];
const CONCURRENCY_DATA = [{"t":0.1,"active":5,"waiting":0,"inactive":11}];
</script>"#;
        let u = extract(html, "UNIT_DATA").expect("UNIT_DATA");
        assert_eq!(u.as_array().unwrap().len(), 2);
        assert_eq!(u[0]["features"][0], "x]y");
        let c = extract(html, "CONCURRENCY_DATA").expect("CONCURRENCY_DATA");
        assert_eq!(c[0]["waiting"], 0);
    }

    #[test]
    fn a_report_is_matched_to_its_own_build() {
        // cargo never cleans cargo-timings/, so `send` run days later would
        // otherwise attach whichever report happened to be newest.
        let p = stamp_prefix("2026-08-22T09:16:06.131984Z").unwrap();
        assert_eq!(p, "cargo-timing-20260822T0916");
        assert!("cargo-timing-20260822T0916 07Z-62713dc.html"
            .replace(' ', "")
            .starts_with(&p));
        assert!(!"cargo-timing-20260822T1042Z-62713dc.html".starts_with(&p));
        assert!(stamp_prefix("not a timestamp").is_none());
    }

    #[test]
    fn missing_block_is_absent_not_an_error() {
        assert!(extract("<script>const OTHER = [1];</script>", "UNIT_DATA").is_none());
    }
}
