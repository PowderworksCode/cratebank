//! Run one stable Cargo build under samply, parse both outputs, and send them.

use crate::cli::Common;

const RATE_HZ: u32 = 4999;

fn cargo_args(args: &[String]) -> Vec<String> {
    let mut command = vec!["build".into()];
    if !args.iter().any(|arg| arg.starts_with("--timings")) {
        command.push("--timings".into());
    }
    command.extend_from_slice(args);
    command
}

fn fail(message: impl std::fmt::Display) -> i32 {
    eprintln!("cratebank: {message}");
    1
}

pub fn run(options: &Common, args: &[String]) -> i32 {
    if !crate::sample::samply_available() {
        return fail("samply is required; install it with `cargo install samply`");
    }

    let initial_project = match crate::timings::project(args) {
        Ok(project) => project,
        Err(error) => return fail(error),
    };
    let reports_before = crate::timings::reports(&initial_project.target_dir);
    let temporary = std::env::temp_dir().join(format!(
        "cratebank-profile-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let load_sampler = crate::load::Sampler::start();

    eprintln!(
        "cratebank: samply record -- cargo {}",
        cargo_args(args).join(" ")
    );
    let recorded = crate::sample::record(&cargo_args(args), &temporary, RATE_HZ);
    let load = load_sampler.finish();
    let (profile, symbols) = match recorded {
        Ok(paths) => paths,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            return fail(error);
        }
    };

    let project = match crate::timings::project(args) {
        Ok(project) => project,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            return fail(error);
        }
    };
    let report_path = match crate::timings::new_report(&initial_project.target_dir, &reports_before)
    {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            return fail(error);
        }
    };
    let capture = match crate::timings::capture(&report_path, &project) {
        Ok(capture) => capture,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            return fail(error);
        }
    };
    let mut units = match crate::sample::attribute(&profile, &symbols) {
        Ok(units) => units,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            return fail(format!("cannot parse samply output: {error}"));
        }
    };
    units.retain(|unit| {
        project.phase_is_public(&unit.crate_name, &unit.package, unit.source_path.as_deref())
    });
    let phases = crate::sample::to_json(&units, RATE_HZ);
    let _ = std::fs::remove_dir_all(&temporary);

    let body = crate::payload::build(&project, capture, phases, load);
    if options.dry_run {
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
        eprintln!("cratebank: dry run, nothing sent");
        return 0;
    }

    match crate::ship::post_sized(&options.endpoint, &body) {
        Ok((response, wire_bytes)) => {
            let counts = &body["counts"];
            let (raw_bytes, _) = crate::ship::sizes(&body);
            println!(
                "sent {}: {} timing units ({} withheld), {} sampled units, \
                 {:.0} KB zstd from {:.0} KB -> {} [{}]",
                body["run_id"].as_str().unwrap_or("?"),
                counts["units"],
                counts["units_withheld"],
                counts["phase_units"],
                wire_bytes as f64 / 1024.0,
                raw_bytes as f64 / 1024.0,
                options.endpoint,
                response.trim()
            );
            0
        }
        Err(error) => fail(format!("POST {} failed: {error}", options.endpoint)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_exactly_one_timings_flag() {
        assert_eq!(cargo_args(&[]), vec!["build", "--timings"]);
        assert_eq!(
            cargo_args(&["--timings".into(), "--release".into()]),
            vec!["build", "--timings", "--release"]
        );
    }
}
