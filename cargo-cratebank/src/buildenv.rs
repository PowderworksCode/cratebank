//! Build configuration that cargo's session log does not record: `RUSTFLAGS`,
//! the linker, and any compiler wrapper.
//!
//! These matter — `-C target-cpu`, `-C lto`, `-Zthreads`, lld vs mold vs the
//! default linker all move compile time, and a census that cannot see them
//! cannot answer its own headline questions. They also live in environment
//! variables and config files that can contain local paths, so this module
//! reads a **whitelist**, never the environment as a whole, and classifies
//! every value before it is allowed out:
//!
//! | shape | treatment |
//! | --- | --- |
//! | `-C opt-level=2`, `-Zthreads=8`, `-C target-cpu=native` | kept verbatim |
//! | `-C linker=/usr/bin/clang` | basename only → `clang` |
//! | any value containing a path separator | value replaced with `<path>` |
//! | `--remap-path-prefix=…` | dropped entirely (paths on both sides) |
//! | unrecognised flag with a value | name kept, value replaced |
//!
//! The rule of thumb: flag *names* and non-path values are build configuration
//! and are the point of collecting this; anything path-shaped is somebody's
//! filesystem and never leaves the machine.
use std::process::Command;

use serde_json::{json, Map, Value};

/// Environment variables we are willing to look at, by exact name.
const ENV_ALLOW: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
];

fn has_path(v: &str) -> bool { v.contains('/') || v.contains('\\') }

fn basename(v: &str) -> &str {
    v.rsplit(['/', '\\']).next().unwrap_or(v)
}

/// A program identity is its basename: `sccache`, `mold`, `clang`. The
/// directory it happens to live in is not build configuration.
pub fn program_name(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() { return None; }
    Some(basename(v).to_string())
}

/// Classify one rustc flag. `None` means "never send this".
pub fn sanitize_flag(tok: &str) -> Option<String> {
    let tok = tok.trim();
    if tok.is_empty() { return None; }
    if tok.starts_with("--remap-path-prefix") { return None; }
    let Some((name, value)) = tok.split_once('=') else {
        // no value: either a bare flag (-O, --verbose) or a path-shaped stray
        return if has_path(tok) { None } else { Some(tok.to_string()) };
    };
    if !has_path(value) {
        return Some(tok.to_string());
    }
    if name.ends_with("linker") {
        return Some(format!("{name}={}", basename(value)));
    }
    Some(format!("{name}=<path>"))
}

/// Flags whose value is the *next* token when written with a space.
const TAKES_VALUE: &[&str] = &["-C", "-Z", "-L", "-l", "--cfg", "--extern", "--edition"];

fn split_flags(raw: &str, encoded: bool) -> Vec<String> {
    let toks: Vec<String> = if encoded {
        raw.split('\u{1f}').map(str::to_string).filter(|s| !s.is_empty()).collect()
    } else {
        raw.split_whitespace().map(str::to_string).collect()
    };
    // rejoin `-C opt-level=2` into one flag so it can be classified as a unit
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut it = toks.into_iter().peekable();
    while let Some(t) = it.next() {
        if TAKES_VALUE.contains(&t.as_str()) {
            if let Some(v) = it.next() {
                out.push(format!("{t} {v}"));
                continue;
            }
        }
        out.push(t);
    }
    out
}

/// Whitelisted keys from cargo's *resolved* config — better than reading
/// config files ourselves, because cargo has already merged every layer.
fn cargo_config(dir: &std::path::Path) -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(o) = Command::new("cargo")
        .args(["config", "get", "-Zunstable-options"])
        .current_dir(dir)
        .output()
    else {
        return out;
    };
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    if stdout.trim().is_empty() {
        return config_from_files(dir);
    }
    for line in stdout.lines() {
        let Some((k, v)) = line.split_once(" = ") else { continue };
        let (k, v) = (k.trim(), v.trim().trim_matches('"'));
        let keep = match k {
            "build.incremental" | "build.target" | "build.jobs" => Some(v.to_string()),
            "build.rustc-wrapper" | "build.rustc-workspace-wrapper" => program_name(v),
            _ if k.starts_with("target.") && k.ends_with(".linker") => program_name(v),
            _ if k == "build.rustflags" || (k.starts_with("target.") && k.ends_with(".rustflags")) => {
                let flags: Vec<Value> = split_flags(v.trim_matches(['[', ']']).replace([',', '"'], " ").as_str(), false)
                    .iter().filter_map(|f| sanitize_flag(f)).map(Value::from).collect();
                out.insert(k.to_string(), Value::Array(flags));
                continue;
            }
            _ => None,
        };
        if let Some(val) = keep {
            out.insert(k.to_string(), Value::from(val));
        }
    }
    out
}

/// `cargo config get` is nightly-only and the client may be run under stable,
/// so fall back to the same whitelist applied to the config files directly:
/// the workspace's `.cargo/config.toml` and `$CARGO_HOME/config.toml`.
fn config_from_files(dir: &std::path::Path) -> Map<String, Value> {
    let mut out = Map::new();
    let files = [dir.join(".cargo").join("config.toml"),
                 crate::cli::cargo_home().join("config.toml")];
    for f in files.iter().rev() {   // workspace overrides home
        let Ok(txt) = std::fs::read_to_string(f) else { continue };
        let Ok(v) = txt.parse::<toml::Value>() else { continue };
        let Some(build) = v.get("build") else { continue };
        for key in ["incremental", "target", "jobs"] {
            if let Some(x) = build.get(key) {
                out.insert(format!("build.{key}"), Value::from(x.to_string().trim_matches('"')));
            }
        }
        for key in ["rustc-wrapper", "rustc-workspace-wrapper"] {
            if let Some(n) = build.get(key).and_then(|x| x.as_str()).and_then(program_name) {
                out.insert(format!("build.{key}"), Value::from(n));
            }
        }
        if let Some(fl) = build.get("rustflags").and_then(|x| x.as_array()) {
            let flags: Vec<Value> = fl.iter().filter_map(|x| x.as_str())
                .filter_map(sanitize_flag).map(Value::from).collect();
            out.insert("build.rustflags".into(), Value::Array(flags));
        }
    }
    out
}

/// The build-configuration block attached to every session.
pub fn snapshot(dir: &std::path::Path) -> Value {
    let mut env = Map::new();
    for key in ENV_ALLOW {
        let Ok(raw) = std::env::var(key) else { continue };
        let value = match *key {
            "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" => {
                let flags: Vec<Value> = split_flags(&raw, *key == "CARGO_ENCODED_RUSTFLAGS")
                    .iter().filter_map(|f| sanitize_flag(f)).map(Value::from).collect();
                Value::Array(flags)
            }
            "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER" => {
                match program_name(&raw) { Some(n) => Value::from(n), None => continue }
            }
            _ => Value::from(raw),
        };
        env.insert(key.to_string(), value);
    }
    json!({ "env": env, "config": cargo_config(dir) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_configuration_drops_paths() {
        assert_eq!(sanitize_flag("-Copt-level=2").as_deref(), Some("-Copt-level=2"));
        assert_eq!(sanitize_flag("-C target-cpu=native").as_deref(), Some("-C target-cpu=native"));
        assert_eq!(sanitize_flag("-Zthreads=8").as_deref(), Some("-Zthreads=8"));
        // linker identity survives, its directory does not
        assert_eq!(sanitize_flag("-Clinker=/usr/bin/clang").as_deref(), Some("-Clinker=clang"));
        // a link arg naming the linker is configuration, not a path
        assert_eq!(sanitize_flag("-Clink-arg=-fuse-ld=mold").as_deref(), Some("-Clink-arg=-fuse-ld=mold"));
        // anything path-shaped loses its value
        assert_eq!(sanitize_flag("-Clink-arg=/home/me/lib.a").as_deref(), Some("-Clink-arg=<path>"));
        assert_eq!(sanitize_flag("-L/home/me/target").as_deref(), None);
        assert_eq!(sanitize_flag("--remap-path-prefix=/home/me=/x").as_deref(), None);
    }

    #[test]
    fn rejoins_spaced_flags() {
        let f = split_flags("-C target-cpu=native -Zthreads=8 -C lto=thin", false);
        assert_eq!(f, vec!["-C target-cpu=native", "-Zthreads=8", "-C lto=thin"]);
        // and the rejoined form still classifies correctly
        assert_eq!(sanitize_flag("-C linker=/usr/bin/clang").as_deref(), Some("-C linker=clang"));
    }

    #[test]
    fn program_identity_is_the_basename() {
        assert_eq!(program_name("/usr/local/bin/sccache").as_deref(), Some("sccache"));
        assert_eq!(program_name("").as_deref(), None);
    }
}
