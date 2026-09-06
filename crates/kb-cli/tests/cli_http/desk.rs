//! `kb desk` HTTP-backed tests: offer, update, expire, promote, ls.
use crate::common::{self, canon_dir};

use assert_cmd::Command;
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

async fn boot() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in ["fullscreen-viz.html", "kitchen-sink.html"] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let daemon_name = format!(
        "cli-desk-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), common::kb_section(source));
    let cfg = common::base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    (tmp, format!("http://{addr}"))
}

fn kb_cmd(url: &str, args: &[&str]) -> Command {
    let mut full = vec!["desk"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--daemon", url]);
    let mut cmd = Command::cargo_bin("kb").unwrap();
    cmd.env("KB_TEST_HTTP_TIMEOUT_SECS", common::http_timeout_secs())
        .args(&full);
    cmd
}

fn run_ok(url: &str, args: &[&str]) -> String {
    let out = kb_cmd(url, args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_offer_prints_id_permalink_and_wait_hint() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# Ticket\n\nbody").unwrap();

    let out = run_ok(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "ticket-123",
            "--kb",
            "smoke",
            "--title",
            "Ticket",
        ],
    );
    assert!(out.contains("ticket-123"), "got: {out}");
    assert!(out.contains("handoff/ticket-123.md"), "got: {out}");
    assert!(out.contains("/a/smoke/"), "got: {out}");
    assert!(
        out.contains("kb desk wait --path handoff/ticket-123.md --once --timeout 900"),
        "got: {out}"
    );
    let written = tmp.path().join("corpus/handoff/ticket-123.md");
    assert!(written.exists(), "missing {}", written.display());
    let body = std::fs::read_to_string(&written).unwrap();
    assert!(body.contains("kb-category: handoff"), "{body}");
    assert!(body.contains("draft"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_offer_passes_session_and_ttl() {
    let (tmp, url) = boot().await;
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(cache.join("kb")).unwrap();
    std::fs::write(cache.join("kb/current-session"), "sess-desk-1\n").unwrap();
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# S\n").unwrap();

    let out = kb_cmd(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "sess-ttl",
            "--kb",
            "smoke",
            "--ttl",
            "45m",
            "--json",
        ],
    )
    .env("XDG_CACHE_HOME", &cache)
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\"created\": true") || out.contains("\"created\":true"),
        "{out}"
    );
    let written = std::fs::read_to_string(tmp.path().join("corpus/handoff/sess-ttl.md")).unwrap();
    assert!(written.contains("kb-session: sess-desk-1"), "{written}");
    assert!(written.contains("kb-expires-at:"), "{written}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_offer_ttl_garbage_fails_locally() {
    let err = Command::cargo_bin("kb")
        .unwrap()
        .args(["desk", "offer", "-", "--as", "x", "--ttl", "nope"])
        .write_stdin("# hi\n")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("ttl"), "got: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_update_replaces_bytes() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# First\n").unwrap();
    let offered = run_ok(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "upd",
            "--kb",
            "smoke",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&offered).unwrap();
    let id = v["id"].as_str().unwrap().to_string();

    let client = reqwest::Client::new();
    common::poll_until(&format!("desk artifact {id} indexed"), || {
        let client = &client;
        let url = url.clone();
        let id = id.clone();
        async move {
            let resp = client
                .get(format!("{url}/api/kb/smoke/docs/{id}"))
                .send()
                .await
                .ok()?;
            if resp.status().is_success() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;

    let src2 = tmp.path().join("draft2.md");
    std::fs::write(&src2, "# Second\n").unwrap();
    let out = run_ok(
        &url,
        &[
            "update",
            &id,
            src2.to_str().unwrap(),
            "--kb",
            "smoke",
            "--json",
        ],
    );
    assert!(out.contains(&id), "{out}");
    let written = std::fs::read_to_string(tmp.path().join("corpus/handoff/upd.md")).unwrap();
    assert!(written.contains("# Second"), "{written}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_expire_refuses_open_comments() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# C\n").unwrap();
    let offered = run_ok(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "exp",
            "--kb",
            "smoke",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&offered).unwrap();
    let id = v["id"].as_str().unwrap().to_string();

    let client = reqwest::Client::new();
    let add = client
        .post(format!("{url}/api/kb/smoke/review/{id}/comments"))
        .json(&serde_json::json!({
            "body": "please fix",
            "author": "you",
            "anchor": {"kind": "file"}
        }))
        .send()
        .await
        .unwrap();
    assert!(
        add.status().is_success(),
        "add comment: {}",
        add.text().await.unwrap_or_default()
    );

    let err = kb_cmd(&url, &["expire", &id, "--kb", "smoke", "--yes"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("open comment"), "got: {err}");
    assert!(tmp.path().join("corpus/handoff/exp.md").exists());

    run_ok(&url, &["expire", &id, "--kb", "smoke", "--force", "--yes"]);
    assert!(!tmp.path().join("corpus/handoff/exp.md").exists());
}

async fn wait_indexed(url: &str, id: &str) {
    let client = reqwest::Client::new();
    common::poll_until(&format!("desk artifact {id} indexed"), || {
        let client = &client;
        let url = url.to_string();
        let id = id.to_string();
        async move {
            let resp = client
                .get(format!("{url}/api/kb/smoke/docs/{id}"))
                .send()
                .await
                .ok()?;
            if resp.status().is_success() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_promote_moves_and_drops_draft_tag() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# Promo\n\nbody").unwrap();
    let offered = run_ok(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "promo",
            "--kb",
            "smoke",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&offered).unwrap();
    let old_id = v["id"].as_str().unwrap().to_string();
    wait_indexed(&url, &old_id).await;

    let out = run_ok(
        &url,
        &[
            "promote",
            &old_id,
            "--to",
            "notes/promo.md",
            "--category",
            "note",
            "--kb",
            "smoke",
            "--json",
        ],
    );
    let body: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(body["kb"], "smoke");
    assert_eq!(body["old_id"], old_id);
    assert_eq!(body["old_source_relative"], "handoff/promo.md");
    assert_eq!(body["source_relative"], "notes/promo.md");
    let new_id = body["new_id"].as_str().unwrap();
    assert!(!new_id.is_empty());
    assert_ne!(new_id, old_id);
    assert!(
        body["url"]
            .as_str()
            .unwrap()
            .contains("/a/smoke/notes/promo.md"),
        "{out}"
    );

    assert!(!tmp.path().join("corpus/handoff/promo.md").exists());
    let dest = tmp.path().join("corpus/notes/promo.md");
    assert!(dest.exists(), "missing {}", dest.display());
    let written = std::fs::read_to_string(&dest).unwrap();
    assert!(written.contains("kb-category: note"), "{written}");
    let tags_line = written
        .lines()
        .find(|l| l.starts_with("kb-tags:"))
        .unwrap_or("");
    assert!(
        !tags_line.split([',', ' ', ':']).any(|t| t == "draft"),
        "draft tag still present: {written}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_promote_refuses_non_handoff_source() {
    let (_tmp, url) = boot().await;
    let client = reqwest::Client::new();
    common::poll_until("kitchen-sink indexed", || {
        let client = &client;
        let url = url.clone();
        async move {
            let resp = client
                .get(format!("{url}/api/kb/smoke/lookup?q=kitchen-sink.html"))
                .send()
                .await
                .ok()?;
            let body: serde_json::Value = resp.json().await.ok()?;
            if body["kind"].as_str() == Some("exact")
                || body["kind"].as_str() == Some("unique_suffix")
            {
                Some(())
            } else {
                None
            }
        }
    })
    .await;

    let err = kb_cmd(
        &url,
        &[
            "promote",
            "kitchen-sink.html",
            "--to",
            "notes/x.md",
            "--kb",
            "smoke",
        ],
    )
    .assert()
    .failure()
    .get_output()
    .stderr
    .clone();
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("not a desk artifact"), "got: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_promote_refuses_to_handoff() {
    let err = kb_cmd(
        "http://127.0.0.1:9",
        &[
            "promote",
            "abc123def456",
            "--to",
            "handoff/x.md",
            "--kb",
            "smoke",
        ],
    )
    .assert()
    .failure()
    .get_output()
    .stderr
    .clone();
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("handoff/"), "got: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desk_ls_rides_aggregate_and_all() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("draft.md");
    std::fs::write(&src, "# Ls\n").unwrap();
    let offered = run_ok(
        &url,
        &[
            "offer",
            src.to_str().unwrap(),
            "--as",
            "listed",
            "--kb",
            "smoke",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&offered).unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    wait_indexed(&url, &id).await;

    let json_out = run_ok(&url, &["ls", "--kb", "smoke", "--json"]);
    let body: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    assert!(body.get("items").is_some(), "wire shape: {json_out}");
    assert!(body.get("attention").is_some(), "wire shape: {json_out}");
    let items = body["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|i| i["source_relative"] == "handoff/listed.md"),
        "{json_out}"
    );
    assert!(
        items.iter().all(|i| i["source_relative"]
            .as_str()
            .unwrap_or("")
            .starts_with("handoff/")),
        "{json_out}"
    );

    let all_out = run_ok(&url, &["ls", "--all", "--json"]);
    let all_body: serde_json::Value = serde_json::from_str(&all_out).unwrap();
    let all_items = all_body["items"].as_array().unwrap();
    assert!(all_items.iter().any(|i| i["kb"] == "smoke"), "{all_out}");

    let human = run_ok(&url, &["ls", "--kb", "smoke"]);
    assert!(human.contains("handoff/listed.md"), "{human}");
    assert!(human.contains("attention:"), "{human}");
    assert!(
        human.contains('*') || human.contains("never-opened"),
        "{human}"
    );
}
