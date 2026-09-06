//! V74-L1 — end-to-end HTTP tests for the `kbc-canvas/1` board family
//! against a real daemon booted via `serve_on_random_port_with_paths`
//! (`tests/reading_sets_route.rs`'s own conventions: `boot_with_repo`, a
//! `SERIAL` guard, a git fixture repo).
//!
//! What these cover that the crate's unit tests cannot: the Ladder over a
//! REAL working tree (edit / move / delete a target and watch a node go
//! pinned → carried → orphan), apply idempotency through the wire, the
//! status gate, and the three exports over a resolved board.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

const ORDER_RB: &str = "\
class Order
  def place
    lock!
    charge!
  end
end
";

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    common::init_repo(dir);
    std::fs::create_dir_all(dir.join("app/models")).unwrap();
    std::fs::write(dir.join("app/models/order.rb"), ORDER_RB).unwrap();
    std::fs::write(dir.join("README.md"), "# fixture\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot_with_repo(name: &str, path: &Path) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
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

/// A minimal document with one `code` node over `app/models/order.rb`'s
/// `def place` body.
fn doc(slug: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "kbc-canvas/1",
        "repo": "r",
        "slug": slug,
        "title": "Order placement",
        "description_md": "How an order is placed.",
        "nodes": [
            { "id": "n-place", "kind": "code", "title": "Entry point",
              "path": "app/models/order.rb", "range": [2, 5], "symbol": "Order#place" },
            { "id": "n-note", "kind": "note", "body_md": "The lock is held across the call." }
        ],
        "edges": [
            { "from": "n-place", "to": "n-note", "kind": "then", "label": "then" }
        ],
        "steps": [
            { "node": "n-place", "caption": "Start here." },
            { "node": "n-note" }
        ]
    })
}

async fn apply(
    base: &str,
    body: &serde_json::Value,
    query: &[(&str, &str)],
) -> (u16, serde_json::Value) {
    let c = reqwest::Client::new();
    let resp = c
        .post(format!("{base}/api/boards/apply"))
        .query(query)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, v)
}

async fn get(base: &str, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, v)
}

async fn get_text(base: &str, path: &str) -> (u16, String) {
    let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_is_idempotent_and_a_second_identical_apply_is_unchanged() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;

    let (s, v) = apply(&b.base, &doc("checkout"), &[]).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["unchanged"], false);
    assert_eq!(v["revision"], 1);
    assert_eq!(
        v["status"], "pending",
        "an agent-authored board starts PENDING (D21)"
    );

    let (s, v) = apply(&b.base, &doc("checkout"), &[]).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["created"], false);
    assert_eq!(
        v["unchanged"], true,
        "the same document twice must be a no-op: {v}"
    );
    assert_eq!(
        v["revision"], 1,
        "an unchanged apply must not bump revision"
    );

    // A changed document DOES bump.
    let mut changed = doc("checkout");
    changed["title"] = "Order placement (v2)".into();
    let (s, v) = apply(&b.base, &changed, &[]).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["unchanged"], false);
    assert_eq!(v["revision"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_code_node_walks_the_ladder_pinned_then_carried_then_orphan() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    let (s, _) = apply(&b.base, &doc("ladder"), &[]).await;
    assert_eq!(s, 201);

    // 1. Untouched tree → pinned.
    let (s, v) = get(&b.base, "/api/boards/ladder?repo=r").await;
    assert_eq!(s, 200, "{v}");
    let n = &v["nodes"][0];
    assert_eq!(n["state"], "pinned", "{n}");
    assert_eq!(n["reason"], "blob-current");
    assert!(n["code"]["snippet"].as_str().unwrap().contains("def place"));
    assert_eq!(v["honesty"]["orphans"], 0);

    // 2. Insert lines ABOVE the range → the anchored line moved → carried.
    let path = repo.path().join("app/models/order.rb");
    std::fs::write(
        &path,
        format!("# frozen_string_literal: true\n\n{ORDER_RB}"),
    )
    .unwrap();
    let (s, v) = get(&b.base, "/api/boards/ladder?repo=r").await;
    assert_eq!(s, 200, "{v}");
    let n = &v["nodes"][0];
    assert_eq!(n["state"], "carried", "{n}");
    assert_eq!(n["code"]["shifted_by"], 2);
    assert_eq!(n["code"]["range"][0], 4);
    assert!(n["code"]["snippet"].as_str().unwrap().contains("def place"));

    // 3. Delete the anchored content → orphan, SHOWN with its last-known text.
    std::fs::write(&path, "class Order\nend\n").unwrap();
    let (s, v) = get(&b.base, "/api/boards/ladder?repo=r").await;
    assert_eq!(s, 200, "{v}");
    let n = &v["nodes"][0];
    assert_eq!(n["state"], "orphan", "{n}");
    assert_eq!(n["reason"], "no-anchor");
    assert!(
        n["code"]["snippet"].is_null(),
        "an orphan has no CURRENT text"
    );
    assert_eq!(
        n["code"]["anchor_snippet"], "def place",
        "an orphan keeps its last-known text so the card is shown, not dropped"
    );
    assert_eq!(v["honesty"]["orphans"], 1);
    assert!(
        v["nodes"].as_array().unwrap().len() == 2,
        "nothing was dropped"
    );

    // 4. Delete the FILE → orphan with a different reason.
    std::fs::remove_file(&path).unwrap();
    let (_, v) = get(&b.base, "/api/boards/ladder?repo=r").await;
    assert_eq!(v["nodes"][0]["reason"], "path-gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_coordinate_refusal_names_pins_and_nothing_is_written() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    let mut d = doc("coords");
    d["nodes"][0]["x"] = 10.into();
    let (s, v) = apply(&b.base, &d, &[]).await;
    assert_eq!(s, 400, "{v}");
    let msg = v["error"].as_str().unwrap_or_default().to_string() + &v.to_string();
    assert!(msg.contains("coordinate"), "{msg}");
    assert!(msg.contains("pins"), "the refusal must teach `pins`: {msg}");
    let (s, _) = get(&b.base, "/api/boards/coords?repo=r").await;
    assert_eq!(s, 404, "a refused apply must write nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_lints_and_resolves_but_writes_nothing() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    let (s, v) = apply(&b.base, &doc("dry"), &[("dry_run", "1")]).await;
    assert!(s == 200 || s == 201, "{s} {v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["honesty"]["nodes"], 2);
    assert_eq!(
        v["honesty"]["pinned"], 1,
        "a dry run resolves for real: {v}"
    );
    let (s, _) = get(&b.base, "/api/boards/dry?repo=r").await;
    assert_eq!(s, 404, "a dry run must write nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_is_the_only_writer_of_the_accepted_status() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    // A document may not author `accepted`.
    let mut d = doc("gate");
    d["status"] = "accepted".into();
    let (s, v) = apply(&b.base, &d, &[]).await;
    assert_eq!(s, 400, "{v}");
    assert!(v.to_string().contains("status"), "{v}");

    // …but `draft` is fine, and `accept` moves it.
    d["status"] = "draft".into();
    let (s, v) = apply(&b.base, &d, &[]).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["status"], "draft");

    let c = reqwest::Client::new();
    let resp = c
        .post(format!("{}/api/boards/gate/accept?repo=r", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "accepted");
    assert_eq!(v["revision"], 2, "a status change is a change");

    // A CHANGED apply resets an accepted board (D21).
    let mut d2 = doc("gate");
    d2["status"] = "draft".into();
    d2["title"] = "Changed".into();
    let (_, v) = apply(&b.base, &d2, &[]).await;
    assert_eq!(v["status"], "draft");
    assert_eq!(v["status_reset"], true, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_three_exports_render_a_resolved_board() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    apply(&b.base, &doc("exp"), &[]).await;

    let (s, md) = get_text(&b.base, "/api/boards/exp/export?repo=r&format=md").await;
    assert_eq!(s, 200);
    assert!(md.starts_with("# Order placement"), "{md}");
    assert!(md.contains("### 1. Entry point"), "{md}");
    assert!(md.contains("> Start here."), "{md}");
    assert!(md.contains("```rb"), "{md}");
    assert!(md.contains("SNAPSHOT"), "every export carries the caption");

    let (s, canvas) = get_text(&b.base, "/api/boards/exp/export?repo=r&format=jsoncanvas").await;
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&canvas).unwrap();
    let nodes = v["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    for n in nodes {
        for f in ["id", "type", "x", "y", "width", "height"] {
            assert!(!n[f].is_null(), "JSON Canvas requires {f}: {n}");
        }
    }
    assert_eq!(v["edges"].as_array().unwrap().len(), 1);
    assert!(v["kbc_layout"].is_string());

    let (s, html) = get_text(&b.base, "/api/boards/exp/export?repo=r&format=kb-html").await;
    assert_eq!(s, 200);
    assert!(html.contains("<meta name=\"kb-category\" content=\"kb-code-board\">"));
    assert!(html.contains("<meta name=\"kb-tags\""));
    assert!(!html.contains("kb-prompt"));
    assert!(html.contains("<h1>Order placement</h1>"));
    assert!(html.contains("/r/r/app/models/order.rb?line="));

    // Unknown format is a 400 naming the vocabulary.
    let (s, v) = get(&b.base, "/api/boards/exp/export?repo=r&format=svg").await;
    assert_eq!(s, 400);
    assert!(v.to_string().contains("jsoncanvas"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_reports_drift_and_says_nothing_when_there_is_none() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    apply(&b.base, &doc("sw"), &[]).await;

    let (s, v) = get(&b.base, "/api/boards/sweep?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["checked"], 1);
    assert_eq!(v["drifted"], false, "{v}");
    assert_eq!(v["boards"][0]["nodes"].as_array().unwrap().len(), 0);

    std::fs::write(
        repo.path().join("app/models/order.rb"),
        "class Order\nend\n",
    )
    .unwrap();
    let (s, v) = get(&b.base, "/api/boards/sweep?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["drifted"], true, "{v}");
    assert_eq!(v["boards"][0]["orphans"], 1);
    assert_eq!(v["boards"][0]["nodes"][0]["node"], "n-place");
    assert_eq!(v["boards"][0]["nodes"][0]["state"], "orphan");

    // …and sweep NEVER mutates: the board is unchanged.
    let (_, board) = get(&b.base, "/api/boards/sw?repo=r").await;
    assert_eq!(board["revision"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_route_carries_the_status_vocabulary_and_true_counts() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    apply(&b.base, &doc("one"), &[]).await;
    let mut two = doc("two");
    two["status"] = "draft".into();
    apply(&b.base, &two, &[]).await;

    let (s, v) = get(&b.base, "/api/boards?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["boards"].as_array().unwrap().len(), 2);
    let statuses: Vec<&str> = v["statuses_available"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["pending", "draft", "accepted", "archived"]);
    for b in v["boards"].as_array().unwrap() {
        assert_eq!(b["nodes"], 2);
        assert_eq!(b["edges"], 1);
        assert_eq!(b["steps"], 2);
    }

    let (_, v) = get(&b.base, "/api/boards?repo=r&status=draft").await;
    assert_eq!(v["boards"].as_array().unwrap().len(), 1);
    assert_eq!(v["boards"][0]["slug"], "two");

    let (s, v) = get(&b.base, "/api/boards?repo=r&status=nonsense").await;
    assert_eq!(s, 400, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_board_is_a_kbc_seq_board_projection_with_a_true_node_count() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    apply(&b.base, &doc("seqb"), &[]).await;
    let (s, v) = get(&b.base, "/api/seq?repo=r&projection=board").await;
    assert_eq!(s, 200, "{v}");
    let rows = v["projections"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["id"] == "seqb")
        .unwrap_or_else(|| panic!("board projection missing: {v}"));
    assert_eq!(row["projection"], "board");
    assert_eq!(row["source"], "canvas_boards");
    assert_eq!(
        row["size"], 2,
        "a kbc-canvas/1 board reports a TRUE node count, unlike an opaque canvas_sets row"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reference_that_does_not_resolve_warns_but_still_applies() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    let d = serde_json::json!({
        "schema": "kbc-canvas/1", "repo": "r", "slug": "warn", "title": "T",
        "nodes": [
            { "id": "n-gone", "kind": "code", "path": "app/models/nope.rb", "range": [1, 2] },
            { "id": "n-find", "kind": "finding", "review": 999, "finding": "f-nope" }
        ],
        "edges": [{ "from": "n-gone", "to": "n-find", "kind": "question" }]
    });
    let (s, v) = apply(&b.base, &d, &[]).await;
    assert_eq!(
        s, 201,
        "an unresolvable REFERENCE is a warning, not a refusal: {v}"
    );
    let warnings = v["resolution_warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2, "{v}");
    assert!(warnings[0].as_str().unwrap().contains("orphan"));
    assert_eq!(v["honesty"]["orphans"], 2);

    // …but a MALFORMED address is a refusal.
    let mut bad = d.clone();
    bad["slug"] = "bad".into();
    bad["nodes"][0]["path"] = "../../etc/passwd".into();
    let (s, v) = apply(&b.base, &bad, &[]).await;
    assert_eq!(s, 400, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_the_board_and_its_children() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot_with_repo("r", repo.path()).await;
    apply(&b.base, &doc("gone"), &[]).await;
    let c = reqwest::Client::new();
    let resp = c
        .delete(format!("{}/api/boards/gone?repo=r", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 204);
    let (s, _) = get(&b.base, "/api/boards/gone?repo=r").await;
    assert_eq!(s, 404);
    // …and a second delete is an honest 404, not a silent success.
    let resp = c
        .delete(format!("{}/api/boards/gone?repo=r", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}
