//! Stable Cargo `--timings` capture.
//!
//! Cargo writes one timestamped HTML report per build. Its script contains
//! structured `UNIT_DATA`, `CONCURRENCY_DATA`, and `CPU_USAGE` values. This
//! module finds the report created by the sampled build, parses those values,
//! and removes non-public packages before anything leaves the machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

const BLOCKS: [(&str, &str); 3] = [
    ("UNIT_DATA", "unit_data"),
    ("CONCURRENCY_DATA", "concurrency_data"),
    ("CPU_USAGE", "cpu_usage"),
];

/// Stable Cargo metadata needed to locate output and enforce privacy.
pub struct Project {
    pub workspace_root: PathBuf,
    pub target_dir: PathBuf,
    pub repository: Option<String>,
    unit_public: BTreeMap<(String, String), bool>,
    phase_package_public: BTreeMap<String, bool>,
    phase_target_public: BTreeMap<String, bool>,
    phase_checkout_public: BTreeMap<String, bool>,
}

impl Project {
    pub fn phase_is_public(
        &self,
        crate_name: &str,
        package: &str,
        source_path: Option<&Path>,
    ) -> bool {
        if let Some(public) = self.phase_package_public.get(package) {
            return *public;
        }
        let Some(path) = source_path else {
            return false;
        };
        if path.is_absolute() && path.starts_with(&self.workspace_root) {
            return self.phase_target_public.get(crate_name) == Some(&true);
        }
        let text = path.to_string_lossy().replace('\\', "/");
        self.phase_checkout_public
            .iter()
            .any(|(repository, public)| {
                *public && text.contains(&format!("/git/checkouts/{repository}-"))
            })
    }

    fn unit_is_public(&self, name: &str, version: &str) -> bool {
        self.unit_public
            .get(&(name.to_string(), version.to_string()))
            == Some(&true)
    }
}

pub struct Capture {
    pub run_id: String,
    pub env: Value,
    pub timings: Value,
    pub withheld: usize,
}

fn metadata_args(build_args: &[String]) -> Vec<String> {
    let mut out = vec![
        "metadata".into(),
        "--format-version=1".into(),
        "--no-deps".into(),
    ];
    let mut i = 0;
    while i < build_args.len() {
        let arg = &build_args[i];
        if matches!(arg.as_str(), "--locked" | "--offline" | "--frozen") {
            out.push(arg.clone());
        } else if arg == "--manifest-path" {
            if let Some(value) = build_args.get(i + 1) {
                out.push(arg.clone());
                out.push(value.clone());
                i += 1;
            }
        } else if arg.starts_with("--manifest-path=") {
            out.push(arg.clone());
        }
        i += 1;
    }
    out
}

fn target_dir_arg(build_args: &[String]) -> Option<PathBuf> {
    let mut iter = build_args.iter();
    while let Some(arg) = iter.next() {
        let value = if arg == "--target-dir" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--target-dir=")
        };
        if let Some(value) = value {
            let path = PathBuf::from(value);
            return Some(if path.is_absolute() {
                path
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            });
        }
    }
    None
}

fn manifest_settings(root: &Path) -> (bool, Option<String>) {
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return (false, None);
    };
    let Ok(doc) = text.parse::<toml::Value>() else {
        return (false, None);
    };
    for table in ["package", "workspace"] {
        let Some(base) = doc.get(table) else { continue };
        let cratebank = base.get("metadata").and_then(|v| v.get("cratebank"));
        if cratebank
            .and_then(|v| v.get("public"))
            .and_then(toml::Value::as_bool)
            != Some(true)
        {
            continue;
        }
        let repository = cratebank
            .and_then(|v| v.get("repository"))
            .and_then(toml::Value::as_str)
            .or_else(|| base.get("repository").and_then(toml::Value::as_str))
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        return (true, repository);
    }
    (false, None)
}

fn source_is_public(source: Option<&str>) -> bool {
    source.is_some_and(|source| {
        source.starts_with("registry+https://github.com/rust-lang/crates.io-index")
            || source.starts_with("sparse+https://index.crates.io")
            || (source.starts_with("git+http") && !source.contains('@'))
    })
}

fn git_repository_name(source: &str) -> Option<String> {
    let url = source.strip_prefix("git+")?.split(['?', '#']).next()?;
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .map(|name| name.trim_end_matches(".git").to_string())
        .filter(|name| !name.is_empty())
}

fn merge_status<K: Ord>(map: &mut BTreeMap<K, bool>, key: K, public: bool) {
    map.entry(key)
        .and_modify(|current| *current &= public)
        .or_insert(public);
}

/// Resolve the workspace, target directory, and package visibility with stable
/// `cargo metadata`.
pub fn project(build_args: &[String]) -> Result<Project, String> {
    let output = Command::new("cargo")
        .args(metadata_args(build_args))
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot parse cargo metadata: {e}"))?;
    let workspace_root = metadata["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .ok_or("cargo metadata did not report a workspace root")?;
    let target_dir = target_dir_arg(build_args)
        .or_else(|| metadata["target_directory"].as_str().map(PathBuf::from))
        .ok_or("cargo metadata did not report a target directory")?;
    let workspace_members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let (workspace_public, mut repository) = manifest_settings(&workspace_root);

    let mut unit_public = BTreeMap::new();
    let mut phase_package_public = BTreeMap::new();
    let mut phase_target_public = BTreeMap::new();
    let mut phase_checkout_public = BTreeMap::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let Some(name) = package["name"].as_str() else {
            continue;
        };
        let Some(version) = package["version"].as_str() else {
            continue;
        };
        let source = package["source"].as_str();
        let package_public = package["metadata"]["cratebank"]["public"].as_bool() == Some(true);
        let workspace_member = package["id"]
            .as_str()
            .is_some_and(|id| workspace_members.contains(id));
        let public =
            source_is_public(source) || (workspace_member && (workspace_public || package_public));
        if public && workspace_member && repository.is_none() {
            repository = package["repository"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        merge_status(
            &mut unit_public,
            (name.to_string(), version.to_string()),
            public,
        );
        merge_status(
            &mut phase_package_public,
            format!("{name}-{version}"),
            public,
        );
        for target in package["targets"].as_array().into_iter().flatten() {
            if let Some(target_name) = target["name"].as_str() {
                merge_status(
                    &mut phase_target_public,
                    target_name.replace('-', "_"),
                    public,
                );
            }
        }
    }

    // Cargo.lock supplies dependency provenance without asking Cargo to fetch
    // packages for platforms this machine is not building. Source-less lock
    // entries are path packages and remain private; workspace members were
    // classified above from metadata.
    if let Ok(text) = std::fs::read_to_string(workspace_root.join("Cargo.lock")) {
        if let Ok(lock) = text.parse::<toml::Value>() {
            for package in lock
                .get("package")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = package.get("name").and_then(toml::Value::as_str) else {
                    continue;
                };
                let Some(version) = package.get("version").and_then(toml::Value::as_str) else {
                    continue;
                };
                let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
                    continue;
                };
                let public = source_is_public(Some(source));
                merge_status(
                    &mut unit_public,
                    (name.to_string(), version.to_string()),
                    public,
                );
                merge_status(
                    &mut phase_package_public,
                    format!("{name}-{version}"),
                    public,
                );
                if let Some(repository) = git_repository_name(source) {
                    merge_status(&mut phase_checkout_public, repository, public);
                }
            }
        }
    }

    Ok(Project {
        workspace_root,
        target_dir,
        repository,
        unit_public,
        phase_package_public,
        phase_target_public,
        phase_checkout_public,
    })
}

pub fn reports(target_dir: &Path) -> BTreeSet<PathBuf> {
    let dir = target_dir.join("cargo-timings");
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("cargo-timing-") && name.ends_with(".html")
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn new_report(target_dir: &Path, before: &BTreeSet<PathBuf>) -> Result<PathBuf, String> {
    reports(target_dir)
        .into_iter()
        .filter(|path| !before.contains(path))
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
        .ok_or_else(|| {
            format!(
                "cargo produced no timing report in {}",
                target_dir.join("cargo-timings").display()
            )
        })
}

fn extract_json(html: &str, name: &str) -> Result<Value, String> {
    let needle = format!("const {name} = ");
    let start = html
        .find(&needle)
        .map(|at| at + needle.len())
        .ok_or_else(|| format!("timing report has no {name}"))?;
    let bytes = html.as_bytes();
    let open = *bytes
        .get(start)
        .ok_or_else(|| format!("timing report has an empty {name}"))?;
    let close = match open {
        b'[' => b']',
        b'{' => b'}',
        _ => return Err(format!("timing report has malformed {name}")),
    };
    let (mut depth, mut in_string, mut escaped) = (0i32, false, false);
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&html[start..start + offset + 1])
                        .map_err(|e| format!("cannot parse {name}: {e}"));
                }
            }
            _ => {}
        }
    }
    Err(format!("timing report has unterminated {name}"))
}

fn summary_cell<'a>(html: &'a str, label: &str) -> Option<&'a str> {
    let needle = format!("<td>{label}:</td><td>");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find("</td>")? + start;
    Some(&html[start..end])
}

fn summary(html: &str) -> Value {
    let rustc = summary_cell(html, "rustc").unwrap_or_default();
    let rustc_version = rustc.split("<br>").next().unwrap_or_default();
    let host = rustc
        .split("<br>")
        .find_map(|line| line.strip_prefix("Host: "));
    let concurrency = summary_cell(html, "Max concurrency").unwrap_or_default();
    let jobs = concurrency
        .split("jobs=")
        .nth(1)
        .and_then(|rest| rest.split([' ', ')']).next())
        .and_then(|value| value.parse::<u64>().ok());
    json!({
        "timestamp": summary_cell(html, "Build start"),
        "profile": summary_cell(html, "Profile"),
        "rustc_version": if rustc_version.is_empty() { None } else { Some(rustc_version) },
        "host": host,
        "jobs": jobs,
        "ci": std::env::var("CI").is_ok(),
    })
}

fn prune_units(units: Value, project: &Project) -> Result<(Value, usize), String> {
    let input = units.as_array().ok_or("UNIT_DATA is not an array")?;
    let keep: BTreeSet<i64> = input
        .iter()
        .filter(|unit| {
            project.unit_is_public(
                unit["name"].as_str().unwrap_or_default(),
                unit["version"].as_str().unwrap_or_default(),
            )
        })
        .filter_map(|unit| unit["i"].as_i64())
        .collect();
    let mut output = Vec::with_capacity(keep.len());
    for unit in input {
        let Some(index) = unit["i"].as_i64() else {
            continue;
        };
        if !keep.contains(&index) {
            continue;
        }
        let mut unit = unit.clone();
        for field in ["unblocked_units", "unblocked_rmeta_units"] {
            if let Some(edges) = unit[field].as_array() {
                unit[field] = Value::Array(
                    edges
                        .iter()
                        .filter(|edge| edge.as_i64().is_some_and(|i| keep.contains(&i)))
                        .cloned()
                        .collect(),
                );
            }
        }
        output.push(unit);
    }
    Ok((Value::Array(output), input.len().saturating_sub(keep.len())))
}

pub fn capture(path: &Path, project: &Project) -> Result<Capture, String> {
    let html = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut data = Map::new();
    for (source, key) in BLOCKS {
        data.insert(key.to_string(), extract_json(&html, source)?);
    }
    let units = data.remove("unit_data").unwrap_or(Value::Array(vec![]));
    let (units, withheld) = prune_units(units, project)?;
    data.insert("unit_data".into(), units);
    let run_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("cargo-timing-"))
        .unwrap_or("unknown")
        .to_string();
    Ok(Capture {
        run_id,
        env: summary(&html),
        timings: Value::Object(data),
        withheld,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nested_json() {
        let html =
            r#"const UNIT_DATA = [{"name":"a]b","sections":[["frontend",{"start":0,"end":1}]]}];"#;
        let value = extract_json(html, "UNIT_DATA").unwrap();
        assert_eq!(value[0]["name"], "a]b");
        assert_eq!(value[0]["sections"][0][1]["end"], 1);
    }

    #[test]
    fn reads_summary_fields() {
        let html = "<td>Profile:</td><td>release</td>\
                    <td>Max concurrency:</td><td>13 (jobs=12 ncpu=12)</td>\
                    <td>Build start:</td><td>2026-08-22T10:06:52Z</td>\
                    <td>rustc:</td><td>rustc 1.90.0<br>Host: x86_64-unknown-linux-gnu</td>";
        let value = summary(html);
        assert_eq!(value["profile"], "release");
        assert_eq!(value["jobs"], 12);
        assert_eq!(value["rustc_version"], "rustc 1.90.0");
        assert_eq!(value["host"], "x86_64-unknown-linux-gnu");
    }

    #[test]
    fn withholds_private_units_and_prunes_edges() {
        let project = Project {
            workspace_root: PathBuf::new(),
            target_dir: PathBuf::new(),
            repository: None,
            unit_public: BTreeMap::from([
                (("serde".into(), "1.0.228".into()), true),
                (("private-app".into(), "0.1.0".into()), false),
            ]),
            phase_package_public: BTreeMap::new(),
            phase_target_public: BTreeMap::new(),
            phase_checkout_public: BTreeMap::new(),
        };
        let units = json!([
            {"i": 1, "name": "serde", "version": "1.0.228", "unblocked_units": [2, 3]},
            {"i": 2, "name": "private-app", "version": "0.1.0", "unblocked_units": []},
            {"i": 3, "name": "serde", "version": "1.0.228", "unblocked_units": []}
        ]);
        let (filtered, withheld) = prune_units(units, &project).unwrap();
        assert_eq!(withheld, 1);
        assert_eq!(filtered.as_array().unwrap().len(), 2);
        assert_eq!(filtered[0]["unblocked_units"], json!([3]));
    }
}
