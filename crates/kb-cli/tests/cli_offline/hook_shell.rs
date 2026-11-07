//! Runs the recall/wake hook shell tests under `cargo test`, so CI executes
//! them. They are plain bash scripts with a fake `kb` on PATH and a real
//! `jq`; before this module nothing in CI ran any of them, which is how a
//! hook claim ("6s is under the harness budget") could go unchecked.
//!
//! Every script runs in a per-child throwaway HOME/XDG tree with the inherited
//! `KB_*`/`CLAUDE_*` variables stripped (`hook_command`): a hook test once ran
//! the real `kb` with a developer's real HOME and its replay deleted their
//! parked capture spool.

use std::path::PathBuf;
use std::process::Command;

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/kb-memory/hooks/tests")
        .canonicalize()
        .unwrap()
}

/// The ONE place this file spawns bash. Incident: a hook test that ran the
/// real `kb` with the developer's HOME replayed and deleted their parked
/// capture spool. So the child gets a throwaway HOME/XDG tree and loses every
/// inherited `KB_*`/`CLAUDE_*` variable (callers re-add what they need).
/// Per-child `Command::env`/`env_remove` only, never `std::env::set_var`
/// (docs/architecture-invariants.md, the test-env pitfalls). The returned
/// `TempDir` must outlive the child: bind it to `_sandbox`, never `_`.
fn hook_command(script: &str) -> (Command, tempfile::TempDir) {
    let sandbox = tempfile::tempdir().expect("create the hook-test sandbox");
    let mut cmd = Command::new("bash");
    cmd.arg(tests_dir().join(script));
    // Nothing the parent shell says about where kb state lives reaches the
    // script: KB_SESSIONS_DIR, KB_CAPTURE_SPOOL, KB_HOME, CLAUDE_ENV_FILE, ...
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|k| k.starts_with("KB_") || k.starts_with("CLAUDE_"))
        {
            cmd.env_remove(&key);
        }
    }
    for (var, dir) in [
        ("HOME", "home"),
        ("XDG_CACHE_HOME", "xdg-cache"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_DATA_HOME", "xdg-data"),
        ("XDG_STATE_HOME", "xdg-state"),
    ] {
        let path = sandbox.path().join(dir);
        std::fs::create_dir_all(&path).expect("create a hook-test sandbox dir");
        cmd.env(var, path);
    }
    (cmd, sandbox)
}

fn run_hook(script: &str, real_kb: bool, env: &[(&str, &str)]) {
    let (mut cmd, _sandbox) = hook_command(script);
    if real_kb {
        let kb = PathBuf::from(env!("CARGO_BIN_EXE_kb"));
        cmd.env("KB_BIN_DIR", kb.parent().unwrap());
    }
    let out = cmd
        .envs(env.iter().copied())
        .output()
        .expect("bash is required to run the hook shell tests");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains("not ok"),
        "{script} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(script: &str) {
    run_hook(script, false, &[]);
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

/// OL IDEA4 - the live-beat hooks (kb-beat.sh, the PreToolUse tool-start gate,
/// hooks.json wiring); test-beat.sh was not run by any CI lane before.
#[test]
fn beat_hook_shell_tests() {
    run("test-beat.sh");
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
    run_with_real_kb("test-capture-scrub.sh");
}

/// v0.45 N4 - the kimi adapter matrix (stand-in `kb`, fixtures) was not run by
/// any CI lane before; it needs `jq`, which CI runners ship. omp's matrix runs
/// in `omp_sidecar_names_disambiguate_only_on_collision_and_spool_with_the_item`.
#[test]
fn kimi_adapter_shell_tests() {
    run("test-capture-kimi.sh");
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

/// The four distill nudges (which since SLATE2 post nothing to the slate);
/// these run each end to end.
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
    run_with_real_kb("test-capture-spool.sh");
}

/// v0.45 N10 - omp sidecar agent names stay readable and gain a short hash
/// only on collision; a failed capture parks the translated sidecars in the
/// spool beside the main transcript. Needs the REAL `kb` binary for the
/// sidecar-digest block.
#[test]
fn omp_sidecar_names_disambiguate_only_on_collision_and_spool_with_the_item() {
    run_with_real_kb("test-capture-omp.sh");
}

/// T20 - codex `input_image` is captured as a mime-only blob_ref (the
/// base64 never survives). Needs the REAL `kb` binary.
#[test]
fn codex_input_image_becomes_a_mime_only_ref() {
    run_with_real_kb("test-capture-codex-images.sh");
}

/// Run `script` with `KB_BIN_DIR` pointing at the freshly built `kb`.
fn run_with_real_kb(script: &str) {
    run_with_real_kb_env(script, &[]);
}

fn run_with_real_kb_env(script: &str, env: &[(&str, &str)]) {
    run_hook(script, true, env);
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

/// v0.45 OC - the omp capture adapter's lifecycle with REAL subprocesses:
/// per-session exclusion + request coalescing (one conversion at a time, the
/// newest state published), independent sessions concurrent, SIGTERM and the
/// hard deadline reap every owned descendant and leave the previous capture,
/// lock and scratch clean, recovery, the fingerprint shortcut (in-place
/// retitle, sidecar change, deleted capture, failed landing) and the
/// translator's linear leaf-chain walk (byte-identical to the former reduce).
#[test]
fn omp_capture_lifecycle_excludes_coalesces_reaps_and_recovers() {
    run("test-capture-omp-lifecycle.sh");
}

/// omp capture round 5 - the spool is shared by every session and adapter; a
/// replayed snapshot older than a capture that already landed is dropped, never
/// published over it (the real writer enforces it, so this runs the real kb).
#[test]
fn spool_replay_never_overwrites_a_fresher_capture() {
    run_with_real_kb("test-capture-replay-order.sh");
}

/// v0.45 N4 - the grok adapter's distill-pending relay on the landed path.
#[test]
fn grok_distill_pending_relay() {
    run("test-grok-distill-pending.sh");
}

/// v0.46 SEG-PR2 - segmented omp capture (flag-gated): a long session lands as
/// an ordered chain of ordinary sessions planned by the REAL
/// `kb sessions segment-plan`; frozen parts convert once, kill -9 mid catch-up
/// resumes, rc 1/2 never record landed, a fork/rewind or /clear re-lands in
/// place and deletes orphan parts through the REAL `kb sessions drop-part`, a
/// sidecar change re-lands only its part, the size cap halves the target, the
/// spool is capped, TERM reaps every descendant, and below the threshold the
/// capture is byte-identical to the pre-segmentation golden.
#[test]
fn omp_segmented_capture_lands_a_chain_resumes_and_drops_orphans() {
    run_with_real_kb("test-capture-omp-segments.sh");
}

/// v0.46 SEG-PR2 - the scale gate at a CI-sized session: a linked synthetic
/// chain of ~16 MiB with 2 MiB parts, run under `ulimit -v`; asserts that one
/// appended turn converts at most the live tail and costs far less than the
/// catch-up. (The production-size numbers are measured by hand; see the PR.)
#[test]
fn omp_segmented_capture_per_turn_cost_does_not_scale_with_the_session() {
    run_with_real_kb_env(
        "test-capture-omp-segments-scale.sh",
        &[
            ("SEG_SCALE_MB", "16"),
            ("SEG_SCALE_TARGET", "2097152"),
            ("SEG_SCALE_SIDECARS", "300"),
        ],
    );
}

/// AH1 - the sandbox helper keeps a parked spool item in a (simulated)
/// developer HOME untouched by a real `--replay-spool`.
#[test]
fn sandbox_helper_keeps_a_real_home_spool_untouched() {
    run_with_real_kb("test-lib-sandbox.sh");
}
