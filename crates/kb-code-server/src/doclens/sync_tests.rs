//! DCB W3.A — `doclens::sync` unit coverage.
//!
//! Every test here drives the REAL engine (`run_doclens_sync`) against a real
//! `SharedState` (`crate::build_state_for_test`) and a mock kb daemon
//! (`join::kb_client::test_support::mock_kb_server`) — no HTTP round trip to
//! self, no live `:4000` daemon, no watcher wait. The `files` table is seeded
//! directly (`Store::upsert_file`) rather than waiting for a mirror walk:
//! `bind_and_spawn`'s initial index is a separate spawn this fixture does not
//! run, which is what makes every assertion below deterministic on a loaded
//! host.

use super::*;
use crate::config::{DoclensSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection};
use crate::doclens::resolve::LineState;
use crate::join::kb_client::test_support::mock_kb_server;
use crate::store::DocLensPin;
use axum::{
    extract::{Path as AxPath, Query as AxQuery},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// --- the fixture repo ------------------------------------------------------

const RESOLVER: &str = "pub fn resolve() {\n    // one\n    let a = 1;\n    // two\n    let b = 2;\n    // three\n    let c = 3;\n    // four\n    let d = 4;\n}\n";
const STORE_RS: &str = "pub struct Store;\nimpl Store {\n    pub fn open() {}\n}\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        dir.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `src/resolver.rs` + `src/store.rs` + `vendor/resolver.rs` — the third file
/// exists so a BARE `resolver.rs` hint has two `/`-anchored suffix candidates
/// and resolves `ambiguous`, which pins "only present-unique refs become
/// claims" to a real cardinality rather than a stubbed state.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    for (rel, body) in [
        ("src/resolver.rs", RESOLVER),
        ("src/store.rs", STORE_RS),
        ("vendor/resolver.rs", RESOLVER),
    ] {
        let abs = dir.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    tmp
}

const SEEDED_FILES: &[&str] = &["src/resolver.rs", "src/store.rs", "vendor/resolver.rs"];

// --- the mock kb daemon ----------------------------------------------------

/// What the mock observed + the two knobs a test can flip MID-RUN (so a
/// "kb went away" case reuses one state rather than rebuilding it).
#[derive(Default)]
struct MockHits {
    feed: AtomicUsize,
    body: AtomicUsize,
    /// Every `?cursor=` the feed saw, in order (`None` = the param was
    /// absent, i.e. "start from the beginning").
    cursors: Mutex<Vec<Option<String>>>,
    /// Every `?limit=` the feed saw, in order (DCB-W3.A.R fix 4 — the
    /// page-clamp/remaining-budget tests read this to prove a LATER kb in
    /// one pass requests only what's left of `batch_cap`, not a fresh page).
    limits: Mutex<Vec<u32>>,
    /// Doc ids the per-doc route 404s even though the feed still lists them.
    gone: Mutex<Vec<String>>,
    /// Every route answers `503` — a deterministic stand-in for "kb is down"
    /// that needs no socket teardown race.
    down: AtomicBool,
    /// Path-segment doc id → the BODY to serve instead of the normal
    /// by-doc_id lookup (DCB-W3.A.R fix 2's moves re-key test): the
    /// requested id is the OLD one, the served body's own `doc_id` field is
    /// the NEW one — exactly the shape kb's moves-301 chain produces
    /// (D13/R13: the client follows the redirect and reads the new id off
    /// the BODY, never off the URL).
    body_override: Mutex<HashMap<String, Value>>,
}

impl MockHits {
    fn cursors(&self) -> Vec<Option<String>> {
        self.cursors.lock().unwrap().clone()
    }

    fn limits(&self) -> Vec<u32> {
        self.limits.lock().unwrap().clone()
    }
}

/// One `coderef/1` doc body. `extracted_at` doubles as the feed's sort key.
fn doc(id: &str, extracted_at: i64, refs: Vec<Value>) -> Value {
    json!({
        "schema": "coderef/1",
        "kb": "fixture-kb",
        "doc_id": id,
        "doc_path": format!("features/{id}.html"),
        "doc_hash": format!("hash-{id}"),
        "title": format!("Doc {id}"),
        "extracted_at": extracted_at,
        "never_scanned": false,
        "code_rev": null,
        "ref_count": refs.len(),
        "ungrouped_count": 0,
        "truncated": false,
        "groups": [{"ordinal": 0, "key": "kb-h-g1", "label": "G1 · Resolver", "anchor": "kb-h-g1"}],
        "refs": refs,
    })
}

fn code_ref(ordinal: u32, kind: &str, raw: &str, hint: &str, line: Option<u32>) -> Value {
    json!({
        "ordinal": ordinal,
        "group": "kb-h-g1",
        "kind": kind,
        "raw": raw,
        "path_hint": hint,
        "line_start": line,
        "line_end": null,
        "line_spans": null,
        "symbol_container": null,
        "symbol_member": null,
        "context": "prose around the citation",
        "context_tokens": ["resolve"],
        "declared": false,
    })
}

/// The two-doc corpus the happy-path tests use: `doc1` cites
/// `src/resolver.rs` (present) plus an absent and an AMBIGUOUS ref; `doc2`
/// cites `src/resolver.rs` AND `src/store.rs` (both present) — so
/// `src/resolver.rs` ends up cited by TWO documents, the only shape that
/// proves the reverse index is a real many-to-one join.
fn two_doc_corpus() -> Vec<Value> {
    vec![
        doc(
            "doc1",
            1_754_500_000,
            vec![
                code_ref(
                    0,
                    "path_line",
                    "src/resolver.rs:3",
                    "src/resolver.rs",
                    Some(3),
                ),
                code_ref(1, "path", "src/nope.rs", "src/nope.rs", None),
                code_ref(2, "path", "resolver.rs", "resolver.rs", None),
            ],
        ),
        doc(
            "doc2",
            1_754_500_001,
            vec![
                code_ref(
                    0,
                    "path_line",
                    "src/resolver.rs:9",
                    "src/resolver.rs",
                    Some(9),
                ),
                code_ref(1, "path_line", "src/store.rs:2", "src/store.rs", Some(2)),
            ],
        ),
    ]
}

/// The cap test's own corpus: a THIRD pinned doc (citing
/// `vendor/resolver.rs`, which nothing else cites) beyond a `batch_cap` of 2,
/// so "the next pass reaches what the capped one skipped" is observable
/// rather than assumed.
fn three_doc_corpus() -> Vec<Value> {
    let mut docs = two_doc_corpus();
    docs.push(doc(
        "doc3",
        1_754_500_002,
        vec![code_ref(
            0,
            "path_line",
            "vendor/resolver.rs:1",
            "vendor/resolver.rs",
            Some(1),
        )],
    ));
    docs
}

fn doc_key(d: &Value) -> (i64, String) {
    (
        d["extracted_at"].as_i64().unwrap_or(0),
        d["doc_id"].as_str().unwrap_or_default().to_string(),
    )
}

/// A faithful keyset feed: `"<extracted_at>:<artifact_id>"`, strictly-after
/// semantics, `next_cursor` OMITTED on the last page (kb's own contract).
fn feed_page(docs: &[Value], cursor: Option<&str>, page_size: usize, with_refs: bool) -> Value {
    let after = cursor.and_then(|c| {
        c.split_once(':')
            .and_then(|(h, id)| h.parse::<i64>().ok().map(|t| (t, id.to_string())))
    });
    let mut sorted: Vec<&Value> = docs.iter().collect();
    sorted.sort_by_key(|d| doc_key(d));
    let remaining: Vec<&Value> = sorted
        .into_iter()
        .filter(|d| match &after {
            None => true,
            Some(cur) => doc_key(d) > *cur,
        })
        .collect();
    let page: Vec<&Value> = remaining.iter().copied().take(page_size).collect();
    let more = remaining.len() > page.len();
    let out: Vec<Value> = page
        .iter()
        .map(|d| {
            let mut d = (*d).clone();
            if !with_refs {
                // `?refs=0` — headers only, exactly as kb answers it.
                d["refs"] = json!([]);
                d["groups"] = json!([]);
            }
            d
        })
        .collect();
    let mut body = json!({ "schema": "coderef-feed/1", "kb": "fixture-kb", "docs": out });
    if more {
        if let Some(last) = page.last() {
            let (t, id) = doc_key(last);
            body["next_cursor"] = json!(format!("{t}:{id}"));
        }
    }
    body
}

async fn spawn_mock(
    docs: Vec<Value>,
    page_size: usize,
) -> (
    std::net::SocketAddr,
    Arc<MockHits>,
    tokio::task::JoinHandle<()>,
) {
    let hits = Arc::new(MockHits::default());
    let docs = Arc::new(docs);

    let feed_hits = hits.clone();
    let feed_docs = docs.clone();
    let body_hits = hits.clone();
    let body_docs = docs.clone();

    let router = Router::new()
        .route(
            "/api/kb/{kb}/code-refs",
            get(move |AxQuery(q): AxQuery<HashMap<String, String>>| {
                let hits = feed_hits.clone();
                let docs = feed_docs.clone();
                async move {
                    hits.feed.fetch_add(1, Ordering::SeqCst);
                    let cursor = q.get("cursor").cloned();
                    hits.cursors.lock().unwrap().push(cursor.clone());
                    if hits.down.load(Ordering::SeqCst) {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": "kb is down"})),
                        )
                            .into_response();
                    }
                    let with_refs = q.get("refs").map(|v| v != "0").unwrap_or(true);
                    // A real feed honours `?limit=` — and sync CLAMPS that
                    // limit to `batch_cap`, so a mock that ignored it would
                    // hide the very interaction `batch_cap_…` pins.
                    let limit = q
                        .get("limit")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(page_size);
                    hits.limits.lock().unwrap().push(limit as u32);
                    Json(feed_page(
                        &docs,
                        cursor.as_deref(),
                        page_size.min(limit.max(1)),
                        with_refs,
                    ))
                    .into_response()
                }
            }),
        )
        .route(
            "/api/kb/{kb}/docs/{doc}/code-refs",
            get(move |AxPath((_kb, id)): AxPath<(String, String)>| {
                let hits = body_hits.clone();
                let docs = body_docs.clone();
                async move {
                    hits.body.fetch_add(1, Ordering::SeqCst);
                    // DCB-W3.A.R fix 2 — an override always wins: it stands
                    // in for kb serving the MOVED doc's body at the OLD id's
                    // URL, whose own `doc_id` field names the NEW id.
                    if let Some(body) = hits.body_override.lock().unwrap().get(&id).cloned() {
                        return Json(body).into_response();
                    }
                    let gone = hits.gone.lock().unwrap().contains(&id);
                    if gone || !docs.iter().any(|d| d["doc_id"] == json!(id)) {
                        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"})))
                            .into_response();
                    }
                    let d = docs.iter().find(|d| d["doc_id"] == json!(id)).unwrap();
                    Json(d.clone()).into_response()
                }
            }),
        );
    let (addr, handle) = mock_kb_server(router).await;
    (addr, hits, handle)
}

// --- the fixture -----------------------------------------------------------

struct Fx {
    state: SharedState,
    hits: Arc<MockHits>,
    repo_id: i64,
    #[allow(dead_code)]
    repo: tempfile::TempDir,
    #[allow(dead_code)]
    home: tempfile::TempDir,
    #[allow(dead_code)]
    server: tokio::task::JoinHandle<()>,
}

impl Fx {
    fn pin(&self, kb: &str, doc_id: &str) {
        let root = self.state.repos[0].path.to_string_lossy().to_string();
        self.state
            .store
            .put_doc_lens_pin(&DocLensPin {
                kb: kb.to_string(),
                doc_id: doc_id.to_string(),
                repo: "alpha".to_string(),
                repo_root: root,
                doc_hash: None,
                pinned_at: 1,
            })
            .expect("put pin");
    }

    fn claims(&self, path: &str) -> DocRefsOut {
        doc_refs_for(&self.state, "alpha", path).expect("doc_refs_for")
    }

    fn pinned(&self, kb: &str, doc_id: &str) -> bool {
        self.state
            .store
            .get_doc_lens_pin(kb, doc_id)
            .unwrap()
            .is_some()
    }
}

async fn fixture(docs: Vec<Value>, page_size: usize, kbs: &[&str], batch_cap: usize) -> Fx {
    let repo = fixture_repo();
    let (addr, hits, server) = spawn_mock(docs, page_size).await;
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "alpha".to_string(),
            path: std::fs::canonicalize(repo.path()).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: true,
            url: format!("http://{addr}"),
            token_file: None,
            // Pinned so `doc_public_href` is deterministic.
            public_url: Some("https://kb.example.com".to_string()),
        },
        doclens: DoclensSection {
            kbs: kbs.iter().map(|s| (*s).to_string()).collect(),
            // Raised well above the 3 s production default: this suite runs on
            // an IO-bound host and the subject here is the SYNC, not the
            // per-request budget.
            deadline_ms: 60_000,
            batch_cap,
            ..DoclensSection::default()
        },
        // Hermetic: never arm a watcher over the operator's real
        // `~/.claude/projects` for a doc_refs unit test.
        transcripts: TranscriptsSection {
            enabled: false,
            ..TranscriptsSection::default()
        },
        ..KbCodeConfig::default()
    };
    let home = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(home.path(), "kb-code");
    let state = crate::build_state_for_test(cfg, paths)
        .await
        .expect("build_state_for_test");
    let repo_id = *state.repo_ids.get("alpha").expect("repo id");
    for rel in SEEDED_FILES {
        state
            .store
            .upsert_file(repo_id, rel, "deadbeef", "rust", 64)
            .expect("seed files row");
    }
    Fx {
        state,
        hits,
        repo_id,
        repo,
        home,
        server,
    }
}

/// The default shape: one page, allowlisted kb, production batch_cap.
async fn fx() -> Fx {
    fixture(two_doc_corpus(), 100, &["fixture-kb"], 200).await
}

// --- the cursor rewind (pure) ---------------------------------------------

#[test]
fn rewind_cursor_subtracts_one_second_and_keeps_the_artifact_id() {
    // §1.6 — kb's own prescription for the same-second keyset gap: resume at
    // `extracted_at - 1`, SAME artifact_id.
    assert_eq!(rewind_cursor("1754500000:abc123"), "1754499999:abc123");
    // An id containing its own colon survives (the split is on the FIRST).
    assert_eq!(rewind_cursor("10:a:b"), "9:a:b");
    assert_eq!(rewind_cursor("0:abc"), "-1:abc", "no underflow panic");
}

#[test]
fn rewind_cursor_is_total_and_never_invents_a_cursor() {
    // A grammar this client does not recognise comes back VERBATIM — losing
    // the rewind for one pass is survivable; resuming from an arbitrary
    // position is not.
    for opaque in ["", "no-colon", "notanumber:abc", ":abc"] {
        assert_eq!(rewind_cursor(opaque), opaque, "{opaque:?}");
    }
}

#[test]
fn line_state_strings_match_the_wire_exactly() {
    // `line_state` is persisted as TEXT and echoed back on `doc-refs/1`, so
    // `as_str` and the `Serialize` derive must never drift apart.
    for st in [
        LineState::Confirmed,
        LineState::Drifted,
        LineState::Unverifiable,
        LineState::Absent,
    ] {
        assert_eq!(
            serde_json::to_value(st).unwrap(),
            serde_json::Value::String(st.as_str().to_string()),
            "{st:?}"
        );
    }
}

/// DCB-W3.A.R fix 6 — `doc_refs.doc_title` must never be persisted empty:
/// title, then the doc path's basename, then the doc id, in that order.
#[test]
fn doc_title_or_fallback_prefers_title_then_basename_then_id() {
    assert_eq!(
        doc_title_or_fallback(Some("Real Title"), Some("features/doc1.html"), "doc1"),
        "Real Title"
    );
    assert_eq!(
        doc_title_or_fallback(None, Some("features/doc1.html"), "doc1"),
        "doc1.html",
        "no title — falls back to the path's basename"
    );
    assert_eq!(
        doc_title_or_fallback(None, None, "doc1"),
        "doc1",
        "no title, no path — falls back to the id"
    );
    assert_eq!(
        doc_title_or_fallback(Some("   "), Some("features/doc1.html"), "doc1"),
        "doc1.html",
        "a blank/whitespace-only title is treated as absent"
    );
    assert_eq!(
        doc_title_or_fallback(None, Some(""), "doc1"),
        "doc1",
        "an empty path is treated as absent, falls through to the id"
    );
    assert_eq!(
        doc_title_or_fallback(None, Some("doc1.html"), "doc1"),
        "doc1.html",
        "no `/` in the path — the basename is the whole path"
    );
    assert_eq!(
        doc_title_or_fallback(None, Some("a/b/"), "doc1"),
        "doc1",
        "a trailing slash yields an empty basename — falls through to the id"
    );
}

// --- the pass --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_over_a_two_doc_corpus_makes_claims_queryable_by_repo_and_path() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 2, "{stats:?}");
    assert_eq!(stats.kbs_synced, 1);
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);

    // `src/resolver.rs` is cited by BOTH docs.
    let out = fx.claims("src/resolver.rs");
    assert_eq!(out.schema, DOC_REFS_SCHEMA);
    assert!(out.live, "the file genuinely exists in this repo");
    assert_eq!(out.claims.len(), 2, "{:?}", out.claims);
    let docs: Vec<&str> = out.claims.iter().map(|c| c.doc_id.as_str()).collect();
    assert_eq!(docs, vec!["doc1", "doc2"]);
    let first = &out.claims[0];
    assert_eq!(first.raw_hint, "src/resolver.rs:3");
    assert_eq!(first.doc_title, "Doc doc1");
    assert_eq!(first.doc_path, "features/doc1.html");
    assert_eq!(first.group_label.as_deref(), Some("G1 · Resolver"));
    assert_eq!(first.line_start, Some(3));
    assert!(first.line_state.is_some());
    assert!(first.seen_at > 0);
    assert_eq!(
        first.doc_public_href.as_deref(),
        Some("https://kb.example.com/a/fixture-kb/features/doc1.html"),
        "built by the ONE link-out builder (doclens::doc_href), never a second encoder"
    );

    // `src/store.rs` is cited by doc2 alone.
    let out = fx.claims("src/store.rs");
    assert_eq!(out.claims.len(), 1);
    assert_eq!(out.claims[0].doc_id, "doc2");

    // A file nothing cites answers an empty, LIVE response — not a 404.
    let out = fx.claims("vendor/resolver.rs");
    assert!(out.live);
    assert!(out.claims.is_empty());
}

/// DCB-W3.A.R fix 6, the write-time half against the real pipeline (the pure
/// fallback logic itself is `doc_title_or_fallback_prefers_title_then_basename_then_id`):
/// a kb doc with no `title` must never persist an empty `doc_title`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_less_docs_claims_fall_back_to_the_path_basename() {
    let mut untitled = doc(
        "doc1",
        1_754_500_000,
        vec![code_ref(
            0,
            "path_line",
            "src/resolver.rs:3",
            "src/resolver.rs",
            Some(3),
        )],
    );
    untitled["title"] = json!(null);
    let fx = fixture(vec![untitled], 100, &["fixture-kb"], 200).await;
    fx.pin("fixture-kb", "doc1");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 1, "{stats:?}");
    let claims = fx.claims("src/resolver.rs").claims;
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(
        claims[0].doc_title, "doc1.html",
        "falls back to the doc_path's basename, never an empty title"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_present_unique_refs_become_claims() {
    // doc1 cites three paths: one present, one absent, one AMBIGUOUS (two
    // `/`-anchored suffix candidates). Only the present-unique one may be
    // persisted — an ambiguous ref has no single honest path to store.
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 1);
    assert_eq!(stats.docs_skipped_unpinned, 1, "doc2 is unpinned");

    let rows = fx
        .state
        .store
        .doc_refs_for_path(fx.repo_id, "src/resolver.rs")
        .unwrap();
    assert_eq!(rows.len(), 1, "one claim from doc1, not three: {rows:?}");
    assert_eq!(
        rows[0].ordinal, 0,
        "the ref's OWN ordinal, never reassigned"
    );
    assert_eq!(rows[0].kind, "path_line");
    assert_eq!(rows[0].resolved_path, "src/resolver.rs");
    assert_eq!(rows[0].doc_hash.as_deref(), Some("hash-doc1"));
    assert!(
        rows[0].head_sha.is_some(),
        "the resolving worktree label is recorded (amendment 11)"
    );
    assert!(!rows[0].dirty, "the fixture repo is committed clean");
    // Nothing was filed for the absent/ambiguous hints.
    for never in ["src/nope.rs", "vendor/resolver.rs"] {
        assert!(
            fx.state
                .store
                .doc_refs_for_path(fx.repo_id, never)
                .unwrap()
                .is_empty(),
            "{never} must have no claim"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_whose_file_disappeared_reads_as_cited_but_not_present() {
    // Amendment 11's third case: sync, then the cited file goes away. The
    // claim must SURVIVE and render "cited, path no longer present" — never
    // a live link, and never a silently dropped citation.
    let fx = fx().await;
    fx.pin("fixture-kb", "doc2");
    run_doclens_sync(&fx.state, false).await;
    assert!(fx.claims("src/store.rs").live);

    fx.state
        .store
        .delete_file(fx.repo_id, "src/store.rs")
        .unwrap();

    let out = fx.claims("src/store.rs");
    assert!(!out.live, "re-validated against the LIVE files table, NOW");
    assert_eq!(
        out.claims.len(),
        1,
        "the claim is still visible — it is the LINK that must not be live"
    );
    assert_eq!(out.claims[0].doc_id, "doc2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_doc_that_404s_drops_both_its_pin_and_its_claims() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc2");
    run_doclens_sync(&fx.state, false).await;
    assert_eq!(fx.claims("src/store.rs").claims.len(), 1);
    assert!(fx.pinned("fixture-kb", "doc2"));

    // kb now 404s doc2's body while the feed still lists it (the exact shape
    // amendment 11 names: "claims dropped when the doc 404s on the feed").
    fx.hits.gone.lock().unwrap().push("doc2".to_string());

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_dropped_404, 1, "{stats:?}");
    assert_eq!(stats.docs_resolved, 0);
    assert!(
        fx.claims("src/store.rs").claims.is_empty(),
        "a doc that no longer exists cites nothing"
    );
    assert!(
        !fx.pinned("fixture-kb", "doc2"),
        "resolve_lens owns the pin drop; sync must not leave a pin behind it"
    );
}

/// DCB-W3.A.R fix 2 — spec §5's re-key case, previously ZERO coverage:
/// `write_claims`'s `lens.moved_from` branch deletes the OLD id's claims
/// across every repo when kb's moves-301 chain re-keys a doc mid-pass. The
/// mock serves a body whose OWN `doc_id` field ("doc1-moved-to") differs
/// from the path segment requested ("doc1") — exactly kb's moves shape
/// (D13/R13: the new id comes from the BODY, never a parsed redirect URL).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moved_from_drops_the_old_ids_claims_and_writes_the_new_ids() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");

    // First pass, no override: doc1 gets claims filed under its OWN id,
    // citing `src/resolver.rs` (ordinal 0 of `two_doc_corpus`'s doc1).
    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 1, "{stats:?}");
    assert_eq!(
        fx.claims("src/resolver.rs")
            .claims
            .iter()
            .map(|c| c.doc_id.as_str())
            .collect::<Vec<_>>(),
        vec!["doc1"]
    );

    // Now kb's moves chain fires: a request for "doc1" answers with a body
    // whose OWN doc_id is "doc1-moved-to", citing a DIFFERENT path
    // (`src/store.rs`) so the old vs. new claim sets are trivially
    // distinguishable.
    let moved_body = doc(
        "doc1-moved-to",
        1_754_500_000,
        vec![code_ref(
            0,
            "path_line",
            "src/store.rs:2",
            "src/store.rs",
            Some(2),
        )],
    );
    fx.hits
        .body_override
        .lock()
        .unwrap()
        .insert("doc1".to_string(), moved_body);

    // `force` so the feed is re-walked from the top — the feed's own header
    // for this doc is UNCHANGED (still declares doc_id "doc1"; only the
    // per-doc BODY is overridden), which is exactly how a real moves-301
    // reaches sync: the pin is still keyed on the OLD id when the pass looks
    // it up, and `fetch_doc` is what discovers the re-key mid-resolve.
    let stats = run_doclens_sync(&fx.state, true).await;
    assert_eq!(stats.docs_resolved, 1, "{stats:?}");

    assert!(
        fx.claims("src/resolver.rs").claims.is_empty(),
        "the OLD id's claims must be dropped, not left as an orphan under a \
         doc no longer pinned"
    );
    let new_claims = fx.claims("src/store.rs").claims;
    assert_eq!(new_claims.len(), 1, "{new_claims:?}");
    assert_eq!(
        new_claims[0].doc_id, "doc1-moved-to",
        "the NEW id's rows are written"
    );

    // The pin itself was re-keyed too (resolve_lens's own responsibility,
    // already covered elsewhere) — asserted here as well since it is the
    // reason the NEXT pass would still find this doc under its new id.
    assert!(!fx.pinned("fixture-kb", "doc1"));
    assert!(fx.pinned("fixture-kb", "doc1-moved-to"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpinned_docs_are_skipped_and_counted_but_never_fetched() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 1);
    assert_eq!(stats.docs_skipped_unpinned, 1);
    assert_eq!(
        fx.hits.body.load(Ordering::SeqCst),
        1,
        "an unpinned doc's BODY is never pulled — the header walk is the whole cost"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_fetches_each_pinned_doc_body_exactly_once() {
    // The regression guard for the double-fetch this design removed: sync
    // walks the feed with `?refs=0` and reaches a body ONLY through
    // `resolve_lens`, never via its own `KbClient::code_refs` call.
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 2);
    assert_eq!(
        fx.hits.body.load(Ordering::SeqCst),
        2,
        "exactly one body fetch per PINNED doc"
    );
    assert_eq!(
        fx.hits.feed.load(Ordering::SeqCst),
        1,
        "one header-mode feed page covered the whole corpus"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kb_dropped_from_the_allowlist_is_skipped_wholesale() {
    // R8/M11 — the pass must not even TALK to a kb the operator removed from
    // `[doclens] kbs`: the allowlist gates the background pull of prose, not
    // merely the routes.
    let fx = fixture(two_doc_corpus(), 100, &["some-other-kb"], 200).await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_skipped_not_allowlisted, 2, "counted per PIN");
    assert_eq!(stats.kbs_synced, 0);
    assert_eq!(stats.docs_resolved, 0);
    assert_eq!(
        fx.hits.feed.load(Ordering::SeqCst),
        0,
        "the feed was never called for a non-allowlisted kb"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_cap_stops_the_pass_and_reports_the_skip_loudly() {
    // Three pinned docs, a cap of two. The cap must be a HARD bound on one
    // pass AND leave the third doc reachable by the next one — a cap that
    // starved a doc forever would be a silent corpus-wide blind spot, not a
    // throttle.
    let fx = fixture(three_doc_corpus(), 100, &["fixture-kb"], 2).await;
    for id in ["doc1", "doc2", "doc3"] {
        fx.pin("fixture-kb", id);
    }

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 2, "the cap is a HARD bound: {stats:?}");
    assert_eq!(
        stats.docs_skipped_cap, 1,
        "the skip is COUNTED, never silent truncation"
    );
    assert!(
        fx.claims("vendor/resolver.rs").claims.is_empty(),
        "doc3 was beyond this pass's budget"
    );
    // The capped pass persisted the cursor of the last page it FINISHED, so
    // the next pass picks up from there (paying at most §1.6's one-second
    // rewind) instead of re-walking from the top forever. Deterministic ==,
    // not a loose lower bound (DCB-W3.A.R): §1.6's rewind re-presents doc2
    // (already resolved once) alongside doc3, so this pass resolves BOTH,
    // exactly 2, never fewer and never more.
    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.docs_resolved, 2, "{stats:?}");
    assert_eq!(
        stats.docs_skipped_cap, 0,
        "the whole remainder fit in one pass: {stats:?}"
    );
    assert_eq!(
        fx.claims("vendor/resolver.rs").claims.len(),
        1,
        "doc3 landed on the next pass — capped, never starved"
    );
    assert_eq!(fx.claims("vendor/resolver.rs").claims[0].doc_id, "doc3");
}

/// DCB-W3.A.R fix 4 — a LATER kb in one pass must request only what's LEFT
/// of `batch_cap`, never a fresh full page. Two kbs share the same mock
/// corpus (the mock ignores the `{kb}` path segment — see `spawn_mock`),
/// each with its own two pins; `batch_cap = 3` lets kb-a (alphabetically
/// first, so walked first — `doc_lens_pin_kbs` orders by `kb`) spend 2 of
/// the budget WITHOUT tripping the cap, leaving exactly 1 slot for kb-b.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_kb_in_one_pass_requests_only_the_remaining_budget_not_a_full_page() {
    let fx = fixture(two_doc_corpus(), 100, &["kb-a", "kb-b"], 3).await;
    for kb in ["kb-a", "kb-b"] {
        fx.pin(kb, "doc1");
        fx.pin(kb, "doc2");
    }

    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.kbs_synced, 2, "{stats:?}");
    assert_eq!(
        stats.docs_resolved, 3,
        "kb-a's 2 + kb-b's 1 remaining: {stats:?}"
    );
    assert_eq!(
        stats.docs_skipped_cap, 1,
        "kb-b's doc2 is beyond the shared budget: {stats:?}"
    );

    // kb-a's own first request is UNCHANGED by this fix: nothing has been
    // attempted yet when its walk starts, so it still asks for
    // min(PAGE_LIMIT, cap) = 3. kb-b's first request is the one this fix
    // changes: only 1 slot is left (cap 3 − kb-a's 2), so it must ask for
    // 1 — never a fresh 3 (the configured cap) or 100 (PAGE_LIMIT), either
    // of which would trip the cap mid-page and waste the round trip.
    let limits = fx.hits.limits();
    assert_eq!(
        limits[0], 3,
        "kb-a's first request: the full remaining budget, since it's first: {limits:?}"
    );
    assert_eq!(
        limits[1], 1,
        "kb-b's first request must ask for only what kb-a left, not a full page: {limits:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resyncing_replaces_rather_than_duplicating() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    run_doclens_sync(&fx.state, true).await;
    let first = fx.claims("src/resolver.rs");
    run_doclens_sync(&fx.state, true).await;
    let second = fx.claims("src/resolver.rs");

    assert_eq!(first.claims.len(), 2);
    assert_eq!(
        second.claims.len(),
        2,
        "DELETE+INSERT per (kb, doc_id) — a repeat pass overwrites, never duplicates"
    );
    let ids =
        |o: &DocRefsOut| -> Vec<String> { o.claims.iter().map(|c| c.doc_id.clone()).collect() };
    assert_eq!(ids(&first), ids(&second));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_next_pass_resumes_one_second_earlier_than_the_stored_cursor() {
    // `page_size = 1` so the feed actually hands back a `next_cursor` to
    // persist (kb omits it on the last page).
    let fx = fixture(two_doc_corpus(), 1, &["fixture-kb"], 200).await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    run_doclens_sync(&fx.state, false).await;
    let stored = fx
        .state
        .store
        .get_doclens_sync_cursor("fixture-kb")
        .unwrap()
        .expect("a cursor row")
        .cursor;
    assert_eq!(
        stored.as_deref(),
        Some("1754500000:doc1"),
        "the feed's own `next_cursor`, persisted VERBATIM"
    );
    assert_eq!(
        fx.hits.cursors(),
        vec![None, Some("1754500000:doc1".to_string())],
        "paging INSIDE a pass round-trips the cursor verbatim — no rewind per page"
    );

    run_doclens_sync(&fx.state, false).await;
    assert_eq!(
        fx.hits.cursors()[2..],
        [
            Some("1754499999:doc1".to_string()),
            Some("1754500000:doc1".to_string())
        ],
        "a NEW pass resumes one second earlier (the same-second keyset gap), then pages verbatim"
    );
    // …and the rewind is never PERSISTED: it would compound one second per
    // pass and never converge.
    assert_eq!(
        fx.state
            .store
            .get_doclens_sync_cursor("fixture-kb")
            .unwrap()
            .unwrap()
            .cursor
            .as_deref(),
        Some("1754500000:doc1")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_restarts_the_walk_from_the_beginning() {
    let fx = fixture(two_doc_corpus(), 1, &["fixture-kb"], 200).await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");

    run_doclens_sync(&fx.state, false).await;
    assert!(fx
        .state
        .store
        .get_doclens_sync_cursor("fixture-kb")
        .unwrap()
        .unwrap()
        .cursor
        .is_some());

    let before = fx.hits.cursors().len();
    run_doclens_sync(&fx.state, true).await;
    let after = fx.hits.cursors()[before..].to_vec();
    assert_eq!(
        after.first(),
        Some(&None),
        "--force reset the cursor to NULL before walking: {after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_kb_records_one_error_and_never_drops_claims() {
    let fx = fx().await;
    fx.pin("fixture-kb", "doc1");
    fx.pin("fixture-kb", "doc2");
    run_doclens_sync(&fx.state, false).await;
    assert_eq!(fx.claims("src/resolver.rs").claims.len(), 2);

    fx.hits.down.store(true, Ordering::SeqCst);
    let stats = run_doclens_sync(&fx.state, false).await;
    assert_eq!(stats.errors.len(), 1, "ONE line per kb: {:?}", stats.errors);
    assert!(stats.errors[0].starts_with("fixture-kb: "));
    assert_eq!(stats.docs_resolved, 0);
    assert_eq!(
        fx.claims("src/resolver.rs").claims.len(),
        2,
        "an unreachable kb is not evidence that a doc stopped citing code"
    );
    assert!(
        fx.state
            .store
            .get_doclens_sync_cursor("fixture-kb")
            .unwrap()
            .unwrap()
            .last_error
            .is_some(),
        "surfaced on the cursor row too, not only in the pass stats"
    );
}

// --- the read route's own guards ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doc_refs_refuses_a_traversal_path_and_an_unknown_repo() {
    // DCB-W2.B.R fix 1's lesson, applied to every path param on this lane —
    // `safe_rel_path` alone accepts `a/./b` (Rust's `Components` normalises
    // `.` away), which is exactly why `path_has_dot_segment` runs too.
    let fx = fx().await;
    for bad in ["../etc/passwd", "a/../b", "a/./b", "/etc/passwd", "   "] {
        let err = match doc_refs_for(&fx.state, "alpha", bad) {
            Ok(ok) => panic!("{bad:?} must be refused, got {ok:?}"),
            Err(e) => e,
        };
        assert!(
            err.message().contains("invalid") || err.message().contains("must not be empty"),
            "{bad:?}: {}",
            err.message()
        );
    }
    assert!(doc_refs_for(&fx.state, "nope", "src/resolver.rs").is_err());
}

// --- the worker ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sync_worker_idles_out_when_the_interval_is_zero() {
    let fx = fx().await;
    // `sync_interval_secs = 0` (the default) — the worker must RETURN, not
    // spin, and `POST /api/doc-lens/sync` stays the only trigger.
    let handle = spawn_doclens_sync_worker(fx.state.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the worker returns promptly when periodic sync is off")
        .expect("no panic");
}
