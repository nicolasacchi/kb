//! `--version` of the two Rust bins reports the build's stamp (FXVER): the
//! workspace version is pinned at 0.0.0, so the bins read `KB_BUILD_VERSION`
//! at compile time. This test is compiled with the same env as the bins, so
//! it holds for a stamped CI/release build and an unstamped dev build alike.

use std::process::Command;

fn expected() -> &'static str {
    kb_term::version::stamped_version(option_env!("KB_BUILD_VERSION"))
}

fn first_line(bin: &str) -> String {
    let out = Command::new(bin).arg("--version").output().expect("run");
    assert!(out.status.success(), "{bin} --version failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

#[test]
fn kb_term_reports_the_build_stamp_not_the_workspace_zero() {
    let line = first_line(env!("CARGO_BIN_EXE_kb-term"));
    assert_eq!(line, format!("kb-term {}", expected()));
    assert_ne!(line, "kb-term 0.0.0");
}

#[test]
fn kb_termd_reports_the_build_stamp_not_the_workspace_zero() {
    let line = first_line(env!("CARGO_BIN_EXE_kb-termd"));
    assert_eq!(line, format!("kb-termd {}", expected()));
    assert_ne!(line, "kb-termd 0.0.0");
}

#[test]
fn an_unstamped_build_says_so_plainly() {
    if option_env!("KB_BUILD_VERSION").is_none() {
        assert_eq!(expected(), "0.0.0-dev");
    }
}
