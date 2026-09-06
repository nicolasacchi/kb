//! W4.1 (security-review Finding 3) — regression test for `spa::serve`'s
//! client-route carve-out (`spa::is_client_route`): a hard navigate/refresh
//! on a dotted `/r/{repo}/*` reader URL (a real repo source-file path, e.g.
//! `.../src/app.tsx`) must serve the SPA shell rather than 404 (the bug —
//! `has_extension` alone can't tell that apart from a missing built asset);
//! a real built asset under `/assets/...` must still resolve from disk; and
//! a genuinely missing dotted path OUTSIDE the `/r/...` carve-out must still
//! 404 (the fix must not turn into "any dotted path falls back to the
//! shell").
//!
//! Boots a real daemon via `serve_on_random_port_with_paths` with
//! `KB_CODE_SPA_DIST` pointed at a hand-built fixture dist dir — the same
//! env-var override `spa::resolve_spa_dist`'s own unit tests use, just
//! exercised over real HTTP. The env var is only ever read once, at boot
//! (`bind_and_spawn` → `spa::resolve_spa_dist` → baked into
//! `AppState::spa_dist`), so it's safe to unset again immediately after
//! `serve_on_random_port_with_paths` returns and before this file's one test
//! makes any request — no cross-test env lock needed (this file boots
//! exactly once).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;

fn write_fixture_dist(root: &std::path::Path) {
    std::fs::write(
        root.join("index.html"),
        b"<html><head><title>kb-code</title></head><body><div id=\"root\"></div></body></html>",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(root.join("assets").join("main-abc.js"), b"console.log(1);").unwrap();
}

#[tokio::test]
async fn spa_fallback_serves_client_route_asset_and_404_precedence_correctly() {
    let dist_tmp = tempfile::tempdir().unwrap();
    write_fixture_dist(dist_tmp.path());
    std::env::set_var("KB_CODE_SPA_DIST", dist_tmp.path());

    let state_tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(state_tmp.path(), "kb-code");
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(KbCodeConfig::default(), paths)
            .await
            .expect("serve_on_random_port_with_paths");
    // Only consulted at boot (see the module doc) — safe to clear now.
    std::env::remove_var("KB_CODE_SPA_DIST");

    let client = reqwest::Client::new();

    // 1. A dotted `/r/{repo}/*` reader URL must serve the shell (200,
    //    text/html), not 404 — the fix under test.
    let resp = client
        .get(format!("http://{addr}/r/kb/blob/main/src/app.tsx"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("<div id=\"root\"></div>"), "got: {body}");

    // 2. A real built asset still resolves from disk — the fix must not
    //    regress the ordinary asset path.
    let resp = client
        .get(format!("http://{addr}/assets/main-abc.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "console.log(1);");

    // 3. A dotted path OUTSIDE the `/r/{repo}/*` carve-out that isn't a real
    //    asset must still 404.
    let resp = client
        .get(format!("http://{addr}/no-such.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
