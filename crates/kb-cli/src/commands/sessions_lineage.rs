//! `kb sessions backfill-tree` — host-side tree walk into
//! `POST /api/sessions/backfill-tree` (V0047).
//!
//! Walks `--sessions-dir` (default `~/.omp/agent/sessions`) recursively
//! for `*.jsonl`, reads each file's `session` header (`id`,
//! `parentSession`, `cwd`), folds `(parent, root)` with the Rust
//! `kb_core::sessions::lineage::fold_lineage`, computes `project_slug`
//! via `current_repo_slug_in(cwd)` only when the header cwd is absolute
//! and a directory, and posts throttled batches (default 200, 50 ms
//! sleep). Default is DRY-RUN (`?apply=false`); `--apply` writes. There
//! is no offline path: with no daemon the command refuses with a clear
//! error. Unresolved parents stay NULL.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::http;

/// One folded item to post: `(session_id, parent, root, project_slug)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeItem {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub project_slug: Option<String>,
}

/// The route + query for one run (PURE: pinned by a unit test).
pub(crate) fn backfill_tree_path(apply: bool) -> String {
    format!("/api/sessions/backfill-tree?apply={apply}")
}

/// Read one `session` header line from a JSONL file: `(id, parentSession,
/// cwd)` — first `type:"session"` record, head-capped like the hook.
fn read_header(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(bytes) = std::fs::read(path) else {
        return (None, None, None);
    };
    let head = &bytes[..bytes.len().min(262144)];
    let text = String::from_utf8_lossy(head);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|x| x.as_str()) == Some("session") {
            let id = v.get("id").and_then(|x| x.as_str()).map(str::to_string);
            let parent = v
                .get("parentSession")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            return (id, parent, cwd);
        }
    }
    (None, None, None)
}

/// Walk `sessions_dir` and fold every `*.jsonl` file into [`TreeItem`]s.
/// Reads the parent/sibling header for `parent_header_id` so the Oct
/// `parentSession` path and the Sep sibling-directory fallback both
/// resolve through the same `fold_lineage`. Sorting is deterministic
/// (path order) so the posted batches are stable.
pub fn collect_tree_items(sessions_dir: &Path) -> Vec<TreeItem> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_jsonl(sessions_dir, &mut files);
    files.sort();

    // Two passes: first every header, so a parent file's id is known
    // before any child folds.
    let mut headers: HashMap<PathBuf, (Option<String>, Option<String>, Option<String>)> =
        HashMap::new();
    for f in &files {
        headers.insert(f.clone(), read_header(f));
    }
    let by_path: HashMap<String, (Option<String>, Option<String>, Option<String>)> = headers
        .iter()
        .map(|(p, h)| (p.to_string_lossy().into_owned(), h.clone()))
        .collect();

    let mut items = Vec::new();
    for f in &files {
        let (id, parent_session, cwd) = headers.get(f).cloned().unwrap_or((None, None, None));
        let header_id = id.as_deref().unwrap_or("");
        // Resolve the parent file's header id: the file named by the
        // header's `parentSession` if present, else the sibling
        // `<dir>/<Name>.jsonl` for a nested `<Name>/<Name>.<Child>.jsonl`.
        let parent_header_id: Option<String> = if let Some(ps) = parent_session.as_deref() {
            by_path.get(ps).and_then(|h| h.0.clone()).or_else(|| {
                let p = PathBuf::from(ps);
                read_header(&p).0
            })
        } else {
            nested_sibling_id(f)
        };
        let folded = kb_core::sessions::lineage::fold_lineage(
            &f.to_string_lossy(),
            header_id,
            parent_session.as_deref(),
            parent_header_id.as_deref(),
        );
        if header_id.is_empty() {
            continue;
        }
        let project_slug = cwd
            .as_deref()
            .filter(|c| Path::new(c).is_absolute() && Path::new(c).is_dir())
            .map(|c| super::memory::current_repo_slug_in(Path::new(c)))
            .filter(|s| !s.is_empty());
        items.push(TreeItem {
            session_id: header_id.to_string(),
            parent_session_id: folded.parent,
            root_session_id: folded.root,
            project_slug,
        });
    }
    items
}

/// The sibling `<dir>/<Name>.jsonl` header id for a nested
/// `<dir>/<Name>/<Name>.<Child>.jsonl` file, if it exists.
fn nested_sibling_id(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_string_lossy();
    let dot = name.find('.')?;
    let stem = &name[..dot];
    if stem.is_empty() {
        return None;
    }
    let dir = file.parent()?;
    let sibling = dir.join(format!("{stem}.jsonl"));
    if sibling == *file {
        return None;
    }
    read_header(&sibling).0
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn items_to_json(items: &[TreeItem]) -> Value {
    json!({
        "items": items.iter().map(|it| {
            let mut o = serde_json::Map::new();
            o.insert("session_id".into(), json!(it.session_id));
            if let Some(p) = &it.parent_session_id {
                o.insert("parent_session_id".into(), json!(p));
            }
            if let Some(r) = &it.root_session_id {
                o.insert("root_session_id".into(), json!(r));
            }
            if let Some(s) = &it.project_slug {
                o.insert("project_slug".into(), json!(s));
            }
            Value::Object(o)
        }).collect::<Vec<_>>()
    })
}

/// `kb sessions backfill-tree [--apply] [--sessions-dir DIR] [--batch N]`
/// — dry-run by default; prints `would_change=<n> changed=<n>
///unmatched=<n>`. Refuses with a clear error when no daemon answers.
pub async fn backfill_tree(
    apply: bool,
    sessions_dir: Option<&PathBuf>,
    batch: usize,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let dir = sessions_dir
        .cloned()
        .unwrap_or_else(|| default_sessions_dir());
    let items = collect_tree_items(&dir);
    let Some(url) = http::detect_daemon(daemon, bearer).await else {
        bail!(
            "backfill-tree: no kb daemon answers — start the daemon first \
             (there is no offline path; the daemon's storage actor stays the \
             single writer of index.db)"
        );
    };
    let client = http::client_with_timeout_and_bearer(120, bearer)?;
    let batch = batch.max(1);
    let mut would_change = 0u64;
    let mut changed = 0u64;
    let mut unmatched = 0u64;
    for chunk in items.chunks(batch) {
        let body = items_to_json(chunk);
        let resp = client
            .post(format!("{url}{}", backfill_tree_path(apply)))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}/api/sessions/backfill-tree"))?
            .error_for_status()?;
        let v: Value = resp.json().await?;
        let num = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        would_change += num("would_change");
        changed += num("changed");
        unmatched += num("unmatched");
        if let Some(d) = v.get("degraded").and_then(|d| d.as_array()) {
            for lane in d {
                eprintln!(
                    "warning: backfill failed for kb {} ({})",
                    lane["kb"].as_str().unwrap_or("?"),
                    lane["error_class"].as_str().unwrap_or("other")
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    println!("would_change={would_change} changed={changed} unmatched={unmatched}");
    Ok(())
}

fn default_sessions_dir() -> PathBuf {
    if let Some(home) = std::env::home_dir() {
        home.join(".omp/agent/sessions")
    } else {
        PathBuf::from("~/.omp/agent/sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_tree_path_is_dry_run_by_default() {
        assert_eq!(
            backfill_tree_path(false),
            "/api/sessions/backfill-tree?apply=false"
        );
        assert_eq!(
            backfill_tree_path(true),
            "/api/sessions/backfill-tree?apply=true"
        );
    }

    #[test]
    fn collect_tree_items_folds_a_synthetic_tree() {
        let root = std::env::temp_dir().join(format!("kb-tree-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bucket")).unwrap();
        let top = root.join("bucket/2026-09-01T10-00-00-000Z_root0001.jsonl");
        let depth1 = root.join("bucket/2026-09-01T10-00-00-000Z_root0001/child0001.jsonl");
        std::fs::create_dir_all(depth1.parent().unwrap()).unwrap();
        let nested_dir = root.join("bucket/2026-09-01T10-00-00-000Z_root0001/Worker");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let sibling = nested_dir.join("Worker.jsonl");
        let nested = nested_dir.join("Worker.child0003.jsonl");
        let write = |p: &Path, id: &str, parent: Option<&str>| {
            let mut v = json!({"type":"session","id":id});
            if let Some(ps) = parent {
                v["parentSession"] = json!(ps);
            }
            std::fs::write(p, format!("{}\n", serde_json::to_string(&v).unwrap())).unwrap();
        };
        write(&top, "root0001", None);
        write(&depth1, "child0001", Some(&top.to_string_lossy()));
        write(&sibling, "child0002", None);
        write(&nested, "child0003", None);

        let items = collect_tree_items(&root);
        let find = |sid: &str| items.iter().find(|i| i.session_id == sid).unwrap().clone();
        let top_it = find("root0001");
        assert_eq!(
            (top_it.parent_session_id, top_it.root_session_id),
            (None, None)
        );
        let child = find("child0001");
        assert_eq!(
            (
                child.parent_session_id.as_deref(),
                child.root_session_id.as_deref()
            ),
            (Some("root0001"), Some("root0001"))
        );
        let nested_it = find("child0003");
        assert_eq!(
            (
                nested_it.parent_session_id.as_deref(),
                nested_it.root_session_id.as_deref()
            ),
            (Some("child0002"), Some("root0001")),
            "nested with sibling header"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
