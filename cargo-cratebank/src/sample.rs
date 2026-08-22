//! Per-unit compiler phase measurement, by sampling the build.
//!
//! `samply record -- cargo build` profiles the whole build; every compilation
//! unit is its own rustc process, and every sample carries a pid, so samples
//! attribute to units exactly however many run concurrently. Attributing by
//! time window instead would be hopeless on a `-j12` build.
//!
//! Nothing wraps rustc. `--include-args` puts the full rustc command line in
//! the profile's `processName`, so a unit's identity is already in the data --
//! no `RUSTC_WRAPPER`, no sidecar files, and a contributor's `sccache` keeps
//! working untouched.
//!
//! Why this rather than the alternatives, all of which were measured:
//!
//! - `-Ztime-passes` gives 57 named passes but needs nightly.
//! - `-Zself-profile` is finer still and produces 14 MB per *small* build.
//! - `RUSTC_LOG` has no usable timings: release rustc compiles out DEBUG and
//!   TRACE, so the phase spans do not exist, and it costs +72% to collect.
//! - cargo's `--timings` and artifact mtimes give frontend/codegen for free but
//!   cannot split the frontend, which is 80% of the time on some crates.
//!
//! Sampling is the only one that is stable, version-independent, and detailed.
//! Validated against nightly `-Ztime-passes` on a real crate at
//! `-Ccodegen-units=1`: every phase within ~1 point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// Phase markers, matched against demangled symbol names.
///
/// rustc's crate structure *is* its phase structure, and stable rustc ships a
/// full symbol table (233k symbols; nothing is stripped, because backtraces
/// need it). So a symbol prefix recovers the phase for free.
///
/// Two names are mapped to rustc's own vocabulary rather than the crate that
/// implements them, because that is what they mean:
///   - `type_check` is `rustc_hir_analysis`, which *encloses* `rustc_hir_typeck`
///   - `borrowck` includes the MIR pipeline it drives (drop elaboration, const
///     checking) -- `-Ztime-passes` reports those as one span
const MARKERS: &[(&str, &[&str])] = &[
    ("macro_expand", &["rustc_expand::"]),
    ("resolve", &["rustc_resolve::"]),
    ("coherence", &["rustc_hir_analysis::coherence"]),
    (
        "type_check",
        &["rustc_hir_analysis::", "rustc_hir_typeck::"],
    ),
    ("borrowck", &["rustc_borrowck::", "rustc_mir_transform::"]),
    ("monomorphize", &["rustc_monomorphize::"]),
    (
        "metadata_encode",
        &["rustc_metadata::rmeta::encoder", "encode_metadata"],
    ),
    ("codegen", &["rustc_codegen_llvm::", "rustc_codegen_ssa::"]),
];

/// What one compilation unit cost, split by phase.
#[derive(Debug, Default)]
pub struct UnitPhases {
    pub crate_name: String,
    pub crate_type: String,
    /// Package this unit belongs to. Distinguishes the many build scripts,
    /// all of which are named `build_script_build`.
    pub package: String,
    /// Resolved compilation settings: opt-level, debuginfo, codegen-units,
    /// panic, lto, edition, target, features. Scrubbed of paths.
    pub flags: BTreeMap<String, String>,
    /// Samples on the main thread: the serial part of compilation.
    pub serial: BTreeMap<String, u64>,
    /// Samples on per-codegen-unit threads. rustc codegens on a thread per
    /// CGU, so this is real parallel work -- a third of all compile CPU on a
    /// large build. Kept separate because mixing it with `serial` produces a
    /// number comparable to neither wall clock nor CPU.
    pub parallel: BTreeMap<String, u64>,
    /// Wall seconds for this unit, from the profiler's own record of when the
    /// rustc process started and exited. Free -- the profile already carries
    /// it -- and it is the denominator that makes sample counts meaningful:
    /// without it, "1200 samples" says nothing about how long anything took.
    pub wall_s: Option<f64>,
}

impl UnitPhases {
    pub fn total(&self) -> u64 {
        self.serial.values().sum::<u64>() + self.parallel.values().sum::<u64>()
    }
}

pub fn samply_available() -> bool {
    Command::new("samply")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run cargo under the sampler with the given argv (which includes the
/// subcommand). Returns the two profile paths.
///
/// The rate is deliberately high. samply costs a flat ~1s per invocation
/// regardless of rate -- process setup and profile serialisation -- and only
/// ~116us per sample, so on any build worth measuring the fixed cost dominates
/// and a low rate saves almost nothing while starving small units of samples.
pub fn record(args: &[String], out: &Path, rate: u32) -> Result<(PathBuf, PathBuf), String> {
    let prof = out.join("profile.json");
    let syms = out.join("profile.syms.json");
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;

    let mut cmd = Command::new("samply");
    cmd.arg("record")
        .arg("--save-only")
        .arg("--unstable-presymbolicate") // writes the .syms.json sidecar
        .arg("--include-args") // puts the rustc command line in processName
        .arg("-o")
        .arg(&prof)
        .arg("--rate")
        .arg(rate.to_string())
        .arg("--")
        .arg("cargo")
        .args(args);

    let st = cmd
        .status()
        .map_err(|e| format!("cannot run samply: {e}"))?;
    if !st.success() {
        return Err(format!("samply exited {}", st.code().unwrap_or(-1)));
    }
    if !prof.exists() {
        return Err("samply produced no profile".into());
    }
    Ok((prof, syms))
}

/// Identity of one compilation unit, from its rustc command line.
///
/// `crate_name` alone is not unique: every package's build script is called
/// `build_script_build`, so keying on the name merges unrelated packages into
/// one unit -- their samples add up while the wall time is whichever process
/// was seen first. cargo's `-C metadata=<hash>` is unique per unit, so it is
/// the key; `package` is carried alongside for readability.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnitId {
    pub crate_name: String,
    pub crate_type: String,
    /// cargo's per-unit hash. Unique; not human-readable.
    pub metadata: String,
    /// Package directory for registry crates (`proc-macro2-1.0.107`), else the
    /// crate name. What a human wants when the crate name is a build script.
    pub package: String,
    /// Resolved compilation settings, scrubbed of paths.
    pub flags: BTreeMap<String, String>,
}

/// Unit identity from a rustc command line, or None if this process is not a
/// compilation.
///
/// cargo probes the compiler with `--print` invocations before building
/// anything; those are not units and must not be counted.
fn unit_of(process_name: &str) -> Option<UnitId> {
    if !process_name.starts_with("rustc") {
        return None;
    }
    let argv = split_args(process_name);
    let mut name = None;
    let mut kind = None;
    let mut metadata = String::new();
    let mut src = None;
    let mut it = argv.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("metadata=") {
            metadata = v.to_string();
        }
        if a.ends_with(".rs") && src.is_none() {
            src = Some(a.clone());
        }
        if let Some(v) = a.strip_prefix("--crate-name=") {
            name = Some(v.to_string());
        } else if a == "--crate-name" {
            name = it.peek().map(|s| s.to_string());
        } else if let Some(v) = a.strip_prefix("--crate-type=") {
            kind.get_or_insert(v.to_string());
        } else if a == "--crate-type" {
            if let Some(v) = it.peek() {
                kind.get_or_insert(v.to_string());
            }
        }
    }
    let name = name?;
    if name == "___" || argv.iter().any(|a| a.starts_with("--print")) {
        return None;
    }
    // `/…/registry/src/index.crates.io-…/proc-macro2-1.0.107/{build.rs,src/lib.rs}`
    // -> `proc-macro2-1.0.107`: the component just after the registry index
    // directory. Taking the file's parent instead yields `src` for every
    // library, which then merges every crate in the graph into one bogus unit.
    // Workspace paths have no registry component, so fall back to the crate
    // name rather than inventing one.
    let package = src
        .as_deref()
        .and_then(|p| {
            let rest = p.split("/registry/src/").nth(1)?;
            let mut parts = rest.split('/');
            parts.next()?; // the index directory
            parts.next().map(str::to_string)
        })
        .unwrap_or_else(|| name.clone());
    Some(UnitId {
        crate_name: name,
        crate_type: kind.unwrap_or_else(|| "lib".into()),
        metadata,
        package,
        flags: build_flags(&argv),
    })
}

/// The compilation settings from a rustc command line, scrubbed of paths.
///
/// These decide what the numbers mean. An opt-level 0 unit and an opt-level 3
/// unit are not the same specimen, and a census that cannot tell them apart is
/// comparing unlike things -- so this is section A of the capture manifest,
/// "resolved profile", and it was sitting unread in a profile we already
/// collect.
///
/// Values that are paths are deliberately dropped rather than recorded. The
/// command line is full of them -- `--out-dir`, `-L dependency=`, `--extern
/// x=/…`, the source file -- and every one names the builder's machine. What
/// survives is the *shape* of the compilation, never where it happened.
/// `incremental` is the awkward case: its value is a path, but whether it was
/// on changes every timing in the session, so it is reduced to a boolean.
fn build_flags(argv: &[String]) -> BTreeMap<String, String> {
    const KEEP_C: [&str; 11] = [
        "opt-level",
        "debuginfo",
        "codegen-units",
        "panic",
        "overflow-checks",
        "lto",
        "embed-bitcode",
        "split-debuginfo",
        "target-cpu",
        "target-feature",
        "strip",
    ];
    let mut out = BTreeMap::new();
    let mut features: Vec<String> = Vec::new();
    let mut it = argv.iter().peekable();
    while let Some(a) = it.next() {
        // `-C key=value`, which samply may render as one token or two
        let c = a
            .strip_prefix("-C")
            .map(|r| r.trim_start().to_string())
            .filter(|r| !r.is_empty())
            .or_else(|| (a == "-C").then(|| it.peek().map(|s| s.to_string()))?);
        if let Some(kv) = c {
            if let Some((k, v)) = kv.split_once('=') {
                if KEEP_C.contains(&k) {
                    out.insert(k.to_string(), v.to_string());
                } else if k == "incremental" {
                    out.insert("incremental".into(), "true".into());
                }
            }
            continue;
        }
        if let Some(v) = a.strip_prefix("--edition=") {
            out.insert("edition".into(), v.to_string());
        } else if a == "--target" {
            if let Some(v) = it.peek() {
                out.insert("target".into(), v.to_string());
            }
        } else if let Some(v) = a.strip_prefix("--target=") {
            out.insert("target".into(), v.to_string());
        } else if let Some(v) = a.strip_prefix("feature=") {
            // from `--cfg feature="serde"`; quotes already stripped by the
            // splitter, which is why this matches the bare form
            features.push(v.trim_matches('"').to_string());
        }
    }
    if !features.is_empty() {
        features.sort();
        features.dedup();
        out.insert("features".into(), features.join(","));
    }
    out
}

/// Shell-ish split honouring the single quotes samply writes around arguments.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '\'' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Leaf frames that mean "this thread is parked", not "this thread is working".
///
/// A thread waiting on a condition variable burns no CPU, so counting its
/// samples as phase work inflates whatever bucket they land in. On one build
/// 448 samples across `coordinator`, `ctrl-c` and unnamed threads were ~99%
/// parked in these -- 1.2% of the total, and worse the more parallel the
/// build. Reported as its own bucket rather than dropped: "this unit spent 40%
/// of its wall clock waiting" is real information, it just is not compile time.
const BLOCKING: &[&str] = &[
    "semaphore_wait_trap",
    "__psynch_cvwait",
    "__ulock_wait",
    "kevent",
    "__wait4",
    "mach_msg2_trap",
    "poll",
    "epoll_wait",
    "futex_wait",
];

fn is_blocked(sym: &str) -> bool {
    BLOCKING.iter().any(|b| sym == *b || sym.ends_with(b))
}

fn phase_of(sym: &str) -> Option<&'static str> {
    MARKERS
        .iter()
        .find(|(_, pats)| pats.iter().any(|p| sym.contains(p)))
        .map(|(label, _)| *label)
}

/// Per-library symbol table, keyed by the id the profile uses.
struct Symbols(std::collections::HashMap<String, (Vec<u64>, Vec<String>)>);

impl Symbols {
    fn load(path: &Path) -> Result<Self, String> {
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        )
        .map_err(|e| e.to_string())?;
        let strings: Vec<String> = v["string_table"]
            .as_array()
            .ok_or("no string_table")?
            .iter()
            .map(|s| s.as_str().unwrap_or_default().to_string())
            .collect();
        let mut map = std::collections::HashMap::new();
        for lib in v["data"].as_array().into_iter().flatten() {
            let mut rvas = Vec::new();
            let mut names = Vec::new();
            for e in lib["symbol_table"].as_array().into_iter().flatten() {
                let (Some(rva), Some(i)) = (e["rva"].as_u64(), e["symbol"].as_u64()) else {
                    continue;
                };
                rvas.push(rva);
                names.push(strings.get(i as usize).cloned().unwrap_or_default());
            }
            // The same library is spelled three ways: `code_id` matches the
            // profile's `codeId` exactly, `debug_id` is dashed-lowercase, and
            // the profile's `breakpadId` is uppercase-undashed with a trailing
            // age digit. Keying on the wrong one resolves nothing at all, with
            // no error -- it just looks like sampling does not work.
            if let Some(id) = lib["code_id"].as_str() {
                map.insert(id.to_string(), (rvas.clone(), names.clone()));
            }
            if let Some(id) = lib["debug_id"].as_str() {
                map.insert(id.replace('-', "").to_uppercase(), (rvas, names));
            }
        }
        Ok(Symbols(map))
    }

    fn resolve(&self, id: &str, addr: u64) -> Option<&str> {
        let (rvas, names) = self.0.get(id)?;
        let i = rvas.partition_point(|r| *r <= addr).checked_sub(1)?;
        names.get(i).map(String::as_str)
    }
}

/// Attribute every sample to a unit and a phase.
pub fn attribute(prof: &Path, syms: &Path) -> Result<Vec<UnitPhases>, String> {
    let symbols = Symbols::load(syms)?;
    let p: Value = serde_json::from_str(
        &std::fs::read_to_string(prof).map_err(|e| format!("{}: {e}", prof.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let libs = p["libs"].as_array().cloned().unwrap_or_default();

    let mut units: BTreeMap<UnitId, UnitPhases> = BTreeMap::new();
    for t in p["threads"].as_array().into_iter().flatten() {
        let Some(unit) = t["processName"].as_str().and_then(unit_of) else {
            continue;
        };
        // rustc names its codegen threads `opt cgu.NN`, and the thread that
        // hands them work `coordinator`. The coordinator is codegen
        // infrastructure, not frontend work; classifying it as serial put
        // codegen's scheduling cost into the frontend's column.
        //
        // Note this is by thread *name*, never `isMainThread`. rustc parks its
        // process main thread and does the work on a spawned thread called
        // `rustc`, so a main-thread test would attribute a whole compile to
        // roughly a dozen samples.
        let is_cgu = t["name"]
            .as_str()
            .map(|n| n.starts_with("opt cgu") || n.starts_with("codegen cgu") || n == "coordinator")
            .unwrap_or(false);

        let ft = &t["frameTable"];
        let fn_ = &t["funcTable"];
        let rt = &t["resourceTable"];
        let st = &t["stackTable"];
        let (addrs, funcs) = (&ft["address"], &ft["func"]);
        let (frames, prefixes) = (&st["frame"], &st["prefix"]);

        let e = units.entry(unit.clone()).or_insert_with(|| UnitPhases {
            crate_name: unit.crate_name.clone(),
            crate_type: unit.crate_type.clone(),
            package: unit.package.clone(),
            flags: unit.flags.clone(),
            ..Default::default()
        });

        // Process start/exit are per-process, repeated on each of its threads;
        // take them once. Milliseconds in the profile.
        if e.wall_s.is_none() {
            if let (Some(a), Some(b)) = (
                t["processStartupTime"].as_f64(),
                t["processShutdownTime"].as_f64(),
            ) {
                if b > a {
                    e.wall_s = Some(((b - a) / 1000.0 * 1e6).round() / 1e6);
                }
            }
        }

        for s in t["samples"]["stack"].as_array().into_iter().flatten() {
            let Some(mut node) = s.as_u64() else { continue };
            // Walk leaf -> root collecting names, then take the OUTERMOST
            // phase marker: that is the enclosing phase, which is what
            // `-Ztime-passes` reports. The innermost marker answers a
            // different question (which subsystem the CPU was in) and is a
            // deliberate choice left to the reader, not baked in here.
            let mut chain: Vec<&str> = Vec::new();
            loop {
                let fr = frames[node as usize].as_u64().unwrap_or(0) as usize;
                if let (Some(addr), Some(func)) =
                    (addrs[fr].as_u64(), funcs[fr].as_u64().map(|x| x as usize))
                {
                    if let Some(res) = fn_["resource"][func].as_u64() {
                        if let Some(li) = rt["lib"][res as usize].as_u64() {
                            let lib = &libs[li as usize];
                            let id = lib["codeId"].as_str().unwrap_or_default();
                            if let Some(n) = symbols.resolve(id, addr) {
                                chain.push(n);
                            }
                        }
                    }
                }
                match prefixes[node as usize].as_u64() {
                    Some(p) => node = p,
                    None => break,
                }
            }
            // `chain` is leaf-first. A leaf in a blocking syscall means the
            // thread was parked, so this sample is not compile work at all --
            // decided before any phase marker is consulted, because the
            // enclosing frames still look like whatever phase was waiting.
            let hit = if chain.first().is_some_and(|n| is_blocked(n)) {
                "blocked"
            } else {
                chain
                    .iter()
                    .rev()
                    .find_map(|n| phase_of(n))
                    .unwrap_or("unattributed")
            };
            let bucket = if is_cgu {
                &mut e.parallel
            } else {
                &mut e.serial
            };
            *bucket.entry(hit.to_string()).or_insert(0) += 1;
        }
    }
    let mut v: Vec<_> = units.into_values().filter(|u| u.total() > 0).collect();
    v.sort_by_key(|u| std::cmp::Reverse(u.total()));
    Ok(v)
}

/// Shape for the payload. Counts, not stacks: the payload stays proportional
/// to distinct phases rather than to samples.
pub fn to_json(units: &[UnitPhases], rate: u32) -> Value {
    json!({
        "sampler": "samply",
        "rate_hz": rate,
        "units": units.iter().map(|u| json!({
            "crate": u.crate_name,
            "package": u.package,
            "crate_type": u.crate_type,
            "flags": u.flags,
            "wall_s": u.wall_s,
            "serial": u.serial,
            "parallel": u.parallel,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_rustc_command_line() {
        let pn = "rustc --crate-name bun_css '--edition=2024' src/css/lib.rs --crate-type lib '--emit=dep-info,metadata,link'";
        let u = unit_of(pn).expect("quoted args must not break the split");
        assert_eq!(u.crate_name, "bun_css");
        assert_eq!(u.crate_type, "lib");
    }

    #[test]
    fn ignores_cargos_compiler_probe() {
        // cargo runs this before building anything; counting it as a unit
        // would invent a crate called `___`.
        let pn = "rustc - --crate-name ___ '--print=file-names' --crate-type bin";
        assert!(unit_of(pn).is_none());
    }

    #[test]
    fn build_scripts_of_different_packages_are_different_units() {
        // Every package's build script is called `build_script_build`. Keying
        // on the name merged them: samples added up while wall time came from
        // whichever ran first, which showed as one unit accounting for 223% of
        // its own wall clock.
        let a = unit_of("rustc --crate-name build_script_build /home/u/.cargo/registry/src/index.crates.io-1/proc-macro2-1.0.107/build.rs --crate-type bin -C 'metadata=aaa'").unwrap();
        let b = unit_of("rustc --crate-name build_script_build /home/u/.cargo/registry/src/index.crates.io-1/libc-0.2.1/build.rs --crate-type bin -C 'metadata=bbb'").unwrap();
        assert_ne!(a, b, "build scripts of different packages must not merge");
        assert_eq!(a.package, "proc-macro2-1.0.107");
        assert_eq!(b.package, "libc-0.2.1");
        // a library's source lives under `<package>/src/lib.rs`; taking the
        // file's parent directory would call every one of them `src`
        let lib = unit_of("rustc --crate-name serde /home/u/.cargo/registry/src/index.crates.io-1/serde-1.0.229/src/lib.rs --crate-type lib -C 'metadata=ccc'").unwrap();
        assert_eq!(lib.package, "serde-1.0.229");
    }

    #[test]
    fn build_flags_keep_the_shape_and_drop_the_machine() {
        let pn = "rustc --crate-name serde /home/zack/.cargo/registry/src/index.crates.io-1/serde-1.0.0/src/lib.rs                   --crate-type lib '--edition=2021' --target aarch64-apple-darwin                   -C 'opt-level=3' -C 'debuginfo=2' -C 'codegen-units=16' -C 'panic=abort'                   -C 'incremental=/home/zack/proj/target/debug/incremental'                   -C 'metadata=abc' -C 'extra-filename=-abc'                   --cfg 'feature=\"derive\"' --cfg 'feature=\"std\"'                   --out-dir /home/zack/proj/target/debug/deps                   -L 'dependency=/home/zack/proj/target/debug/deps'";
        let f = unit_of(pn).unwrap().flags;
        assert_eq!(f.get("opt-level").map(String::as_str), Some("3"));
        assert_eq!(f.get("codegen-units").map(String::as_str), Some("16"));
        assert_eq!(f.get("panic").map(String::as_str), Some("abort"));
        assert_eq!(f.get("edition").map(String::as_str), Some("2021"));
        assert_eq!(
            f.get("target").map(String::as_str),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(f.get("features").map(String::as_str), Some("derive,std"));
        // incremental changes every timing in the session, so its presence is
        // recorded -- but its value is a path and must not be
        assert_eq!(f.get("incremental").map(String::as_str), Some("true"));
        // nothing naming the builder's machine may survive
        let all = f.values().cloned().collect::<Vec<_>>().join(" ");
        assert!(!all.contains("/home/zack"), "leaked a path: {all}");
        assert!(!f.contains_key("extra-filename"));
    }

    #[test]
    fn parked_threads_are_not_compile_work() {
        // A thread waiting on a condition variable burns no CPU. Counting it
        // as a phase inflated whichever bucket the enclosing frames pointed
        // at -- and the enclosing frames still look like the phase that is
        // waiting, so this must be decided on the leaf.
        assert!(is_blocked("semaphore_wait_trap"));
        assert!(is_blocked("__psynch_cvwait"));
        assert!(is_blocked("__ulock_wait"));
        assert!(!is_blocked("rustc_borrowck::mir_borrowck"));
        assert!(!is_blocked("llvm::runPasses"));
    }

    #[test]
    fn maps_symbols_to_the_phase_rustc_calls_them() {
        assert_eq!(phase_of("rustc_expand::mbe::expand"), Some("macro_expand"));
        assert_eq!(phase_of("rustc_borrowck::mir_borrowck"), Some("borrowck"));
        // hir_analysis encloses typeck, and -Ztime-passes calls the pair
        // `type_check_crate`
        assert_eq!(
            phase_of("rustc_hir_analysis::check_crate"),
            Some("type_check")
        );
        // the MIR pipeline borrowck drives is part of that span
        assert_eq!(
            phase_of("rustc_mir_transform::run_passes"),
            Some("borrowck")
        );
        assert_eq!(phase_of("std::vec::Vec::push"), None);
    }
}
