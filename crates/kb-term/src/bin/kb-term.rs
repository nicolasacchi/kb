//! `kb-term` operator-local client binary. Implements the `doctor`
//! subcommand (host-local diagnostics); the other client commands land
//! in later units.

use clap::{Arg, ArgAction, Command};

fn main() {
    let cmd = Command::new("kb-term")
        .version(env!("CARGO_PKG_VERSION"))
        .about("kb-term operator-local client")
        .subcommand(
            Command::new("doctor")
                .about("host-local diagnostics for the terminal substrate")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("print the report as JSON"),
                )
                .arg(
                    Arg::new("strict")
                        .long("strict")
                        .action(ArgAction::SetTrue)
                        .help("exit 1 on WARN as well as FAIL"),
                ),
        );
    let matches = cmd.get_matches();
    match matches.subcommand() {
        Some(("doctor", sub)) => std::process::exit(kb_term::doctor::run(
            sub.get_flag("json"),
            sub.get_flag("strict"),
        )),
        _ => {
            eprintln!("kb-term: not implemented yet (scaffold)");
            std::process::exit(2);
        }
    }
}
