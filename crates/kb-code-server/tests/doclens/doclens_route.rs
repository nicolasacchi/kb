//! DCB W1.C — end-to-end HTTP tests for the doc-lens lane
//! (`GET /api/doc-lens`, `GET /api/doc-lens/repos`,
//! `PUT`/`DELETE /api/doc-lens/pin`) against a real daemon booted via
//! `serve_on_random_port_with_paths`, two real git fixture repos, and a MOCK
//! kb daemon serving the frozen `coderef/1` fixture.
//!
//! Two families live here:
//!
//! 1. **The golden table** — every `path_state × line_state` combination
//!    pinned INDIVIDUALLY against the `alpha` fixture repo, plus the
//!    scorecard over `alpha` (clean) and `beta` (dirty). The expected map is
//!    a table constant and the counts are DERIVED from it, so a fixture edit
//!    can never silently desync the table from the assertion.
//! 2. **The CORS probes, including the negatives** — the layer must be
//!    scoped to the doc-lens sub-routers ONLY. `/api/file` and
//!    `/api/search/transcripts` carrying `access-control-allow-origin` would
//!    hand any allowlisted page the full contents of every configured repo
//!    and the raw transcript lane; `cors_layer_route_set_is_pinned` asserts
//!    the ACAO-carrying route set is EXACTLY the doc-lens routes meant to be
//!    on it (four in W1.C/W2.A, five since SL7e's path lens) so a later
//!    `/doc-lens/*` route cannot join it by accident.
//!
//! `[kb_daemon]` is pointed at the mock (or explicitly disabled) on every
//! boot here — this suite must never risk reaching a real kb daemon that
//! happens to be listening on `127.0.0.1:4000`.

use crate::common::git;
use axum::{
    extract::Path as AxPath,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use kb_code_server::config::{DoclensSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- fixture repos ---------------------------------------------------------

fn write(dir: &Path, rel: &str, body: &str) {
    let abs = dir.join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, body).unwrap();
}

/// `alpha`'s `carts_controller.rb` — 39 lines, transcribed from the real
/// checkout. `algolia_user_token` is at 12 and NOWHERE else in the file;
/// `algolia_query_id` is at 23 AND 34 (each unique inside its own ±3 window,
/// which is the window-scoped-uniqueness property golden #1 pins);
/// `add_cart_bundle` is at 30 (the drift case cites 19).
const CARTS: &str = r#"# frozen_string_literal: true

class CartsController < ApplicationController
  include SharedControllerConcern

  def checkout
    return unless current_cart

    order = CreateOrderService.new(
      current_cart,
      current_user,
      algolia_user_token:,
      cookies:
    ).call

    redirect_to cart_path unless order
  end

  def add_cart_product(product, quantity)
    response = UpdateCartService.new(cart: current_cart).upsert_product(
      minsan: product.minsan,
      options: {
        algolia_query_id: params[:query_id],
        origin: params[:source]
      }
    )
    render_updated_cart(response:)
  end

  def add_cart_bundle(bundle, quantity)
    response = UpdateCartService.new(cart: current_cart).upsert_bundle(
      code: bundle.code,
      options: {
        algolia_query_id: params[:query_id]
      }
    )
    render_updated_cart(response:)
  end
end
"#;

/// `BATCH_SIZE` at 5 (and again at 12, outside the ±3 window of 5);
/// `dispatch_conversion` at 13 (and again at 19, outside the ±3 window of 13).
const CONVERSION: &str = r#"# frozen_string_literal: true

module AlgoliaEventsDispatcher
  class Conversion
    BATCH_SIZE = 20

    def initialize(client)
      @client = client
    end

    def dispatch(events)
      events.each_slice(BATCH_SIZE) do |batch|
        dispatch_conversion(batch)
      end
    end

    private

    def dispatch_conversion(batch)
      @client.send_events(
        batch.map { |e| { event_type: 'conversion', user_token: e.user_token } }
      )
    end
  end
end
"#;

/// Purpose-built for the ambiguity arm: `filters` occurs on 3, 4, 7, 8, 11
/// and 12, so a hint of 8 has TWO hits inside ±3 and is honestly
/// unverifiable. 15 lines total, so a hint of 99 reports `file_lines: 15`.
const SEARCH_SERVICE: &str = r#"module Algolia
  class SearchService
    def listable_results(filters)
      index.search(query, filters:)
    end

    def facet_results(filters)
      index.search(query, filters:)
    end

    def raw_results(filters)
      index.search(query, filters:)
    end
  end
end
"#;

const RECOMMEND: &str = r#"module Algolia
  class RecommendService
    def recommendations(minsan)
      client.get_recommendations(minsan)
    end
  end
end
"#;

const LEGACY_RECOMMEND: &str = r#"module Algolia
  class LegacyRecommendService
    def recommendations(minsan)
      client.legacy_recommendations(minsan)
    end
  end
end
"#;

fn concern(name: &str) -> String {
    format!("module {name}\n  module Algolia\n    extend ActiveSupport::Concern\n  end\nend\n")
}

/// `algolia.rb` → 6 candidates, `upsell.html.erb` → 3, everything else
/// unique — the real checkout's own numbers.
fn fixture_alpha() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(dir, "app/controllers/carts_controller.rb", CARTS);
    // The near-miss the `/`-anchored suffix rule must never match.
    write(
        dir,
        "spec/controllers/carts_controller_spec.rb",
        "RSpec.describe CartsController do\nend\n",
    );
    write(
        dir,
        "app/services/algolia_events_dispatcher/conversion.rb",
        CONVERSION,
    );
    write(
        dir,
        "app/services/algolia/search_service.rb",
        SEARCH_SERVICE,
    );
    write(dir, "app/services/algolia/recommend_service.rb", RECOMMEND);
    write(
        dir,
        "app/services/algolia/legacy_recommend_service.rb",
        LEGACY_RECOMMEND,
    );
    for (path, name) in [
        ("app/models/concerns/products/algolia.rb", "Products"),
        ("app/models/concerns/bundles/algolia.rb", "Bundles"),
        ("app/models/concerns/categories/algolia.rb", "Categories"),
        (
            "app/models/concerns/mapped_brands/algolia.rb",
            "MappedBrands",
        ),
        ("app/services/pub_sub/event_handlers/algolia.rb", "PubSub"),
        ("spec/support/algolia.rb", "Support"),
    ] {
        write(dir, path, &concern(name));
    }
    for path in [
        "app/views/carts/upsell.html.erb",
        "app/views/products/upsell.html.erb",
        "app/views/checkout/upsell.html.erb",
    ] {
        write(dir, path, "<div class=\"upsell\"></div>\n");
    }
    write(
        dir,
        "app/javascript/website/algolia.js",
        "export const client = null;\n",
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "alpha"]);
    tmp
}

/// `beta` has a DIFFERENT `carts_controller.rb` and nothing else the fixture
/// doc cites — and one uncommitted line, so `git status --porcelain` is
/// non-empty and the scorecard must flag it `dirty`.
fn fixture_beta() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(
        dir,
        "app/controllers/carts_controller.rb",
        "class CartsController < ApplicationController\n  def checkout\n    head :ok\n  end\nend\n",
    );
    write(
        dir,
        "lib/tasks/send_events.rake",
        "task :send_events do\nend\n",
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "beta"]);
    // Uncommitted ⇒ dirty.
    std::fs::write(
        dir.join("lib/tasks/send_events.rake"),
        "task :send_events do\n  puts 'uncommitted'\nend\n",
    )
    .unwrap();
    tmp
}

// --- mock kb daemon --------------------------------------------------------

fn fixture_doc() -> Value {
    serde_json::from_str(include_str!("../fixtures/doclens/piano-fixture.json")).unwrap()
}

const FIX_DOC_ID: &str = "fixdoc000001";

async fn mock_kb(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, handle)
}

/// The default mock: serves the fixture for `fixdoc000001`, 404s anything
/// else, and serves a `never_scanned: true` payload for `neverscanned01`.
/// Also answers kb's by-path lookup for `a.html` — DCB-W2.B.R fix 12: without
/// this, `cors_layer_route_set_is_pinned`'s resolve-path probe (`path=a.html`)
/// would get a genuine `doc_not_found` 404 from kb-code-server itself
/// (indistinguishable, at the HTTP status level, from the route not being
/// mounted at all — exactly the ambiguity that hardening closes).
async fn mock_kb_default() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|AxPath((kb, id)): AxPath<(String, String)>| async move {
                match id.as_str() {
                    FIX_DOC_ID => Json(fixture_doc()).into_response(),
                    "neverscanned01" => Json(json!({
                        "schema": "coderef/1",
                        "kb": kb,
                        "doc_id": "neverscanned01",
                        "doc_path": "features/never.html",
                        "title": "Never scanned",
                        "doc_hash": null,
                        "extracted_at": null,
                        "never_scanned": true,
                        "code_rev": null,
                        "ref_count": 0,
                        "ungrouped_count": 0,
                        "truncated": false,
                        "groups": [],
                        "refs": []
                    }))
                    .into_response(),
                    _ => (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"})))
                        .into_response(),
                }
            }),
        )
        .route(
            "/api/kb/{kb}/docs/by-path/{*path}",
            get(|AxPath((_kb, path)): AxPath<(String, String)>| async move {
                if path == "a.html" {
                    Json(json!({"id": FIX_DOC_ID, "path": path})).into_response()
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"}))).into_response()
                }
            }),
        );
    mock_kb(router).await
}

/// [`mock_kb_default`] plus a `coderef/1` FEED route (`GET
/// /api/kb/{kb}/code-refs`) naming `fixdoc000001` in its one header —
/// `mock_kb_default` alone has no feed route at all, which 404s
/// `doclens::sync`'s walk before it ever reaches the per-doc body route
/// (DCB-W3.A.R's own tests are the first callers in this file that drive
/// `POST /api/doc-lens/sync` against an actual PIN, rather than an empty
/// pin ledger).
async fn mock_kb_default_with_feed() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route(
            "/api/kb/{kb}/code-refs",
            get(|| async move {
                Json(json!({
                    "schema": "coderef-feed/1",
                    "kb": "platform",
                    "docs": [{"doc_id": FIX_DOC_ID}],
                }))
                .into_response()
            }),
        )
        .route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|AxPath((_kb, id)): AxPath<(String, String)>| async move {
                match id.as_str() {
                    FIX_DOC_ID => Json(fixture_doc()).into_response(),
                    _ => (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"})))
                        .into_response(),
                }
            }),
        );
    mock_kb(router).await
}

// --- boot ------------------------------------------------------------------

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// `deadline_ms` is raised well above the 3 s production default for every
/// boot here: this suite runs ~35 daemons in parallel on an IO-bound host,
/// and the point of these tests is the RESOLUTION, not the budget. The budget
/// itself has its own dedicated test
/// (`doc_lens_deadline_returns_partials_marked_incomplete`).
fn doclens_cfg(kbs: &[&str], origins: &[&str]) -> DoclensSection {
    DoclensSection {
        kbs: kbs.iter().map(|s| s.to_string()).collect(),
        origins: origins.iter().map(|s| s.to_string()).collect(),
        deadline_ms: 60_000,
        ..DoclensSection::default()
    }
}

async fn boot(
    repos: &[(&str, &Path)],
    kb_addr: Option<SocketAddr>,
    doclens: DoclensSection,
) -> Boot {
    boot_at(tempfile::tempdir().unwrap(), repos, kb_addr, doclens).await
}

/// [`boot`] over a CALLER-SUPPLIED state root, so a test can seed the daemon's
/// own sqlite store (`<state>/kb-code/index.db`) BEFORE it boots — which is
/// the only way to exercise W2.A's boot-time pin prune, since a pin written
/// over HTTP always records the live configured root.
async fn boot_at(
    tmp: tempfile::TempDir,
    repos: &[(&str, &Path)],
    kb_addr: Option<SocketAddr>,
    doclens: DoclensSection,
) -> Boot {
    let cfg = KbCodeConfig {
        repos: repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: (*name).to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        kb_daemon: KbDaemonSection {
            enabled: kb_addr.is_some(),
            url: kb_addr
                .map(|a| format!("http://{a}"))
                .unwrap_or_else(|| "http://127.0.0.1:0".to_string()),
            token_file: None,
            // Pinned so `doc_href` is deterministic in the wire assertions.
            public_url: Some("https://kb.example.com".to_string()),
        },
        doclens,
        ..KbCodeConfig::default()
    };
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

/// Poll `GET /api/repos` until the boot walk has landed — the same wait
/// `tests/agentview_routes.rs` established.
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["name"] == repo))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "repo {repo:?} never reached file_count >= {expected_files}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_symbols(base: &str, repo: &str, expected: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["name"] == repo))
                {
                    if entry["symbol_count"].as_u64().unwrap_or(0) as usize >= expected {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "repo {repo:?} never reached symbol_count >= {expected}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const ALPHA_FILES: usize = 16;

async fn http_get(base: &str, path: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn by_ordinal(body: &Value, ordinal: u64) -> &Value {
    body["refs"]
        .as_array()
        .expect("refs[]")
        .iter()
        .find(|r| r["ordinal"] == ordinal)
        .unwrap_or_else(|| panic!("no ref with ordinal {ordinal}"))
}

// --- the golden table ------------------------------------------------------

/// `(ordinal, path_state, line_state, note)` — every combination the design
/// can produce, pinned INDIVIDUALLY. `path_state` `""` means JSON `null` (a
/// ref carrying no resolvable path hint: the three symbol refs + the issue).
const GOLDEN: &[(u64, &str, &str)] = &[
    (0, "present", "confirmed"),    // unique token at the hint
    (1, "present", "confirmed"),    // path_list, BOTH spans confirm
    (2, "present", "drifted"),      // unique token at +11
    (3, "present", "unverifiable"), // ambiguous_in_window
    (4, "present", "unverifiable"), // token_not_found, hint past EOF
    (5, "present", "absent"),       // no_line_hint
    (6, "present", "confirmed"),    // const
    (7, "present", "confirmed"),    // method
    (8, "ambiguous", "absent"),     // algolia.rb ×6
    (9, "ambiguous", "absent"),     // upsell.html.erb ×3
    (10, "present", "absent"),      // products/algolia.rb — tail disambiguates
    (11, "absent", "absent"),       // missing_service.rb
    (12, "external", "absent"),     // gem path — never resolved
    (13, "absent", "absent"),       // declared but absent
    (14, "", "absent"),             // symbol: hit_unique
    (15, "", "absent"),             // symbol: hit_ambiguous
    (16, "", "absent"),             // symbol: hit_container_matched
    (17, "", "absent"),             // issue
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_resolves_the_fixture_doc_against_alpha() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    wait_for_symbols(&boot.base, "alpha", 6).await;

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "codelens/1");
    assert_eq!(body["repo"]["name"], "alpha");
    assert_eq!(body["repo"]["state"], "ready");
    assert_eq!(body["repo"]["source"], "param");
    assert_eq!(body["repo"]["dirty"], false);
    assert!(!body["partial"].as_bool().unwrap());
    assert!(!body["truncated"].as_bool().unwrap());

    // W2.A — the fixture doc names a `kb-code-rev` whose sha no fixture repo
    // can contain (each `fixture_alpha()` mints fresh commits), so the remap
    // is honestly UNAVAILABLE and every golden below is still the pure token
    // pass. An unresolvable rev degrades the lens by exactly nothing.
    assert_eq!(body["rev_remap"]["state"], "unavailable", "{body}");
    assert_eq!(body["rev_remap"]["reason"], "rev_unknown");
    assert_eq!(body["rev_remap"]["repo_label"], "alpha");
    assert!(body["rev_remap"]["resolved_sha"].is_null());
    assert_eq!(body["rev_remap"]["paths_mapped"], 0);

    // --- every golden row, individually --------------------------------
    for (ordinal, path_state, line_state) in GOLDEN {
        let r = by_ordinal(&body, *ordinal);
        let got_path = r["path_state"].as_str().unwrap_or("");
        assert_eq!(got_path, *path_state, "ref {ordinal} path_state: {r}");
        assert_eq!(
            r["line_state"].as_str().unwrap_or(""),
            *line_state,
            "ref {ordinal} line_state: {r}"
        );
    }

    // --- the specific evidence each row is supposed to carry -----------
    let confirmed = by_ordinal(&body, 0);
    assert_eq!(
        confirmed["resolved_path"],
        "app/controllers/carts_controller.rb"
    );
    assert_eq!(confirmed["confirm_token"], "algolia_user_token");
    assert_eq!(confirmed["token_line"], 12);
    // D6 — confirmed keeps the CITED line; the ±3 window is a tolerance on
    // the evidence, not a correction.
    assert_eq!(confirmed["resolved_line"], 12);
    assert_eq!(confirmed["line_hint_delta"], 0);
    assert_eq!(confirmed["line_evidence"], "context_token");
    assert_eq!(confirmed["file_lines"], 39);
    assert_eq!(confirmed["reader"]["line"], 12);
    assert_eq!(confirmed["reader"]["repo"], "alpha");

    // path_list — ONE ref, two spans, one file read, both confirmed.
    let list = by_ordinal(&body, 1);
    let spans = list["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0]["resolved_line"], 23);
    assert_eq!(spans[1]["resolved_line"], 34);
    assert!(spans.iter().all(|s| s["line_state"] == "confirmed"));
    assert_eq!(
        list["resolved_line"], 23,
        "the FIRST span drives the reader"
    );

    let drifted = by_ordinal(&body, 2);
    assert_eq!(drifted["confirm_token"], "add_cart_bundle");
    assert_eq!(drifted["resolved_line"], 30);
    assert_eq!(drifted["line_hint_delta"], 11);
    assert_eq!(drifted["reader"]["line"], 30, "drift DOES move the reader");

    assert_eq!(by_ordinal(&body, 3)["line_reason"], "ambiguous_in_window");
    let past_eof = by_ordinal(&body, 4);
    assert_eq!(past_eof["line_reason"], "token_not_found");
    assert_eq!(past_eof["file_lines"], 15);
    assert_eq!(by_ordinal(&body, 5)["line_reason"], "no_line_hint");

    // Ambiguity tiers: >3 drops the list but keeps the exact count.
    let six = by_ordinal(&body, 8);
    assert_eq!(six["candidate_count"], 6);
    assert_eq!(six["candidates"].as_array().unwrap().len(), 0);
    assert_eq!(six["search"]["q"], "algolia.rb");
    assert_eq!(six["search"]["repo"], "alpha");
    let three = by_ordinal(&body, 9);
    assert_eq!(three["candidate_count"], 3);
    assert_eq!(three["candidates"].as_array().unwrap().len(), 3);

    // The multi-segment tail disambiguates a 6-way basename.
    assert_eq!(
        by_ordinal(&body, 10)["resolved_path"],
        "app/models/concerns/products/algolia.rb"
    );

    // external is never resolved and never gets a deep link INTO the repo.
    let ext = by_ordinal(&body, 12);
    assert!(ext["resolved_path"].is_null());
    assert!(ext["search"].is_null());
    assert_eq!(ext["line_reason"], "external");

    assert_eq!(by_ordinal(&body, 13)["note"], "declared but absent");

    // Symbols — doc-lens's OWN vocabulary.
    assert_eq!(by_ordinal(&body, 14)["symbol_state"], "hit_unique");
    assert_eq!(by_ordinal(&body, 15)["symbol_state"], "hit_ambiguous");
    assert_eq!(by_ordinal(&body, 15)["symbol_hit_count"], 2);
    assert_eq!(
        by_ordinal(&body, 16)["symbol_state"],
        "hit_container_matched"
    );

    // The issue arm: never resolved against the tree, href REBUILT.
    let issue = by_ordinal(&body, 17);
    assert!(issue["path_state"].is_null());
    assert!(issue["search"].is_null());
    assert_eq!(issue["line_reason"], "issue");
    assert_eq!(issue["issue"]["owner"], "acme");
    assert_eq!(issue["issue"]["repo"], "shopfront");
    assert_eq!(issue["issue"]["number"], 15357);
    assert_eq!(
        issue["issue"]["href"],
        "https://github.com/acme/shopfront/issues/15357"
    );

    // --- counts DERIVED from the table, never hand-typed ---------------
    for field in ["present", "ambiguous", "absent", "external"] {
        let n = GOLDEN.iter().filter(|(_, p, _)| *p == field).count() as u64;
        assert_eq!(body["counts"][field], n, "counts.{field}");
    }
    for (field, want) in [
        ("confirmed", "confirmed"),
        ("drifted", "drifted"),
        ("unverifiable", "unverifiable"),
        ("line_absent", "absent"),
    ] {
        let n = GOLDEN.iter().filter(|(_, _, l)| *l == want).count() as u64;
        assert_eq!(body["counts"][field], n, "counts.{field}");
    }
    assert_eq!(body["counts"]["total"], 18);
    assert_eq!(body["counts"]["resolved"], 18);
    assert_eq!(body["counts"]["declared_but_absent"], 1);

    // present + ambiguous + absent + external + null == total (14 + 4 == 18)
    let c = &body["counts"];
    let sum = ["present", "ambiguous", "absent", "external"]
        .iter()
        .map(|k| c[k].as_u64().unwrap())
        .sum::<u64>();
    let nulls = GOLDEN.iter().filter(|(_, p, _)| p.is_empty()).count() as u64;
    assert_eq!(sum + nulls, c["total"].as_u64().unwrap());
}

/// Amendment 5's determinism golden: opening a file through `/api/file`
/// perturbs the frecency state the FUZZY files lane ranks by. doc-lens must
/// be byte-identical afterwards, because it never consults that lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_resolution_is_invariant_under_frecency_state() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    let url = format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha");
    let (_, before) = http_get(&boot.base, &url).await;

    // Perturb: open two of the ambiguous candidates, which bumps
    // `file_opens` and would re-rank a frecency-weighted lane.
    for p in [
        "app/models/concerns/bundles/algolia.rb",
        "app/views/checkout/upsell.html.erb",
    ] {
        let (status, _) = http_get(&boot.base, &format!("/api/file?repo=alpha&path={p}")).await;
        assert_eq!(status, StatusCode::OK);
    }

    let (_, after) = http_get(&boot.base, &url).await;
    // `resolved_unix` is wall clock by design — everything else must match.
    let strip = |mut v: Value| {
        v["resolved_unix"] = Value::Null;
        v
    };
    assert_eq!(
        strip(before),
        strip(after),
        "resolution must not depend on frecency state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_scorecard_scores_alpha_and_beta_differently_and_flags_beta_dirty() {
    let alpha = fixture_alpha();
    let beta = fixture_beta();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path()), ("beta", beta.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    wait_for_indexed(&boot.base, "beta", 2).await;

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "codelens-scorecard/1");
    assert_eq!(body["counted_refs"], 18);
    assert!(body["pinned_repo"].is_null());
    // No `confirmed` column, by ruling (D7) — it would cost a file read per
    // distinct path × every configured repo.
    let repos = body["repos"].as_array().unwrap();
    assert!(repos.iter().all(|r| r.get("confirmed").is_none()));
    // Config order, deterministically.
    assert_eq!(repos[0]["name"], "alpha");
    assert_eq!(repos[1]["name"], "beta");

    let a = &repos[0];
    assert_eq!(a["state"], "ready");
    assert_eq!(a["dirty"], false);
    assert_eq!(a["present"], 9);
    assert_eq!(a["ambiguous"], 2);
    assert_eq!(a["absent"], 2);
    assert_eq!(a["external"], 1);
    assert!(a["head_sha"].as_str().unwrap().len() == 40);

    let b = &repos[1];
    assert_eq!(b["state"], "ready");
    assert_eq!(b["dirty"], true, "beta has an uncommitted line");
    assert_eq!(b["present"], 3, "beta only has carts_controller.rb");
    assert_eq!(b["ambiguous"], 0);
    assert_eq!(b["absent"], 10);
    assert_eq!(b["external"], 1);

    // Per repo: present + ambiguous + absent + external + null == 18.
    for r in repos {
        let sum: u64 = ["present", "ambiguous", "absent", "external"]
            .iter()
            .map(|k| r[k].as_u64().unwrap())
            .sum();
        assert_eq!(sum + 4, 18, "{}", r["name"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_scorecard_carries_never_scanned_and_still_scores_every_repo() {
    // [G4] `never_scanned` DOES ride the `codelens-scorecard/1` wire — it's a
    // top-level `ScorecardOut` field (`d.never_scanned`, unconditional),
    // sibling to (and independent of) `resolve_lens`'s own already-pinned
    // `never_scanned` (`doc_lens_reports_never_scanned_instead_of_an_empty_lens`
    // above). Unlike the lens route, the scorecard route does NOT gate
    // scoring on it: a never-scanned doc has zero refs, so every configured,
    // `ready` repo is still scored — trivially, at zero — rather than being
    // skipped or blanked out the way an `indexing`/`error` repo is.
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/repos?kb=platform&doc=neverscanned01",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["never_scanned"], true, "the THIRD state, not refs: []");
    assert_eq!(body["counted_refs"], 0);

    let repos = body["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    let a = &repos[0];
    assert_eq!(a["name"], "alpha");
    assert_eq!(
        a["state"], "ready",
        "scoring runs regardless of never_scanned"
    );
    assert_eq!(a["present"], 0);
    assert_eq!(a["ambiguous"], 0);
    assert_eq!(a["absent"], 0);
    assert_eq!(a["external"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_refuses_a_kb_that_is_not_allowlisted_with_reason() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    for path in [
        format!("/api/doc-lens?kb=research&doc={FIX_DOC_ID}&repo=alpha"),
        format!("/api/doc-lens/repos?kb=research&doc={FIX_DOC_ID}"),
    ] {
        let (status, body) = http_get(&boot.base, &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["reason"], "kb_not_allowlisted", "{body}");
        assert!(body["error"].as_str().unwrap().contains("research"));
    }
    // The pin route enforces the SAME gate — a pin for a corpus this daemon
    // will not read is never stored.
    let resp = reqwest::Client::new()
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "research", "doc": FIX_DOC_ID, "repo": "alpha"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["reason"],
        "kb_not_allowlisted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_is_off_entirely_when_the_kbs_allowlist_is_empty() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        DoclensSection::default(),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["reason"], "doclens_disabled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_rejects_a_path_shaped_doc_segment() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    // R2 — the doc key is kb's artifact id; a source-relative path 400s
    // rather than being silently accepted onto a second key.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens?kb=platform&doc=features%2Fa%2Fpiano.html&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["reason"], "invalid_segment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_without_repo_or_pin_is_400_repo_required_and_never_auto_picks() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    // Exactly ONE configured repo, and it STILL refuses to guess.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["reason"], "repo_required");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("/api/doc-lens/repos"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_uses_the_pin_when_no_repo_param_is_given() {
    let alpha = fixture_alpha();
    let beta = fixture_beta();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path()), ("beta", beta.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "beta", 2).await;

    let client = reqwest::Client::new();
    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "beta"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    assert_eq!(
        put.json::<Value>().await.unwrap()["schema"],
        "codelens-pin/1"
    );

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["repo"]["name"], "beta");
    assert_eq!(body["repo"]["source"], "pin");

    // and the scorecard pre-selects it
    let (_, card) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(card["pinned_repo"], "beta");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_reports_kb_unreachable_not_500_when_the_kb_daemon_is_down() {
    let alpha = fixture_alpha();
    // A port nothing is listening on.
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(dead),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["reason"], "kb_unreachable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_reports_kb_daemon_disabled_distinctly() {
    let alpha = fixture_alpha();
    let boot = boot(
        &[("alpha", alpha.path())],
        None,
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["reason"], "kb_daemon_disabled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_404s_and_drops_the_pin_when_kb_no_longer_has_the_doc() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();
    // Pin a doc the mock will 404 …
    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": "goneaway0001", "repo": "alpha"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let (status, body) = http_get(&boot.base, "/api/doc-lens?kb=platform&doc=goneaway0001").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["reason"], "doc_not_found");

    // … and the pin is GONE (dropped loudly, never silently repaired) rather
    // than left pointing at a checkout chosen for a dead document.
    let (status, card) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(card["pinned_repo"].is_null());
    let (_, card) = http_get(
        &boot.base,
        "/api/doc-lens/repos?kb=platform&doc=goneaway0001",
    )
    .await;
    assert!(
        card["pinned_repo"].is_null() || card["error"].is_string(),
        "the dead doc's pin must not survive: {card}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_rekeys_the_pin_when_kb_301s_to_a_new_doc_id() {
    let alpha = fixture_alpha();
    // The mock 301s `oldid0000001` to a Location the test deliberately makes
    // UNGRAMMATICAL — an implementation that parsed the redirect URL instead
    // of the response BODY would fail here (D13/R13).
    let router = Router::new()
        .route(
            "/api/kb/{kb}/docs/oldid0000001/code-refs",
            get(|| async {
                Response::builder()
                    .status(StatusCode::MOVED_PERMANENTLY)
                    .header("location", "/api/kb/platform/docs/fixdoc000001/code-refs")
                    .header("x-kb-nonsense", "not-a-url-at-all/../..")
                    .body(axum::body::Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|AxPath((_kb, id)): AxPath<(String, String)>| async move {
                if id == FIX_DOC_ID {
                    Json(fixture_doc()).into_response()
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({"error": "no"}))).into_response()
                }
            }),
        );
    let (kb_addr, _kb) = mock_kb(router).await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    let client = reqwest::Client::new();
    client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": "oldid0000001", "repo": "alpha"}))
        .send()
        .await
        .unwrap();

    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens?kb=platform&doc=oldid0000001&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["doc_id"], FIX_DOC_ID, "the BODY names the final id");
    assert_eq!(body["moved_from"], "oldid0000001");

    // The pin followed the chain: asking under the NEW id with no ?repo=
    // resolves from the pin.
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["repo"]["source"], "pin");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_truncates_at_max_refs_and_says_so() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        DoclensSection {
            max_refs: Some(5),
            ..doclens_cfg(&["platform"], &[])
        },
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refs"].as_array().unwrap().len(), 5);
    assert_eq!(body["truncated"], true);
    // `counts.total` still reports the FULL feed count — never a silent
    // shrink.
    assert_eq!(body["counts"]["total"], 18);
    assert_eq!(body["counts"]["resolved"], 5);
    // Truncation is by ordinal ASCENDING.
    let ords: Vec<u64> = body["refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ordinal"].as_u64().unwrap())
        .collect();
    assert_eq!(ords, vec![0, 1, 2, 3, 4]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_marks_an_indexing_repo_distinctly_from_absent() {
    // A git repo with no COMMITS has nothing for the boot walk to index, so
    // `file_count` stays 0 while `GitRepo::open` succeeds — the structural
    // definition of "still indexing" (amendment 5). It must never render as
    // an all-absent lens, which reads as doc-rot.
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    git(tmp.path(), &["config", "user.email", "t@e.com"]);
    git(tmp.path(), &["config", "user.name", "T"]);
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("fresh", tmp.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=fresh"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["reason"], "repo_indexing");

    // The scorecard reports it as a per-REPO state with NULL columns — never
    // zeroes, which would read like a verdict.
    let (status, card) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = &card["repos"][0];
    assert_eq!(row["state"], "indexing");
    assert_eq!(row["reason"], "repo has no indexed files yet");
    for k in [
        "present",
        "ambiguous",
        "absent",
        "external",
        "dirty",
        "head_sha",
    ] {
        assert!(row[k].is_null(), "{k} must be null on an indexing repo");
    }
    // `indexing` is a per-repo state and is NEVER a path_state.
    assert!(!card.to_string().contains("\"path_state\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_scorecard_survives_one_broken_repo() {
    let alpha = fixture_alpha();
    let broken = tempfile::tempdir().unwrap();
    git(broken.path(), &["init", "-q", "-b", "main"]);
    git(broken.path(), &["config", "user.email", "t@e.com"]);
    git(broken.path(), &["config", "user.name", "T"]);
    write(broken.path(), "a.rb", "class A\nend\n");
    git(broken.path(), &["add", "-A"]);
    git(broken.path(), &["commit", "-q", "-m", "c"]);
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path()), ("broken", broken.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    wait_for_indexed(&boot.base, "broken", 1).await;

    // Pull the rug: the root goes away AFTER boot.
    std::fs::remove_dir_all(broken.path().join(".git")).unwrap();
    std::fs::remove_file(broken.path().join("a.rb")).ok();

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    // One broken repo must NEVER sink the scorecard.
    assert_eq!(status, StatusCode::OK, "{body}");
    let repos = body["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0]["name"], "alpha");
    assert_eq!(repos[0]["state"], "ready");
    assert_eq!(repos[1]["state"], "error");
    assert!(repos[1]["reason"].as_str().unwrap().contains("git"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_reports_never_scanned_instead_of_an_empty_lens() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens?kb=platform&doc=neverscanned01&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["never_scanned"], true, "the THIRD state, not refs: []");
    assert_eq!(body["refs"].as_array().unwrap().len(), 0);
    assert_eq!(body["counts"]["total"], 0);
    // The checkout is still NAMED, so a consumer can say what it would have
    // resolved against.
    assert_eq!(body["repo"]["name"], "alpha");
    assert_eq!(body["repo"]["state"], "ready");
    assert!(body["doc_extracted_at"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_carries_doc_path_ungrouped_count_and_a_built_doc_href() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["doc_path"],
        "features/algolia-2026-events/piano-fixture.html"
    );
    // ONE server-side link-out builder, percent-encoded per segment.
    assert_eq!(
        body["doc_href"],
        "https://kb.example.com/a/platform/features/algolia-2026-events/piano-fixture.html"
    );
    assert_eq!(body["ungrouped_count"], 2);
    // R9 — the ungrouped trailer is a COUNT plus null FKs, never a sentinel
    // groups[] row.
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert!(groups
        .iter()
        .all(|g| !g["key"].as_str().unwrap().is_empty()));
    assert!(groups.iter().all(|g| g["key"] == g["anchor"]));
    assert_eq!(groups[0]["key"], "kb-h-a2-token-identit");
    assert_eq!(
        body["refs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["group"].is_null())
            .count(),
        2
    );
    // The inbound `code_rev.label` is re-emitted as `repo_label`.
    assert_eq!(body["doc_code_rev"]["repo_label"], "alpha");
    assert_eq!(body["doc_code_rev"]["dirty"], false);
    // The honesty note rides every response.
    assert!(body["note"]
        .as_str()
        .unwrap()
        .contains("never cached or stored"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pin_put_and_delete_round_trip_over_http() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();
    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "alpha", "doc_hash": "abc"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let body: Value = put.json().await.unwrap();
    assert_eq!(body["schema"], "codelens-pin/1");
    assert_eq!(body["repo"], "alpha");
    assert_eq!(body["doc_hash"], "abc");
    // `repo_root` is what makes a re-pointed repo detectable later.
    assert!(body["repo_root"].as_str().unwrap().len() > 1);

    // Idempotent DELETE: 204 whether or not a row existed.
    for _ in 0..2 {
        let del = client
            .delete(format!(
                "{}/api/doc-lens/pin?kb=platform&doc={FIX_DOC_ID}",
                boot.base
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
    }
    let (_, card) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert!(card["pinned_repo"].is_null());
}

// --- W2.A: the pin lifecycle ----------------------------------------------

/// The daemon's own store file for a `boot_at` state root.
fn store_at(tmp: &Path) -> kb_code_server::store::Store {
    let paths = KbPaths::rooted_at(tmp, "kb-code");
    std::fs::create_dir_all(&paths.state).unwrap();
    kb_code_server::store::Store::open(&paths.state.join("index.db")).unwrap()
}

fn seed_pin(store: &kb_code_server::store::Store, doc: &str, repo: &str, root: &str) {
    store
        .put_doc_lens_pin(&kb_code_server::store::DocLensPin {
            kb: "platform".into(),
            doc_id: doc.into(),
            repo: repo.into(),
            repo_root: root.into(),
            doc_hash: None,
            pinned_at: 1_754_500_000,
        })
        .unwrap();
}

/// E7 — a pin whose repo is gone, and one whose root was re-pointed, are both
/// DELETED at boot. `upsert_repo` is `ON CONFLICT(name) DO UPDATE SET root`,
/// so the re-pointed one silently re-uses the repo id and would otherwise
/// pre-select a tree the operator never picked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_prunes_pins_whose_repo_vanished_or_whose_root_was_re_pointed() {
    let alpha = fixture_alpha();
    let other = fixture_tiny();
    let tmp = tempfile::tempdir().unwrap();
    {
        let store = store_at(tmp.path());
        // (a) repo no longer configured
        seed_pin(&store, "vanished0001", "beta", "/gone");
        // (b) same NAME, different root
        seed_pin(
            &store,
            "repointed001",
            "alpha",
            &other.path().to_string_lossy(),
        );
        // (c) healthy — must survive
        seed_pin(
            &store,
            FIX_DOC_ID,
            "alpha",
            &std::fs::canonicalize(alpha.path())
                .unwrap()
                .to_string_lossy(),
        );
    }
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot_at(
        tmp,
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;

    let (status, body) = http_get(&boot.base, "/api/doc-lens/pins?kb=platform").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pins = body["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 1, "only the healthy pin survives boot: {body}");
    assert_eq!(pins[0]["doc_id"], FIX_DOC_ID);
    assert_eq!(pins[0]["repo"], "alpha");
}

/// §6.3 — the ledger route, plus the two liveness columns. After a boot prune
/// they are always `true`; the fields make the invariant visible instead of
/// implicit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_pins_route_reports_repo_configured_and_root_matches() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();
    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "alpha", "doc_hash": "abc"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let (status, body) = http_get(&boot.base, "/api/doc-lens/pins").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "codelens-pins/1");
    let pins = body["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0]["kb"], "platform");
    assert_eq!(pins[0]["doc_id"], FIX_DOC_ID);
    assert_eq!(pins[0]["repo_configured"], true);
    assert_eq!(pins[0]["root_matches"], true);
    assert_eq!(pins[0]["doc_hash"], "abc");
    assert!(pins[0]["pinned_at"].as_i64().unwrap() > 0);
    assert!(pins[0]["repo_root"].as_str().unwrap().len() > 1);

    // `?kb=` filters; a kb outside `[doclens] kbs` is refused with a reason
    // rather than answering an empty list (the allowlist is the ONE scope).
    let (_, scoped) = http_get(&boot.base, "/api/doc-lens/pins?kb=platform").await;
    assert_eq!(scoped["pins"].as_array().unwrap().len(), 1);
    let (status, body) = http_get(&boot.base, "/api/doc-lens/pins?kb=research").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["reason"], "kb_not_allowlisted");
    let (status, body) = http_get(&boot.base, "/api/doc-lens/pins?kb=bad/kb").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["reason"], "invalid_segment");
}

/// §6.2 — the read-time re-check, defence in depth BEHIND the boot prune. The
/// pin is seeded AFTER boot (so the prune cannot have seen it), and the lens
/// must refuse rather than resolve against a tree the pin was never chosen
/// against — and must DELETE the row, not merely ignore it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_refuses_a_pin_whose_root_no_longer_matches_and_deletes_it() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let boot = boot_at(
        tmp,
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    {
        let store = store_at(&state_root);
        seed_pin(&store, FIX_DOC_ID, "alpha", "/somewhere/else/entirely");
    }

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["reason"], "repo_required");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("/somewhere/else/entirely"),
        "the message must name the root the pin was chosen against: {body}"
    );
    // Deleted, not ignored — a stale pin that survives keeps pre-selecting a
    // tree it was never chosen against on every subsequent read.
    let (_, pins) = http_get(&boot.base, "/api/doc-lens/pins?kb=platform").await;
    assert!(
        pins["pins"].as_array().unwrap().is_empty(),
        "the stale pin must be dropped, not merely ignored: {pins}"
    );
}

/// DCB-W2.A.R fix 5 — the scorecard's `pinned_repo` is the ONE field the
/// SPA's repo picker actually reads on load, and until this fix it mapped
/// the stored pin straight through with no `root_matches` re-check at all —
/// the read-time defence `select_repo` already had (and the test just
/// above pins on `/api/doc-lens` itself) was silently absent on this route.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scorecard_pinned_repo_drops_a_pin_whose_root_no_longer_matches() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let boot = boot_at(
        tmp,
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    {
        let store = store_at(&state_root);
        seed_pin(&store, FIX_DOC_ID, "alpha", "/somewhere/else/entirely");
    }

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["pinned_repo"].is_null(),
        "a stale pin must never be reported as pinned: {body}"
    );
    // Deleted, not ignored — same defence, same consequence as the
    // `/api/doc-lens` read-time re-check above.
    let (_, pins) = http_get(&boot.base, "/api/doc-lens/pins?kb=platform").await;
    assert!(
        pins["pins"].as_array().unwrap().is_empty(),
        "the stale pin must be dropped, not merely ignored: {pins}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pin_put_404s_for_an_unconfigured_repo() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let resp = reqwest::Client::new()
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(resp.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("nope"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_non_2xx_body_carries_a_reason() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let cases = [
        (
            format!("/api/doc-lens?kb=research&doc={FIX_DOC_ID}&repo=alpha"),
            "kb_not_allowlisted",
        ),
        (
            format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}"),
            "repo_required",
        ),
        (
            "/api/doc-lens?kb=platform&doc=nosuchdoc001&repo=alpha".to_string(),
            "doc_not_found",
        ),
        (
            "/api/doc-lens?kb=bad%2Fkb&doc=x&repo=alpha".to_string(),
            "invalid_segment",
        ),
    ];
    for (path, want) in cases {
        let resp = reqwest::Client::new()
            .get(format!("{}{path}", boot.base))
            .send()
            .await
            .unwrap();
        assert!(!resp.status().is_success(), "{path}");
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.starts_with("application/json"), "{path}: {ct}");
        let body: Value = resp.json().await.unwrap();
        assert!(body["error"].is_string(), "{path}: {body}");
        assert_eq!(body["reason"], want, "{path}: {body}");
    }
}

/// The additive `reason` field must not change ANY pre-DCB route's body by a
/// byte — `skip_serializing_if` semantics, verified on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_doclens_error_body_still_has_no_reason_key() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(&boot.base, "/api/file?repo=nope&path=a.rb").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
    assert!(
        body.as_object().unwrap().get("reason").is_none(),
        "pre-DCB error bodies must be unchanged: {body}"
    );
}

/// Amendment 8's per-request deadline: on expiry the response still comes
/// back, marked `partial`, with every ref keeping its path and symbol state
/// and only the LINE half degraded — honestly, and with a reason. A hung or
/// silently-truncated response would be the failure this exists to prevent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_lens_deadline_returns_partials_marked_incomplete() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        DoclensSection {
            // An instantly-expired budget: the loop trips on its first ref.
            deadline_ms: 0,
            ..doclens_cfg(&["platform"], &[])
        },
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a partial is a 200, never a 5xx");
    assert_eq!(body["partial"], true);
    assert_eq!(body["partial_reason"], "deadline");
    // Every ref is still THERE, with its path/symbol verdict intact …
    assert_eq!(body["refs"].as_array().unwrap().len(), 18);
    assert_eq!(body["counts"]["present"], 9);
    assert_eq!(body["counts"]["ambiguous"], 2);
    assert_eq!(by_ordinal(&body, 14)["symbol_state"], "hit_unique");
    // … and only the LINE half degrades, with an honest reason.
    let r = by_ordinal(&body, 0);
    assert_eq!(r["line_state"], "unverifiable");
    assert_eq!(r["line_reason"], "deadline");
    assert_eq!(r["resolved_path"], "app/controllers/carts_controller.rb");
    assert!(r["spans"].as_array().unwrap().is_empty());
}

// --- resolve-path (DCB W2.B) ------------------------------------------------
//
// `GET /api/doc-lens/resolve-path?kb=&path=` — the path-addressed lens entry
// ramp's server side (R2/R20). These never touch `code-refs`, so a dedicated
// mock (serving ONLY kb's by-path lookup) keeps this family from drifting
// against `mock_kb_default`'s fixture. The CORS-absence half of this route's
// contract is covered by `cors_layer_route_set_is_pinned` below, not here.

const RESOLVE_PATH_KNOWN: &str = "features/algolia/piano.html";

async fn mock_kb_by_path() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/api/kb/{kb}/docs/by-path/{*path}",
        get(|AxPath((_kb, path)): AxPath<(String, String)>| async move {
            if path == RESOLVE_PATH_KNOWN {
                Json(json!({"id": FIX_DOC_ID, "path": path})).into_response()
            } else {
                (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"}))).into_response()
            }
        }),
    );
    mock_kb(router).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_resolves_a_known_path_to_the_doc_id() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/resolve-path?kb=platform&path={RESOLVE_PATH_KNOWN}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["doc_id"], FIX_DOC_ID);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_404s_for_an_unknown_path_and_never_500s() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/resolve-path?kb=platform&path=nowhere.html",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["reason"], "doc_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_refuses_a_malformed_kb_segment() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    // R2/D14 precedent (`doc_lens_rejects_a_path_shaped_doc_segment`): a
    // percent-encoded `/` still decodes to a `/` before validation runs.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/resolve-path?kb=bad%2Fkb&path=a.html",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["reason"], "invalid_segment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_refuses_an_empty_path() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(&boot.base, "/api/doc-lens/resolve-path?kb=platform&path=").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["reason"], "invalid_segment");
}

/// A mock that counts every request it receives — used ONLY by the
/// traversal test below, which must prove NO outbound request happens at
/// all (not merely that the eventual response is a 400).
async fn mock_kb_by_path_counting(
    counter: Arc<AtomicUsize>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/api/kb/{kb}/docs/by-path/{*path}",
        get(move |AxPath((_kb, path)): AxPath<(String, String)>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                if path == RESOLVE_PATH_KNOWN {
                    Json(json!({"id": FIX_DOC_ID, "path": path})).into_response()
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"}))).into_response()
                }
            }
        }),
    );
    mock_kb(router).await
}

/// DCB-W2.B.R fix 1 (security, MAJOR) — the traversal-path regression test.
/// `path=../../../research/docs/by-path/a.html` must 400 with
/// `invalid_segment` at kb-code-server's OWN route, and — the part a bare
/// status-code assertion can't prove — must never even ATTEMPT the outbound
/// call to kb: before the fix, `KbClient::resolve_doc_by_path` built a URL
/// whose dot-segments reqwest's WHATWG `Url::parse` normalizes away before
/// the request leaves this process, silently re-targeting a DIFFERENT kb
/// path than the one the `[doclens] kbs` allowlist check (on `?kb=`) ever
/// saw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_refuses_a_traversal_path_and_makes_no_outbound_request() {
    let alpha = fixture_alpha();
    let calls = Arc::new(AtomicUsize::new(0));
    let (kb_addr, _kb) = mock_kb_by_path_counting(calls.clone()).await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    for traversal in [
        "../../../research/docs/by-path/a.html",
        "features/../../../research/a.html",
        "..",
        "a/./b",
    ] {
        let (status, body) = http_get(
            &boot.base,
            &format!("/api/doc-lens/resolve-path?kb=platform&path={traversal}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{traversal:?}: {body}");
        assert_eq!(body["reason"], "invalid_segment", "{traversal:?}: {body}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a dot-segment path must never reach kb — the whole point of the guard"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_refuses_a_kb_that_is_not_allowlisted() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/resolve-path?kb=research&path={RESOLVE_PATH_KNOWN}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["reason"], "kb_not_allowlisted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_path_is_off_entirely_when_the_kbs_allowlist_is_empty() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_by_path().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        DoclensSection::default(),
    )
    .await;
    let (status, body) = http_get(
        &boot.base,
        &format!("/api/doc-lens/resolve-path?kb=platform&path={RESOLVE_PATH_KNOWN}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["reason"], "doclens_disabled");
}

// --- CORS ------------------------------------------------------------------

const ALLOWED_ORIGIN: &str = "https://kb.example.com";

/// The CORS probes never resolve a ref, so they boot against a ONE-FILE repo
/// rather than the full `alpha` fixture — 12 fixture builds of 16 files each
/// is pure git I/O this suite gains nothing from.
fn fixture_tiny() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(dir, "a.rb", "class A\nend\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "tiny"]);
    tmp
}

async fn cors_boot(origins: &[&str]) -> (Boot, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let alpha = fixture_tiny();
    let (kb_addr, kb_task) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], origins),
    )
    .await;
    (boot, alpha, kb_task)
}

async fn probe_get(base: &str, path: &str, origin: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header("Origin", origin)
        .send()
        .await
        .unwrap()
}

fn acao(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_allows_a_loopback_origin_get_on_doc_lens() {
    let (boot, _alpha, _kb) = cors_boot(&[]).await;
    let resp = probe_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
        "http://localhost:3000",
    )
    .await;
    assert_eq!(acao(&resp).as_deref(), Some("http://localhost:3000"));
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let vary = resp
        .headers()
        .get_all("vary")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(",")
        .to_ascii_lowercase();
    assert!(vary.contains("origin"), "vary must name origin: {vary}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_refuses_a_non_loopback_origin_when_no_allowlist_is_configured() {
    let (boot, _alpha, _kb) = cors_boot(&[]).await;
    let resp = probe_get(
        &boot.base,
        &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
        ALLOWED_ORIGIN,
    )
    .await;
    assert_eq!(acao(&resp), None, "empty allowlist = loopback-only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_allows_an_allowlisted_non_loopback_origin() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    let resp = probe_get(
        &boot.base,
        &format!("/api/doc-lens/repos?kb=platform&doc={FIX_DOC_ID}"),
        ALLOWED_ORIGIN,
    )
    .await;
    assert_eq!(acao(&resp).as_deref(), Some(ALLOWED_ORIGIN));
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_allowlist_match_is_exact_not_suffix() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    for evil in [
        "https://evil-kb.example.com",
        "https://kb.example.com.evil.test",
        "http://kb.example.com",
        "https://kb.example.com:8443",
    ] {
        let resp = probe_get(
            &boot.base,
            &format!("/api/doc-lens?kb=platform&doc={FIX_DOC_ID}&repo=alpha"),
            evil,
        )
        .await;
        assert_eq!(acao(&resp), None, "must refuse {evil}");
    }
}

async fn preflight(
    base: &str,
    path: &str,
    origin: &str,
    method: &str,
    headers: Option<&str>,
) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, format!("{base}{path}"))
        .header("Origin", origin)
        .header("Access-Control-Request-Method", method);
    if let Some(h) = headers {
        req = req.header("Access-Control-Request-Headers", h);
    }
    req.send().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_preflight_on_the_pin_route_allows_put_and_delete() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    let resp = preflight(
        &boot.base,
        "/api/doc-lens/pin",
        ALLOWED_ORIGIN,
        "PUT",
        Some("content-type"),
    )
    .await;
    assert!(resp.status().is_success(), "{}", resp.status());
    let methods = resp
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_uppercase();
    assert!(methods.contains("PUT"), "{methods}");
    assert!(methods.contains("DELETE"), "{methods}");
    let allowed = resp
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(allowed.contains("content-type"), "{allowed}");
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_preflight_on_the_pin_route_refuses_the_authorization_header() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    let resp = preflight(
        &boot.base,
        "/api/doc-lens/pin",
        ALLOWED_ORIGIN,
        "PUT",
        Some("authorization"),
    )
    .await;
    let allowed = resp
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    // In prod the bearer is injected at the EDGE and locally the loopback
    // bypass applies — a browser page never holds a token, so allowing the
    // header would only invite one to.
    assert!(
        !allowed.contains("authorization"),
        "allow-headers must not advertise authorization: {allowed}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_preflight_on_the_pin_route_is_a_plain_success() {
    // [A1] NOT a layer-order pin, despite this test's former name
    // (`cors_preflight_is_not_401ed_by_auth_bearer`) claiming one: `boot()`
    // sets no token, and every request in this suite originates from
    // 127.0.0.1, so `auth_bearer`'s UNCONDITIONAL loopback bypass
    // (`request_is_admitted` — loopback always admits, token or not, see
    // `kb_server::middleware`) would let this preflight through even if the
    // `CorsLayer` were wired INSIDE `auth_bearer` instead of outside it. A
    // real layer-order pin needs a non-loopback peer, which this HTTP-level
    // harness (a real `TcpListener` bound to loopback) has no cheap, honest
    // way to fake — spoofing `ConnectInfo` would need a lower-level harness
    // this suite doesn't have. What this DOES still pin, and is worth
    // keeping: a CORS preflight against a real mutating route (`PUT
    // /api/doc-lens/pin`) resolves to a plain 2xx, never a 401/403/5xx —
    // i.e. the route's CORS wiring itself doesn't error.
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    let resp = preflight(
        &boot.base,
        "/api/doc-lens/pin",
        ALLOWED_ORIGIN,
        "PUT",
        Some("content-type"),
    )
    .await;
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.status().is_success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_read_routes_do_not_advertise_put() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    let resp = preflight(&boot.base, "/api/doc-lens", ALLOWED_ORIGIN, "PUT", None).await;
    let methods = resp
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_uppercase();
    assert!(
        !methods.contains("PUT"),
        "read routes are GET-only: {methods}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_is_not_applied_to_api_file() {
    // THE major finding: an `/api`-wide layer would hand any allowlisted page
    // the full contents of every configured repo.
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    wait_for_indexed(&boot.base, "alpha", 1).await;
    for origin in ["http://localhost:3000", ALLOWED_ORIGIN] {
        let resp = probe_get(&boot.base, "/api/file?repo=alpha&path=a.rb", origin).await;
        assert_eq!(resp.status(), StatusCode::OK, "the route itself must work");
        assert_eq!(acao(&resp), None, "/api/file must carry NO ACAO ({origin})");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_is_not_applied_to_api_search_transcripts() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    for origin in ["http://localhost:3000", ALLOWED_ORIGIN] {
        let resp = probe_get(&boot.base, "/api/search/transcripts?q=x", origin).await;
        assert_eq!(
            acao(&resp),
            None,
            "transcripts lane must carry NO ACAO ({origin})"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_is_not_applied_to_api_repos_or_healthz() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    for path in ["/api/repos", "/healthz"] {
        let resp = probe_get(&boot.base, path, ALLOWED_ORIGIN).await;
        assert_eq!(acao(&resp), None, "{path} must carry no ACAO");
    }
}

/// The pinned route set. Its whole job is to stop a later builder sliding
/// another `/doc-lens/*` route under the CORS layer — the ACAO-carrying set
/// must be EXACTLY the four W1.C/W2.A doc-lens routes. Every phase that lands
/// one of the asserted-absent routes lands its probe entry in the SAME
/// commit: an entry may never be added ahead of the route (the test would
/// probe a 404 and pass vacuously) nor after it (a window where the route's
/// CORS posture is unpinned).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_layer_route_set_is_pinned() {
    let (boot, _alpha, _kb) = cors_boot(&[ALLOWED_ORIGIN]).await;
    // (path, expected-to-carry-ACAO, route-is-supposed-to-exist, note)
    let probes: &[(&str, bool, bool, &str)] = &[
        (
            "/api/doc-lens?kb=platform&doc=x0001&repo=alpha",
            true,
            true,
            "W1.C",
        ),
        (
            "/api/doc-lens/repos?kb=platform&doc=x0001",
            true,
            true,
            "W1.C",
        ),
        (
            "/api/doc-lens/pin?kb=platform&doc=x0001",
            true,
            true,
            "W1.C",
        ),
        // W2.A mounts the pin LIST on doclens_read deliberately (E8), so it
        // JOINS the ACAO set by design — a real widening, made explicit here
        // rather than as a silent side effect of a `.route()` call landing in
        // the wrong `Router::new()`. Flipped from asserted-ABSENT to
        // asserted-PRESENT in W2.A's own commit, alongside the route itself.
        ("/api/doc-lens/pins?kb=platform", true, true, "W2.A"),
        // SL7e mounts the PATH-addressed lens on doclens_read deliberately
        // (kb's board is cross-origin and this is a strictly smaller read
        // than `/doc-lens` already served to that origin) — flipped from
        // asserted-ABSENT to asserted-PRESENT in SL7e's own commit, so the
        // widening is a reviewed decision, not a `.route()` call that landed
        // in whichever `Router::new()` was nearest.
        (
            "/api/doc-lens/path?kb=platform&path=a.rb",
            true,
            true,
            "SL7e",
        ),
        // R20: W2.B adds this route but mounts it on the PLAIN api router. A
        // corpus-wide path→id lookup behind an exact-origin ACAO +
        // Allow-Credentials would hand every allowlisted origin a
        // doc-existence oracle no consumer asked for.
        (
            "/api/doc-lens/resolve-path?kb=platform&path=a.html",
            false,
            true,
            "W2.B — stays absent",
        ),
        // W3.A ships both, and BOTH stay off the ACAO set — flipped from
        // asserted-absent-and-unmounted to asserted-absent-but-MOUNTED in
        // W3.A's own commit, so the widening that never happened is visible
        // in review rather than implicit. The GET probe below reaches the
        // POST-only sync route as a 405 and `/api/doc-refs` (no `path=`) as a
        // 400 — both non-404, which is exactly what the `route_exists`
        // hardening needs to tell "mounted without CORS" from "not mounted".
        (
            "/api/doc-lens/sync",
            false,
            true,
            "W3.A — loopback-only, never CORS'd",
        ),
        (
            "/api/doc-refs?repo=alpha",
            false,
            true,
            "W3.A — same-origin only, never CORS'd",
        ),
        (
            "/api/sets/from-doc",
            false,
            true,
            "W3.C — loopback-only, never CORS'd",
        ),
        ("/api/file?repo=alpha&path=a.rb", false, true, "never"),
        ("/api/search/transcripts?q=x", false, true, "never"),
        ("/api/repos", false, true, "never"),
        ("/api/lenses?repo=alpha&path=a.rb", false, true, "never"),
    ];
    let mut carrying: Vec<&str> = Vec::new();
    for (path, expect_acao, route_exists, note) in probes {
        let resp = probe_get(&boot.base, path, ALLOWED_ORIGIN).await;
        let status = resp.status();
        let has = acao(&resp).is_some();
        if has {
            carrying.push(path);
        }
        assert_eq!(
            has, *expect_acao,
            "{path} ({note}): expected ACAO={expect_acao}, got {has} (status {status})",
        );
        // DCB-W2.B.R fix 12 — cheap hardening, scoped to the no-ACAO-expected
        // rows whose route genuinely IS shipped: without this, a probe
        // returning no ACAO because the route is simply UNMOUNTED (a typo,
        // or a route that regressed away) would pass this test just as
        // happily as one deliberately kept off the CORS'd set — "route
        // exists without ACAO" and "no route" must stay distinguishable. A
        // `true`-ACAO row needs no such check: the ACAO header's presence
        // already proves the CORS layer matched a real route.
        if !*expect_acao && *route_exists {
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{path} ({note}): route is supposed to exist — a 404 here is \
                 indistinguishable from the route being unmounted",
            );
        }
    }
    assert_eq!(
        carrying,
        vec![
            "/api/doc-lens?kb=platform&doc=x0001&repo=alpha",
            "/api/doc-lens/repos?kb=platform&doc=x0001",
            "/api/doc-lens/pin?kb=platform&doc=x0001",
            "/api/doc-lens/pins?kb=platform",
            "/api/doc-lens/path?kb=platform&path=a.rb",
        ],
        "the ACAO-carrying set is EXACTLY the shipped doc-lens routes"
    );
}

// --- DCB W3.A — the reverse index's own route posture ----------------------

/// The route-level half of W3.A (the engine's own behaviour is unit-covered
/// in `doclens::sync`'s tests, against a mock kb and a seeded store): the
/// sync TRIGGER is loopback-only and the reverse LOOKUP is an ordinary
/// `auth_bearer` read that validates its `path` param. One boot, because a
/// daemon boot is the expensive part of this suite and these three
/// assertions are all about the same router.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doc_refs_and_sync_route_posture() {
    let alpha = fixture_tiny();
    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();

    // A pass with no pins at all is a legal, empty pass — never an error.
    let resp = client
        .post(format!("{}/api/doc-lens/sync", boot.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "doclens-sync/1");
    assert_eq!(body["docs_resolved"], 0);
    assert_eq!(body["kbs_synced"], 0);
    assert_eq!(body["forced"], false);

    // D-A: the pin moved onto `auth_bearer`; the SYNC did not. Same XFF spoof
    // every other loopback-only mutation test in this crate uses.
    let resp = client
        .post(format!("{}/api/doc-lens/sync", boot.base))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "POST /api/doc-lens/sync must 404 a non-loopback caller"
    );

    // The reverse lookup: a known repo with nothing cited answers an empty,
    // LIVE-flagged response rather than a 404.
    let (status, body) = http_get(&boot.base, "/api/doc-refs?repo=alpha&path=a.rb").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "doc-refs/1");
    assert_eq!(body["repo"], "alpha");
    assert_eq!(body["live"], true);
    assert_eq!(body["claims"].as_array().unwrap().len(), 0);

    // DCB-W2.B.R fix 1's lesson: dot-segment guards on EVERY path param of
    // this lane, not only the one that reaches an outbound URL.
    for bad in ["../etc/passwd", "a/./b"] {
        let (status, body) =
            http_get(&boot.base, &format!("/api/doc-refs?repo=alpha&path={bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = http_get(&boot.base, "/api/doc-refs?repo=nope&path=a.rb").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown repo");
}

// --- DCB-W3.A.R fix 1 [BLOCKER] — a claim exists only under a live pin -----

/// `DELETE /api/doc-lens/pin` must drop the doc's `doc_refs` rows too, not
/// only its own row — otherwise an unpinned doc's claims become immortal
/// orphans no future sync pass ever revisits (the doc is no longer pinned,
/// so `sync_one_kb` never looks at it again).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpinning_a_doc_drops_its_doc_refs_claims() {
    let alpha = fixture_alpha();
    let (kb_addr, _kb) = mock_kb_default_with_feed().await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();

    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "alpha"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let sync = client
        .post(format!("{}/api/doc-lens/sync", boot.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(sync.status(), StatusCode::OK);
    let sync_body: Value = sync.json().await.unwrap();
    assert_eq!(sync_body["docs_resolved"], 1, "{sync_body}");

    let path = "app/controllers/carts_controller.rb";
    let (status, body) =
        http_get(&boot.base, &format!("/api/doc-refs?repo=alpha&path={path}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Ordinals 0, 1 and 2 of the fixture doc all resolve `present` against
    // this exact path (the GOLDEN table) — three claims, not one.
    assert_eq!(
        body["claims"].as_array().unwrap().len(),
        3,
        "sync wrote claims while the doc was pinned: {body}"
    );

    let del = client
        .delete(format!(
            "{}/api/doc-lens/pin?kb=platform&doc={FIX_DOC_ID}",
            boot.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);

    let (status, body) =
        http_get(&boot.base, &format!("/api/doc-refs?repo=alpha&path={path}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["claims"].as_array().unwrap().is_empty(),
        "the claim must die WITH the pin, not outlive it: {body}"
    );
}

/// The exact reused-repo_id scenario the review named: `store::upsert_repo`
/// is `ON CONFLICT(name) DO UPDATE SET root`, so a repo NAME re-pointed to a
/// different root keeps the SAME `repos.id`. A pin's `doc_refs` claims,
/// seeded under that id while the pin was still valid, must die in the SAME
/// boot-prune pass that drops the stale pin — otherwise the re-pointed
/// checkout would serve those claims as live links against a tree they were
/// never resolved on (a persisted verdict outliving the tree it was computed
/// against, which this whole feature refuses to produce).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_prune_of_a_root_mismatched_pin_also_drops_its_claims() {
    let alpha = fixture_alpha();
    let old_root = fixture_tiny();
    let tmp = tempfile::tempdir().unwrap();
    let repo_id = {
        let store = store_at(tmp.path());
        // Pre-create the "alpha" repo row against the OLD root — boot's own
        // `upsert_repo("alpha", <new root>)` then RE-POINTS this same row's
        // `root`, reusing the id, which is exactly the mechanism the review
        // named.
        let repo_id = store
            .upsert_repo("alpha", &old_root.path().to_string_lossy())
            .unwrap();
        seed_pin(
            &store,
            FIX_DOC_ID,
            "alpha",
            &old_root.path().to_string_lossy(),
        );
        store
            .replace_doc_refs(
                &kb_code_server::store::DocRefWrite {
                    kb: "platform",
                    doc_id: FIX_DOC_ID,
                    repo_id,
                    doc_title: "Checkout flow",
                    doc_path: "features/checkout.html",
                    doc_hash: None,
                    head_sha: None,
                    dirty: false,
                    seen_at: 1,
                },
                &[kb_code_server::store::NewDocRef {
                    ordinal: 0,
                    kind: "path_line".into(),
                    raw_hint: "carts_controller.rb:12".into(),
                    resolved_path: "app/controllers/carts_controller.rb".into(),
                    line_start: Some(12),
                    line_end: None,
                    line_state: Some("confirmed".into()),
                    group_key: None,
                    group_label: None,
                }],
            )
            .unwrap();
        repo_id
    };

    let (kb_addr, _kb) = mock_kb_default().await;
    let boot = boot_at(
        tmp,
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;

    // The pin itself is gone (already covered by the sibling prune test) —
    // the point of THIS test is that its claim died WITH it.
    let (_, pins) = http_get(&boot.base, "/api/doc-lens/pins?kb=platform").await;
    assert!(pins["pins"].as_array().unwrap().is_empty(), "{pins}");

    let (status, body) = http_get(
        &boot.base,
        "/api/doc-refs?repo=alpha&path=app/controllers/carts_controller.rb",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["claims"].as_array().unwrap().is_empty(),
        "a stale pin's claim must not survive the boot prune — it would serve as a live link \
         against a tree (repo_id {repo_id}) it was never resolved on: {body}"
    );
}

/// DCB-W3.A.R fix 5 — two overlapping `POST /api/doc-lens/sync` calls must
/// not both run: the second must answer `409` while the first is still in
/// flight, never queue behind it. The mock's feed route notifies a
/// `tokio::sync::Notify` the instant it is entered (so the test synchronizes
/// on a real event, not a wall-clock guess — the "isolated-rerun-green"
/// standard on a loaded host) and then sleeps, holding the first pass open
/// long enough for the second request to land and observe the guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_concurrent_sync_pass_is_refused_with_409() {
    let alpha = fixture_tiny();
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_srv = entered.clone();
    let router = Router::new().route(
        "/api/kb/{kb}/code-refs",
        get(move || {
            let entered = entered_srv.clone();
            async move {
                entered.notify_one();
                tokio::time::sleep(Duration::from_millis(400)).await;
                Json(json!({"schema": "coderef-feed/1", "kb": "platform", "docs": []}))
                    .into_response()
            }
        }),
    );
    let (kb_addr, _kb) = mock_kb(router).await;
    let boot = boot(
        &[("alpha", alpha.path())],
        Some(kb_addr),
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    let client = reqwest::Client::new();

    // A pin is required — `run_doclens_sync` only ever calls the feed for a
    // kb that has at least one (`doc_lens_pin_kbs`), which is what makes the
    // mock's slow route actually get hit.
    let put = client
        .put(format!("{}/api/doc-lens/pin", boot.base))
        .json(&json!({"kb": "platform", "doc": FIX_DOC_ID, "repo": "alpha"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let base = boot.base.clone();
    let first = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base}/api/doc-lens/sync"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
    });

    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the first pass's feed request must land");

    let second = client
        .post(format!("{}/api/doc-lens/sync", boot.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "must not queue behind the running pass"
    );
    let second_body: Value = second.json().await.unwrap();
    assert_eq!(
        second_body["reason"], "sync_already_running",
        "{second_body}"
    );

    let first_resp = first.await.unwrap();
    assert_eq!(first_resp.status(), StatusCode::OK);

    // The guard is released — a THIRD request, now that the first has
    // completed, must succeed rather than staying wedged.
    let third = client
        .post(format!("{}/api/doc-lens/sync", boot.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        third.status(),
        StatusCode::OK,
        "the guard must release after the pass ends"
    );
}

// --- SL7e (v0.42, slate D29) — the path-addressed lens ---------------------

/// The whole `path_state × line_state` table this route can produce, against
/// `alpha`, in ONE boot (a daemon boot is this suite's expensive part).
///
/// The kb daemon is DISABLED here on purpose, and that is an assertion in
/// itself: `/api/doc-lens` answers `503 kb_daemon_disabled` on this very
/// boot (`doc_lens_reports_kb_daemon_disabled_distinctly`), so every 200
/// below proves the path lens makes no kb hop at all — the `?kb=` param is
/// the `[doclens] kbs` gate and nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn path_lens_resolves_a_repo_path_with_and_without_a_line() {
    let alpha = fixture_alpha();
    let boot = boot(
        &[("alpha", alpha.path())],
        None,
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    // 1. An exact repo-relative path, no line: the path IS the whole claim.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "codelens-path/1");
    assert_eq!(body["repo"], "alpha");
    assert_eq!(body["path"], "app/controllers/carts_controller.rb");
    assert_eq!(body["path_state"], "present");
    assert_eq!(body["resolved_path"], "app/controllers/carts_controller.rb");
    assert_eq!(body["candidate_count"], 1);
    // `null`, NEVER "absent" — a caption consumer reads "absent" as "the
    // line is not there", which an unasked question does not support.
    assert!(body["line_state"].is_null(), "{body}");
    assert_eq!(body["line_reason"], "no_line_hint");
    assert!(body["file_lines"].is_null(), "{body}");

    // 2. The same path WITH a line. There is no document here, so there is
    //    no context, so there are no confirm tokens — and a bare line number
    //    with no token is `unverifiable` BY DEFINITION (doclens' own rule).
    //    The file's real length rides along so the caller can judge.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb&line=12",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "present");
    assert_eq!(body["line_hint"], 12);
    assert_eq!(body["line_state"], "unverifiable", "{body}");
    assert_eq!(body["line_reason"], "no_token");
    assert_eq!(body["file_lines"], 39);
    assert!(body["resolved_line"].is_null(), "{body}");

    // 3. A line PAST the end of the file is still `unverifiable`, not
    //    `absent`: golden #6's rule (`line_state_reports_file_lines_for_an_
    //    out_of_range_hint`) — this route mints no verdict the engine
    //    wouldn't. `file_lines` is what makes the answer usable.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb&line=9999",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["line_state"], "unverifiable", "{body}");
    assert_eq!(body["file_lines"], 39);

    // 4. A bare basename that is uniquely a `/`-anchored suffix resolves —
    //    and `spec/controllers/carts_controller_spec.rb` must NOT count, or
    //    this would be `ambiguous` (the anti-fuzzy property).
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=carts_controller.rb",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "present", "{body}");
    assert_eq!(body["resolved_path"], "app/controllers/carts_controller.rb");

    // 5. Six candidates ⇒ `ambiguous`, a cardinality — never a ranked guess.
    let (status, body) =
        http_get(&boot.base, "/api/doc-lens/path?kb=platform&path=algolia.rb").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "ambiguous", "{body}");
    assert_eq!(body["candidate_count"], 6);
    assert!(body["resolved_path"].is_null(), "{body}");

    // 6. A path this checkout does not have is `absent` at 200 — a VERDICT.
    //    A 404 would say "this daemon cannot answer", which is a different
    //    (and false) statement.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/models/nope.rb&line=3",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "absent", "{body}");
    assert!(body["line_state"].is_null(), "{body}");
    assert_eq!(body["line_reason"], "path_not_present");
    assert!(body["file_lines"].is_null(), "{body}");

    // 7. A traversal hint is `absent` with the reason SAID — never a read.
    //    `normalize_path_hint` refuses it, so nothing is ever joined to the
    //    repo root; the only path this route ever opens is a row from its own
    //    `files` table. `path_note` is what distinguishes "not a path" from
    //    "not in this tree", which would otherwise both be a bare `absent`.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=../../../etc/passwd",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "absent", "{body}");
    assert_eq!(body["path_note"], "unusable path", "{body}");
    assert!(body["resolved_path"].is_null(), "{body}");
}

/// SL7f (v0.42 amendment) — `?context=` wires `confirm_tokens` onto the path
/// lens, reusing `algolia_user_token`/`add_cart_bundle`'s same known lines in
/// `carts_controller.rb` that the document-based golden (ordinals 0 and 2)
/// already pins: `algolia_user_token` is at 12 and nowhere else (confirmed);
/// `add_cart_bundle` is at 30 but the citation is 19, +11 outside the ±3
/// window and inside ±64 (drifted). No fixture-file change — this route's
/// synthetic ref now carries the SAME context text a document would, run
/// through the SAME `confirm_tokens`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn path_lens_context_confirms_or_drifts_a_line() {
    let alpha = fixture_alpha();
    let boot = boot(
        &[("alpha", alpha.path())],
        None,
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;

    // 1. A context token unique at the cited line ⇒ confirmed, D6 keeps the
    //    CITED line (resolved_line == line_hint), never the token's own line.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb\
         &line=12&context=algolia_user_token",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "present", "{body}");
    assert_eq!(body["line_state"], "confirmed", "{body}");
    assert!(body["line_reason"].is_null(), "{body}");
    assert_eq!(body["resolved_line"], 12, "{body}");
    assert_eq!(body["file_lines"], 39, "{body}");

    // 2. A context token unique only OUTSIDE the ±3 window (but inside ±64)
    //    ⇒ drifted, and `resolved_line` MOVES to the token's real line — the
    //    one case D6 exempts.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb\
         &line=19&context=add_cart_bundle",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "present", "{body}");
    assert_eq!(body["line_state"], "drifted", "{body}");
    assert!(body["line_reason"].is_null(), "{body}");
    assert_eq!(body["resolved_line"], 30, "{body}");
    assert_eq!(body["file_lines"], 39, "{body}");

    // 3. No `?context=` at all is BYTE-IDENTICAL to SL7e (unchanged): a bare
    //    line number has no confirm token, so the verdict is unverifiable BY
    //    DEFINITION even though the same file/line combination now HAS a
    //    resolvable token when asked case 1's way.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb&line=12",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["line_state"], "unverifiable", "{body}");
    assert_eq!(body["line_reason"], "no_token", "{body}");
    assert!(body["resolved_line"].is_null(), "{body}");

    // 4. A context carrying no usable token (too short, or pure stoplist
    //    prose) degrades to the same honest `unverifiable` — never an
    //    optimistic guess from an unrelated word.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb\
         &line=12&context=the+file+has+a+method",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["line_state"], "unverifiable", "{body}");
    assert_eq!(body["line_reason"], "no_token", "{body}");
}

/// The refusals: a missing `path`, an empty one, a kb outside the allowlist,
/// and — the one that is a real decision rather than validation — no
/// `?repo=` on a daemon serving more than one checkout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn path_lens_refuses_a_missing_path_an_empty_path_and_an_unchosen_checkout() {
    let alpha = fixture_alpha();
    let beta = fixture_beta();
    let boot = boot(
        &[("alpha", alpha.path()), ("beta", beta.path())],
        None,
        doclens_cfg(&["platform"], &[]),
    )
    .await;
    wait_for_indexed(&boot.base, "alpha", ALPHA_FILES).await;
    wait_for_indexed(&boot.base, "beta", 1).await;

    // No `path` at all — axum's own `Query` rejection, before the handler.
    let (status, _) = http_get(&boot.base, "/api/doc-lens/path?kb=platform&repo=alpha").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Present but empty — the handler's own refusal, with a `reason`.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["reason"], "invalid_segment");

    // The `[doclens] kbs` gate applies here exactly as it does to every
    // sibling read, even though this route never calls kb.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=research&path=app/controllers/carts_controller.rb&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["reason"], "kb_not_allowlisted");

    // TWO checkouts and no opinion: refused, not guessed. Answering against
    // whichever repo sorted first would be a confident answer to the wrong
    // question — doc-lens' "a checkout is NEVER auto-selected".
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/controllers/carts_controller.rb",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["reason"], "repo_required");
    assert!(
        body["error"].as_str().unwrap().contains("alpha, beta"),
        "the refusal names the choices: {body}"
    );

    // Named explicitly, both answer — and they disagree, which is the whole
    // reason the pick may not be made for the caller: `beta` has its own
    // `carts_controller.rb` at a different path and none of alpha's
    // services.
    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/services/algolia/search_service.rb&repo=alpha",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "present");
    assert_eq!(body["repo"], "alpha");

    let (status, body) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=app/services/algolia/search_service.rb&repo=beta",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path_state"], "absent");
    assert_eq!(body["repo"], "beta");

    // An unconfigured checkout IS a 404 — the one 404 this route has.
    let (status, _) = http_get(
        &boot.base,
        "/api/doc-lens/path?kb=platform&path=a.rb&repo=nosuchrepo",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
