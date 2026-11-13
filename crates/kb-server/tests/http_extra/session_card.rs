//! A3 card HTTP tests: tests 2, 3, 4, 5, 6, 7, 9, 10 (HTTP half) and 11's
//! card/live-status halves, over the in-process daemon (the `slates.rs`
//! boot shape) with SYNTHETIC session rows seeded straight into the kb's
//! `index.db` via `kb_core::storage::sqlite::Db` — the exact write surface
//! the enrichment hook uses (`sessions_upsert` + the `*_replace` fns), so
//! `is_newest` maintenance runs just as in production.
//!
//! What these are FOR: the wire contract of `GET
//! /api/sessions/{sid}/card`, `GET /api/sessions/{sid}/children` and the
//! `tree=` list param — newest-capture scoping, byte-identical repeat
//! reads, per-lane drop-on-error, no transcript body, 120-child paging,
//! single-slate reads, tree default/cursor/child_count over HTTP, and the
//! mid-tree child_count consistency between list and card (the one
//! card-predicate definition). The pure half of each behaviour is pinned
//! by `kb_core`'s own unit tests; nothing here re-asserts storage
//! arithmetic.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::storage::sqlite::{
    Db, SessionDecisionRow, SessionFileRow, SessionResearchRow, SessionRow,
};
use kb_core::types::KbName;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const FIXTURE_TOKEN: &str = "test-token-session-card-fixture";

async fn boot() -> (tempfile::TempDir, std::net::SocketAddr, KbPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();

    let daemon_name = format!(
        "test-session-card-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source.clone(),
            reconcile_secs: None,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            project_slugs: Vec::new(),
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            git_dir: None,
            git_prefix: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            capture_dir: None,
            resurface: None,
            slo: None,
            id_patterns: Vec::new(),
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
        terminal: Default::default(),
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths.clone())
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr, paths)
}

pub(crate) fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

pub(crate) async fn get_json(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    path: &str,
) -> (u16, Value, String) {
    let resp = client.get(url(addr, path)).send().await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (status, v, body)
}

fn row(artifact_id: &str, sid: &str, started_at: i64) -> SessionRow {
    SessionRow {
        artifact_id: artifact_id.into(),
        session_id: sid.into(),
        started_at,
        ended_at: started_at + 600,
        message_count: 42,
        first_user_prompt: Some("synthetic prompt".into()),
        source_relative: format!("sessions/session-{started_at}-{sid}.html"),
        title: Some("synthetic title".into()),
        cwd: Some("/work/alpha".into()),
        git_branch: Some("main".into()),
        files_read_count: 3,
        files_edited_count: 2,
        token_total: 12_345,
        tool_calls: 9,
        model: Some("synthetic-model".into()),
        error_count: 1,
        subagent_count: 1,
        subagent_tokens: 5_000,
        subagent_tool_calls: 4,
        subagent_files_edited: 1,
        subagent_launched_unstatted: 0,
        project_key: Some("-work-alpha".into()),
        repo_root: Some("/work/alpha".into()),
        harness: "claude".into(),
        cc_version: None,
        last_assistant_text: Some("synthetic closing prose".into()),
        all_cwds: None,
        commit_count: 0,
        user_turns: 2,
        active_secs: 321,
        substance: None,
        segment_of: None,
        segment_idx: None,
        evidence_json: None,
        parent_session_id: None,
        root_session_id: None,
        project_slug: None,
    }
}

fn file_row(artifact_id: &str, sid: &str, path: &str, action: &str) -> SessionFileRow {
    SessionFileRow {
        artifact_id_session: artifact_id.into(),
        session_id: sid.into(),
        path: path.into(),
        basename: path.rsplit('/').next().unwrap_or(path).to_string(),
        action: action.into(),
        in_corpus: false,
        target_kb: None,
        target_artifact_id: None,
        via_subagent: false,
    }
}

fn open_db(paths: &KbPaths) -> Db {
    Db::open(&paths.kb_sqlite(&KbName::new("smoke").unwrap())).unwrap()
}

fn seed_file(db: &mut Db, artifact_id: &str, sid: &str, n: usize) {
    let files: Vec<SessionFileRow> = (0..n)
        .map(|i| file_row(artifact_id, sid, &format!("/work/f{i}.rs"), "edit"))
        .collect();
    db.session_files_replace(artifact_id, &files).unwrap();
}

/// Test 2 — the card is scoped to the NEWEST capture: an older capture's 1
/// file must not leak in beside the newer capture's 3.
#[tokio::test]
async fn card_is_scoped_to_the_newest_capture() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        db.sessions_upsert(&row("art-old", "cap-1", 1_000)).unwrap();
        db.sessions_upsert(&row("art-new", "cap-1", 2_000)).unwrap();
        seed_file(&mut db, "art-old", "cap-1", 1);
        seed_file(&mut db, "art-new", "cap-1", 3);
    }
    let (status, card, _) = get_json(&client, addr, "/api/sessions/cap-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["schema"], "kb-session-card/1");
    assert_eq!(card["artifact_id"], "art-new");
    assert_eq!(card["files"]["total"], 3, "only the newest capture's files");
    assert_eq!(card["files"]["files"].as_array().unwrap().len(), 3);
}

/// Test 3 — repeat reads of a non-live session are byte-identical.
#[tokio::test]
async fn card_repeat_reads_are_byte_identical_for_a_non_live_session() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        let mut r = row("art-1", "rep-1", 1_000);
        r.project_slug = Some("alpha".into());
        db.sessions_upsert(&r).unwrap();
        seed_file(&mut db, "art-1", "rep-1", 2);
    }
    let (s1, _, b1) = get_json(&client, addr, "/api/sessions/rep-1/card").await;
    let (s2, _, b2) = get_json(&client, addr, "/api/sessions/rep-1/card").await;
    assert_eq!((s1, s2), (200, 200));
    assert_eq!(b1, b2, "two card reads must be byte-identical");
    // The terminal lane is registry-derived in both reads (disabled here).
    assert!(b1.contains(
        r#""terminal":{"available":false,"reason":"terminal disabled","admitted":false}"#
    ));
}

/// Test 4 — a failing lane degrades that lane only: an invalid
/// `project_slug` yields `slate.available == false` with a reason, 200,
/// and every sibling lane intact (including `outcome`, whose verdict
/// comes from the loaded row, not the slate reader).
#[tokio::test]
async fn a_failing_lane_degrades_that_lane_only() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        let mut r = row("art-1", "bad-1", 1_000);
        r.project_slug = Some("Bad Slug!".into());
        db.sessions_upsert(&r).unwrap();
        seed_file(&mut db, "art-1", "bad-1", 1);
    }
    let (status, card, _) = get_json(&client, addr, "/api/sessions/bad-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["slate"]["available"], false);
    assert_eq!(card["slate"]["reason"], "no project slug");
    assert_eq!(card["summary"]["available"], true);
    assert_eq!(card["outcome"]["available"], true);
    assert_eq!(card["files"]["available"], true);
    assert_eq!(card["files"]["total"], 1);
}

/// Test 5 — the card carries no transcript body: the sentinel seeded as the
/// capped outcome preview rides the wire exactly twice — structured
/// (`outcome.last_assistant_text`) and rendered (`text`) — and never as a
/// raw turn; no transcript-bearing keys ride along.
#[tokio::test]
async fn card_carries_no_transcript_body() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    const SENTINEL: &str = "TRANSCRIPT-BODY-SENTINEL-7f3a";
    {
        let mut db = open_db(&paths);
        let mut r = row("art-1", "sent-1", 1_000);
        r.last_assistant_text = Some(SENTINEL.to_string());
        db.sessions_upsert(&r).unwrap();
    }
    let (status, card, body) = get_json(&client, addr, "/api/sessions/sent-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["outcome"]["last_assistant_text"], SENTINEL);
    assert!(
        card["text"].as_str().unwrap_or_default().contains(SENTINEL),
        "rendered text carries the capped preview"
    );
    assert_eq!(
        body.matches(SENTINEL).count(),
        2,
        "the preview appears only as outcome.last_assistant_text + rendered text — no raw turn body rides along"
    );
    // No transcript-bearing keys anywhere on the card.
    for forbidden in ["turns", "transcript", "body_text", "jsonl"] {
        assert!(
            !body.contains(&format!("\"{forbidden}\"")),
            "card must not carry a `{forbidden}` lane: {body}"
        );
    }
}

/// Test 6 — a parent's `files` keeps its own rows; the children's file
/// counts are never summed into it.
#[tokio::test]
async fn parent_files_are_not_summed_with_children() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        db.sessions_upsert(&row("art-root", "tree-R", 1_000))
            .unwrap();
        let mut child = row("art-c1", "tree-C1", 1_100);
        child.root_session_id = Some("tree-R".into());
        child.parent_session_id = Some("tree-R".into());
        child.files_edited_count = 40;
        db.sessions_upsert(&child).unwrap();
        seed_file(&mut db, "art-root", "tree-R", 1);
        seed_file(&mut db, "art-c1", "tree-C1", 2);
    }
    let (status, card, _) = get_json(&client, addr, "/api/sessions/tree-R/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(
        card["files"]["total"], 1,
        "the parent keeps its own row only"
    );
    assert_eq!(card["children"]["total"], 1);
    let item = &card["children"]["items"][0];
    assert_eq!(item["session_id"], "tree-C1");
    assert_eq!(
        item["files_edited_count"], 40,
        "children show their own counts, never folded into the parent"
    );
}

/// Test 7 — 120 children + 2 grandchildren (the root's flood is 122
/// rows) page 50/50/22 with no gaps/dups, `next_after` null on the
/// last page, a colon-bearing id round-trips through the cursor, a
/// malformed cursor 400s, and a non-root uses the parent predicate.
#[tokio::test]
async fn children_are_paged_by_started_at_then_session_id_without_gaps_or_dups() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        db.sessions_upsert(&row("art-root", "page-root", 500))
            .unwrap();
        for i in 0..120 {
            // One id carries a colon so the cursor grammar is exercised
            // exactly as the brief pins it.
            let sid = if i == 57 {
                "page-child:with:colons".to_string()
            } else {
                format!("page-child-{i:03}")
            };
            let mut c = row(&format!("art-c{i}"), &sid, 1_000 + i as i64);
            c.root_session_id = Some("page-root".into());
            c.parent_session_id = Some("page-root".into());
            db.sessions_upsert(&c).unwrap();
        }
        // Two grandchildren under the first child (the non-root case).
        for j in 0..2 {
            let mut g = row(&format!("art-g{j}"), &format!("page-gc-{j}"), 5_000 + j);
            g.root_session_id = Some("page-root".into());
            g.parent_session_id = Some("page-child-000".into());
            db.sessions_upsert(&g).unwrap();
        }
    }
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    for page in 0..3 {
        let path = match &after {
            Some(a) => format!("/api/sessions/page-root/children?limit=50&after={a}"),
            None => "/api/sessions/page-root/children?limit=50".to_string(),
        };
        let (status, v, _) = get_json(&client, addr, &path).await;
        assert_eq!(status, 200, "page {page}: {v}");
        assert_eq!(v["total"], 122, "the root's flood includes grandchildren");
        let items = v["items"].as_array().unwrap();
        assert_eq!(
            items.len(),
            if page < 2 { 50 } else { 22 },
            "page {page} size"
        );
        for it in items {
            seen.push(it["session_id"].as_str().unwrap().to_string());
        }
        after = v["next_after"].as_str().map(str::to_string);
        if page < 2 {
            assert!(after.is_some(), "page {page} must continue");
        }
    }
    assert_eq!(after, None, "next_after is null on the last page");
    let mut dedup = seen.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(seen.len(), 122, "no gaps, no dups across 50/50/22");
    assert_eq!(dedup.len(), 122);
    assert!(
        seen.contains(&"page-child:with:colons".to_string()),
        "{seen:?}"
    );

    // Malformed cursor → 400.
    let (status, v, _) =
        get_json(&client, addr, "/api/sessions/page-root/children?after=nope").await;
    assert_eq!(status, 400, "{v}");

    // The card's children total agrees with /children's total.
    let (status, card, _) = get_json(&client, addr, "/api/sessions/page-root/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["children"]["total"], 122);
    assert_eq!(card["summary"]["child_total"], 122);

    // A non-root page uses the parent predicate (direct children only).
    let (status, v, _) = get_json(&client, addr, "/api/sessions/page-child-000/children").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["total"], 2);
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["page-gc-0", "page-gc-1"]);
}

/// Test 9 — the slate lane reads exactly one slate (this session's
/// project slug) and only this session's posts (`prov.session_id` and
/// `session:<sid>` refs); a NULL slug yields the reason while another
/// slate exists.
#[tokio::test]
async fn slate_lane_reads_exactly_one_slate_and_only_this_sessions_posts() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        let mut r = row("art-1", "slate-1", 1_000);
        r.project_slug = Some("alpha".into());
        db.sessions_upsert(&r).unwrap();
        let mut n = row("art-2", "noslug-1", 1_000);
        n.project_slug = None;
        db.sessions_upsert(&n).unwrap();
    }
    // One slate for this session, one for another; posts from a third
    // session and an unrelated ref must not leak in.
    let post = |slug: &str, line: &str, sid: &str, refs: Value| {
        let client = client.clone();
        let slug = slug.to_string();
        let line = line.to_string();
        let sid = sid.to_string();
        async move {
            let mut b = json!({
                "kind": "found",
                "line": line,
                "prov": {"harness": "claude", "session_id": sid, "cwd": "/work"},
            });
            b["refs"] = refs;
            let resp = client
                .post(url(addr, &format!("/api/slates/{slug}/posts")))
                .json(&b)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 201, "post {line} into {slug}");
        }
    };
    post(
        "alpha",
        "mine via prov",
        "slate-1",
        json!(["path:synthetic/alpha.txt"]),
    )
    .await;
    post(
        "alpha",
        "mine via ref",
        "other-sess",
        json!(["session:slate-1"]),
    )
    .await;
    post(
        "alpha",
        "someone else's",
        "other-sess",
        json!(["path:synthetic/beta.txt"]),
    )
    .await;
    post(
        "beta",
        "wrong slate entirely",
        "slate-1",
        json!(["session:slate-1"]),
    )
    .await;

    let (status, card, _) = get_json(&client, addr, "/api/sessions/slate-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["slate"]["available"], true);
    assert_eq!(card["slate"]["slug"], "alpha");
    assert_eq!(card["slate"]["total"], 2);
    let lines: Vec<&str> = card["slate"]["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["line"].as_str().unwrap())
        .collect();
    assert_eq!(
        lines,
        vec!["mine via ref", "mine via prov"],
        "20 newest, seq desc"
    );

    let (status, card, _) = get_json(&client, addr, "/api/sessions/noslug-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["slate"]["available"], false);
    assert_eq!(card["slate"]["reason"], "no project slug");
}

/// Test 10 (HTTP half) — `tree=` grammar is closed, collapsed is the
/// default, `child_count` rides the list, and the keyset cursor stays
/// coherent across a page boundary with hidden children interleaved.
#[tokio::test]
async fn tree_param_over_http_hides_children_and_cursor_stays_coherent() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        // Roots and children interleaved in started_at so a collapsed
        // walk must skip children mid-page without dropping a root.
        db.sessions_upsert(&row("art-r1", "tree-r1", 1_000))
            .unwrap();
        let mut c1 = row("art-c1", "tree-c1", 1_100);
        c1.root_session_id = Some("tree-r1".into());
        c1.parent_session_id = Some("tree-r1".into());
        db.sessions_upsert(&c1).unwrap();
        db.sessions_upsert(&row("art-r2", "tree-r2", 1_200))
            .unwrap();
        let mut c2 = row("art-c2", "tree-c2", 1_300);
        c2.root_session_id = Some("tree-r1".into());
        c2.parent_session_id = Some("tree-r1".into());
        db.sessions_upsert(&c2).unwrap();
        db.sessions_upsert(&row("art-r3", "tree-r3", 1_400))
            .unwrap();
        // A mid-tree node with children of its own: list and card must
        // agree on its child count (the ONE card-predicate definition).
        let mut mid = row("art-mid", "tree-mid", 1_050);
        mid.root_session_id = Some("tree-r1".into());
        mid.parent_session_id = Some("tree-r1".into());
        db.sessions_upsert(&mid).unwrap();
        for j in 0..2 {
            let mut g = row(&format!("art-g{j}"), &format!("tree-g{j}"), 1_060 + j);
            g.root_session_id = Some("tree-r1".into());
            g.parent_session_id = Some("tree-mid".into());
            db.sessions_upsert(&g).unwrap();
        }
    }
    // Closed grammar: anything but collapsed|flat 400s.
    let (status, v, _) = get_json(&client, addr, "/api/sessions?tree=weird").await;
    assert_eq!(status, 400, "{v}");

    // Default = collapsed: 4 roots (mid is rooted, so hidden too).
    let (status, v, _) = get_json(&client, addr, "/api/sessions?limit=100").await;
    assert_eq!(status, 200, "{v}");
    let ids: Vec<&str> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["tree-r3", "tree-r2", "tree-r1"], "{ids:?}");

    // Flat shows every newest row.
    let (status, v, _) = get_json(&client, addr, "/api/sessions?limit=100&tree=flat").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["sessions"].as_array().unwrap().len(), 8);

    // child_count on the root = its whole flood; on the mid node = its
    // direct children — the same numbers its card reports.
    let (status, v, _) = get_json(&client, addr, "/api/sessions?limit=100&tree=flat").await;
    assert_eq!(status, 200, "{v}");
    let counts: BTreeMap<&str, u64> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["session_id"].as_str().unwrap(),
                s["child_count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(counts["tree-r1"], 5, "c1, c2, mid, g0, g1");
    assert_eq!(counts["tree-mid"], 2, "g0, g1");
    assert_eq!(counts["tree-r2"], 0);

    let (status, card, _) = get_json(&client, addr, "/api/sessions/tree-mid/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["summary"]["child_total"], 2);
    assert_eq!(card["children"]["total"], 2);

    // Keyset coherence: limit=1 walk over the collapsed default yields
    // each root exactly once, in order, no gap despite interleaved
    // hidden children.
    let mut walked: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let path = match &cursor {
            Some(c) => format!("/api/sessions?limit=1&cursor={c}"),
            None => "/api/sessions?limit=1".to_string(),
        };
        let (status, v, _) = get_json(&client, addr, &path).await;
        assert_eq!(status, 200, "{v}");
        let rows = v["sessions"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        walked.push(rows[0]["session_id"].as_str().unwrap().to_string());
        cursor = v["next_cursor"].as_i64().map(|n| n.to_string());
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        walked,
        vec![
            "tree-r3".to_string(),
            "tree-r2".to_string(),
            "tree-r1".to_string()
        ],
        "coherent keyset walk over the collapsed set"
    );
}

/// Test 11 (card + wire halves) — an omp child's card resumes the ROOT,
/// a live registry row for the child resolves to the root over
/// live-status, and the card's `summary.resume` equals the detail
/// route's `resume`.
#[tokio::test]
async fn resume_targets_the_root_for_omp_children_over_http() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        let mut root = row("art-root", "tree-root", 1_000);
        root.harness = "omp".into();
        db.sessions_upsert(&root).unwrap();
        let mut child = row("art-child", "child-1", 1_100);
        child.harness = "omp".into();
        child.root_session_id = Some("tree-root".into());
        child.parent_session_id = Some("tree-root".into());
        db.sessions_upsert(&child).unwrap();
    }
    let (status, detail, _) = get_json(&client, addr, "/api/sessions/child-1").await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["resume"], "omp -r tree-root");

    let (status, card, _) = get_json(&client, addr, "/api/sessions/child-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["summary"]["resume"], "omp -r tree-root");
    assert_eq!(card["summary"]["resume"], detail["resume"]);

    // The child's card is live-lane silent until it beats.
    assert_eq!(card["live"]["available"], false);
    let resp = client
        .post(url(addr, "/api/sessions/beat"))
        .json(&json!({
            "session_id": "child-1",
            "harness": "omp",
            "event": "start",
            "at": chrono::Utc::now().to_rfc3339(),
            "cwd": "/work/alpha",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "beat for child-1");

    let (status, live, _) = get_json(&client, addr, "/api/sessions/live-status").await;
    assert_eq!(status, 200, "{live}");
    let rows = live["sessions"].as_array().unwrap();
    let reg = rows
        .iter()
        .find(|r| r["session_id"] == "child-1")
        .expect("registry row for child-1 in live-status");
    assert_eq!(
        reg["resume"], "omp -r tree-root",
        "a registry row resolves its resume to the root via sessions_root_of"
    );

    // The card's live lane now names the same session with the reduced
    // field set (no last_tool/background/last_line/cwd).
    let (status, card, _) = get_json(&client, addr, "/api/sessions/child-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["live"]["available"], true);
    for forbidden in ["last_tool", "background", "last_line", "cwd"] {
        assert!(
            card["live"].get(forbidden).is_none(),
            "live lane carries no `{forbidden}` (scrub grammar stays in live-status)"
        );
    }
}

/// Research/decisions lanes ride the card (happy path): counts and first
/// items. Pinned indirectly by test 3's byte-identical reads, but the
/// lane shapes themselves are asserted here so a reader regression fails
/// loudly instead of silently degrading.
#[tokio::test]
async fn research_and_decisions_lanes_ride_the_card() {
    let (_tmp, addr, paths) = boot().await;
    let client = reqwest::Client::new();
    {
        let mut db = open_db(&paths);
        db.sessions_upsert(&row("art-1", "lane-1", 1_000)).unwrap();
        db.session_research_replace(
            "art-1",
            &[
                SessionResearchRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "lane-1".into(),
                    seq: 1,
                    kind: "web".into(),
                    query: "synthetic query".into(),
                },
                SessionResearchRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "lane-1".into(),
                    seq: 2,
                    kind: "kb_search".into(),
                    query: "another query".into(),
                },
            ],
        )
        .unwrap();
        db.session_decisions_replace(
            "art-1",
            &[SessionDecisionRow {
                artifact_id_session: "art-1".into(),
                session_id: "lane-1".into(),
                seq: 1,
                kind: "plan".into(),
                prompt: "synthetic prompt".into(),
                answer: Some("synthetic answer".into()),
            }],
        )
        .unwrap();
    }
    let (status, card, _) = get_json(&client, addr, "/api/sessions/lane-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["research"]["total"], 2);
    assert_eq!(card["research"]["kind_counts"]["web"], 1);
    assert_eq!(card["research"]["items"][0]["query"], "synthetic query");
    assert_eq!(card["decisions"]["total"], 1);
    assert_eq!(card["decisions"]["decisions"][0]["kind"], "plan");
    // Unknown session → 404 on both routes.
    let (status, _, _) = get_json(&client, addr, "/api/sessions/nope/card").await;
    assert_eq!(status, 404);
    let (status, _, _) = get_json(&client, addr, "/api/sessions/nope/children").await;
    assert_eq!(status, 404);
}

// ===== T13: the registry-derived terminal lane ===============================

/// The fake runner: a `UnixListener` in the test tempdir answering
/// scripted `list`/`enter` replies, with a connection counter.
struct FakeRunner {
    conns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    socket: std::path::PathBuf,
}

fn spawn_fake_runner(
    dir: &std::path::Path,
    replies: std::collections::HashMap<String, String>,
) -> FakeRunner {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = conns.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let replies = replies.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(stream);
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let op = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v.get("op").and_then(|v| v.as_str()).map(String::from))
                    .unwrap_or_default();
                let reply = replies.get(&op).cloned().unwrap_or_else(|| {
                    "{\"status\":\"err\",\"code\":\"not_found\",\"message\":\"no script\"}"
                        .to_string()
                });
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    FakeRunner {
        conns,
        socket: sock,
    }
}

fn runner_list_reply(ids: &[&str]) -> String {
    let items: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "harness": "omp",
                "cwd": "/work/proj",
                "started_unix": 1234567890,
                "resume_sid": null,
                "pane_pid": 4242,
                "cols": 120,
                "rows": 40,
                "bracket_paste_flag": true,
                "pane_dead": false,
                "exit_code": null,
                "grid_owner": null,
            })
        })
        .collect();
    json!({"status":"ok","data":items}).to_string()
}

fn origin_header(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

async fn boot_terminal(
    runner_socket: std::path::PathBuf,
) -> (tempfile::TempDir, std::net::SocketAddr, KbPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let daemon_name = format!(
        "test-session-card-term-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
            reconcile_secs: None,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
            memory_scope: None,
            project_slugs: Vec::new(),
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            git_dir: None,
            git_prefix: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            capture_dir: None,
            resurface: None,
            slo: None,
            id_patterns: Vec::new(),
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
        terminal: TerminalSection {
            enabled: Some(true),
            runner_socket: Some(runner_socket),
            ticket_ttl_secs: None,
            refuse_token_callers: None,
            runner_timeout_secs: None,
        },
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths.clone())
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr, paths)
}

/// A child whose root is bound to a kb-term terminal resolves the card's
/// `terminal` lane to that root's terminal with `via: "root"`; the root
/// itself carries no `via`.
#[tokio::test]
async fn card_terminal_lane_for_a_child_resolves_to_the_root_terminal() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), runner_list_reply(&["abc123"]));
    let fake_dir = tempfile::tempdir().unwrap();
    let fake = spawn_fake_runner(fake_dir.path(), replies);
    let (_tmp, addr, paths) = boot_terminal(fake.socket.clone()).await;
    let client = reqwest::Client::new();
    // Deterministic directory refresh: the list reply populates it.
    let resp = client
        .get(url(addr, "/api/terms"))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "terms list: {:?}",
        resp.text().await
    );
    {
        let mut db = open_db(&paths);
        let mut root = row("art-root", "tree-root", 1_000);
        root.harness = "omp".into();
        db.sessions_upsert(&root).unwrap();
        let mut child = row("art-child", "child-1", 1_100);
        child.harness = "omp".into();
        child.root_session_id = Some("tree-root".into());
        child.parent_session_id = Some("tree-root".into());
        db.sessions_upsert(&child).unwrap();
    }
    let resp = client
        .post(url(addr, "/api/sessions/beat"))
        .json(&json!({
            "v": 1,
            "session_id": "tree-root",
            "harness": "omp",
            "event": "start",
            "at": chrono::Utc::now().to_rfc3339(),
            "term": "abc123",
            "pid": 4242,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "root beat: {:?}",
        resp.text().await
    );
    let (status, card, _) = get_json(&client, addr, "/api/sessions/child-1/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["terminal"]["available"], true, "{card}");
    assert_eq!(card["terminal"]["term_id"], "abc123", "{card}");
    assert_eq!(card["terminal"]["harness"], "omp", "{card}");
    assert_eq!(card["terminal"]["state"], "working", "{card}");
    assert_eq!(card["terminal"]["blocked"], false, "{card}");
    assert_eq!(card["terminal"]["admitted"], true, "{card}");
    assert_eq!(card["terminal"]["via"], "root", "{card}");
    assert!(card["terminal"].get("reason").is_none(), "{card}");
    let (status, card, _) = get_json(&client, addr, "/api/sessions/tree-root/card").await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["terminal"]["available"], true, "{card}");
    assert_eq!(card["terminal"]["term_id"], "abc123", "{card}");
    assert!(card["terminal"].get("via").is_none(), "{card}");
    assert!(
        fake.conns.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the directory refresh must have dialed the runner"
    );
}
