//! VS1 — with no build stamp installed (dev build, embedded consumer) the
//! identity route reports the documented dev fallback, never a bare `0.0.0`.
//! Own test binary: the stamp is a process-wide `OnceLock`, so the stamped
//! case lives in `version_stamped.rs`.

use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;

#[tokio::test]
async fn unstamped_identity_reports_the_documented_dev_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(KbCodeConfig::default(), paths)
            .await
            .unwrap();
    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/identity"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["version"].as_str(), Some("0.0.0-dev"));
    assert_eq!(
        body["version"].as_str(),
        Some(kb_code_server::UNSTAMPED_VERSION)
    );
    assert_eq!(body["build_sha"].as_str(), Some("dev"));
}
