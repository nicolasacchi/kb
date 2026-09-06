//! L1 — the standalone `kb-server` binary writes ndjson logs under
//! `<state>/log/`. Mirrors kb-cli's `tests/daemon_logging.rs` (the other
//! wired entry point); level-gating precision is unit-pinned in
//! `kb_core::tracing_init`. Spawns the real binary — the in-process
//! `serve_*` helpers other suites use never run `main`'s tracing init.

use crate::common::{free_port, ServerProc};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn read_log(home: &Path) -> String {
    let dir = home.join("state").join("log-smoke-bin").join("log");
    let mut body = String::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().starts_with("kb.ndjson") {
                body.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
    }
    body
}

#[test]
fn kb_server_bin_writes_ndjson_log_file_at_info_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let corpus = home.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(
        corpus.join("smoke.html"),
        "<!doctype html><html><head><title>smoke</title></head><body><h1>smoke</h1></body></html>",
    )
    .unwrap();

    let cfg_path = home.join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"log-smoke-bin\"\n\n\
             [server]\naddr = \"127.0.0.1:{}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.smoke]\npath = \"{}\"\n",
            free_port(),
            corpus.display()
        ),
    )
    .unwrap();

    let stderr_path = home.join("server-stderr.log");
    let mut server = ServerProc {
        child: Command::new(env!("CARGO_BIN_EXE_kb-server"))
            .env("KB_HOME", home)
            .env_remove("KB_STATE_DIR")
            .env_remove("KB_CONFIG_DIR")
            .env_remove("KB_CACHE_DIR")
            .env_remove("RUST_LOG")
            .env_remove("KB_LOG_FILE_LEVEL")
            .args(["--config"])
            .arg(&cfg_path)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .expect("spawn kb-server"),
        stderr_path,
    };

    // Poll until the boot INFO line lands (or the process dies / 120 s).
    let deadline = Instant::now() + Duration::from_secs(120);
    let body = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!(
                "kb-server exited early ({status}); stderr:\n{}",
                std::fs::read_to_string(&server.stderr_path).unwrap_or_default()
            );
        }
        let body = read_log(home);
        if body.contains("kb-server listening") {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the boot INFO line; log so far:\n{body}\nstderr:\n{}",
            std::fs::read_to_string(&server.stderr_path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(200));
    };

    // Complete lines only (the daemon may be mid-write on the last one):
    // parseable ndjson, nothing below the info default.
    let upto = body.rfind('\n').map(|i| &body[..i]).unwrap_or("");
    for line in upto.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not ndjson ({e}): {line}"));
        let level = v["level"].as_str().unwrap_or_default();
        assert!(
            level != "DEBUG" && level != "TRACE",
            "sub-info line leaked past the default info filter: {line}"
        );
    }
}
