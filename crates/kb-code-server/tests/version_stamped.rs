//! VS1 — a stamped binary reports the stamp on `GET /api/identity`: a
//! non-empty version that is not `0.0.0`. Own test binary (process-wide
//! `OnceLock`); the unstamped fallback is pinned in `version_unstamped.rs`.

use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;

#[tokio::test]
async fn stamped_identity_reports_the_stamp() {
    kb_code_server::set_build_stamp("9.1-3-gabc1234", "abc123456789");
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
    let v = body["version"].as_str().unwrap();
    assert!(!v.is_empty());
    assert_ne!(v, "0.0.0");
    assert_eq!(v, "9.1-3-gabc1234");
    assert_eq!(body["build_sha"].as_str(), Some("abc123456789"));
}
