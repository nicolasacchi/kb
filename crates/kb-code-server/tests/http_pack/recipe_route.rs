//! V74-L3a — `kbc-recipe/1` over a real daemon and a real repo.
//!
//! The pure half (loading, the DAG type-check, the closed op set, trust
//! arithmetic, param validation, the census vocabulary) is unit-tested in
//! `src/recipe/tests.rs`. What needs a repo lives here: trust-on-first-use
//! against the ODB, the collision rule, determinism, materialise + replay,
//! the census over lanes that really are disabled, and the six `recipes/1`
//! repairs.
//!
//! The fixture is the synthetic Rails app (`tests/fixtures/rails-app`,
//! `acme-app`) plus two committed `.kbc/recipes/*.toml` files — the only
//! way to exercise "read ONLY from the default ref through the ODB".

use crate::common::git;
use kb_code_server::config::{
    BehavioralSection, KbCodeConfig, KbDaemonSection, RepoEntry, ScopesSection,
};
use kb_core::paths::KbPaths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPO: &str = "acme-app";

/// A repo-versioned recipe that LOADS — the trust ladder's happy path.
const REPO_RECIPE: &str = r#"
slug = "team:models"
title = "Our models"
intent = "rails"
description_md = "Every model the entity index knows about."
steps = [ { id = "models", op = "rails", args = { noun = "model", limit = 50 } } ]
views = [ { id = "models", kind = "table", step = "models", columns = [
  { header = "Model", field = "entity" },
  { header = "Path", field = "path" },
  { header = "Trust", field = "trust" },
] } ]
"#;

/// A repo file whose slug collides with a shipped built-in. The repo file
/// must WIN and the catalog must say what it shadowed (D11).
const COLLIDING_RECIPE: &str = r#"
slug = "rails:orphans"
title = "Our orphan rules"
intent = "rails"
steps = [ { id = "o", op = "rails", args = { orphans = "job_never_enqueued", limit = 10 } } ]
views = [ { id = "o", kind = "list", step = "o", columns = [ { field = "path" } ] } ]
"#;

fn fixture_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rails-app")
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

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_src(), tmp.path());
    write(tmp.path(), ".kbc/recipes/team:models.toml", REPO_RECIPE);
    write(
        tmp.path(),
        ".kbc/recipes/rails:orphans.toml",
        COLLIDING_RECIPE,
    );
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    git(tmp.path(), &["config", "user.email", "dev@example.com"]);
    git(tmp.path(), &["config", "user.name", "Dev"]);
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-q", "-m", "acme-app fixture"]);
    tmp
}

struct Boot {
    #[allow(dead_code)]
    state_dir: tempfile::TempDir,
    repo: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot() -> Boot {
    let repo = fixture_repo();
    let mut scopes = BTreeMap::new();
    scopes.insert("app".to_string(), vec!["app/**".to_string()]);
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
        scopes: ScopesSection { map: scopes },
        behavioral: BehavioralSection {
            enabled: true,
            window_days: 3650,
            max_commit_files: 50,
        },
        ..KbCodeConfig::default()
    };
    let state_dir = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(state_dir.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    let base = format!("http://{addr}");
    wait_for_indexed(&base, REPO, 15).await;
    Boot {
        state_dir,
        repo,
        base,
        task,
    }
}

async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["name"] == repo))
                {
                    let files = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if files >= expected_files && symbols > 0 {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        return;
                    }
                }
            }
        }
        assert!(Instant::now() < deadline, "index timeout");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `YYYY-MM-DD` for tomorrow — see `every_shipped_recipe_runs` for why.
fn tomorrow() -> String {
    (chrono::Utc::now() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

async fn file_count(base: &str, repo: &str) -> usize {
    let (_, body) = get(base, "/api/repos").await;
    body["repos"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["name"] == repo))
        .and_then(|r| r["file_count"].as_u64())
        .unwrap_or(0) as usize
}

async fn get(base: &str, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("GET");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

async fn post(
    base: &str,
    path: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .expect("POST");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

fn row<'a>(cat: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    cat["recipes"]
        .as_array()
        .expect("recipes array")
        .iter()
        .find(|r| r["slug"] == slug)
        .unwrap_or_else(|| panic!("no recipe {slug} in the catalog"))
}

// ---------------------------------------------------------------------------
// Catalog + homes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_reports_every_home_and_the_cli_line() {
    let b = boot().await;
    let (status, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    assert_eq!(status, 200, "{cat}");
    assert_eq!(cat["schema"], "kbc-recipe/1");

    // Eight DAG built-ins + six native adapters + one repo file that does
    // NOT collide = 15 rows (the colliding one replaces a builtin).
    let slugs: Vec<&str> = cat["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"orient:entry-points"), "{slugs:?}");
    assert!(
        slugs.contains(&"god-functions"),
        "the six natives are adopted"
    );
    assert!(slugs.contains(&"team:models"), "the repo file is listed");

    let builtin = row(&cat, "orient:hot-and-cold");
    assert_eq!(builtin["home"], "builtin");
    assert_eq!(builtin["trust"], "trusted");
    assert_eq!(
        builtin["cli"].as_str().unwrap(),
        format!("kb-code recipe run orient:hot-and-cold --repo {REPO}")
    );
    let with_param = row(&cat, "review:blast-radius");
    assert!(
        with_param["cli"].as_str().unwrap().contains("--p since="),
        "the CLI line names every required param: {}",
        with_param["cli"]
    );
}

/// D11: "repo file wins on slug collision and the UI shows the home".
#[tokio::test]
async fn a_repo_file_wins_a_slug_collision_and_reports_what_it_shadowed() {
    let b = boot().await;
    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    let collided = row(&cat, "rails:orphans");
    assert_eq!(collided["home"], "repo");
    assert_eq!(collided["title"], "Our orphan rules");
    assert_eq!(
        collided["shadowed_by"], "builtin",
        "the shadowed home is REPORTED, never silently dropped"
    );
    assert!(
        collided["source"]
            .as_str()
            .unwrap()
            .starts_with(".kbc/recipes/rails:orphans.toml".trim_start_matches('.'))
            || collided["source"].as_str().unwrap().starts_with("repo:"),
        "{}",
        collided["source"]
    );
}

// ---------------------------------------------------------------------------
// Trust on first use
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repo_recipe_is_untrusted_on_first_sight_and_refuses_to_run() {
    let b = boot().await;
    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    assert_eq!(row(&cat, "team:models")["trust"], "untrusted");

    let (status, body) = get(&b.base, &format!("/api/recipe/team:models/run?repo={REPO}")).await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("untrusted"),
        "the refusal names the STATE: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("recipe trust"),
        "…and tells the operator what to do: {body}"
    );
}

#[tokio::test]
async fn trusting_it_lets_it_run_and_a_later_change_surfaces_a_diff() {
    let b = boot().await;
    let (status, _) = post(
        &b.base,
        "/api/recipe/team:models/trust",
        serde_json::json!({ "repo": REPO }),
    )
    .await;
    assert_eq!(status, 200);

    let (status, run) = get(&b.base, &format!("/api/recipe/team:models/run?repo={REPO}")).await;
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["schema"], "kbc-recipe-run/1");
    assert_eq!(run["trust"], "trusted");
    assert_eq!(run["home"], "repo");

    // Change the file at the DEFAULT REF and the decision is re-armed
    // with a diff — never inherited by the new bytes.
    let changed = REPO_RECIPE.replace("Our models", "Our models (v2)");
    write(b.repo.path(), ".kbc/recipes/team:models.toml", &changed);
    git(b.repo.path(), &["add", "-A"]);
    git(b.repo.path(), &["commit", "-q", "-m", "edit the recipe"]);

    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    let r = row(&cat, "team:models");
    assert_eq!(r["trust"], "changed");
    let diff = r["trust_diff"].as_str().expect("a diff is surfaced");
    assert!(diff.contains("-title = \"Our models\""), "{diff}");
    assert!(diff.contains("+title = \"Our models (v2)\""), "{diff}");

    let (status, _) = get(&b.base, &format!("/api/recipe/team:models/run?repo={REPO}")).await;
    assert_eq!(
        status, 403,
        "a changed recipe does not run on the old decision"
    );
}

/// The working tree is NOT a source of recipes: an uncommitted file must
/// be invisible, because a recipe is something the team agreed to.
#[tokio::test]
async fn an_uncommitted_recipe_file_is_invisible() {
    let b = boot().await;
    write(
        b.repo.path(),
        ".kbc/recipes/scratch.toml",
        &REPO_RECIPE.replace("team:models", "scratch"),
    );
    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    let slugs: Vec<&str> = cat["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["slug"].as_str().unwrap())
        .collect();
    assert!(
        !slugs.contains(&"scratch"),
        "a working-tree file is not a repo-versioned recipe: {slugs:?}"
    );
}

// ---------------------------------------------------------------------------
// Params + caps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_required_param_is_a_400_naming_the_field() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipe/review:blast-radius/run?repo={REPO}"),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("p.since"), "{msg}");
    assert!(msg.contains("required"), "{msg}");
}

#[tokio::test]
async fn an_out_of_range_param_is_a_400_naming_the_bound() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipe/orient:hot-and-cold/run?repo={REPO}&p.top=9999"),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("9999") && msg.contains("500"), "{msg}");
}

/// D11's repair list, both halves: the NEW runner refuses over-cap, and
/// so does the FROZEN `recipes/1` route (which used to clamp silently).
#[tokio::test]
async fn repair_limit_over_the_cap_is_a_400_on_both_surfaces() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipe/orient:entry-points/run?repo={REPO}&limit=2000"),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("2000") && msg.contains("500"), "{msg}");

    let (status, body) = get(
        &b.base,
        &format!("/api/recipes/god-functions?repo={REPO}&limit=2000"),
    )
    .await;
    assert_eq!(status, 400, "recipes/1 no longer clamps silently: {body}");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("2000") && msg.contains("500"), "{msg}");
}

// ---------------------------------------------------------------------------
// Determinism, census, materialise
// ---------------------------------------------------------------------------

/// A run is deterministic for a given mirror state. The only fields that
/// legitimately move are the clock/elapsed ones, which are scrubbed.
#[tokio::test]
async fn two_runs_are_byte_identical() {
    let b = boot().await;
    // NOT `rails:orphans`: the fixture deliberately shadows that slug with
    // an UNTRUSTED repo file, and two 403s would agree with each other for
    // the wrong reason.
    let url = format!("/api/recipe/orient:hot-and-cold/run?repo={REPO}");
    let (_, mut a) = get(&b.base, &url).await;
    let (_, mut c) = get(&b.base, &url).await;
    for v in [&mut a, &mut c] {
        v["honesty"]["as_of"] = serde_json::Value::Null;
        v["honesty"]["elapsed_ms"] = serde_json::Value::Null;
        // The claim is "deterministic for a given mirror state", so the
        // mirror's own counter is scrubbed with the clock: what must
        // agree is every step, every row and every census.
        v["honesty"]["generation"] = serde_json::Value::Null;
        if let Some(steps) = v["steps"].as_array_mut() {
            for s in steps {
                s["ms"] = serde_json::Value::Null;
            }
        }
    }
    assert_eq!(a, c, "two runs against one mirror generation must agree");
}

/// The census is the whole point: an empty step must say WHY, and
/// "filtered out" (the only clean reason) must be distinguishable from
/// "that lane is switched off".
#[tokio::test]
async fn an_empty_step_says_why_and_a_disabled_lane_is_not_a_clean_zero() {
    let b = boot().await;
    let (status, run) = get(
        &b.base,
        &format!("/api/recipe/review:untested-changes/run?repo={REPO}&p.since=2000-01-01"),
    )
    .await;
    assert_eq!(status, 200, "{run}");
    let steps = run["steps"].as_array().unwrap();
    let covered = steps
        .iter()
        .find(|s| s["id"] == "covered")
        .expect("the coverage step ran");
    assert_eq!(
        covered["census"]["empty_reason"], "lane-disabled",
        "no [lanes] config ⇒ the lane is OFF, which is not \"no coverage gaps\": {}",
        covered["census"]
    );
    assert!(
        covered["census"]["filters_applied"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap_or("").contains("[lanes]")),
        "{}",
        covered["census"]
    );
    // …and every step carries a census at all.
    for s in steps {
        assert!(s.get("census").is_some(), "step {} has no census", s["id"]);
    }
}

#[tokio::test]
async fn materialise_then_replay_says_it_is_a_snapshot() {
    let b = boot().await;
    let (status, receipt) = post(
        &b.base,
        &format!("/api/recipe/orient:hot-and-cold/materialise?repo={REPO}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 201, "{receipt}");
    let id = receipt["run_id"].as_str().expect("a run id");
    assert!(id.starts_with("run_"), "{id}");

    let (status, replay) = get(&b.base, &format!("/api/recipe/runs/{id}")).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["schema"], "kbc-recipe-run/1");
    assert_eq!(replay["replay"]["run_id"], id);
    // The contract is that a replay SAYS whether it is a stale snapshot —
    // not that nothing moved. The live mirror may bump the generation
    // counter between the two calls, and a test that demanded otherwise
    // would be asserting the watcher is idle rather than the payload is
    // honest.
    let snap = replay["replay"]["generation"].as_u64().unwrap();
    let now = replay["replay"]["current_generation"].as_u64().unwrap();
    assert_eq!(
        replay["replay"]["stale"],
        serde_json::json!(snap != now),
        "`stale` must be exactly `generation != current_generation`"
    );

    let (status, _) = get(&b.base, "/api/recipe/runs/run_doesnotexist").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn recipe_new_stores_on_the_daemon_and_shows_up_as_a_server_home() {
    let b = boot().await;
    let doc = serde_json::json!({
        "slug": "agent:mine",
        "title": "Agent-authored",
        "intent": "hygiene",
        "steps": [ { "id": "t", "op": "tree", "args": { "limit": 5 } } ],
        "views": [ { "id": "v", "kind": "list", "step": "t",
                     "columns": [ { "field": "path" } ] } ],
    });
    let (status, body) = post(
        &b.base,
        "/api/recipe/new",
        serde_json::json!({ "repo": REPO, "recipe": doc }),
    )
    .await;
    assert_eq!(status, 201, "{body}");

    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    assert_eq!(row(&cat, "agent:mine")["home"], "server");

    // …and the tree was never written to.
    assert!(
        !b.repo.path().join(".kbc/recipes/agent:mine.toml").exists(),
        "`recipe new` writes to the DAEMON, never into the tree"
    );

    // A document that does not load is refused before anything is stored.
    let (status, body) = post(
        &b.base,
        "/api/recipe/new",
        serde_json::json!({ "recipe": { "slug": "bad", "title": "B", "intent": "nope" } }),
    )
    .await;
    assert_eq!(status, 400, "{body}");
}

// ---------------------------------------------------------------------------
// The catalog runs
// ---------------------------------------------------------------------------

/// Every shipped recipe answers over the fixture. Row CONTENT depends on
/// how far the derived lanes have got; what is pinned here is that each
/// one runs, declares its views, and accounts for every step.
#[tokio::test]
async fn every_shipped_recipe_runs() {
    let b = boot().await;
    let (_, cat) = get(&b.base, &format!("/api/recipe?repo={REPO}")).await;
    for r in cat["recipes"].as_array().unwrap() {
        let slug = r["slug"].as_str().unwrap();
        if r["trust"] != "trusted" {
            continue; // the untrusted repo file has its own test
        }
        let mut url = format!("/api/recipe/{slug}/run?repo={REPO}");
        for p in r["params"].as_array().cloned().unwrap_or_default() {
            if p["required"].as_bool().unwrap_or(false) {
                // TOMORROW, not an ancient date: `complexity-climbers`
                // resolves `since` through `git rev-list -1 --before=`,
                // which legitimately REFUSES when no commit is that old,
                // and this fixture's commits are made now. Tomorrow is a
                // date every `since` grammar in the catalog accepts and
                // that always resolves against this repo.
                url.push_str(&format!(
                    "&p.{}={}",
                    p["name"].as_str().unwrap(),
                    tomorrow()
                ));
            }
        }
        let (status, body) = get(&b.base, &url).await;
        assert_eq!(status, 200, "{slug}: {body}");
        assert_eq!(body["recipe"], slug);
        assert!(
            !body["views"].as_array().unwrap().is_empty(),
            "{slug} declares no view"
        );
        for s in body["steps"].as_array().unwrap() {
            assert!(s.get("census").is_some(), "{slug}/{}: no census", s["id"]);
            assert!(s.get("total").is_some(), "{slug}/{}: no total", s["id"]);
        }
        // Every cell of every view is a STRING rendering of an address
        // field or one of that address's own scalars — never an object.
        for v in body["views"].as_array().unwrap() {
            for rowv in v["rows"].as_array().unwrap() {
                for cell in rowv.as_array().unwrap() {
                    assert!(cell.is_string(), "{slug}: a cell is not a rendered address");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The six repairs
// ---------------------------------------------------------------------------

/// Repair 1 — a file with no readable baseline at `since` is not a
/// climber with a fabricated delta equal to its whole size.
#[tokio::test]
async fn repair_missing_blob_is_not_a_zero_baseline() {
    let b = boot().await;
    // `since` = the FIRST commit's parent does not exist, so use the tip:
    // every file's baseline at HEAD is readable, and a file added after
    // it is not. Add one.
    let head = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(b.repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let before = file_count(&b.base, REPO).await;
    write(
        b.repo.path(),
        "app/models/brand_new.rb",
        "class BrandNew\n  def a\n    1\n  end\nend\n",
    );
    git(b.repo.path(), &["add", "-A"]);
    git(b.repo.path(), &["commit", "-q", "-m", "add a new model"]);
    // `complexity-climbers` walks `list_files`, so the live mirror has to
    // have SEEN the new commit before the assertion means anything.
    wait_for_indexed(&b.base, REPO, before + 1).await;

    let (status, body) = get(
        &b.base,
        &format!("/api/recipes/complexity-climbers?repo={REPO}&since={head}&limit=500"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().unwrap();
    let new = items
        .iter()
        .find(|i| i["path"] == "app/models/brand_new.rb")
        .expect("the new file is REPORTED, never dropped");
    assert!(
        new["score"].is_null(),
        "a file with no baseline has no delta: {new}"
    );
    assert_eq!(new["terms"]["then_state"], "absent");
    assert!(new["terms"]["loc_then"].is_null());
    assert!(
        body["note"]
            .as_str()
            .unwrap()
            .contains("NOT a baseline of zero"),
        "{}",
        body["note"]
    );
    // …and it never outranks a real climb.
    let first_null = items.iter().position(|i| i["score"].is_null());
    let last_scored = items.iter().rposition(|i| !i["score"].is_null());
    if let (Some(fnull), Some(lscored)) = (first_null, last_scored) {
        assert!(
            fnull > lscored,
            "unknown baselines sort after every real climb"
        );
    }
}

/// Repair 6 — the note said "working tree HEAD", which is not a thing.
#[tokio::test]
async fn repair_the_climbers_note_names_the_working_tree() {
    let b = boot().await;
    let (_, body) = get(
        &b.base,
        &format!("/api/recipes/complexity-climbers?repo={REPO}&since=HEAD"),
    )
    .await;
    let note = body["note"].as_str().unwrap();
    assert!(!note.contains("working tree HEAD"), "{note}");
    assert!(note.contains("WORKING TREE"), "{note}");
}

/// Repair 2 — the gate reads the SAME table the row test reads, and an
/// empty result over a missing input says which input.
#[tokio::test]
async fn repair_agent_only_gate_matches_its_row_test() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipes/agent-only-symbols?repo={REPO}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body["items"].as_array().unwrap().is_empty(),
        "this fixture has no commit→session joins"
    );
    assert!(
        !body["inputs_missing"].as_array().unwrap().is_empty(),
        "an empty set over a missing input must SAY so: {body}"
    );
}

/// Repair 3 — the permanently-zero `fail_count` term is gone rather than
/// reported as measured.
#[tokio::test]
async fn repair_failure_tainted_retires_the_fail_count_term() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipes/failure-tainted?repo={REPO}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("fail_count"),
        "the retired term must not appear anywhere on the wire: {raw}"
    );
}

/// Repair 5 — a repo in a language the export rules do not cover gets a
/// NAMED gap, not a confident empty set.
#[tokio::test]
async fn repair_new_public_api_names_the_languages_it_did_not_examine() {
    let b = boot().await;
    let (status, body) = get(
        &b.base,
        &format!("/api/recipes/new-public-api?repo={REPO}&since=2000-01-01"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let note = body["note"].as_str().unwrap();
    assert!(note.contains("ruby"), "the Rails fixture is Ruby: {note}");
    assert!(note.contains("NOT examined"), "{note}");
    assert_eq!(
        body["inputs_missing"].as_array().unwrap(),
        &vec![serde_json::json!("public-export-rules")],
        "nothing was examinable, so this is a missing input: {body}"
    );
}

/// The frozen `recipes/1` catalog is unchanged for `Recipes.tsx`.
#[tokio::test]
async fn the_recipes_1_catalog_is_unchanged() {
    let b = boot().await;
    let (status, body) = get(&b.base, "/api/recipes").await;
    assert_eq!(status, 200);
    assert_eq!(body["schema"], "recipes/1");
    assert_eq!(body["recipes"].as_array().unwrap().len(), 6);
}
