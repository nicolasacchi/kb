//! `kb backup` / `kb restore` round-trip (B1).
//!
//! Backup now takes a CONSISTENT snapshot — `VACUUM INTO` for the sqlite
//! and a copy-then-validate (re-open) for the lance dataset — so these
//! tests stage REAL stores (a migrated `index.db` + a real lance dataset),
//! not byte fixtures. We also pin that the v0.8 `RequestMetrics` (process
//! RAM only) never leaks into a backup.

use assert_cmd::Command;
use kb_core::storage::lance::Storage;
use kb_core::storage::sqlite::Db;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

/// Stage a REAL kb state tree at `<state>/kb/default/smoke/`: a migrated
/// `index.db`, a real (empty) lance dataset, and a `.review/` comment
/// file. Real stores are required because backup vacuums the sqlite and
/// validates the lance copy by re-opening it.
async fn stage_state(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let state = tmp.join("state");
    let config = tmp.join("config");
    // KB_STATE_DIR is the state root; the daemon segment (`default`) is
    // appended by KbPaths, so the kb's state lives at <state>/default/<kb>.
    let kb_state = state.join("default/smoke");
    fs::create_dir_all(&kb_state).unwrap();

    // Real migrated sqlite (connection closed on drop).
    Db::open(&kb_state.join("index.db")).unwrap();
    // Real (empty) lance dataset.
    Storage::open(&kb_state.join("lance"), Some(384))
        .await
        .unwrap();
    // A comment file — copied verbatim by backup (it isn't parsed).
    fs::create_dir_all(kb_state.join(".review")).unwrap();
    fs::write(
        kb_state.join(".review/abc123def456.json"),
        br#"{"schema":"kb-comments/1","artifact":{"id":"abc123def456","title":"t","kb":"smoke"},"generatedAt":"2026-05-12T10:00:00Z","comments":[]}"#,
    )
    .unwrap();

    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("kb.toml"),
        "[daemon]\nname = \"default\"\n\n[server]\naddr = \"127.0.0.1:4737\"\n\n[ui]\n\n[kb.smoke]\npath = \"/tmp/smoke\"\n",
    )
    .unwrap();
    (state, config)
}

/// `kb backup smoke --out <tarball>` against the staged state.
fn run_backup(
    state: &Path,
    config: &Path,
    cache: &Path,
    kb: &str,
    out: &Path,
) -> assert_cmd::assert::Assert {
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", state)
        .env("KB_CONFIG_DIR", config)
        .env("KB_CACHE_DIR", cache)
        .args(["backup", kb, "--out"])
        .arg(out)
        .assert()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_then_restore_round_trips_into_a_fresh_state_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = stage_state(tmp.path()).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("smoke-backup.tar.gz");

    run_backup(&state, &config, &cache, "smoke", &out_path).success();
    assert!(out_path.exists(), "backup tarball not written");
    assert!(fs::metadata(&out_path).unwrap().len() > 0, "tarball empty");

    // Restore into a FRESH state dir (different KB_STATE_DIR, same config).
    let state_b = tmp.path().join("state-b");
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", &state_b)
        .env("KB_CONFIG_DIR", &config)
        .env("KB_CACHE_DIR", &cache)
        .args(["restore"])
        .arg(&out_path)
        .args(["--kb", "smoke"])
        .assert()
        .success();

    // The restored kb is real: index.db opens, lance opens, comment survives.
    let restored = state_b.join("default/smoke");
    assert!(
        restored.join("index.db").exists(),
        "restored index.db missing"
    );
    Db::open(&restored.join("index.db")).expect("restored index.db must open");
    Storage::open(&restored.join("lance"), None)
        .await
        .expect("restored lance must open");
    assert!(
        restored.join(".review/abc123def456.json").exists(),
        "restored review file missing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_refuses_non_empty_state_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = stage_state(tmp.path()).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("smoke.tar.gz");
    run_backup(&state, &config, &cache, "smoke", &out_path).success();

    // Same (non-empty) state, no --force → refuse.
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", &state)
        .env("KB_CONFIG_DIR", &config)
        .env("KB_CACHE_DIR", &cache)
        .args(["restore"])
        .arg(&out_path)
        .args(["--kb", "smoke"])
        .assert()
        .failure();

    // With --force → wipe + restore succeeds.
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", &state)
        .env("KB_CONFIG_DIR", &config)
        .env("KB_CACHE_DIR", &cache)
        .args(["restore"])
        .arg(&out_path)
        .args(["--kb", "smoke", "--force"])
        .assert()
        .success();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_does_not_capture_process_metrics_state() {
    // Contract: `RequestMetrics` (AtomicU64 counters + per-route latency
    // histograms) is transient process RAM, never persisted. A backup
    // tarball must therefore contain no `metrics.*` / `requests_total`
    // entry. Catches a future regression that serialises metrics to disk.
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = stage_state(tmp.path()).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("smoke-no-metrics.tar.gz");
    run_backup(&state, &config, &cache, "smoke", &out_path).success();

    let listing = StdCommand::new("tar")
        .arg("-tzf")
        .arg(&out_path)
        .output()
        .expect("tar -tzf failed to start");
    assert!(listing.status.success(), "tar -tzf failed");
    let listing = String::from_utf8_lossy(&listing.stdout);
    for line in listing.lines() {
        let lower = line.to_ascii_lowercase();
        assert!(
            !lower.contains("metrics"),
            "backup unexpectedly contains a metrics-y entry: {line}"
        );
        assert!(
            !lower.contains("requests_total"),
            "backup contains a counter snapshot: {line}"
        );
    }
}

/// Append `backup_toml` (e.g. a `[backup]` table) to the `kb.toml`
/// `stage_state` wrote — used by the GC-B4 off-host-copy tests below.
async fn stage_state_with_backup_config(
    tmp: &Path,
    backup_toml: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let (state, config) = stage_state(tmp).await;
    let cfg_path = config.join("kb.toml");
    let mut contents = fs::read_to_string(&cfg_path).unwrap();
    contents.push_str(backup_toml);
    fs::write(&cfg_path, contents).unwrap();
    (state, config)
}

/// Write an executable shell shim named `name` into `dir` with `body`
/// as its script — the "fake uploader" a `[backup] remote_cmd` points
/// at in the tests below.
fn write_shim(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_runs_configured_off_host_copy_and_reports_success() {
    // GC-B4 — a PATH-shimmed "fake uploader" (not an absolute path, so
    // this also pins that `remote_cmd`'s program is resolved via PATH,
    // matching how an operator would invoke a real `rclone`/`scp`
    // sitting on PATH) copies {src} to {dest}; `kb backup` must run it
    // after the local tarball lands and report success.
    let tmp = tempfile::tempdir().unwrap();
    let shim_dir = tmp.path().join("shim-bin");
    fs::create_dir_all(&shim_dir).unwrap();
    write_shim(&shim_dir, "fake-uploader", "#!/bin/sh\ncp \"$1\" \"$2\"\n");

    let remote_dest = tmp.path().join("off-host").join("copy.tar.gz");
    fs::create_dir_all(remote_dest.parent().unwrap()).unwrap();
    let backup_toml = format!(
        "\n[backup]\nremote_cmd = [\"fake-uploader\", \"{{src}}\", \"{{dest}}\"]\nremote_dest = \"{}\"\n",
        remote_dest.display()
    );
    let (state, config) = stage_state_with_backup_config(tmp.path(), &backup_toml).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("smoke-offhost-ok.tar.gz");

    let path_with_shim = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap());
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", &state)
        .env("KB_CONFIG_DIR", &config)
        .env("KB_CACHE_DIR", &cache)
        .env("PATH", &path_with_shim)
        .args(["backup", "smoke", "--out"])
        .arg(&out_path)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("off-host copy") && stdout.contains("ok"),
        "stdout should confirm the off-host copy: {stdout}"
    );
    assert!(remote_dest.exists(), "shim never wrote {{dest}}");
    assert_eq!(
        fs::read(&remote_dest).unwrap(),
        fs::read(&out_path).unwrap(),
        "off-host copy destination doesn't match the local tarball bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_surfaces_off_host_copy_failure_without_failing_the_backup() {
    // GC-B4 — the shim exits 1 (simulating an unreachable remote). The
    // local backup must still succeed (exit 0, tarball on disk); the
    // failure must be unmissable in the CLI output.
    let tmp = tempfile::tempdir().unwrap();
    let shim_dir = tmp.path().join("shim-bin");
    fs::create_dir_all(&shim_dir).unwrap();
    write_shim(
        &shim_dir,
        "fake-uploader",
        "#!/bin/sh\necho 'simulated network failure' >&2\nexit 1\n",
    );

    let backup_toml = "\n[backup]\nremote_cmd = [\"fake-uploader\", \"{src}\", \"{dest}\"]\n\
                        remote_dest = \"remote:bucket/path\"\n";
    let (state, config) = stage_state_with_backup_config(tmp.path(), backup_toml).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("smoke-offhost-fail.tar.gz");

    let path_with_shim = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap());
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", &state)
        .env("KB_CONFIG_DIR", &config)
        .env("KB_CACHE_DIR", &cache)
        .env("PATH", &path_with_shim)
        .args(["backup", "smoke", "--out"])
        .arg(&out_path)
        .assert()
        .success(); // the backup itself must still succeed

    assert!(
        out_path.exists(),
        "local backup must still be written when the off-host copy fails"
    );

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("simulated network failure")
            && stderr.to_ascii_lowercase().contains("fail"),
        "stderr must loudly surface the uploader failure: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.to_ascii_lowercase().contains("fail"),
        "stdout exit message must reflect the off-host failure: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_fails_clearly_when_kb_has_no_state_yet() {
    // Operator typo-protection: backing up a never-indexed kb should
    // surface a clean error, not produce an empty tarball.
    let tmp = tempfile::tempdir().unwrap();
    let (state, config) = stage_state(tmp.path()).await;
    let cache = tmp.path().join("cache");
    let out_path = tmp.path().join("ghost.tar.gz");
    run_backup(&state, &config, &cache, "ghost-kb", &out_path).failure();
    assert!(!out_path.exists(), "no tarball on failure");
}
