//! V72-I1 — end-to-end tests for `rails/1` (`GET /api/rails/*`) against a
//! real daemon booted over the synthetic Rails app in
//! `tests/fixtures/rails-app/`.
//!
//! Three goldens (`tests/fixtures/rails-app-expected/{home,routes,orphans}
//! .json`) are byte-diffed after ONE scrub: the store's monotonic
//! `generation` counter, which is a property of how many times the daemon
//! indexed, not of the app. Everything else — every count, every trust
//! class, every witness, every honesty reason — is pinned verbatim, because
//! a silent change in any of them is exactly the regression these tests
//! exist to catch.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use crate::common::init_repo;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

const REPO: &str = "acme-app";
/// Every tracked file in the fixture tree — what `wait_for_indexed` waits
/// for before a single assertion runs.
const FIXTURE_FILES: usize = 21;

fn fixture_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rails-app")
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rails-app-expected")
        .join(name)
}

fn expected(name: &str) -> serde_json::Value {
    let p = golden_path(name);
    let raw =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read golden {}: {e}", p.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse golden {}: {e}", p.display()))
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// The fixture tree as a real git repo — the daemon indexes HEAD's tree at
/// boot, so an uncommitted copy would index nothing.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_src(), tmp.path());
    init_repo(tmp.path());
    common::git(tmp.path(), &["add", "-A"]);
    common::git(tmp.path(), &["commit", "-q", "-m", "acme-app fixture"]);
    tmp
}

struct Boot {
    #[allow(dead_code)]
    state_dir: tempfile::TempDir,
    #[allow(dead_code)]
    repo: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot() -> Boot {
    let repo = fixture_repo();
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: REPO.to_string(),
            path: std::fs::canonicalize(repo.path()).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let state_dir = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(state_dir.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    let base = format!("http://{addr}");
    wait_for_indexed(&base, REPO, FIXTURE_FILES).await;
    wait_for_rails_settled(&base, REPO).await;
    Boot {
        state_dir,
        repo,
        base,
        task,
    }
}

async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected repo {repo:?} to report file_count >= {expected_files} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `wait_for_indexed` is NOT enough: boot builds the `files` rows from
/// HEAD's tree first and the derived lanes (symbols, `rails_edges`,
/// `entity_defs`) land afterwards, so a `rails/1` read taken the moment the
/// file count is right can legitimately see a half-derived index — which is
/// exactly how the first capture of these goldens recorded `route: 0`.
///
/// The readiness signal is the fixture's own shape: this app has at least
/// one row of EVERY noun, so "all eight counts are non-zero" cannot settle
/// early, and one further identical read guards against catching the
/// pipeline mid-write. A fixture change that empties a noun fails here, by
/// name, rather than silently re-introducing the flake.
///
/// **V72-I2 — the counts alone were not enough EITHER, and the gap is worth
/// naming because it is not obvious.** Every noun count is derived from
/// `files` rows and `entity_defs`, both of which land in the mirror walk;
/// `rails_edges` land LATER, in a separate pass. So the eight counts reach
/// their final values — and stay there across two reads — while the lens is
/// still writing edges. On a slow runner that is a real window, and CI
/// caught it: `counts` byte-identical to the golden, `lens.edges_total` 23
/// against the golden's 26 and `source_files` 7 against 8, i.e. the HAML
/// view's edges had simply not landed yet. Nothing about the counts could
/// ever have detected that, because no noun count moves when an edge is
/// added.
///
/// The settle key therefore includes the LENS's own freshness numbers
/// (`edges_total` + `source_files`), which is the thing the three goldens
/// and `the_rails_lens_reads_haml_through_the_real_ingest_path` actually
/// depend on. Deliberately still fixture-AGNOSTIC — no magic edge count is
/// hard-coded here, only "these numbers stopped moving" — so a fixture that
/// grows a file needs no edit, while a lens that genuinely never emits the
/// HAML edges now fails as a NAMED timeout printing the numbers rather than
/// as a golden diff three tests later.
async fn wait_for_rails_settled(base: &str, repo: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut previous: Option<serde_json::Value> = None;
    loop {
        let body = client
            .get(format!("{base}/api/rails/home?repo={repo}"))
            .send()
            .await
            .ok()
            .and_then(|r| r.status().is_success().then_some(r));
        let body = match body {
            Some(r) => r.json::<serde_json::Value>().await.ok(),
            None => None,
        };
        // The settle KEY: the eight noun counts plus the lens's own two
        // freshness numbers. See this function's doc for why the counts on
        // their own are blind to the edge pass.
        let key = body.map(|b| {
            serde_json::json!({
                "counts": b["counts"].clone(),
                "edges_total": b["lens"]["edges_total"].clone(),
                "source_files": b["lens"]["source_files"].clone(),
            })
        });
        if let Some(key) = key {
            let complete = key["counts"]
                .as_object()
                .is_some_and(|o| o.len() == 8 && o.values().all(|v| v.as_u64().unwrap_or(0) > 0))
                && key["edges_total"].as_u64().unwrap_or(0) > 0
                && key["source_files"].as_u64().unwrap_or(0) > 0;
            if complete && previous.as_ref() == Some(&key) {
                return;
            }
            previous = Some(key);
        }
        assert!(
            Instant::now() < deadline,
            "the rails/1 index never settled for {repo:?} — last reading: {previous:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn get(base: &str, path: &str) -> serde_json::Value {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    // The BODY is in the message on purpose: this daemon's 404s are
    // ambiguous by design (an unregistered route falls through to the SPA
    // handler, which 404s too), so a bare status tells you nothing about
    // which one you hit.
    let body = resp.text().await.unwrap_or_default();
    assert!(status.is_success(), "{path} → {status}\n{body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: json body: {e}\n{body}"))
}

/// The ONE scrub: `lens.generation` is a property of how many times this
/// daemon indexed, not of the application, so it cannot be pinned. Nothing
/// else is touched.
fn scrub(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(g) = v.pointer_mut("/lens/generation") {
        *g = serde_json::json!(0);
    }
    v
}

/// Byte-diff against the golden — or, with `KBC_WRITE_GOLDEN=1`, REGENERATE
/// it. That path exists because these goldens are the daemon's own output
/// over a fixture and cannot be hand-authored honestly; it is an explicit
/// env-var opt-in, never the default, and what it writes is meant to be READ
/// in the diff before it is committed (the posture `tests/rails_lens.rs`'s
/// own "regenerate the golden after manual review" message already takes).
fn assert_golden(name: &str, got: serde_json::Value) {
    let got = scrub(got);
    if std::env::var("KBC_WRITE_GOLDEN").is_ok() {
        let p = golden_path(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        eprintln!("KBC_WRITE_GOLDEN: wrote {} — review the diff", p.display());
        return;
    }
    let want = expected(name);
    if got != want {
        panic!(
            "{name} drifted.\n--- got ---\n{}\n--- want ---\n{}\n\nIf the change is intended, \
             review it and rewrite the golden.",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(&want).unwrap()
        );
    }
}

#[tokio::test]
async fn rails_home_passport_golden() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let body = get(&boot.base, &format!("/api/rails/home?repo={REPO}")).await;
    assert_golden("home.json", body);
}

#[tokio::test]
async fn rails_routes_list_golden() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let body = get(&boot.base, &format!("/api/rails/routes?repo={REPO}")).await;
    assert_golden("routes.json", body);
}

#[tokio::test]
async fn rails_orphans_report_golden() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let body = get(&boot.base, &format!("/api/rails/orphans?repo={REPO}")).await;
    assert_golden("orphans.json", body);
}

/// The oracle bar, on the wire: no row of any noun may carry `exact`.
/// `noun_trust`'s return type already makes it unrepresentable — this walks
/// the SERIALISED surface, which is what a consumer actually reads.
#[tokio::test]
async fn no_rails_row_on_the_wire_is_ever_exact() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let mut seen = 0usize;
    for noun in [
        "models",
        "controllers",
        "actions",
        "routes",
        "jobs",
        "mailers",
        "views",
        "concerns",
    ] {
        let body = get(
            &boot.base,
            &format!("/api/rails/{noun}?repo={REPO}&limit=1000"),
        )
        .await;
        for row in body["rows"].as_array().expect("rows array") {
            seen += 1;
            let trust = row["trust"]
                .as_str()
                .expect("every row carries a trust class");
            assert!(
                trust == "likely" || trust == "candidate",
                "{noun}: {} carries {trust:?} — rails/1 is capped at likely by construction",
                row["name"]
            );
            for w in row["witnesses"].as_array().expect("witnesses array") {
                if let Some(t) = w["trust"].as_str() {
                    assert_ne!(t, "exact", "{noun}: a witness claimed exact");
                }
            }
        }
    }
    let orphans = get(&boot.base, &format!("/api/rails/orphans?repo={REPO}")).await;
    for lane in orphans["lanes"].as_array().expect("lanes") {
        for row in lane["rows"].as_array().expect("lane rows") {
            seen += 1;
            let trust = row["trust"].as_str().expect("trust");
            assert!(trust == "likely" || trust == "candidate", "{trust:?}");
        }
    }
    assert!(
        seen > 20,
        "the fixture produced too few rows to be a real walk"
    );
}

/// The page controls are honest: `total` is the TRUE post-filter total and
/// `truncated` names the gap, rather than a silently short page.
#[tokio::test]
async fn a_capped_page_reports_the_true_total_and_says_it_truncated() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let full = get(
        &boot.base,
        &format!("/api/rails/routes?repo={REPO}&limit=1000"),
    )
    .await;
    let total = full["total"].as_u64().unwrap();
    assert!(total >= 2, "the fixture declares several routes");

    let page = get(
        &boot.base,
        &format!("/api/rails/routes?repo={REPO}&limit=1"),
    )
    .await;
    assert_eq!(page["total"], serde_json::json!(total));
    assert_eq!(page["returned"], serde_json::json!(1));
    assert_eq!(page["truncated"], serde_json::json!(true));
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);

    let last = get(
        &boot.base,
        &format!(
            "/api/rails/routes?repo={REPO}&limit=1000&offset={}",
            total - 1
        ),
    )
    .await;
    assert_eq!(last["truncated"], serde_json::json!(false));

    // `q=` narrows BEFORE the total is counted, so the total stays true.
    let filtered = get(
        &boot.base,
        &format!("/api/rails/models?repo={REPO}&q=order"),
    )
    .await;
    assert!(filtered["total"].as_u64().unwrap() >= 1);
    for row in filtered["rows"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap().to_lowercase();
        let path = row["path"].as_str().unwrap().to_lowercase();
        assert!(name.contains("order") || path.contains("order"));
    }
}

/// A repo that is not a Rails app answers `empty` WITH A REASON, never a
/// bare empty list and never a 500.
#[tokio::test]
async fn a_non_rails_repo_is_empty_with_a_reason() {
    let _g = SERIAL.lock().await;
    let plain = tempfile::tempdir().unwrap();
    init_repo(plain.path());
    std::fs::write(plain.path().join("lib.rs"), "fn a() {}\n").unwrap();
    common::git(plain.path(), &["add", "-A"]);
    common::git(plain.path(), &["commit", "-q", "-m", "c1"]);

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "plain".to_string(),
            path: std::fs::canonicalize(plain.path()).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let state_dir = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(state_dir.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    let base = format!("http://{addr}");

    let body = get(&base, "/api/rails/home?repo=plain").await;
    assert_eq!(body["detected"], serde_json::json!(false));
    assert_eq!(body["honesty"]["state"], serde_json::json!("empty"));
    assert!(body["honesty"]["reason"]
        .as_str()
        .unwrap()
        .contains("config/routes.rb"));
    assert_eq!(body["rails_version"], serde_json::Value::Null);

    let models = get(&base, "/api/rails/models?repo=plain").await;
    assert_eq!(models["rows"].as_array().unwrap().len(), 0);
    assert_eq!(models["honesty"]["state"], serde_json::json!("empty"));
}

/// An unknown repo is a 404 naming it — not an empty page pretending the
/// repo exists and has no Rails in it.
#[tokio::test]
async fn an_unknown_repo_is_a_404() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/api/rails/home?repo=nope", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// The kbcq/1 Rails facet atoms narrow the unified search lanes, and the
/// narrowed section says what narrowed it.
#[tokio::test]
async fn a_rails_facet_atom_narrows_search_and_captions_the_lane() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;

    let narrowed = get(
        &boot.base,
        &format!("/api/search?repo={REPO}&q=%23order%20model%3AOrder"),
    )
    .await;
    let files = narrowed["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["lane"] == "files")
        .expect("a files section");
    let caption = files["caption"]
        .as_str()
        .expect("a narrowed lane is captioned");
    assert!(caption.contains("model"), "{caption:?}");
    for hit in files["results"].as_array().unwrap() {
        assert!(
            hit["path"].as_str().unwrap().starts_with("app/models/"),
            "model:Order kept a non-model file: {}",
            hit["path"]
        );
    }

    // No atom at all ⇒ no caption at all: a pre-V72-I1 response shape.
    let plain = get(&boot.base, &format!("/api/search?repo={REPO}&q=%23order")).await;
    let files = plain["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["lane"] == "files")
        .unwrap();
    assert!(
        files.get("caption").is_none(),
        "an unnarrowed section must carry no caption field at all"
    );
}

/// The per-request join is the design decision this unit recorded; this is
/// the measurement behind it. Not a hard assertion on wall-clock (CI
/// runners vary wildly) — it PRINTS the number so a regression is visible
/// in the log, and fails only on a ceiling no healthy machine approaches.
#[tokio::test]
async fn the_whole_index_answers_inside_a_generous_ceiling() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;
    // Warm the caches the way a second request would find them (the
    // Zeitwerk and locale indexes are both fingerprint-cached per repo
    // root), then measure the steady state.
    let _ = get(&boot.base, &format!("/api/rails/home?repo={REPO}")).await;
    let started = Instant::now();
    for _ in 0..5 {
        let _ = get(&boot.base, &format!("/api/rails/home?repo={REPO}")).await;
    }
    let per_call = started.elapsed() / 5;
    println!("rails/1 home: {per_call:?} per call over the acme-app fixture");
    assert!(
        per_call < Duration::from_millis(2000),
        "the per-request join took {per_call:?} on a 19-file fixture — something is scanning"
    );
}

/// V72-I2 — **HAML in the Rails lens, proven through the SHIPPED path.**
///
/// The dispatch code was already right at V72-I1: `rails::extract` has a
/// `.haml` arm, `rails_lens_relevant_path` lists both `.haml` predicates,
/// `lang::detect` resolves `.haml` to the first-party scanner, and
/// `find_view_files` matches on a template's STEM rather than its extension.
/// What was missing was a test that any of that survives the REAL pipeline:
/// every fixture that runs through `ingest::index_file` → `replace_rails_edges`
/// → `GET /api/rails/*` was 100% ERB, and the only HAML lens assertions
/// (`tests/haml_corpus.rs`) build their own tempdir and call
/// `frameworks::extract_edges` DIRECTLY — bypassing `is_rails`,
/// `rails_lens_relevant_path`, `lang::detect`, the store and the daemon. So a
/// regression that dropped `.haml` from any of those gates would have left
/// `just ci-code` green.
///
/// This test closes that seam, on the two claims a reader of `~rails`
/// actually depends on:
///
///  * a template RENDERED FROM HAML is not in the `view_never_rendered`
///    orphan lane (`summary.html.haml` renders `_haml_row.html.haml`, and
///    `OrdersController#summary`'s own implicit convention render resolves
///    the `.haml` template itself);
///  * a locale key referenced ONLY from HAML is not in the
///    `locale_key_never_referenced` lane — including the LAZY `t(".heading")`
///    form, whose scope is derived from the view path (`view_relative_scope`
///    splits on the first dot, so `.haml` and `.erb` reduce identically).
///
/// The negative control rides along: `orders.unused_key` IS referenced
/// nowhere and must still be reported, so a bug that emptied the lane
/// entirely cannot pass this test either.
#[tokio::test]
async fn the_rails_lens_reads_haml_through_the_real_ingest_path() {
    let _g = SERIAL.lock().await;
    let boot = boot().await;

    const HAML_VIEW: &str = "app/views/orders/summary.html.haml";
    const HAML_PARTIAL: &str = "app/views/orders/_haml_row.html.haml";

    // 1 — both templates are indexed as views at all.
    let views = get(
        &boot.base,
        &format!("/api/rails/views?repo={REPO}&limit=1000"),
    )
    .await;
    let rows = views["rows"].as_array().expect("view rows");
    let find = |path: &str| {
        rows.iter()
            .find(|r| r["path"] == serde_json::json!(path))
            .unwrap_or_else(|| panic!("{path} is missing from /api/rails/views"))
    };
    let view = find(HAML_VIEW);
    let partial = find(HAML_PARTIAL);

    // 2 — the render edge the HAML template itself produced. This is the
    // claim V72-I1 could not check: the partial's inbound count comes from a
    // `render "haml_row"` written in HAML, walked by
    // `support::walk_haml_ruby_fragments`.
    assert!(
        partial["counts"]["rendered_by"].as_u64().unwrap_or(0) >= 1,
        "no render edge reaches {HAML_PARTIAL} — the lens did not read the HAML template that \
         renders it: {partial}"
    );
    assert!(
        view["counts"]["rendered_by"].as_u64().unwrap_or(0) >= 1,
        "no render edge reaches {HAML_VIEW} — OrdersController#summary's implicit convention \
         render did not resolve a .haml template: {view}"
    );

    // 3 — and therefore neither is an orphan.
    let orphans = get(&boot.base, &format!("/api/rails/orphans?repo={REPO}")).await;
    let lane = |id: &str| {
        orphans["lanes"]
            .as_array()
            .expect("lanes")
            .iter()
            .find(|l| l["id"] == serde_json::json!(id))
            .unwrap_or_else(|| panic!("lane {id} is missing"))
    };
    let never_rendered = lane("view_never_rendered");
    for path in [HAML_VIEW, HAML_PARTIAL] {
        assert!(
            !never_rendered["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == serde_json::json!(path)),
            "{path} was reported as never rendered: {never_rendered}"
        );
    }

    // 4 — the i18n keys reached only from HAML, absolute and LAZY.
    let locale = lane("locale_key_never_referenced");
    let unused: Vec<&str> = locale["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    for key in ["orders.summary.heading", "orders.summary.link"] {
        assert!(
            !unused.contains(&key),
            "{key} is referenced only from HAML and was reported unused — the lens did not read \
             the template's `t(...)` calls. Lane rows: {unused:?}"
        );
    }
    // The negative control: a key nothing references must STILL be reported.
    assert!(
        unused.contains(&"orders.unused_key"),
        "the unused-locale lane reported nothing at all, so the assertions above prove nothing: \
         {unused:?}"
    );

    // 5 — the route + action the HAML view hangs off resolve too, so the
    // fixture cannot silently stop exercising the implicit-render path.
    let actions = get(
        &boot.base,
        &format!("/api/rails/actions?repo={REPO}&q=summary&limit=1000"),
    )
    .await;
    assert!(
        actions["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == serde_json::json!("orders#summary")),
        "orders#summary is missing from /api/rails/actions: {actions}"
    );
}
