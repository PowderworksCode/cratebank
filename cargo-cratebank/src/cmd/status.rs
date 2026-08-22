//! Check the required local tools and active endpoint.

use crate::cli::Common;

pub fn run(options: &Common) -> i32 {
    let samply = crate::sample::samply_available();
    println!(
        "samply       {}",
        if samply {
            "available"
        } else {
            "NOT available (`cargo install samply`)"
        }
    );
    println!("cargo        stable `cargo build --timings`");
    println!("endpoint     {}", options.endpoint);
    println!("privacy      public units only (non-public units are never sent)");
    if samply {
        0
    } else {
        1
    }
}
