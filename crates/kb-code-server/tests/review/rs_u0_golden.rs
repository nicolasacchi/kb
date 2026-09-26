//! RS-U0 (c) — the golden harness's committed fixture.
//!
//! Boots a REAL kb-code-server daemon (`serve_on_random_port_with_paths`,
//! the same boot pattern `review_comments.rs`/`review_findings.rs` already
//! use — see those files' own module docs for why each e2e file here
//! duplicates its small helper set rather than sharing one) against a
//! synthetic `acme/widgets` git fixture, drives it through the HTTP API to
//! create ONE review with two patchsets (a plain edit, then a further edit
//! plus a pure rename, exercising `old_path`/rename blob-id resolution), a
//! review-scoped comment (anchored while the working tree still matches
//! ps1's content, so carry-forward is genuinely exercised when ps2 is
//! captured — same technique `review_comments.rs`'s own carry-forward
//! tests use), a manual finding with a disposition, and a verdict — then
//! shells out to `scripts/review-store/review_snapshot.py` (RS-U0 (a), the
//! SAME tool the orchestrator runs against a real volume copy for
//! BUILD-BRIEF §3 gate 1) with `--normalize-fixture` and compares the
//! result against the committed
//! `scripts/review-store/fixtures/golden-review-snapshot.json`.
//!
//! Commit dates are PINNED (`GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE`) so
//! every blob/tree/commit sha this fixture produces is bit-for-bit
//! deterministic across regenerations — only the daemon's own wall-clock
//! fields (`captured_at`, comment/finding timestamps, the verdict `at`)
//! vary run to run, which is exactly what `--normalize-fixture` blanks
//! (see that script's own module doc for the full, itemized normalization
//! list).
//!
//! **Regenerate** after an intentional wire-shape change: run this test
//! once with `RS_U0_UPDATE_GOLDEN=1` set (writes the fixture instead of
//! asserting), inspect the diff, and commit it:
//! ```text
//! flock /tmp/kbc7-cargo.lock env CARGO_BUILD_JOBS=2 RS_U0_UPDATE_GOLDEN=1 \
//!   nice -n 20 ionice -c 3 cargo test -p kb-code-server --profile fast \
//!   --test review rs_u0_golden_fixture_matches_committed_snapshot -- --nocapture
//! git diff -- scripts/review-store/fixtures/golden-review-snapshot.json
//! ```
//! On a mismatch with the flag unset, the assertion panics with BOTH sides
//! written to `$TMPDIR` so a reviewer can `diff` them directly (the JSON is
//! too large to usefully inline in a panic message, unlike
//! `frames.rs`/`syntax.rs`'s smaller goldens).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::git;

/// `common::git` has no env-var hook (every OTHER caller in this crate's
/// tests is fine with the ambient clock) — this is the one fixture in the
/// crate that needs a pinned commit date, so it gets its own tiny wrapper
/// rather than widening the shared helper's signature for one caller.
fn pinned_commit(dir: &Path, msg: &str, iso_date: &str) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", iso_date)
        .env("GIT_COMMITTER_DATE", iso_date)
        .output()
        .expect("git commit runs");
    assert!(
        out.status.success(),
        "git commit -m {msg:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn workspace_root() -> PathBuf {
    // crates/kb-code-server -> workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root exists")
}

/// A small, reproducible acme/widgets scenario (BUILDER-RULES: synthetic
/// org/repo names only — no real host/org names in committed fixtures).
/// Pinned author/committer dates (see the module doc) make every sha this
/// produces deterministic across regenerations.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    std::fs::write(
        dir.join("README.md"),
        "# widgets\n\nA spinnable primitive.\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib/widget.rb"),
        "class Widget\n  def spin\n    true\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    pinned_commit(dir, "base: initial widget", "2026-01-01T00:00:00Z");

    git(dir, &["checkout", "-q", "-b", "feature/spin-faster"]);
    std::fs::write(
        dir.join("lib/widget.rb"),
        "class Widget\n  def spin\n    true\n  end\n\n  def spin_faster\n    spin\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    pinned_commit(dir, "feature: add spin_faster", "2026-01-01T00:01:00Z");

    tmp
}

/// The SECOND patchset's content: two leading comment lines (shifts every
/// following line down by two, so `def spin` moves from line 2 to line
/// 5 — a real carry-forward exercise for the comment anchored at ps1) plus
/// a pure rename of README.md -> docs/README.md (exercises `old_path` and
/// the old-blob lookup for a renamed file).
fn advance_to_ps2(dir: &Path) {
    std::fs::write(
        dir.join("lib/widget.rb"),
        "# Widget: a spinnable primitive.\n# See docs/README.md for usage.\n\nclass Widget\n  def spin\n    true\n  end\n\n  def spin_faster\n    spin\n  end\nend\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    git(dir, &["mv", "README.md", "docs/README.md"]);
    git(dir, &["add", "-A"]);
    pinned_commit(
        dir,
        "feature: doc header + move README under docs/",
        "2026-01-01T00:02:00Z",
    );
}

async fn boot(name: &str, path: &Path) -> (tempfile::TempDir, String) {
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
        review: ReviewSection::default(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs_u0_golden_fixture_matches_committed_snapshot() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot("acme-widgets", dir).await;
    let client = reqwest::Client::new();

    // ps1: create_review mints it from the branch's current tip.
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "acme-widgets",
            "head_ref": "feature/spin-faster",
            "base_ref": "main",
            "title": "widgets: spin faster",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let review: serde_json::Value = resp.json().await.unwrap();
    let review_id = review["id"].as_i64().unwrap();

    // A review-scoped comment, anchored while the working tree still IS
    // ps1's content (the same "anchor now, mutate the tree later" order
    // `review_comments.rs`'s own carry-forward tests use) — so viewing it
    // at ps2 below is a genuine carry-forward resolve, not a trivial exact
    // match by construction.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "acme-widgets",
            "path": "lib/widget.rb",
            "line": 2,
            "body": "Should this call be memoized?",
            "review_id": review_id,
            "side": "new",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );

    // ps2: a further edit (shifts the commented line) + a pure rename.
    advance_to_ps2(dir);
    let resp = client
        .post(format!("{base}/api/reviews/{review_id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        snap["ps_number"].as_i64(),
        Some(2),
        "expected a second patchset"
    );

    // `list_review_annotations`'s SQL orders `created_at ASC, id ASC`
    // (store/annotations.rs) — `created_at` is `now_unix()`, SECOND
    // granularity, and `id` is a RANDOM `ann_<12-hex>` string
    // (annotations.rs::new_annotation_id). Two annotations created within
    // the same wall-clock second therefore tie-break on a value that is
    // random PER FIXTURE REGENERATION, so the comment group's order (and
    // this tool's first-occurrence `<ID:000N>` normalization downstream)
    // would flip between runs with no sleep here — caught by an actual
    // regeneration mismatch while building this harness. 1100ms guarantees
    // crossing a whole second regardless of where in the current second
    // the comment above landed.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // A manual finding, anchored against ps2's (post-shift) content.
    let resp = client
        .post(format!("{base}/api/reviews/{review_id}/findings"))
        .json(&serde_json::json!({
            "severity": "concern",
            "category": "Correctness",
            "location": {"path": "lib/widget.rb", "kind": "single", "lines": [5]},
            "title": "spin_faster has no test coverage",
            "rationale": "Fixture note: spin_faster calls spin but nothing asserts the result.",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let finding: serde_json::Value = resp.json().await.unwrap();
    let slug = finding["slug"].as_str().unwrap().to_string();

    let resp = client
        .put(format!(
            "{base}/api/reviews/{review_id}/findings/{slug}/disposition"
        ))
        .json(&serde_json::json!({"disposition": "agree", "note": "fixture: will add a spec"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    // Verdict, against the latest patchset (ps2).
    let resp = client
        .put(format!("{base}/api/reviews/{review_id}/verdict"))
        .json(&serde_json::json!({"state": "approve", "note": "fixture: looks good"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    // Run the SAME snapshot tool RS-U0 (a) delivers, normalized for a
    // reproducible fixture (see review_snapshot.py's own module doc for
    // the itemized normalization list).
    let root = workspace_root();
    let script = root.join("scripts/review-store/review_snapshot.py");
    assert!(script.is_file(), "missing {}", script.display());
    let out_tmp = tempfile::NamedTempFile::new().unwrap();
    let status = Command::new("python3")
        .arg(&script)
        .args([
            "snapshot",
            "--base",
            &base,
            "--repo",
            "acme-widgets",
            "--normalize-fixture",
            "-o",
        ])
        .arg(out_tmp.path())
        .status()
        .expect("run review_snapshot.py");
    assert!(status.success(), "review_snapshot.py exited {status}");
    let actual = std::fs::read_to_string(out_tmp.path()).unwrap();

    let golden_path = root.join("scripts/review-store/fixtures/golden-review-snapshot.json");

    if std::env::var_os("RS_U0_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden_path, &actual).expect("write golden");
        eprintln!("RS_U0_UPDATE_GOLDEN=1: wrote {}", golden_path.display());
        return;
    }

    let golden = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", golden_path.display()));
    if actual.trim_end() != golden.trim_end() {
        let actual_tmp = std::env::temp_dir().join("rs-u0-actual-snapshot.json");
        std::fs::write(&actual_tmp, &actual).ok();
        panic!(
            "RS-U0 golden fixture mismatch.\n\
             Committed: {}\n\
             Actual written to: {}\n\
             If this is an intentional wire-shape change, regenerate with:\n\
             RS_U0_UPDATE_GOLDEN=1 cargo test -p kb-code-server --profile fast \
             --test review rs_u0_golden_fixture_matches_committed_snapshot -- --nocapture\n\
             then inspect `git diff` on the fixture before committing it.",
            golden_path.display(),
            actual_tmp.display()
        );
    }
}
