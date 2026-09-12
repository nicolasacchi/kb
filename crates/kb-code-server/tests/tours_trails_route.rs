//! V74-L3b — end-to-end HTTP tests for `kbc-tour/1` and `kbc-trail/1`
//! against a real daemon booted via `serve_on_random_port_with_paths`
//! (`tests/boards_route.rs`'s conventions: `boot_with_repo`, a `SERIAL`
//! guard, a git fixture repo).
//!
//! What these cover that the crate's unit tests cannot:
//!
//! * a tour step walking the REAL Ladder over a REAL working tree
//!   (pinned → carried → orphan), through the same resolver a board uses;
//! * that a tour and a board share `canvas_boards` WITHOUT leaking into
//!   each other's list — the one regression the V0039 `kind` column can
//!   cause;
//! * the trail LEDGER-OFF posture end to end: every write refused by name,
//!   every read answering honestly, and the ledger-off vs ledger-on-empty
//!   reads differing only where they must;
//! * the sub-step refusal, the granularity floor, purge, fork, and that the
//!   aggregate never carries a step timestamp.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, TrailsSection};
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

async fn boot(name: &str, path: &Path, trails: TrailsSection) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        trails,
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

fn trails_on() -> TrailsSection {
    TrailsSection {
        enabled: true,
        retention_days: 30,
        step_granularity_secs: 5,
    }
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

async fn post(base: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let c = reqwest::Client::new();
    let resp = c
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, v)
}

// --- kbc-tour/1 --------------------------------------------------------------

/// Two `code` steps (the `ref` sugar and the structured form) plus one
/// prose step — so one document exercises both authoring shapes.
fn tour_doc(slug: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "kbc-tour/1",
        "repo": "r",
        "slug": slug,
        "title": "Order placement",
        "description_md": "How an order is placed.",
        "steps": [
            { "id": "s-entry", "title": "Entry point",
              "body_md": "Everything starts here.",
              "ref": "code:app/models/order.rb:2-5",
              "camera": { "fold": true, "context": 3 } },
            { "id": "s-lock", "title": "The lock",
              "kind": "code", "path": "app/models/order.rb", "range": [3, 3] },
            { "id": "s-why", "body_md": "And that is why the lock is held." }
        ]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tour_applies_idempotently_and_its_steps_walk_the_real_ladder() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), TrailsSection::default()).await;

    let (s, v) = post(&b.base, "/api/tours/apply", &tour_doc("checkout")).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["steps"], 3);
    assert_eq!(v["status"], "pending", "D21: agent-authored starts pending");

    let (s, v) = post(&b.base, "/api/tours/apply", &tour_doc("checkout")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["unchanged"], true, "the same document twice is a no-op");
    assert_eq!(v["revision"], 1);

    // Changing ONLY a camera must still bump the revision — a camera is
    // authored content.
    let mut cam = tour_doc("checkout");
    cam["steps"][0]["camera"]["context"] = serde_json::json!(8);
    let (s, v) = post(&b.base, "/api/tours/apply", &cam).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["unchanged"], false, "a camera is authored content: {v}");
    assert_eq!(v["revision"], 2);

    // PINNED: the file is exactly what the apply captured.
    let (s, v) = get(&b.base, "/api/tours/checkout?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["schema"], "kbc-tour/1");
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    assert_eq!(v["steps"][0]["node"]["state"], "pinned", "{v}");
    assert_eq!(v["steps"][0]["ordinal"], 0);
    assert_eq!(v["steps"][0]["camera"]["fold"], true);
    assert_eq!(v["steps"][0]["camera"]["context"], 8);
    assert!(
        v["steps"][0]["ref"]
            .as_str()
            .unwrap_or_default()
            .starts_with("code:app/models/order.rb:2-5@"),
        "the K1 projection must carry the resolved blob: {v}"
    );
    assert_eq!(
        v["steps"][2]["node"]["state"], "inert",
        "a prose-only step has nothing to resolve"
    );
    assert_eq!(v["honesty"]["pinned"], 2);
    assert_eq!(v["honesty"]["orphans"], 0);

    // CARRIED: prepend lines so the range moves.
    let p = repo.path().join("app/models/order.rb");
    std::fs::write(
        p.clone(),
        format!("# frozen_string_literal: true\n\n{ORDER_RB}"),
    )
    .unwrap();
    let (_, v) = get(&b.base, "/api/tours/checkout?repo=r").await;
    assert_eq!(v["steps"][0]["node"]["state"], "carried", "{v}");
    assert!(v["honesty"]["carried"].as_i64().unwrap() >= 1);

    // ORPHAN: the file is gone.
    std::fs::remove_file(p).unwrap();
    let (_, v) = get(&b.base, "/api/tours/checkout?repo=r").await;
    assert_eq!(v["steps"][0]["node"]["state"], "orphan", "{v}");
    assert_eq!(
        v["steps"][0]["node"]["reason"], "path-gone",
        "an orphan is SHOWN with its reason, never dropped: {v}"
    );
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tour_and_a_board_share_one_table_without_leaking_into_each_others_lists() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), TrailsSection::default()).await;

    let board = serde_json::json!({
        "schema": "kbc-canvas/1", "repo": "r", "slug": "a-board", "title": "B",
        "nodes": [{ "id": "n1", "kind": "note", "body_md": "hi" }]
    });
    let (s, _) = post(&b.base, "/api/boards/apply", &board).await;
    assert_eq!(s, 201);
    let (s, _) = post(&b.base, "/api/tours/apply", &tour_doc("a-tour")).await;
    assert_eq!(s, 201);

    let (_, v) = get(&b.base, "/api/boards?repo=r").await;
    let slugs: Vec<&str> = v["boards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["a-board"], "a tour must never list as a board");

    let (_, v) = get(&b.base, "/api/tours?repo=r").await;
    let slugs: Vec<&str> = v["tours"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["a-tour"], "a board must never list as a tour");

    // …and neither can be read through the other's route.
    assert_eq!(get(&b.base, "/api/boards/a-tour?repo=r").await.0, 404);
    assert_eq!(get(&b.base, "/api/tours/a-board?repo=r").await.0, 404);

    // The shared slug space is enforced with a NAMED conflict, not a
    // constraint error.
    let mut clash = tour_doc("a-board");
    clash["slug"] = "a-board".into();
    let (s, v) = post(&b.base, "/api/tours/apply", &clash).await;
    assert_eq!(s, 409, "{v}");
    assert!(
        v["error"].as_str().unwrap_or_default().contains("board"),
        "the conflict must name the family that holds the slug: {v}"
    );

    // kbc-seq/1 sees both, under their OWN projections.
    let (_, v) = get(&b.base, "/api/seq?repo=r").await;
    let pairs: Vec<(&str, &str)> = v["projections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["projection"].as_str().unwrap(), p["id"].as_str().unwrap()))
        .collect();
    assert!(pairs.contains(&("board", "a-board")), "{pairs:?}");
    assert!(pairs.contains(&("tour", "a-tour")), "{pairs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pack_budget_truncates_as_a_prefix_and_both_exports_say_they_are_snapshots() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), TrailsSection::default()).await;
    post(&b.base, "/api/tours/apply", &tour_doc("checkout")).await;

    let (s, v) = get(&b.base, "/api/tours/checkout/pack?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["steps_packed"], 3);
    assert!(v["dropped"].as_array().unwrap().is_empty());
    assert!(v["text"].as_str().unwrap().contains("Entry point"));

    let (_, v) = get(&b.base, "/api/tours/checkout/pack?repo=r&budget=200").await;
    assert!(
        v["steps_packed"].as_i64().unwrap() < 3,
        "a 200-byte budget cannot hold three steps: {v}"
    );
    let dropped = v["dropped"].as_array().unwrap();
    assert!(!dropped.is_empty());
    assert!(
        dropped[0]["reason"].as_str().unwrap().contains("200-byte"),
        "the drop must name the budget: {v}"
    );

    let (s, md) = get_text(&b.base, "/api/tours/checkout/export?repo=r&format=md").await;
    assert_eq!(s, 200);
    assert!(md.contains("**Snapshot.**"), "{md}");
    assert!(md.contains("# Order placement"));

    let (s, ct) = get_text(&b.base, "/api/tours/checkout/export?repo=r&format=codetour").await;
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&ct).unwrap();
    assert_eq!(v["$schema"], "https://aka.ms/codetour-schema");
    assert_eq!(v["kbc_snapshot"], true);
    assert_eq!(
        v["steps"].as_array().unwrap().len(),
        2,
        "the prose-only step has no file to point at: {v}"
    );
    let lossy = v["kbc_lossy"].as_array().unwrap();
    assert!(lossy
        .iter()
        .any(|l| l.as_str().unwrap().contains("1 step(s)")));
    assert!(lossy.iter().any(|l| l.as_str().unwrap().contains("camera")));

    assert_eq!(
        get(&b.base, "/api/tours/checkout/export?repo=r&format=svg")
            .await
            .0,
        400
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tour_with_a_coordinate_or_an_unsupported_ref_is_refused_by_name() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), TrailsSection::default()).await;

    let mut geo = tour_doc("t");
    geo["steps"][0]["camera"] = serde_json::json!({ "x": 10 });
    let (s, v) = post(&b.base, "/api/tours/apply", &geo).await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("coordinate-free"),
        "the board coordinate rule runs over a tour unchanged: {v}"
    );

    let mut sym = tour_doc("t");
    sym["steps"][0]["ref"] = "sym:Order#place".into();
    let (s, v) = post(&b.base, "/api/tours/apply", &sym).await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("DERIVATION"),
        "the refusal must say WHY, not just no: {v}"
    );

    // Nothing was written by either refusal.
    assert_eq!(get(&b.base, "/api/tours/t?repo=r").await.0, 404);
}

// --- kbc-trail/1 -------------------------------------------------------------

fn steps_batch(entered: i64) -> serde_json::Value {
    serde_json::json!({
        "repo": "r",
        "session_hint": "tab-1",
        "steps": [
            { "via": "search", "path": "app/models/order.rb", "line_start": 2,
              "entered_at": entered, "left_at": entered + 12 },
            { "via": "definition_of", "path": "README.md",
              "entered_at": entered + 12, "left_at": entered + 15 }
        ]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_ledger_off_every_write_is_refused_by_name_and_every_read_is_honest() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    // TrailsSection::default() is `enabled: false` — the FIRST-BOOT shape.
    let b = boot("r", repo.path(), TrailsSection::default()).await;

    let (s, v) = get(&b.base, "/api/trails/state").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["enabled"], false, "OFF on first boot (D17)");
    assert_eq!(v["mode"], "off");
    assert_eq!(v["mutable"], false);
    assert!(v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("aggregate")));

    let (s, v) = post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_000)).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["error"].as_str().unwrap().contains("disabled"), "{v}");

    let (s, v) = post(
        &b.base,
        "/api/trails/state",
        &serde_json::json!({"mode": "recording"}),
    )
    .await;
    assert_eq!(s, 403, "the master switch cannot be flipped over HTTP: {v}");

    // The reads still ANSWER — an off ledger is an empty one that says so,
    // never a 404 or a 500.
    let (s, v) = get(&b.base, "/api/trails?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert!(v["trails"].as_array().unwrap().is_empty());
    assert_eq!(v["mode"], "off");
    let (s, v) = get(&b.base, "/api/trails/aggregate?repo=r").await;
    assert_eq!(s, 200, "{v}");
    assert!(v["rows"].as_array().unwrap().is_empty());
    assert_eq!(v["enabled"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_off_and_ledger_on_but_empty_reads_differ_only_in_the_opt_in_fields() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();

    let off = boot("r", repo.path(), TrailsSection::default()).await;
    let on = boot("r", repo.path(), trails_on()).await;

    for path in ["/api/trails?repo=r", "/api/trails/aggregate?repo=r"] {
        let (_, a) = get(&off.base, path).await;
        let (_, b) = get(&on.base, path).await;
        let mut a = a.as_object().unwrap().clone();
        let mut b = b.as_object().unwrap().clone();
        // `enabled` is the ONE field whose whole job is reporting the
        // opt-in; strip it and the two reads must be byte-identical.
        a.remove("enabled");
        b.remove("enabled");
        a.remove("notes");
        b.remove("notes");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "{path}: turning the ledger ON must change NOTHING about what a read says \
             until something is actually recorded — D17's never-a-gate rule"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recording_ingests_quantised_steps_and_refuses_anything_finer_than_a_step() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), trails_on()).await;

    // Enabled, but not yet opted in: still refused, and with a DIFFERENT
    // reason than "disabled".
    let (s, v) = post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_000)).await;
    assert_eq!(s, 409, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("not been turned on"),
        "{v}"
    );

    let (s, v) = post(
        &b.base,
        "/api/trails/state",
        &serde_json::json!({"mode": "recording"}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["mode"], "recording");

    let (s, v) = post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_000)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["appended"], 2);
    assert_eq!(v["step_granularity_secs"], 5);
    let trail_id = v["trail_id"].as_str().unwrap().to_string();
    assert!(trail_id.starts_with("trl_"));

    let (_, v) = get(&b.base, &format!("/api/trails/{trail_id}?repo=r")).await;
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0]["dwell_secs"], 10,
        "12s floored to a 5s granularity is 10, never 12 and never 15: {v}"
    );
    assert_eq!(
        steps[1]["dwell_secs"], 0,
        "a 3s hop under a 5s floor records the visit with an honest zero dwell"
    );
    assert_eq!(
        steps[0]["state"], "carried",
        "this batch pinned no blob, and no witness means no pin CLAIM — the file is \
         there, so `carried` is the honest answer, never `pinned`"
    );
    assert!(
        steps[0].get("entered_at").is_none(),
        "even the operator's own read carries no wall-clock per step: {v}"
    );

    // A sub-step payload is REFUSED, not rounded.
    let mut fine = steps_batch(1_757_000_100);
    fine["steps"][0]["viewport"] = serde_json::json!([1, 40]);
    let (s, v) = post(&b.base, "/api/trails/steps", &fine).await;
    assert_eq!(s, 400, "{v}");
    let msg = v["error"].as_str().unwrap();
    assert!(msg.contains("viewport"), "{msg}");
    assert!(msg.contains("REFUSED, not rounded"), "{msg}");

    // A client-supplied dwell has nowhere to land either.
    let mut dwell = steps_batch(1_757_000_200);
    dwell["steps"][0]["dwell_secs"] = serde_json::json!(9999);
    assert_eq!(post(&b.base, "/api/trails/steps", &dwell).await.0, 400);

    // PAUSE is one transition, with its own refusal.
    let (_, v) = post(
        &b.base,
        "/api/trails/state",
        &serde_json::json!({"mode": "paused"}),
    )
    .await;
    assert_eq!(v["mode"], "paused");
    let (s, v) = post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_300)).await;
    assert_eq!(s, 409, "{v}");
    assert!(v["error"].as_str().unwrap().contains("PAUSED"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_aggregate_is_counts_per_path_with_no_time_finer_than_a_day() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), trails_on()).await;
    post(
        &b.base,
        "/api/trails/state",
        &serde_json::json!({"mode": "recording"}),
    )
    .await;
    post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_000)).await;
    post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_400)).await;

    let (s, v) = get(&b.base, "/api/trails/aggregate?repo=r").await;
    assert_eq!(s, 200, "{v}");
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "two distinct paths: {v}");
    let top = &rows[0];
    assert_eq!(top["steps"], 2);
    assert_eq!(top["days"], 1);
    assert_eq!(top["first_day"], top["last_day"]);
    assert_eq!(top["first_day"].as_str().unwrap().len(), 10);
    for row in rows {
        for forbidden in ["entered_at", "left_at", "ordinal", "trail_id", "timestamp"] {
            assert!(
                row.get(forbidden).is_none(),
                "the agent-facing read must not carry {forbidden}: {row}"
            );
        }
    }
    // The window grammar is a DAY, not a timestamp.
    assert_eq!(
        get(&b.base, "/api/trails/aggregate?repo=r&since=1757000000")
            .await
            .0,
        400
    );
    let (s, v) = get(&b.base, "/api/trails/aggregate?repo=r&since=2099-01-01").await;
    assert_eq!(s, 200);
    assert!(v["rows"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_carries_the_steps_from_its_branch_point_and_purge_is_wholesale() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    let b = boot("r", repo.path(), trails_on()).await;
    post(
        &b.base,
        "/api/trails/state",
        &serde_json::json!({"mode": "recording"}),
    )
    .await;
    let (_, v) = post(&b.base, "/api/trails/steps", &steps_batch(1_757_000_000)).await;
    let parent = v["trail_id"].as_str().unwrap().to_string();

    let (s, v) = post(
        &b.base,
        &format!("/api/trails/{parent}/fork"),
        &serde_json::json!({"repo": "r", "from_ordinal": 1, "title": "another way"}),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let fork = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["parent_id"], parent);
    assert_eq!(v["parent_ordinal"], 1);
    assert_eq!(v["steps"], 1, "only the branch point onward: {v}");

    let (_, v) = get(&b.base, &format!("/api/trails/{fork}?repo=r")).await;
    assert_eq!(v["parent_id"], parent);
    assert_eq!(v["step_count"], 1);
    assert_eq!(
        v["steps"].as_array().unwrap().len(),
        1,
        "the flattened COUNT and the step ARRAY carry different names, so both are          reachable on the wire: {v}"
    );

    // An out-of-range branch point is a refusal with the count, not an
    // empty trail.
    let (s, v) = post(
        &b.base,
        &format!("/api/trails/{parent}/fork"),
        &serde_json::json!({"repo": "r", "from_ordinal": 99}),
    )
    .await;
    assert_eq!(s, 400, "{v}");

    // One trail.
    let (s, v) = post(
        &b.base,
        "/api/trails/purge",
        &serde_json::json!({"repo": "r", "id": fork}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["trails"], 1);
    assert_eq!(
        get(&b.base, &format!("/api/trails/{fork}?repo=r")).await.0,
        404
    );

    // …then the whole ledger.
    let (s, v) = post(
        &b.base,
        "/api/trails/purge",
        &serde_json::json!({"repo": "r"}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["trails"], 1);
    assert_eq!(v["steps"], 2);
    let (_, v) = get(&b.base, "/api/trails?repo=r").await;
    assert!(v["trails"].as_array().unwrap().is_empty());
    // A purge leaves the opt-in alone: it deletes data, not the decision.
    let (_, v) = get(&b.base, "/api/trails/state").await;
    assert_eq!(v["mode"], "recording");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authored_trail_is_not_gated_on_recording_and_drains_its_dissent_notes() {
    let _g = SERIAL.lock().await;
    let repo = fixture_repo();
    // Enabled but NEVER opted in: an authored trail records nothing about
    // the operator, so it does not need the recording mode.
    let b = boot("r", repo.path(), trails_on()).await;

    let doc = serde_json::json!({
        "schema": "kbc-trail/1",
        "repo": "r",
        "authored": true,
        "title": "Start with the lock",
        "steps": [
            { "via": "agent_suggested", "path": "app/models/order.rb",
              "line_start": 3, "entered_at": 1_757_000_000,
              "note": "this is where the lock is taken" }
        ]
    });
    let (s, v) = post(&b.base, "/api/trails", &doc).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["origin"], "authored");
    let id = v["id"].as_str().unwrap().to_string();

    let (_, v) = get(&b.base, &format!("/api/trails/{id}?repo=r&notes=1")).await;
    assert_eq!(v["origin"], "authored");
    assert!(v["notes_list"].as_array().unwrap().is_empty());
    assert!(
        v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("dissent")),
        "an authored trail must tell the reader it is walkable: {v}"
    );

    // The human dissents — an ORDINARY annotation carrying the trail id.
    let (s, ann) = post(
        &b.base,
        "/api/annotations",
        &serde_json::json!({
            "repo": "r",
            "path": "app/models/order.rb",
            "line": 3,
            "body": "no — the lock is taken by the caller",
            "trail_id": id,
        }),
    )
    .await;
    assert_eq!(s, 201, "{ann}");

    let (_, v) = get(&b.base, &format!("/api/trails/{id}?repo=r&notes=1")).await;
    let notes = v["notes_list"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{v}");
    assert!(notes[0]["body"].as_str().unwrap().starts_with("no —"));

    // A purge takes the movement record and LEAVES the human's own words
    // (invariant 23(a): authored content is not derived data).
    let (s, _) = post(
        &b.base,
        "/api/trails/purge",
        &serde_json::json!({"repo": "r"}),
    )
    .await;
    assert_eq!(s, 200);
    let (s, list) = get(&b.base, "/api/annotations?repo=r&path=app/models/order.rb").await;
    assert_eq!(s, 200);
    assert!(
        serde_json::to_string(&list).unwrap().contains("no —"),
        "a purge must not destroy the human's own dissent: {list}"
    );
}
