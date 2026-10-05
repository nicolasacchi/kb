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

/// v0.44 F7b, reworked in v0.45 N4 - the five harness adapters land through
/// the REAL `kb sessions capture` (which scrubs) and write nothing to the
/// corpus without it.
#[test]
fn codex_and_opencode_adapters_scrub_secrets_and_fail_closed() {
    let kb = PathBuf::from(env!("CARGO_BIN_EXE_kb"));
    let out = Command::new("bash")
        .arg(tests_dir().join("test-capture-scrub.sh"))
        .env("KB_BIN_DIR", kb.parent().unwrap())
        .output()
        .expect("bash is required to run the hook shell tests");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains("not ok"),
        "test-capture-scrub.sh failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// v0.45 N4 - the kimi and omp adapter matrices (stand-in `kb`, fixtures) were
/// not run by any CI lane before; both need `jq`, which CI runners ship.
#[test]
fn kimi_and_omp_adapter_shell_tests() {
    run("test-capture-kimi.sh");
    // omp's sidecar block drives the real engine: hand it the built `kb`.
    run_with_real_kb("test-capture-omp.sh");
}

/// v0.44 F8 — the once-a-day CLI/hook skew notice.
#[test]
fn wake_hook_names_cli_skew_once_a_day() {
    run("test-wake-skew.sh");
}

/// v0.44 F10 — the once-a-day `kb chores --line` rides the wake context.
#[test]
fn wake_hook_appends_the_chores_line() {
    run("test-wake-chores.sh");
}

/// v0.45 N5 - the kimi wake hook carries the same chores line on its first
/// prompt.
#[test]
fn wake_kimi_hook_appends_the_chores_line() {
    run("test-wake-kimi-chores.sh");
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

/// v0.44 X4 — the hooks publish KB_SESSION_ID / KB_HARNESS (also through
/// `$CLAUDE_ENV_FILE`) so shell `kb` writes are attributed to the session.
#[test]
fn hooks_export_session_identity_for_shell_writes() {
    run("test-hook-identity.sh");
}

/// v0.44 X10 - every per-session hook file name comes from ONE collision-free
/// helper (`hook_sid_key`), never the lossy `tr | cut -c1-80` form.
#[test]
fn per_session_hook_names_never_collide() {
    run("test-sid-key.sh");
}

/// v0.44 X6 - a failing `kb sessions capture` spools the RAW transcript
/// privately (never into the corpus); the next successful capture or
/// `--replay-spool` lands it scrubbed. Needs the REAL `kb` binary.
#[test]
fn capture_hook_spools_instead_of_embedding_raw_and_replays_scrubbed() {
    let kb = PathBuf::from(env!("CARGO_BIN_EXE_kb"));
    let out = Command::new("bash")
        .arg(tests_dir().join("test-capture-spool.sh"))
        .env("KB_BIN_DIR", kb.parent().unwrap())
        .output()
        .expect("bash is required to run the hook shell tests");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains("not ok"),
        "test-capture-spool.sh failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// v0.45 N10 - omp sidecar agent names stay readable and gain a short hash
/// only on collision; a failed capture parks the translated sidecars in the
/// spool beside the main transcript. Needs the REAL `kb` binary for the
/// sidecar-digest block.
#[test]
fn omp_sidecar_names_disambiguate_only_on_collision_and_spool_with_the_item() {
    run_with_real_kb("test-capture-omp.sh");
}

/// Run `script` with `KB_BIN_DIR` pointing at the freshly built `kb`.
fn run_with_real_kb(script: &str) {
    let kb = PathBuf::from(env!("CARGO_BIN_EXE_kb"));
    let out = Command::new("bash")
        .arg(tests_dir().join(script))
        .env("KB_BIN_DIR", kb.parent().unwrap())
        .output()
        .expect("bash is required to run the hook shell tests");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains("not ok"),
        "{script} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// v0.45 N4 - codex/opencode/kimi/omp/grok park their translated transcript
/// in the private spool when `kb sessions capture` fails or `kb` is missing
/// (never raw HTML in the corpus); the replay lands it scrubbed with the
/// harness intact and the session's true start stamp in the filename.
#[test]
fn harness_adapters_spool_and_replay_through_the_real_kb() {
    run_with_real_kb("test-capture-adapters-spool.sh");
}

/// v0.45 N4 - the capture throttle and the Rust capture engine agree on the
/// per-session file name (distinct non-UUID ids do not share a throttle slot;
/// pre-v0.45 lossy-named captures still throttle their own id).
#[test]
fn capture_throttle_agrees_with_the_rust_capture_file_name() {
    run_with_real_kb("test-capture-throttle.sh");
}

/// v0.45 N4 - the grok adapter's distill-pending relay on the landed path.
#[test]
fn grok_distill_pending_relay() {
    run("test-grok-distill-pending.sh");
}
