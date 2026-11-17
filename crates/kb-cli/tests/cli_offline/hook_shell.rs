//! Runs the recall/wake hook shell tests under `cargo test`, so CI executes
//! them. They are plain bash scripts with a fake `kb` on PATH and a real
//! `jq`; before this module nothing in CI ran any of them, which is how a
//! hook claim ("6s is under the harness budget") could go unchecked.
//!
//! Every script runs in a per-child throwaway HOME/XDG tree with the inherited
//! `KB_*`/`CLAUDE_*` variables stripped (`hook_command`): a hook test once ran
//! the real `kb` with a developer's real HOME and its replay deleted their
//! parked capture spool.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Outer deadline for one hook script. Well under nextest's 360 s terminate
/// limit (never raised): a script that stalls fails HERE with the output it
/// had produced, instead of a bare TIMEOUT with nothing in the log. The long
/// scripts also carry their own, earlier per-step deadlines and diagnostics.
const HOOK_SCRIPT_DEADLINE: Duration = Duration::from_secs(300);

/// Collect a pipe on its own thread into a shared buffer, so a descendant that
/// still holds the pipe open after the script is gone can never block the test
/// (`Command::output` waits for EOF on both pipes).
fn drain<R: Read + Send + 'static>(
    mut pipe: R,
) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buf);
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    (buf, handle)
}

/// What one script run left behind (see `spawn_hook`).
struct HookRun {
    status: Option<std::process::ExitStatus>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

fn run_hook(script: &str, real_kb: bool, env: &[(&str, &str)]) {
    let HookRun {
        status,
        timed_out,
        stdout,
        stderr,
    } = spawn_hook(script, real_kb, env);
    assert!(
        !timed_out,
        "{script} exceeded {HOOK_SCRIPT_DEADLINE:?} and was killed (its own step deadlines did not fire first):\n{stdout}\n{stderr}"
    );
    let status = status.expect("exit status when not timed out");
    assert!(
        status.success() && !stdout.contains("not ok"),
        "{script} failed:\n{stdout}\n{stderr}"
    );
}

/// Spawn one script under the shared deadline and collect its output without
/// judging the exit status (a self-test that EXPECTS a failure needs it).
fn spawn_hook(script: &str, real_kb: bool, env: &[(&str, &str)]) -> HookRun {
    let (mut cmd, _sandbox) = hook_command(script);
    if real_kb {
        let kb = PathBuf::from(env!("CARGO_BIN_EXE_kb"));
        cmd.env("KB_BIN_DIR", kb.parent().unwrap());
    }
    let mut child = cmd
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash is required to run the hook shell tests");
    let (out_buf, out_thread) = drain(child.stdout.take().unwrap());
    let (err_buf, err_thread) = drain(child.stderr.take().unwrap());
    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll the hook script") {
            break (Some(status), false);
        }
        if started.elapsed() > HOOK_SCRIPT_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Give the readers a moment to flush; never join a thread whose pipe a
    // leaked descendant may still hold.
    let flush_until = Instant::now() + Duration::from_secs(10);
    while !(out_thread.is_finished() && err_thread.is_finished()) && Instant::now() < flush_until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).into_owned();
    HookRun {
        status,
        timed_out,
        stdout,
        stderr,
    }
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

/// The three distill nudges (which since SLATE2 post nothing to the slate);
/// these run each end to end.
#[test]
fn distill_nudge_hook_shell_tests() {
    for s in [
        "test-distill-nudge.sh",
        "test-distill-nudge-codex.sh",
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

/// v0.45 N4 - codex/opencode/omp/grok park their translated transcript
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
///
/// FXHANG: the matrix is one script, run as seven tests (`SEG_GROUP`), because
/// as a single test it took 236-340 s on healthy CI runs against nextest's
/// 360 s limit: not a hang, a load-sensitive wall time with nothing in the log
/// when it crossed. `SEG_GROUPS` and the script's `if seg_group gN; then`
/// markers are checked against each other by
/// `omp_segmented_capture_groups_cover_the_whole_script`.
const SEG_GROUPS: [&str; 7] = ["g1", "g2", "g3", "g4", "g5", "g6", "g7"];

fn omp_segments_group(group: &str) {
    assert!(SEG_GROUPS.contains(&group), "unknown group {group}");
    run_with_real_kb_env("test-capture-omp-segments.sh", &[("SEG_GROUP", group)]);
}

/// Flag/probe/lib helpers, the below-threshold golden, a long session's ids and
/// links, concatenation equals the single capture, title-only re-land, rc 1/2.
#[test]
fn omp_segmented_capture_flag_golden_chain_and_failed_landings() {
    omp_segments_group("g1");
}

/// kill -9 mid catch-up resumes; rewind/fork/clear re-land in place and drop
/// orphans (a failed drop is retried); TERM during a slow exit scan / chain walk.
#[test]
fn omp_segmented_capture_kill9_resume_rewind_and_term_reaping() {
    omp_segments_group("g2");
}

/// Sidecar change re-lands only its part; size cap halves the target; spool cap;
/// TERM mid planner-emit.
#[test]
fn omp_segmented_capture_sidecar_size_cap_spool_cap_and_emit_term() {
    omp_segments_group("g3");
}

/// The real capture engine, the session-exit marker follows the resolved chain,
/// parked-but-unlanded parts, flag switching, a landing that leaves no file,
/// out-of-time with nothing landed.
#[test]
fn omp_segmented_capture_real_engine_exit_marker_and_flag_switching() {
    omp_segments_group("g4");
}

/// SEG-PERF: sidecar-heavy sessions catch up (title slot, mtime placement,
/// the per-sidecar translation cache, a slow tail).
#[test]
fn omp_segmented_capture_sidecar_heavy_sessions_catch_up() {
    omp_segments_group("g5");
}

/// The landing cap scales with the payload and stays bounded; the pass after a
/// failed tail is frozen-first. (Holds the deliberate 20 s+ stand-in sleeps.)
#[test]
fn omp_segmented_capture_landing_cap_scales_and_is_bounded() {
    omp_segments_group("g6");
}

/// A permanently failing frozen part starves nothing; a drifted translator
/// fails closed.
#[test]
fn omp_segmented_capture_stuck_frozen_part_and_translator_drift() {
    omp_segments_group("g7");
}

/// FXMINTEST - the deadline diagnostics of the segmented capture script had
/// never run: `seg_die`/`seg_diag` fire only on a stall. `SEG_SELFTEST=deadline`
/// makes the script stall on purpose in its own sandbox (a step sleeping past
/// a 1 s deadline, with a held lock, a parked spool item and a log line), so
/// this asserts from outside that the run FAILS and that the dump names the
/// step and carries every section a real stall would need.
#[test]
fn omp_segmented_capture_deadline_dump_names_the_step_and_every_section() {
    let run = spawn_hook(
        "test-capture-omp-segments.sh",
        false,
        &[("SEG_SELFTEST", "deadline")],
    );
    let out = format!("{}\n{}", run.stdout, run.stderr);
    assert!(
        !run.timed_out,
        "the self-test must hit its own 1 s deadline, not the outer one:\n{out}"
    );
    let status = run.status.expect("exit status when not timed out");
    assert!(
        !status.success(),
        "a deadline expiry must exit non-zero:\n{out}"
    );
    let step = "selftest deadline step still running after 1s";
    let deadline = format!("DEADLINE: {step}");
    let header = format!("=== DIAGNOSTICS: {step}");
    for want in [
        "not ok  - [",
        deadline.as_str(),
        header.as_str(),
        "--- processes",
        "sleep 31",
        "--- lock files",
        "HELD  ",
        "selftest.lock",
        "--- locks dir",
        "--- spool dir",
        "selftest-item",
        "--- corpus dir",
        "--- tail ",
        "selftest-log-marker",
        "=== END DIAGNOSTICS ===",
        "passed=0 failed=1",
    ] {
        assert!(
            out.contains(want),
            "the deadline dump lacks {want:?}:\n{out}"
        );
    }
}

/// The group split cannot silently drop a scenario: the script carries exactly
/// one `if seg_group gN; then` per group named here, each closed by a `fi`.
#[test]
fn omp_segmented_capture_groups_cover_the_whole_script() {
    let src = std::fs::read_to_string(tests_dir().join("test-capture-omp-segments.sh"))
        .expect("read the segmented capture script");
    let markers: Vec<&str> = src
        .lines()
        .filter(|l| l.starts_with("if seg_group "))
        .collect();
    let want: Vec<String> = SEG_GROUPS
        .iter()
        .map(|g| format!("if seg_group {g}; then"))
        .collect();
    assert_eq!(
        markers, want,
        "group markers must be g1..g7 in order, each once"
    );
    // Markers alternate open/close (`fi # end seg_group`), and no scenario
    // (`fresh` on its own line) sits outside a group: it would run in all
    // seven tests and in none of them by name.
    let mut open = false;
    let mut closed = 0;
    for l in src.lines() {
        if l.starts_with("if seg_group ") {
            assert!(!open, "a group opened inside another");
            open = true;
        } else if l == "fi # end seg_group" {
            assert!(open, "a group closed without being open");
            open = false;
            closed += 1;
        } else if !open && l.trim() == "fresh" {
            panic!("a scenario (`fresh`) sits outside every group");
        }
    }
    assert!(
        !open && closed == SEG_GROUPS.len(),
        "every group must be closed once"
    );
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
