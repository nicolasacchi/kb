//! V3.G1 + V3.G2 — oracle bar test for same-file locals + cross-file
//! import-filtered resolution.
//!
//! Boots a daemon on a fixture repo containing the hand-written oracle
//! files under `tests/fixtures/oracle/`, ingests, and asserts a golden
//! table of (path, line, col) → expected definition + class.
//!
//! Release bar:
//! 1. every golden expecting `exact` resolves to EXACTLY the expected
//!    location with `class="exact"` (≥90% of same-file goldens must be
//!    exact-class — goldens are honest, not padded);
//! 2. ZERO lookups return `class="exact"` with a location that differs
//!    from the golden (asserted over EVERY golden row);
//! 3. unbound identifiers fall through with `class != "exact"`;
//! 4. (V3.G2) ≥70% of cross-file goldens reach class likely-or-better
//!    with the CORRECT target file+line at rank 1;
//! 5. (V3.G2) fuzzy-fallback case asserts class=candidate.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn commit_tree(dir: &Path, files: &[(&str, &str)], message: &str) {
    for (rel, contents) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repo_name: &str, repo_dir: &Path) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        semantic: SemanticSection::default(),
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

async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    // (1) file_count — files table is written at the start of each
    // `index_file` call, so this can trip BEFORE that file's occurrences
    // rows land (last-file race under background initial index).
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    if count >= expected_files {
                        break;
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
    // (2) occurrences readiness canary — poll a same-file locals resolve
    // on shadow.ts (alphabetically last fixture; most exposed to the
    // upsert-before-occurrences race). Wait until precision=locals lands
    // so the bar doesn't flake under load.
    loop {
        if let Ok(resp) = client
            .get(format!("{base}/api/resolve"))
            .query(&[
                ("repo", repo),
                ("path", "shadow.ts"),
                ("line", "7"),
                ("col", "9"),
            ])
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let ready = body["candidates"]
                        .as_array()
                        .and_then(|cs| cs.first())
                        .map(|c| {
                            c["precision"].as_str() == Some("locals")
                                && c["class"].as_str() == Some("exact")
                        })
                        .unwrap_or(false);
                    if ready {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected locals occurrences for shadow.ts:7:9 within the deadline \
             (file_count alone is not enough — background initial index)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One golden row.
///
/// `expect_exact`: when true, the first candidate must be class=exact at
/// `def_line` (and `def_col` when `Some`). When false and not cross-file,
/// the row is an unbound / fall-through case: class must NOT be exact.
///
/// V3.G2 cross-file: set `cross_file = true` and `def_path` to the expected
/// defining file. Rank-1 must be likely-or-better at that path+line (unless
/// `fuzzy_fallback`, which requires class=candidate).
struct Golden {
    path: &'static str,
    /// 1-based line of the reference.
    line: u32,
    /// 0-based col of the reference.
    col: u32,
    /// Expected definition line (1-based). 0 for unbound rows.
    def_line: u32,
    /// Optional expected definition col (0-based). Documented for
    /// humans/debugging; the bar asserts on line (and path) only, since
    /// occurrence enrichment does not surface def col on the wire.
    #[allow(dead_code)]
    def_col: Option<u32>,
    /// When true, require class=exact at def_line (same-file locals).
    expect_exact: bool,
    /// V3.G2 — cross-file golden (import-filtered / fuzzy).
    cross_file: bool,
    /// Expected definition path (defaults to `path` when None).
    def_path: Option<&'static str>,
    /// Fuzzy-fallback case: top name match must be class=candidate.
    fuzzy_fallback: bool,
    note: &'static str,
}

impl Golden {
    fn same_file_exact(
        path: &'static str,
        line: u32,
        col: u32,
        def_line: u32,
        def_col: Option<u32>,
        note: &'static str,
    ) -> Self {
        Self {
            path,
            line,
            col,
            def_line,
            def_col,
            expect_exact: true,
            cross_file: false,
            def_path: None,
            fuzzy_fallback: false,
            note,
        }
    }

    fn unbound(path: &'static str, line: u32, col: u32, note: &'static str) -> Self {
        Self {
            path,
            line,
            col,
            def_line: 0,
            def_col: None,
            expect_exact: false,
            cross_file: false,
            def_path: None,
            fuzzy_fallback: false,
            note,
        }
    }

    fn cross_file_likely(
        path: &'static str,
        line: u32,
        col: u32,
        def_path: &'static str,
        def_line: u32,
        note: &'static str,
    ) -> Self {
        Self {
            path,
            line,
            col,
            def_line,
            def_col: None,
            expect_exact: false,
            cross_file: true,
            def_path: Some(def_path),
            fuzzy_fallback: false,
            note,
        }
    }

    fn fuzzy_candidate(
        path: &'static str,
        line: u32,
        col: u32,
        def_path: &'static str,
        def_line: u32,
        note: &'static str,
    ) -> Self {
        Self {
            path,
            line,
            col,
            def_line,
            def_col: None,
            expect_exact: false,
            cross_file: true,
            def_path: Some(def_path),
            fuzzy_fallback: true,
            note,
        }
    }
}

fn goldens() -> Vec<Golden> {
    vec![
        // --- shadow.rs -------------------------------------------------
        Golden::same_file_exact(
            "shadow.rs",
            10,
            4,
            8,
            Some(7),
            "param shadows module fn value",
        ),
        Golden::same_file_exact(
            "shadow.rs",
            18,
            17,
            16,
            Some(12),
            "inner block let shadows outer x",
        ),
        Golden::same_file_exact(
            "shadow.rs",
            25,
            17,
            23,
            Some(8),
            "loop var shadows param item",
        ),
        Golden::same_file_exact(
            "shadow.rs",
            28,
            13,
            22,
            Some(15),
            "after loop, item is the param",
        ),
        Golden::unbound("shadow.rs", 33, 13, "unbound missing_name"),
        // --- closures.rs -----------------------------------------------
        Golden::same_file_exact(
            "closures.rs",
            7,
            8,
            4,
            Some(8),
            "closure captures outer let",
        ),
        Golden::same_file_exact(
            "closures.rs",
            16,
            8,
            14,
            Some(13),
            "closure param shadows outer n",
        ),
        Golden::same_file_exact(
            "closures.rs",
            27,
            12,
            22,
            Some(8),
            "nested closure captures a",
        ),
        Golden::same_file_exact(
            "closures.rs",
            27,
            16,
            24,
            Some(12),
            "nested closure captures b",
        ),
        Golden::same_file_exact(
            "closures.rs",
            36,
            4,
            39,
            Some(3),
            "same-file fn call helper",
        ),
        // --- shadow.ts -------------------------------------------------
        Golden::same_file_exact(
            "shadow.ts",
            7,
            9,
            5,
            Some(13),
            "ts param shadows module const value",
        ),
        Golden::same_file_exact(
            "shadow.ts",
            15,
            14,
            13,
            Some(8),
            "ts inner block let shadows outer x",
        ),
        Golden::same_file_exact(
            "shadow.ts",
            21,
            9,
            22,
            Some(11),
            "ts function decl hoisted later()",
        ),
        Golden::unbound("shadow.ts", 29, 12, "ts let not visible before declaration"),
        Golden::unbound("shadow.ts", 34, 12, "ts unbound missingName"),
        // --- arrows.tsx ------------------------------------------------
        Golden::same_file_exact("arrows.tsx", 7, 9, 5, Some(20), "tsx arrow param label"),
        Golden::same_file_exact(
            "arrows.tsx",
            12,
            9,
            10,
            Some(23),
            "tsx arrow param shadows module outer",
        ),
        Golden::same_file_exact(
            "arrows.tsx",
            19,
            11,
            17,
            Some(13),
            "tsx nested arrow param n",
        ),
        Golden::same_file_exact(
            "arrows.tsx",
            29,
            11,
            27,
            Some(8),
            "tsx block let shadows outer x",
        ),
        Golden::unbound("arrows.tsx", 34, 9, "tsx unbound missingTsx"),
        // --- scopes.py -------------------------------------------------
        Golden::same_file_exact(
            "scopes.py",
            9,
            11,
            7,
            Some(8),
            "py param shadows module fn value",
        ),
        Golden::same_file_exact(
            "scopes.py",
            14,
            10,
            14,
            Some(16),
            "py comp body x binds to comp target",
        ),
        Golden::same_file_exact(
            "scopes.py",
            16,
            8,
            13,
            Some(4),
            "py after comp, x is outer assignment",
        ),
        Golden::same_file_exact(
            "scopes.py",
            23,
            15,
            21,
            Some(14),
            "py nested param shadows outer param",
        ),
        Golden::unbound("scopes.py", 28, 11, "py unbound missing_py"),
        // --- V3.G2 cross-file ------------------------------------------
        Golden::cross_file_likely(
            "src/uses_a.rs",
            6,
            4,
            "src/lib_a.rs",
            3,
            "rust use crate::lib_a::helper call",
        ),
        Golden::cross_file_likely(
            "consumer.ts",
            5,
            9,
            "util.ts",
            2,
            "ts relative import greet",
        ),
        Golden::cross_file_likely(
            "consumer.py",
            7,
            11,
            "mod_a.py",
            4,
            "py from mod_a import helper",
        ),
        Golden::fuzzy_candidate(
            "src/fuzzy_near.rs",
            5,
            4,
            "other/orphan.rs",
            4,
            "fuzzy fallback: no import path → candidate",
        ),
    ]
}

fn fixture_contents() -> Vec<(&'static str, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    let names = [
        "shadow.rs",
        "closures.rs",
        "shadow.ts",
        "arrows.tsx",
        "scopes.py",
        "src/lib.rs",
        "src/lib_a.rs",
        "src/uses_a.rs",
        "src/fuzzy_near.rs",
        "other/orphan.rs",
        "util.ts",
        "consumer.ts",
        "index.ts",
        "mod_a.py",
        "pkg/__init__.py",
        "consumer.py",
    ];
    names
        .iter()
        .map(|n| {
            let text = std::fs::read_to_string(dir.join(n))
                .unwrap_or_else(|e| panic!("read fixture {n}: {e}"));
            (*n, text)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oracle_resolution_bar() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    let files = fixture_contents();
    let owned: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    commit_tree(dir, &owned, "oracle fixtures");

    let boot = boot("oracle", dir).await;
    wait_for_indexed(&boot.base, "oracle", files.len()).await;

    let client = reqwest::Client::new();
    let goldens = goldens();
    let total = goldens.len();
    // Eventually-consistent index: the background initial index can land a
    // file's derived rows (symbols / occurrences / import edges) AFTER
    // file_count trips, and per-file canaries can't cover every row class
    // (the 2026-08-02 gate flaked on util.ts's defs missing for a cross-file
    // golden). Re-evaluate the WHOLE golden set until the bars pass or the
    // deadline hits. A wrong exact is NEVER retried — the trust bar is not
    // timing-dependent, so it fails on first sight.
    let eval_deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let mut same_file = 0usize;
        let mut exact_hits = 0usize;
        let mut wrong_exact = 0usize;
        let mut cross_file = 0usize;
        let mut cross_file_likely_hits = 0usize;
        let mut fuzzy_ok = 0usize;
        let mut fuzzy_total = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for g in &goldens {
            if g.expect_exact {
                same_file += 1;
            }
            if g.cross_file && !g.fuzzy_fallback {
                cross_file += 1;
            }
            if g.fuzzy_fallback {
                fuzzy_total += 1;
            }
            let resp = client
                .get(format!("{}/api/resolve", boot.base))
                .query(&[
                    ("repo", "oracle"),
                    ("path", g.path),
                    ("line", &g.line.to_string()),
                    ("col", &g.col.to_string()),
                ])
                .send()
                .await
                .expect("resolve request");
            if resp.status() != reqwest::StatusCode::OK {
                failures.push(format!(
                    "{}:{}:{} [{}] HTTP {}",
                    g.path,
                    g.line,
                    g.col,
                    g.note,
                    resp.status()
                ));
                continue;
            }
            let body: serde_json::Value = resp.json().await.unwrap();
            let candidates = body["candidates"].as_array().cloned().unwrap_or_default();
            let expected_def_path = g.def_path.unwrap_or(g.path);

            // Bar (2): no wrong exact — scan EVERY candidate.
            for c in &candidates {
                if c["class"].as_str() == Some("exact") {
                    let cline = c["line"].as_u64().unwrap_or(0) as u32;
                    let cpath = c["path"].as_str().unwrap_or("");
                    // An exact hit is only "correct" when this golden expected
                    // exact at that location (same-file locals).
                    let ok_line = g.expect_exact && cpath == g.path && cline == g.def_line;
                    if !ok_line {
                        wrong_exact += 1;
                        failures.push(format!(
                            "{}:{}:{} [{}] WRONG exact at {}:{} (expected def={}:{})",
                            g.path,
                            g.line,
                            g.col,
                            g.note,
                            cpath,
                            cline,
                            expected_def_path,
                            g.def_line
                        ));
                    }
                }
            }

            if g.expect_exact {
                let first = candidates.first();
                let ok = first
                    .map(|c| {
                        c["class"].as_str() == Some("exact")
                            && c["precision"].as_str() == Some("locals")
                            && c["path"].as_str() == Some(g.path)
                            && c["line"].as_u64() == Some(g.def_line as u64)
                    })
                    .unwrap_or(false);
                if ok {
                    exact_hits += 1;
                } else {
                    let summary = first
                        .map(|c| {
                            format!(
                                "class={:?} precision={:?} path={:?} line={:?}",
                                c["class"], c["precision"], c["path"], c["line"]
                            )
                        })
                        .unwrap_or_else(|| "(no candidates)".into());
                    failures.push(format!(
                        "{}:{}:{} [{}] expected exact locals → L{} ; got {summary}",
                        g.path, g.line, g.col, g.note, g.def_line
                    ));
                }
            } else if g.fuzzy_fallback {
                // Bar (5): fuzzy fallback → class=candidate at the def (or any
                // top hit must be candidate, and the correct def must appear
                // somewhere as candidate).
                let has_cand = candidates.iter().any(|c| {
                    c["class"].as_str() == Some("candidate")
                        && c["path"].as_str() == Some(expected_def_path)
                        && c["line"].as_u64() == Some(g.def_line as u64)
                });
                // Also: no likely/exact for a wrong confident hit required —
                // top name-match class should be candidate when filter emptied.
                let top_is_candidate = candidates
                    .first()
                    .map(|c| c["class"].as_str() == Some("candidate"))
                    .unwrap_or(false);
                if has_cand && top_is_candidate {
                    fuzzy_ok += 1;
                } else {
                    let summary = candidates
                        .first()
                        .map(|c| {
                            format!(
                                "class={:?} path={:?} line={:?}",
                                c["class"], c["path"], c["line"]
                            )
                        })
                        .unwrap_or_else(|| "(no candidates)".into());
                    failures.push(format!(
                        "{}:{}:{} [{}] fuzzy fallback expected candidate at {}:{} ; got {summary}",
                        g.path, g.line, g.col, g.note, expected_def_path, g.def_line
                    ));
                }
            } else if g.cross_file {
                // Bar (4): rank-1 likely-or-better at correct path+line.
                let first = candidates.first();
                let ok = first
                    .map(|c| {
                        let class = c["class"].as_str().unwrap_or("");
                        (class == "likely" || class == "exact")
                            && c["path"].as_str() == Some(expected_def_path)
                            && c["line"].as_u64() == Some(g.def_line as u64)
                    })
                    .unwrap_or(false);
                if ok {
                    cross_file_likely_hits += 1;
                } else {
                    let summary = first
                        .map(|c| {
                            format!(
                                "class={:?} precision={:?} path={:?} line={:?}",
                                c["class"], c["precision"], c["path"], c["line"]
                            )
                        })
                        .unwrap_or_else(|| "(no candidates)".into());
                    failures.push(format!(
                        "{}:{}:{} [{}] expected likely+ at {}:{} ; got {summary}",
                        g.path, g.line, g.col, g.note, expected_def_path, g.def_line
                    ));
                }
            } else {
                // Bar (3): unbound — no exact-class result.
                let any_exact = candidates
                    .iter()
                    .any(|c| c["class"].as_str() == Some("exact"));
                if any_exact {
                    failures.push(format!(
                        "{}:{}:{} [{}] unbound golden returned class=exact",
                        g.path, g.line, g.col, g.note
                    ));
                }
            }
        }

        let exact_rate = if same_file == 0 {
            0.0
        } else {
            exact_hits as f64 / same_file as f64
        };
        let cross_rate = if cross_file == 0 {
            0.0
        } else {
            cross_file_likely_hits as f64 / cross_file as f64
        };

        // Summary line — MUST appear in report.md test evidence.
        println!(
            "oracle_resolution: goldens={total} same_file_exact_expect={same_file} \
         exact_hits={exact_hits} exact_rate={:.1}% wrong_exact={wrong_exact} \
         cross_file={cross_file} cross_likely_hits={cross_file_likely_hits} \
         cross_likely_rate={:.1}% fuzzy_ok={fuzzy_ok}/{fuzzy_total}",
            exact_rate * 100.0,
            cross_rate * 100.0
        );

        let bars_pass = wrong_exact == 0
            && exact_rate >= 0.90
            && cross_rate >= 0.70
            && (fuzzy_total == 0 || fuzzy_ok == fuzzy_total)
            && failures.is_empty();
        if wrong_exact > 0 || bars_pass || Instant::now() >= eval_deadline {
            boot.task.abort();

            assert!(
                wrong_exact == 0,
                "RELEASE BAR: zero wrong exact required; failures:\n{}",
                failures.join("\n")
            );
            assert!(
                exact_rate >= 0.90,
                "exact-class hit rate {:.1}% < 90% ({exact_hits}/{same_file}); failures:\n{}",
                exact_rate * 100.0,
                failures.join("\n")
            );
            assert!(
            cross_rate >= 0.70,
            "cross-file likely+ rate {:.1}% < 70% ({cross_file_likely_hits}/{cross_file}); failures:\n{}",
            cross_rate * 100.0,
            failures.join("\n")
        );
            assert!(
                fuzzy_total == 0 || fuzzy_ok == fuzzy_total,
                "fuzzy-fallback goldens failed ({fuzzy_ok}/{fuzzy_total}); failures:\n{}",
                failures.join("\n")
            );
            assert!(
                failures.is_empty(),
                "oracle failures:\n{}",
                failures.join("\n")
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
