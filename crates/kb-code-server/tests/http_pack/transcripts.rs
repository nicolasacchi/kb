//! W2.5 — the raw-transcripts lane's daemon-level integration tests: the
//! `loopback_only` guard actually wired into the router (a non-loopback
//! request gets a 404 INDISTINGUISHABLE from an unknown route, even with a
//! valid bearer token), and the startup walk actually indexes a seeded
//! transcript fixture end-to-end over real HTTP. Unit-level parse/tail/FTS
//! coverage lives in `src/transcripts/{parse,indexer,search}.rs`; this file
//! only proves the WIRING (config → boot → route → guard).
//!
//! Uses the SAME `KbPaths::rooted_at`/XFF-spoofing techniques as
//! `tests/boot.rs` — see that file's doc for the rationale.

use kb_code_server::config::{KbCodeConfig, TranscriptsSection};
use kb_core::paths::KbPaths;

static ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

fn isolated_paths(root: &std::path::Path) -> KbPaths {
    KbPaths::rooted_at(root, "kb-code")
}

/// Seed one project dir + one session transcript with a planted, unusual
/// search term — the startup walk's fixture.
fn seed_transcript_root(root: &std::path::Path) {
    let proj = root.join("-fixture-project");
    std::fs::create_dir_all(&proj).unwrap();
    let line = r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s-fixture","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{"role":"user","content":"a very unusual gorgonzola marker string"}}"#;
    std::fs::write(proj.join("s-fixture.jsonl"), format!("{line}\n")).unwrap();
}

fn transcripts_config(root: &std::path::Path) -> TranscriptsSection {
    TranscriptsSection {
        enabled: true,
        root: root.to_string_lossy().into_owned(),
        exclude_projects: Vec::new(),
        index_thinking: true,
    }
}

/// Poll `GET /api/transcripts/status` (loopback) until `turns > 0` or a
/// generous deadline — the startup walk runs on its own thread, so this
/// bridges the boot/index race deterministically rather than a fixed sleep.
async fn wait_for_indexed(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/transcripts/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["turns"].as_u64().unwrap_or(0) > 0 || tokio::time::Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn loopback_search_finds_the_seeded_turn_and_status_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let transcripts_root = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    seed_transcript_root(&transcripts_root);

    let cfg = KbCodeConfig {
        transcripts: transcripts_config(&transcripts_root),
        ..KbCodeConfig::default()
    };
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let client = reqwest::Client::new();
    let status = wait_for_indexed(&client, addr).await;
    assert_eq!(status["turns"].as_u64(), Some(1), "status: {status}");
    assert_eq!(status["files"].as_u64(), Some(1));
    assert!(status["enabled"].as_bool().unwrap_or(false));

    let resp = client
        .get(url(addr, "/api/search/transcripts"))
        .query(&[("q", "gorgonzola")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1, "body: {body}");
    assert_eq!(hits[0]["session_id"].as_str(), Some("s-fixture"));
    assert_eq!(hits[0]["kind"].as_str(), Some("user"));
    assert!(hits[0]["snippet"].as_str().unwrap().contains("gorgonzola"));
}

#[tokio::test]
async fn non_loopback_request_gets_404_on_both_transcripts_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let transcripts_root = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    seed_transcript_root(&transcripts_root);

    let cfg = KbCodeConfig {
        transcripts: transcripts_config(&transcripts_root),
        ..KbCodeConfig::default()
    };
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let client = reqwest::Client::new();
    wait_for_indexed(&client, addr).await;

    // Simulated non-loopback client (peer=loopback is a trusted hop, so XFF
    // is consulted — same technique `tests/boot.rs` uses).
    let resp = client
        .get(url(addr, "/api/search/transcripts"))
        .query(&[("q", "gorgonzola")])
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "search must 404 a non-loopback caller");

    let resp = client
        .get(url(addr, "/api/transcripts/status"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "status must 404 a non-loopback caller");
}

#[tokio::test]
async fn a_valid_bearer_token_does_not_open_the_transcripts_lane_to_a_non_loopback_caller() {
    const FIXTURE_TOKEN: &str = "kb-code-transcripts-test-token";
    let _guard = ENV_TEST_LOCK.lock().await;
    std::env::remove_var("KB_ALLOW_NO_AUTH");
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let tmp = tempfile::tempdir().unwrap();
    let transcripts_root = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    seed_transcript_root(&transcripts_root);

    let cfg = KbCodeConfig {
        transcripts: transcripts_config(&transcripts_root),
        ..KbCodeConfig::default()
    };
    let boot =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path())).await;
    std::env::remove_var("KB_CODE_TOKEN");
    let (addr, _task) = boot.expect("serve_on_random_port_with_paths with KB_CODE_TOKEN set");

    let client = reqwest::Client::new();
    wait_for_indexed(&client, addr).await;

    // Ordinary /api/identity: a correct bearer token + simulated
    // non-loopback peer DOES get through (auth_bearer's normal contract —
    // sanity check that the token/XFF plumbing in this test is real).
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "sanity: bearer auth must work on ordinary routes"
    );

    // The SAME token + peer against the transcripts lane must still 404 —
    // loopback_only is not layered alongside auth_bearer, it REPLACES it
    // for these two routes.
    let resp = client
        .get(url(addr, "/api/search/transcripts"))
        .query(&[("q", "gorgonzola")])
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "a valid bearer token must NOT open the transcripts lane to a non-loopback caller"
    );
}
