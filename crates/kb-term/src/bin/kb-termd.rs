//! Scaffold `kb-termd` runner binary: accepts `--help` and `--version`, then
//! exits with a not-implemented message. The runner implementation lands in a
//! later unit.

use clap::Command;

fn main() {
    let mut cmd = Command::new("kb-termd")
        .version(env!("CARGO_PKG_VERSION"))
        .about("kb-term runner (scaffold)");
    let _ = cmd.get_matches_mut();
    eprintln!("kb-termd: not implemented yet (scaffold)");
    std::process::exit(2);
}
