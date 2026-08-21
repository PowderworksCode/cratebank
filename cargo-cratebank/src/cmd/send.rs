//! `cargo cratebank send` — ship logs from builds that already happened.
use serde_json::Value;

use crate::cli::{Common, SendArgs};
use crate::session::{log_dir, payload, read_session, sessions};

pub fn run(o: &Common, a: &SendArgs) -> i32 {
    // shipped after the fact: no honest load figure is available
    run_with_load(o, a, Value::Null)
}

/// `build` measures load across the build it just ran, so it can supply one.
pub fn run_with_load(o: &Common, a: &SendArgs, load: Value) -> i32 {
    let mut list = sessions();
    if let Some(id) = &a.session {
        list.retain(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy().contains(id))
                .unwrap_or(false)
        });
    } else if !a.all {
        list.truncate(a.since);
    }
    if list.is_empty() {
        eprintln!("cratebank: no session logs in {}", log_dir().display());
        eprintln!("  enable them:  cargo cratebank status");
        return 1;
    }
    crate::rusage::prune(); // sidecar rusage files older than a day
    let mut sent = 0;
    let mut skipped = 0;
    for p in &list {
        let Some(mut s) = read_session(p) else {
            continue;
        };
        let run_id = s.run_id.clone();
        // `--session <id>` names one deliberately, so it resends; everything
        // else skips what has already been delivered. Without this, `send
        // --all` re-uploads every log on every run -- and a census cannot tell
        // a duplicate from an independent observation, so the duplicates are a
        // data-quality problem, not just wasted requests.
        //
        // The ledger is keyed on the run_id read from the log's contents, not
        // the filename, because that is the id the payload actually carries.
        if a.session.is_none() && crate::ship::already_sent(&run_id) {
            skipped += 1;
            continue;
        }
        let env = crate::buildenv::snapshot(&s.dir);
        let body = payload(&mut s, env, load.clone());
        if o.dry_run {
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
            continue;
        }
        match crate::ship::post_sized(&o.endpoint, &body) {
            Ok((resp, wire)) => {
                sent += 1;
                // Only on a 2xx. A failed upload leaves the session unmarked,
                // so it stays queued and goes out next time.
                crate::ship::mark_sent(&run_id);
                let c = &body["counts"];
                let (raw, _) = crate::ship::sizes(&body);
                println!(
                    "sent {run_id}: {} events, {} units ({} withheld), {} sections, \
                          {:.0} KB zstd from {:.0} KB -> {} [{}]",
                    c["events"],
                    c["units"],
                    c["units_withheld"],
                    c["sections"],
                    wire as f64 / 1024.0,
                    raw as f64 / 1024.0,
                    o.endpoint,
                    resp.trim()
                );
            }
            Err(e) => eprintln!("cratebank: POST {} failed: {e}", o.endpoint),
        }
    }
    if skipped > 0 {
        eprintln!(
            "cratebank: {skipped} session(s) already sent, skipped \
             (--session <id> to send one again)"
        );
    }
    if o.dry_run {
        eprintln!(
            "cratebank: dry run, nothing sent ({} session(s))",
            list.len() - skipped
        );
    }
    // Everything having been sent already is success, not failure.
    if !o.dry_run && sent == 0 && skipped == 0 {
        1
    } else {
        0
    }
}
