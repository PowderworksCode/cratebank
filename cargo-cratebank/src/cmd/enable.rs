//! `cargo cratebank enable` — wire a project up for automatic sending.

use crate::cli::Common;
use crate::project::{opted_in, workspace_root};

const BUILD_RS_SNIPPET: &str = r#"fn main() {
    // cratebank: ship this build's timings once it finishes (opt-in via
    // [package.metadata.cratebank] share = true). No-op if not installed.
    //
    // Deliberately no `cargo:rerun-if-changed` line: the default is to rerun
    // whenever any file in this package changes, which is exactly "this
    // package was rebuilt" -- the moment worth reporting.
    let _ = std::process::Command::new("cargo-cratebank")
        .args(["cratebank", "autosend", "--detach"])
        .status();
}
"#;

/// The two unstable flags must be enabled at the WORKSPACE root, or a build
/// started from the root never sees them.
pub(crate) fn write_cargo_config(root: &std::path::Path, o: &Common, changed: &mut Vec<&'static str>) {
    let cfg = root.join(".cargo").join("config.toml");
    let want = "[unstable]\nbuild-analysis = true\nsection-timings = true\n\n[build.analysis]\nenabled = true\n";
    let have = std::fs::read_to_string(&cfg).unwrap_or_default();
    if have.contains("build-analysis") { return; }
    if o.dry_run {
        println!("--- {} ---\n{want}", cfg.display());
    } else {
        let _ = std::fs::create_dir_all(cfg.parent().unwrap());
        let _ = std::fs::write(&cfg, format!("{}{}{want}", have.trim_end(),
                                             if have.trim().is_empty() { "" } else { "\n\n" }));
    }
    changed.push(".cargo/config.toml (workspace root): build-analysis + section-timings");
}

pub fn run(o: &Common) -> i32 {
    let dir = std::env::current_dir().unwrap_or_default();
    let manifest = dir.join("Cargo.toml");
    let Ok(txt) = std::fs::read_to_string(&manifest) else {
        eprintln!("cratebank: no Cargo.toml here"); return 1;
    };
    let mut changed = vec![];
    // A virtual workspace manifest has no [package]; writing package.metadata
    // there yields "missing field `package.name`" and breaks the manifest.
    let parsed = txt.parse::<toml::Value>().ok();
    let is_package = parsed.as_ref().map(|v| v.get("package").is_some()).unwrap_or(false);
    let is_virtual_ws = !is_package
        && parsed.as_ref().map(|v| v.get("workspace").is_some()).unwrap_or(false);

    if !opted_in(&dir) {
        let table = if is_package { "package" } else { "workspace" };
        let add = format!("\n[{table}.metadata.cratebank]\nshare = true\n");
        if o.dry_run { println!("--- append to Cargo.toml ---{add}"); }
        else { std::fs::write(&manifest, format!("{}{add}", txt.trim_end())).ok(); }
        changed.push(if is_package { "Cargo.toml: [package.metadata.cratebank] share = true" }
                     else { "Cargo.toml: [workspace.metadata.cratebank] share = true" });
    }

    write_cargo_config(&workspace_root(&dir), o, &mut changed);

    if is_virtual_ws {
        // The opt-in is inherited by members (opted_in walks up), but the
        // trigger must live in a package that actually builds.
        for c in &changed { println!("  + {c}"); }
        println!("\nThis is a virtual workspace: the opt-in now covers all members.");
        println!("Run `cargo cratebank enable` inside one member to add the build.rs trigger,");
        println!("or use `cargo cratebank build` / a CI step instead.");
        return 0;
    }

    let build_rs = dir.join("build.rs");
    if !build_rs.exists() {
        if o.dry_run { println!("--- build.rs ---
{BUILD_RS_SNIPPET}"); }
        else { std::fs::write(&build_rs, BUILD_RS_SNIPPET).ok(); }
        changed.push("build.rs: created (spawns autosend --detach)");
    } else if !std::fs::read_to_string(&build_rs).unwrap_or_default().contains("cargo-cratebank") {
        println!("build.rs already exists — add these three lines to its main():
");
        println!("    let _ = std::process::Command::new(\"cargo-cratebank\")");
        println!("        .args([\"cratebank\", \"autosend\", \"--detach\"])");
        println!("        .status();\n");
    }

    if o.dry_run { eprintln!("\ncratebank: dry run, nothing written"); return 0; }
    if changed.is_empty() { println!("cratebank: already enabled here."); }
    else { for c in &changed { println!("  + {c}"); } }

    println!("\nEvery `cargo build` on a nightly toolchain will now ship its session log to\n\
              {}\nafter the build finishes. Disable any time with  share = false.", o.endpoint);
    explain_machine_id(&dir);
    0
}

/// Enablement is the moment to say what identifies these builds — before any
/// data moves, not after somebody finds an id in a payload.
fn explain_machine_id(dir: &std::path::Path) {
    println!("\n  Who these builds are from");
    println!("  ------------------------");
    match crate::machine::machine_id_with(Some(dir), false) {
        None => {
            println!("  No machine id is being sent (machine_id is set to `none`).");
            println!("  Your builds will be counted, but not attributed to anyone.");
        }
        Some(id) => {
            let chosen = std::env::var("CRATEBANK_MACHINE_ID").is_ok()
                || crate::machine::configured_id(dir).is_some();
            if chosen {
                println!("  Your builds will be attributed to  {id}");
            } else {
                println!("  A random machine id was generated for you:  {id}");
                println!("  It links your own sessions together — which is what allows");
                println!("  comparisons like \"did this crate get slower on the same machine?\"");
                println!();
                println!("  If you would rather take credit for the work, name yourself:");
                println!();
                println!("      [package.metadata.cratebank]   # or [workspace.metadata…]");
                println!("      machine_id = \"your-org\"");
                println!();
                println!("  On CI, set CRATEBANK_MACHINE_ID instead — a fresh runner would");
                println!("  otherwise invent a new random id on every job.");
            }
            println!("  Send no id at all with  machine_id = \"none\".");
        }
    }
}

