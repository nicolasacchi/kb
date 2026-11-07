//! Scaffold `kb-term` operator-local client binary: accepts `--help` and
//! `--version`, then exits with a not-implemented message. The client
//! implementation lands in a later unit.

use clap::Command;

fn main() {
    let mut cmd = Command::new("kb-term")
        .version(env!("CARGO_PKG_VERSION"))
        .about("kb-term operator-local client (scaffold)");
    let _ = cmd.get_matches_mut();
    eprintln!("kb-term: not implemented yet (scaffold)");
    std::process::exit(2);
}
