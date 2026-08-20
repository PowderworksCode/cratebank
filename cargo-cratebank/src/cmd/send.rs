//! `cargo cratebank send` — ship logs from builds that already happened.
use crate::cli::{Common, SendArgs};
use crate::session::{log_dir, payload, read_session, sessions};
use crate::ship::post;

pub fn run(o: &Common, a: &SendArgs) -> i32 {
    let mut list = sessions();
    if let Some(id) = &a.session {
        list.retain(|p| p.file_name().map(|f| f.to_string_lossy().contains(id)).unwrap_or(false));
    } else if !a.all {
        list.truncate(a.since);
    }
    if list.is_empty() {
        eprintln!("cratebank: no session logs in {}", log_dir().display());
        eprintln!("  enable them:  cargo cratebank status");
        return 1;
    }
    let mut sent = 0;
    for p in &list {
        let Some(s) = read_session(p, o.public_only()) else { continue };
        let run_id = s.run_id.clone();
        let body = payload(&s.run_id, s.events, &s.header, o.public_only(), s.withheld);
        if o.dry_run {
            println!("{}", serde_json::to_string_pretty(&body).unwrap());
            continue;
        }
        match post(&o.endpoint, &body) {
            Ok(resp) => {
                sent += 1;
                let c = &body["counts"];
                println!("sent {run_id}: {} events, {} units ({} withheld), {} sections -> {} [{}]",
                         c["events"], c["units"], c["units_withheld"], c["sections"],
                         o.endpoint, resp.trim());
            }
            Err(e) => eprintln!("cratebank: POST {} failed: {e}", o.endpoint),
        }
    }
    if o.dry_run { eprintln!("cratebank: dry run, nothing sent ({} session(s))", list.len()); }
    if !o.dry_run && sent == 0 { 1 } else { 0 }
}

