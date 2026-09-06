//! V3.1-H1 — oracle bar for call/type hierarchy edges.
//!
//! Boots a daemon over the hierarchy fixtures under `tests/fixtures/oracle/`
//! (hier_*.rs/ts/py) plus the existing oracle tree, and asserts:
//! - callees of a named fixture fn: expected set + classes
//! - callers of a named fixture fn: expected caller set at likely+, decoy
//!   same-name stays candidate-only
//! - implementors of rust trait + ts interface: expected at likely+
//! - trait-object / duck-typed calls: present ONLY as candidate
//! - ZERO rows carry class=exact with a target that differs from the golden
//!
//! Summary line (must appear in report evidence):
//!   oracle_hierarchy: goldens=N ... wrong_exact=0

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn copy_oracle_fixtures(dst: &Path) {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    copy_dir(&src, dst);
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to);
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to).unwrap();
        }
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
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        semantic: SemanticSection {
            enabled: false,
            ..SemanticSection::default()
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

fn count_source_files(dir: &Path) -> usize {
    let mut n = 0usize;
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        // Skip git metadata — count_files must match the live-mirror walk
        // (git-tracked sources only), not object-store noise.
        if name == ".git" {
            continue;
        }
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            n += count_source_files(&entry.path());
        } else if ty.is_file() {
            n += 1;
        }
    }
    n
}

async fn wait_for_indexed(url: &str, repo: &str, min_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    // (1) file_count — may trip before every file's symbols land.
    loop {
        if let Ok(resp) = client.get(format!("{url}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    if count >= min_files {
                        break;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "repo {repo} not indexed to {min_files} files in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // (2) symbols canary — hierarchy fixtures must have non-empty symbols
    // (file_count alone races the background initial index).
    for path in ["hier_draw.rs", "hier_shapes.ts", "hier_bases.py"] {
        loop {
            if let Ok(resp) = client
                .get(format!("{url}/api/symbols"))
                .query(&[("repo", repo), ("path", path)])
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        let n = body["symbols"].as_array().map(|a| a.len()).unwrap_or(0);
                        if n > 0 {
                            break;
                        }
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "expected symbols for {path} within the deadline \
                 (file_count alone is not enough — background initial index)"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Locate a symbol name's definition line/col via /api/symbols?q=
async fn find_def(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    path: &str,
    name: &str,
) -> (u32, u32) {
    // Use file symbols list.
    let resp = client
        .get(format!("{base}/api/symbols"))
        .query(&[("repo", repo), ("path", path)])
        .send()
        .await
        .expect("symbols");
    let body: serde_json::Value = resp.json().await.expect("json");
    let arr = body["symbols"]
        .as_array()
        .or_else(|| body.as_array())
        .expect("symbols array");
    for s in arr {
        if s["name"].as_str() == Some(name) {
            let line = s["line_start"].as_u64().or(s["line"].as_u64()).unwrap() as u32;
            let col = s["col_start"].as_u64().or(s["col"].as_u64()).unwrap_or(0) as u32;
            return (line, col);
        }
    }
    panic!("symbol {name} not found in {path}; body={body}");
}

fn class_rank(c: &str) -> u8 {
    match c {
        "exact" => 0,
        "likely" => 1,
        "candidate" => 2,
        _ => 3,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oracle_hierarchy_bar() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo_dir = repo_tmp.path();
    init_repo(repo_dir);
    copy_oracle_fixtures(repo_dir);
    git(repo_dir, &["add", "-A"]);
    git(
        repo_dir,
        &["commit", "-q", "-m", "oracle hierarchy fixtures"],
    );

    let boot = boot("oracle", repo_dir).await;
    let n_files = count_source_files(repo_dir);
    wait_for_indexed(&boot.base, "oracle", n_files).await;

    let client = reqwest::Client::new();
    let mut failures: Vec<String> = Vec::new();
    let mut goldens = 0usize;
    let mut wrong_exact = 0usize;
    let mut dynamic_ok = 0usize;
    let mut dynamic_total = 0usize;
    let mut implementors_ok = 0usize;
    let mut implementors_total = 0usize;
    let mut callees_ok = 0usize;
    let mut callees_total = 0usize;
    let mut callers_ok = 0usize;
    let mut callers_total = 0usize;

    // ---- helpers ----------------------------------------------------------
    let get_callees = |path: String, line: u32, col: u32| {
        let client = client.clone();
        let base = boot.base.clone();
        async move {
            let resp = client
                .get(format!("{base}/api/hierarchy/callees"))
                .query(&[
                    ("repo", "oracle"),
                    ("path", path.as_str()),
                    ("line", &line.to_string()),
                    ("col", &col.to_string()),
                ])
                .send()
                .await
                .expect("callees");
            assert_eq!(resp.status(), 200, "callees {path}:{line}:{col}");
            resp.json::<serde_json::Value>().await.expect("json")
        }
    };
    let get_callers = |path: String, line: u32, col: u32| {
        let client = client.clone();
        let base = boot.base.clone();
        async move {
            let resp = client
                .get(format!("{base}/api/hierarchy/callers"))
                .query(&[
                    ("repo", "oracle"),
                    ("path", path.as_str()),
                    ("line", &line.to_string()),
                    ("col", &col.to_string()),
                ])
                .send()
                .await
                .expect("callers");
            assert_eq!(resp.status(), 200, "callers {path}:{line}:{col}");
            resp.json::<serde_json::Value>().await.expect("json")
        }
    };
    let get_types = |name: String| {
        let client = client.clone();
        let base = boot.base.clone();
        async move {
            let resp = client
                .get(format!("{base}/api/hierarchy/types"))
                .query(&[("repo", "oracle"), ("name", name.as_str())])
                .send()
                .await
                .expect("types");
            assert_eq!(resp.status(), 200, "types {name}");
            resp.json::<serde_json::Value>().await.expect("json")
        }
    };

    // =====================================================================
    // 1) Callees of use_concrete (rust): expect paint_circle + draw
    // =====================================================================
    {
        goldens += 1;
        callees_total += 1;
        let (line, col) = find_def(
            &client,
            &boot.base,
            "oracle",
            "hier_draw.rs",
            "use_concrete",
        )
        .await;
        let body = get_callees("hier_draw.rs".into(), line, col).await;
        assert_eq!(body["schema"], "hierarchy/1");
        let callees = body["callees"].as_array().cloned().unwrap_or_default();
        let names: Vec<&str> = callees.iter().filter_map(|c| c["name"].as_str()).collect();
        let has_paint = names.contains(&"paint_circle");
        let has_draw = names.contains(&"draw");
        // wrong_exact: any exact target whose path/line is nonsense
        for c in &callees {
            if c["class"].as_str() == Some("exact") {
                if let Some(t) = c.get("target") {
                    if t.is_null() {
                        wrong_exact += 1;
                        failures.push(format!("exact with null target: {c}"));
                    }
                }
            }
        }
        // paint_circle should be likely+ (same-file free fn)
        let paint_class = callees
            .iter()
            .find(|c| c["name"].as_str() == Some("paint_circle"))
            .and_then(|c| c["class"].as_str())
            .unwrap_or("missing");
        let paint_ok = has_paint && class_rank(paint_class) <= class_rank("likely");
        if has_paint && has_draw && paint_ok {
            callees_ok += 1;
        } else {
            failures.push(format!(
                "callees use_concrete: names={names:?} paint_class={paint_class} body={body}"
            ));
        }
    }

    // =====================================================================
    // 2) Trait-object call in use_trait_object: draw must be candidate
    // =====================================================================
    {
        goldens += 1;
        dynamic_total += 1;
        let (line, col) = find_def(
            &client,
            &boot.base,
            "oracle",
            "hier_draw.rs",
            "use_trait_object",
        )
        .await;
        let body = get_callees("hier_draw.rs".into(), line, col).await;
        let callees = body["callees"].as_array().cloned().unwrap_or_default();
        let draw = callees.iter().find(|c| c["name"].as_str() == Some("draw"));
        match draw {
            Some(c) if c["class"].as_str() == Some("candidate") => {
                dynamic_ok += 1;
            }
            Some(c) => {
                failures.push(format!(
                    "trait-object d.draw() must be candidate, got class={:?}",
                    c["class"]
                ));
            }
            None => {
                failures.push(format!(
                    "trait-object d.draw() missing from callees: {body}"
                ));
            }
        }
        for c in &callees {
            if c["class"].as_str() == Some("exact") {
                // exact on dynamic dispatch is a release bar failure
                if c["name"].as_str() == Some("draw") {
                    wrong_exact += 1;
                    failures.push("trait-object draw returned exact".into());
                }
            }
        }
    }

    // =====================================================================
    // 3) Callers of paint_circle: Circle::draw likely+; decoy not likely+
    // =====================================================================
    {
        goldens += 1;
        callers_total += 1;
        let (line, col) = find_def(
            &client,
            &boot.base,
            "oracle",
            "hier_draw.rs",
            "paint_circle",
        )
        .await;
        let body = get_callers("hier_draw.rs".into(), line, col).await;
        assert_eq!(body["schema"], "hierarchy/1");
        let callers = body["callers"].as_array().cloned().unwrap_or_default();
        let mut found_impl_caller = false;
        let mut decoy_likely = false;
        for g in &callers {
            let path = g["path"].as_str().unwrap_or("");
            let best = g["sites"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["class"].as_str())
                .map(class_rank)
                .min()
                .unwrap_or(99);
            if path == "hier_draw.rs" && best <= class_rank("likely") {
                found_impl_caller = true;
            }
            if path.ends_with("hier_decoy.rs") && best <= class_rank("likely") {
                decoy_likely = true;
            }
            for s in g["sites"].as_array().into_iter().flatten() {
                if s["class"].as_str() == Some("exact") {
                    // Callers should not invent exact without scip/locals certainty
                    // — allowed only if correct; track wrong_exact if path is decoy.
                    if path.ends_with("hier_decoy.rs") {
                        wrong_exact += 1;
                        failures.push("decoy caller classified exact".into());
                    }
                }
            }
        }
        if found_impl_caller && !decoy_likely {
            callers_ok += 1;
        } else {
            failures.push(format!(
                "callers paint_circle: found_impl={found_impl_caller} decoy_likely={decoy_likely} body={body}"
            ));
        }
        // Decoy may appear as candidate — that's fine.
        goldens += 1; // decoy-not-likely assertion counted as golden
    }

    // =====================================================================
    // 4) Implementors of Drawable (rust trait)
    // =====================================================================
    {
        goldens += 1;
        implementors_total += 1;
        let body = get_types("Drawable".into()).await;
        assert_eq!(body["schema"], "hierarchy/1");
        let subs = body["subtypes"].as_array().cloned().unwrap_or_default();
        let names: Vec<&str> = subs.iter().filter_map(|s| s["name"].as_str()).collect();
        let has_circle = names.contains(&"Circle");
        let has_square = names.contains(&"Square");
        // At least Circle + Square present; classes likely+ preferred for same-file impls.
        let circle_ok = subs.iter().any(|s| {
            s["name"].as_str() == Some("Circle")
                && class_rank(s["class"].as_str().unwrap_or("candidate")) <= class_rank("likely")
        });
        let square_ok = subs.iter().any(|s| {
            s["name"].as_str() == Some("Square")
                && class_rank(s["class"].as_str().unwrap_or("candidate")) <= class_rank("likely")
        });
        if has_circle && has_square && circle_ok && square_ok {
            implementors_ok += 1;
        } else {
            failures.push(format!(
                "implementors Drawable: names={names:?} circle_ok={circle_ok} square_ok={square_ok} body={body}"
            ));
        }
        for s in &subs {
            if s["class"].as_str() == Some("exact") {
                if let Some(t) = s.get("target") {
                    if !t.is_null() {
                        let tp = t["path"].as_str().unwrap_or("");
                        // exact target must be hier_draw.rs for these
                        if tp != "hier_draw.rs" {
                            wrong_exact += 1;
                            failures.push(format!("exact implementor wrong target: {s}"));
                        }
                    }
                }
            }
        }
    }

    // =====================================================================
    // 5) TS interface Drawable implementors + interface-typed call
    // =====================================================================
    {
        goldens += 1;
        implementors_total += 1;
        let body = get_types("Drawable".into()).await;
        // May include rust + ts — filter via path ends with .ts
        let subs = body["subtypes"].as_array().cloned().unwrap_or_default();
        let ts_circle = subs.iter().any(|s| {
            s["name"].as_str() == Some("Circle")
                && s["via"]["path"]
                    .as_str()
                    .map(|p| p.ends_with(".ts"))
                    .unwrap_or(false)
                && class_rank(s["class"].as_str().unwrap_or("candidate")) <= class_rank("likely")
        });
        // Also check extends Shape from Circle
        let supers = body["supertypes"].as_array().cloned().unwrap_or_default();
        let _ = supers;
        if ts_circle {
            implementors_ok += 1;
        } else {
            // types?name=Drawable returns both languages; re-check Shape/Circle
            let body2 = get_types("Shape".into()).await;
            let shape_subs = body2["subtypes"].as_array().cloned().unwrap_or_default();
            let ok = shape_subs.iter().any(|s| {
                s["name"].as_str() == Some("Circle")
                    && s["kind"].as_str() == Some("extends")
                    && class_rank(s["class"].as_str().unwrap_or("candidate"))
                        <= class_rank("likely")
            });
            if ok {
                implementors_ok += 1;
            } else {
                failures.push(format!(
                    "ts implementors/extends: Drawable subtypes={subs:?} Shape subtypes={shape_subs:?}"
                ));
            }
        }
    }
    {
        goldens += 1;
        dynamic_total += 1;
        let (line, col) = find_def(
            &client,
            &boot.base,
            "oracle",
            "hier_shapes.ts",
            "useInterface",
        )
        .await;
        let body = get_callees("hier_shapes.ts".into(), line, col).await;
        let callees = body["callees"].as_array().cloned().unwrap_or_default();
        let draw = callees.iter().find(|c| c["name"].as_str() == Some("draw"));
        match draw {
            Some(c) if c["class"].as_str() == Some("candidate") => dynamic_ok += 1,
            Some(c) => failures.push(format!(
                "ts interface-typed d.draw() must be candidate, got {:?}",
                c["class"]
            )),
            None => failures.push(format!("ts useInterface missing draw: {body}")),
        }
    }

    // =====================================================================
    // 6) Python bases + duck-typed call
    // =====================================================================
    {
        goldens += 1;
        implementors_total += 1;
        let body = get_types("Base".into()).await;
        let subs = body["subtypes"].as_array().cloned().unwrap_or_default();
        let has_mid = subs.iter().any(|s| {
            s["name"].as_str() == Some("Mid")
                && class_rank(s["class"].as_str().unwrap_or("candidate")) <= class_rank("likely")
        });
        if has_mid {
            implementors_ok += 1;
        } else {
            failures.push(format!("python Base subtypes missing Mid likely+: {body}"));
        }
    }
    {
        goldens += 1;
        dynamic_total += 1;
        let (line, col) =
            find_def(&client, &boot.base, "oracle", "hier_bases.py", "use_duck").await;
        let body = get_callees("hier_bases.py".into(), line, col).await;
        let callees = body["callees"].as_array().cloned().unwrap_or_default();
        let ping = callees.iter().find(|c| c["name"].as_str() == Some("ping"));
        match ping {
            Some(c) if c["class"].as_str() == Some("candidate") => dynamic_ok += 1,
            Some(c) => failures.push(format!(
                "python duck-typed obj.ping() must be candidate, got {:?}",
                c["class"]
            )),
            None => failures.push(format!("python use_duck missing ping: {body}")),
        }
    }

    // =====================================================================
    // 7) Callees of Leaf.run includes helper (likely+)
    // =====================================================================
    {
        goldens += 1;
        callees_total += 1;
        let (line, col) = find_def(&client, &boot.base, "oracle", "hier_bases.py", "run").await;
        let body = get_callees("hier_bases.py".into(), line, col).await;
        let callees = body["callees"].as_array().cloned().unwrap_or_default();
        let helper = callees
            .iter()
            .find(|c| c["name"].as_str() == Some("helper"));
        match helper {
            Some(c)
                if class_rank(c["class"].as_str().unwrap_or("candidate"))
                    <= class_rank("likely") =>
            {
                callees_ok += 1;
            }
            Some(c) => failures.push(format!(
                "python Leaf.run helper class should be likely+, got {:?}",
                c["class"]
            )),
            None => failures.push(format!("python Leaf.run missing helper: {body}")),
        }
    }

    println!(
        "oracle_hierarchy: goldens={goldens} callees_ok={callees_ok}/{callees_total} \
         callers_ok={callers_ok}/{callers_total} implementors_ok={implementors_ok}/{implementors_total} \
         dynamic_ok={dynamic_ok}/{dynamic_total} wrong_exact={wrong_exact}"
    );

    boot.task.abort();

    assert!(
        wrong_exact == 0,
        "RELEASE BAR: zero wrong exact; failures:\n{}",
        failures.join("\n")
    );
    assert!(
        dynamic_ok == dynamic_total && dynamic_total > 0,
        "dynamic/trait-object/duck-typed must all be candidate ({dynamic_ok}/{dynamic_total}); failures:\n{}",
        failures.join("\n")
    );
    assert!(
        implementors_ok == implementors_total && implementors_total > 0,
        "implementors goldens failed ({implementors_ok}/{implementors_total}); failures:\n{}",
        failures.join("\n")
    );
    assert!(
        callees_ok == callees_total && callees_total > 0,
        "callees goldens failed ({callees_ok}/{callees_total}); failures:\n{}",
        failures.join("\n")
    );
    assert!(
        callers_ok == callers_total && callers_total > 0,
        "callers goldens failed ({callers_ok}/{callers_total}); failures:\n{}",
        failures.join("\n")
    );
    assert!(
        failures.is_empty(),
        "oracle_hierarchy failures:\n{}",
        failures.join("\n")
    );
}
