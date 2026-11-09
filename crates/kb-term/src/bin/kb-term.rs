//! `kb-term` operator-local client binary: `attach|new|ls` (the
//! local mirror — see [`kb_term::attach`]) and `doctor` (host-local
//! diagnostics).

use std::future::Future;

use clap::{Arg, ArgAction, ArgGroup, Command};

/// The closed harness set for `new <harness>`. Single source of truth:
/// [`kb_term::profiles::HARNESSES`] (the pinned copy of
/// `kb_core::sessions::HARNESSES` at
/// `crates/kb-core/src/sessions/constants.rs:29`, drift-tested in
/// kb-server; this crate has no kb-core edge).
const HARNESS_NAMES: [&str; 6] = kb_term::profiles::HARNESSES;

fn runner_sock_arg() -> Arg {
    Arg::new("runner-sock")
        .long("runner-sock")
        .value_name("PATH")
        .env("KB_TERM_RUNNER_SOCK")
        .help(
            "runner.sock path (also $KB_TERM_RUNNER_SOCK; default /run/kb-term/runner/runner.sock)",
        )
}

fn cli() -> Command {
    Command::new("kb-term")
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
        )
        .subcommand(
            Command::new("attach")
                .about("attach to a session (the local mirror)")
                .long_about(
                    "Attach to a session: the session is mirrored into this \
                    terminal's native scrollback (seed, then the live stream).\n\n\
                    Keystrokes go to the session's hidden client. Detach with \
                    Ctrl-] then '.' (Ctrl-] followed by anything else sends both \
                    bytes; everything else, including Ctrl-C, goes to the harness).\
                    \n\nExit codes: 0 normal; 2 usage; 3 runner unreachable \
                    (the kb-termd user unit may not be running); 4 session not found.",
                )
                .arg(
                    Arg::new("id")
                        .value_name("ID")
                        .required(true)
                        .help("session id"),
                )
                .arg(
                    Arg::new("watch")
                        .long("watch")
                        .action(ArgAction::SetTrue)
                        .help("watch only: no take-size, no stdin"),
                )
                .arg(runner_sock_arg())
                .arg(
                    Arg::new("no-labels-dim")
                        .long("no-labels-dim")
                        .action(ArgAction::SetTrue)
                        .help("do not dim the picture labels"),
                ),
        )
        .subcommand(
            Command::new("new")
                .about("launch a new session (and attach, unless --no-attach)")
                .arg(
                    Arg::new("harness")
                        .value_name("HARNESS")
                        .required(true)
                        .value_parser(HARNESS_NAMES)
                        .help("harness to run (claude|codex|opencode|grok|kimi|omp)"),
                )
                .arg(Arg::new("resume").long("resume").value_name("SID").help(
                    "resume this harness session id (passed to the runner; \
                            it builds the resume argv)",
                ))
                .arg(
                    Arg::new("cwd")
                        .long("cwd")
                        .value_name("DIR")
                        .help("working directory (default: the current directory)"),
                )
                .arg(
                    Arg::new("cols")
                        .long("cols")
                        .value_name("N")
                        .value_parser(clap::value_parser!(u16))
                        .help("grid columns (runner default when omitted)"),
                )
                .arg(
                    Arg::new("rows")
                        .long("rows")
                        .value_name("N")
                        .value_parser(clap::value_parser!(u16))
                        .help("grid rows (runner default when omitted)"),
                )
                .arg(
                    Arg::new("attach")
                        .long("attach")
                        .action(ArgAction::SetTrue)
                        .help("attach after launch (the default)"),
                )
                .arg(
                    Arg::new("no-attach")
                        .long("no-attach")
                        .action(ArgAction::SetTrue)
                        .help("print the new session id and exit"),
                )
                .group(ArgGroup::new("attach_policy").args(["attach", "no-attach"]))
                .arg(runner_sock_arg()),
        )
        .subcommand(
            Command::new("ls")
                .about("list sessions")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("print the raw RPC rows"),
                )
                .arg(runner_sock_arg()),
        )
}

fn main() {
    let matches = cli().get_matches();
    let code = match matches.subcommand() {
        Some(("doctor", sub)) => {
            kb_term::doctor::run(sub.get_flag("json"), sub.get_flag("strict")) as u8
        }
        Some(("attach", sub)) => {
            let args = kb_term::attach::AttachArgs {
                id: sub.get_one::<String>("id").cloned().unwrap_or_default(),
                watch: sub.get_flag("watch"),
                runner_sock: sub.get_one::<String>("runner-sock").cloned(),
                no_labels_dim: sub.get_flag("no-labels-dim"),
            };
            block_on(kb_term::attach::run_attach(args))
        }
        Some(("new", sub)) => {
            let args = kb_term::attach::NewArgs {
                harness: sub
                    .get_one::<String>("harness")
                    .cloned()
                    .unwrap_or_default(),
                resume_sid: sub.get_one::<String>("resume").cloned(),
                cwd: sub.get_one::<String>("cwd").cloned(),
                cols: sub.get_one::<u16>("cols").copied(),
                rows: sub.get_one::<u16>("rows").copied(),
                attach: !sub.get_flag("no-attach"),
                runner_sock: sub.get_one::<String>("runner-sock").cloned(),
            };
            // Cols/rows bounds (the runner re-checks; this keeps the
            // usage failure a 2, not a 1).
            for (v, name, lo, hi) in [
                (args.cols, "cols", 20u16, 500u16),
                (args.rows, "rows", 5, 200),
            ] {
                if let Some(n) = v {
                    if n < lo || n > hi {
                        eprintln!("kb-term: {name} must be {lo}..={hi}");
                        std::process::exit(2);
                    }
                }
            }
            block_on(kb_term::attach::run_new(args))
        }
        Some(("ls", sub)) => {
            let args = kb_term::attach::LsArgs {
                json: sub.get_flag("json"),
                runner_sock: sub.get_one::<String>("runner-sock").cloned(),
            };
            block_on(kb_term::attach::run_ls(args))
        }
        _ => {
            eprintln!("kb-term: missing subcommand (see --help)");
            2
        }
    };
    std::process::exit(code);
}

/// Build a current-thread runtime and block on `fut`.
fn block_on<F>(fut: F) -> u8
where
    F: Future<Output = u8>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt build");
    rt.block_on(fut)
}
