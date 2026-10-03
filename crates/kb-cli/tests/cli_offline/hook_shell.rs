//! Runs the recall/wake hook shell tests under `cargo test`, so CI executes
//! them. They are plain bash scripts with a fake `kb` on PATH and a real
//! `jq`; before this module nothing in CI ran any of them, which is how a
//! hook claim ("6s is under the harness budget") could go unchecked.

use std::path::PathBuf;
use std::process::Command;

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/kb-memory/hooks/tests")
        .canonicalize()
        .unwrap()
}

fn run(script: &str) {
    let out = Command::new("bash")
        .arg(tests_dir().join(script))
        .output()
        .expect("bash is required to run the hook shell tests");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains("not ok"),
        "{script} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn hook_deadlines_and_recall_block_shape() {
    run("test-hook-deadlines.sh");
}

#[test]
fn recall_hook_shell_tests() {
    for s in [
        "test-recall-scent.sh",
        "test-recall-slate.sh",
        "test-recall-layout.sh",
        "test-recall-turn.sh",
    ] {
        run(s);
    }
}

#[test]
fn wake_hook_shell_tests() {
    run("test-wake-slate.sh");
}

/// The four distill nudges share `post_distill_ask` from `kb-hook-lib.sh`
/// (previously four hand-copied definitions); these run each end to end.
#[test]
fn distill_nudge_hook_shell_tests() {
    for s in [
        "test-distill-nudge.sh",
        "test-distill-nudge-codex.sh",
        "test-distill-nudge-kimi.sh",
        "test-distill-nudge-omp.sh",
    ] {
        run(s);
    }
}
