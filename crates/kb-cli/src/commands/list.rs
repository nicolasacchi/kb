//! `kb list {create,ls,show,add,rm,update,move,rename,edit,delete,
//! reanchor,import,export,prune}` — reading lists (RL-track, v0.18;
//! prune = v0.33 X3).
//!
//! Every verb talks to the daemon over HTTP. Addressing:
//!
//! - `<list>` is an `l_…` id, or a case-insensitive unique title
//!   (resolved against `GET /api/lists` filtered to the kb).
//! - `<entry>` is an `le_…` id, or a 1-based index into the list's
//!   current order (`kb list show` prints both).
//! - `<target>` (artifact) is a 12-hex id, a source-relative path, or a
//!   unique filename — resolved via `/api/kb/{kb}/lookup`
//!   (`http::resolve_artifact_target`).
//!
//! `import`/`export` speak the portable kb-list/1 document (JSON, or the
//! hand/Claude-writable Markdown form). One heredoc materializes a whole
//! curated list:
//!
//! ```text
//! kb list import - --kb platform <<'EOF'
//! # Async Rust, properly
//! 1. [ ] [from scratch](research/async/from-scratch.html)
//! 2. [ ] [pinning](research/async/pinning.html#why-pin)
//! EOF
//! ```

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::io::Read;

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_artifact_target,
    resolve_default_kb, send_json,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

fn lists_url(daemon: Option<&str>) -> String {
    format!("{}/api/lists?include_archived=true", base_url(daemon))
}

fn list_url(daemon: Option<&str>, kb: &str, id: &str, tail: &str) -> String {
    format!(
        "{}/api/kb/{}/lists/{}{}",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(id),
        tail
    )
}

/// Resolve `<list>` to its index row: exact `l_…` id first, else a
/// case-insensitive title match within the (resolved) kb.
pub(crate) async fn resolve_list(
    kb: Option<&str>,
    list: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, Value)> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let body = send_json(client.get(lists_url(daemon)), "list index").await?;
    let empty = Vec::new();
    let rows = body["lists"].as_array().unwrap_or(&empty);
    let in_kb: Vec<&Value> = rows
        .iter()
        .filter(|l| l["kb"].as_str() == Some(resolved_kb.as_str()))
        .collect();
    if let Some(l) = in_kb.iter().find(|l| l["id"].as_str() == Some(list)) {
        return Ok((resolved_kb, (*l).clone()));
    }
    let matched: Vec<&&Value> = in_kb
        .iter()
        .filter(|l| {
            l["title"]
                .as_str()
                .is_some_and(|t| t.eq_ignore_ascii_case(list))
        })
        .collect();
    match matched.len() {
        1 => Ok((resolved_kb, (*matched[0]).clone())),
        0 => {
            let mut msg =
                format!("no reading list {list:?} in kb {resolved_kb}; existing lists:\n");
            if in_kb.is_empty() {
                msg.push_str("  (none — `kb list create <title>`)\n");
            }
            for l in &in_kb {
                msg.push_str(&format!(
                    "  {}  {}\n",
                    l["id"].as_str().unwrap_or("?"),
                    l["title"].as_str().unwrap_or("?")
                ));
            }
            bail!(msg)
        }
        _ => {
            // Titles are NOCASE-unique per kb, so this is defensive only.
            let mut msg = format!("{list:?} matched {} lists; pick by id:\n", matched.len());
            for l in matched {
                msg.push_str(&format!(
                    "  {}  {}\n",
                    l["id"].as_str().unwrap_or("?"),
                    l["title"].as_str().unwrap_or("?")
                ));
            }
            bail!(msg)
        }
    }
}

async fn fetch_detail(
    daemon: Option<&str>,
    bearer: Option<&str>,
    kb: &str,
    list_id: &str,
) -> Result<Value> {
    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(client.get(list_url(daemon, kb, list_id, "")), "list detail").await
}

/// Resolve `<entry>` against the detail's ordered entries: an `le_…` id
/// verbatim, else a 1-based index.
fn resolve_entry<'a>(entries: &'a [Value], entry: &str) -> Result<&'a Value> {
    if entry.starts_with("le_") {
        return entries
            .iter()
            .find(|e| e["id"].as_str() == Some(entry))
            .ok_or_else(|| anyhow!("entry {entry} is not in this list"));
    }
    let n: usize = entry
        .parse()
        .map_err(|_| anyhow!("<entry> must be an le_… id or a 1-based index, got {entry:?}"))?;
    if n == 0 || n > entries.len() {
        bail!(
            "entry index {n} out of range (list has {} entries)",
            entries.len()
        );
    }
    Ok(&entries[n - 1])
}

/// Translate `--to N` (1-based target position) into the wire's
/// before/after referencing a sibling — computed on the order WITHOUT
/// the moving entry, mirroring the daemon's PositionSpec resolution.
fn position_body_for_to(entries: &[Value], moving_id: &str, n: usize) -> Result<(String, String)> {
    let reduced: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["id"].as_str())
        .filter(|id| *id != moving_id)
        .collect();
    if n == 0 {
        bail!("--to is 1-based");
    }
    let idx = (n - 1).min(reduced.len());
    if idx == reduced.len() {
        match reduced.last() {
            Some(last) => Ok(("after".into(), (*last).to_string())),
            None => bail!("the list has no other entries to order against"),
        }
    } else {
        Ok(("before".into(), reduced[idx].to_string()))
    }
}

fn section_anchor(id: &str) -> Value {
    json!({ "kind": "section", "id": id })
}

fn dot(state: &str) -> &'static str {
    match state {
        "read" => "●",
        "in_progress" => "◐",
        _ => "○",
    }
}

fn fmt_list_row(l: &Value) -> String {
    let mut flags: Vec<&str> = Vec::new();
    if l["pinned"].as_bool().unwrap_or(false) {
        flags.push("pinned");
    }
    if l["archived"].as_bool().unwrap_or(false) {
        flags.push("archived");
    }
    let left = l["remaining_minutes"].as_u64().unwrap_or(0);
    format!(
        "{:<10} {:<15} {:<32} {:>4}  {:>5}  {:>6}  {}",
        l["kb"].as_str().unwrap_or("?"),
        l["id"].as_str().unwrap_or("?"),
        l["title"].as_str().unwrap_or("?"),
        l["entry_count"].as_u64().unwrap_or(0),
        format!(
            "{}/{}",
            l["read_count"].as_u64().unwrap_or(0),
            l["entry_count"].as_u64().unwrap_or(0)
        ),
        if left > 0 {
            format!("~{left}m")
        } else {
            "-".into()
        },
        flags.join(",")
    )
}

fn print_detail_human(detail: &Value) {
    let l = &detail["list"];
    println!(
        "{}  [{} · {}]  {}/{} read{}",
        l["title"].as_str().unwrap_or("?"),
        l["kb"].as_str().unwrap_or("?"),
        l["id"].as_str().unwrap_or("?"),
        l["read_count"].as_u64().unwrap_or(0),
        l["entry_count"].as_u64().unwrap_or(0),
        match l["remaining_minutes"].as_u64().unwrap_or(0) {
            0 => String::new(),
            m => format!(" · ~{m}m left"),
        }
    );
    if let Some(desc) = l["description"].as_str() {
        println!("  {desc}");
    }
    let empty = Vec::new();
    for (i, e) in detail["entries"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .enumerate()
    {
        let mut loc = e["source_relative"]
            .as_str()
            .unwrap_or("(removed)")
            .to_string();
        if let Some(anchor) = e.get("anchor").filter(|a| !a.is_null()) {
            match anchor["kind"].as_str() {
                Some("section") => {
                    loc.push_str(&format!(" §{}", anchor["id"].as_str().unwrap_or("?")))
                }
                Some("chapter") => {
                    let path = anchor["path"].as_str().unwrap_or("?");
                    let leaf = path.rsplit(" > ").next().unwrap_or(path);
                    loc.push_str(&format!(" §{leaf}"));
                }
                Some("selection") => loc.push_str(" ❝"),
                _ => {}
            }
        }
        let minutes = e["est_minutes"]
            .as_u64()
            .map(|m| format!("~{m}m"))
            .unwrap_or_default();
        let mut tail = String::new();
        if e["anchor_stale"].as_bool().unwrap_or(false) {
            tail.push_str(" ⚠stale");
        }
        if !e["read_override"].is_null() && e["read_override"].as_str().is_some() {
            tail.push_str(" (override)");
        }
        println!(
            "  {:>3}. {} {}  {:<34} {:<44} {:>5}{}",
            i + 1,
            dot(e["read_state"].as_str().unwrap_or("unread")),
            e["id"].as_str().unwrap_or("?"),
            e["title"].as_str().unwrap_or("(removed)"),
            loc,
            minutes,
            tail
        );
        if let Some(note) = e["note"].as_str() {
            for line in note.lines() {
                println!("       note: {line}");
            }
        }
    }
}

// --- verbs -------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn create(
    title: &str,
    description: Option<&str>,
    pin: bool,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let url = format!(
        "{}/api/kb/{}/lists",
        base_url(daemon),
        encode_path_segment(&resolved_kb)
    );
    let body = send_json(
        client
            .post(&url)
            .json(&json!({ "title": title, "description": description, "pinned": pin })),
        "list create",
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!(
            "✓ created {}  {:?} in {}",
            body["id"].as_str().unwrap_or("?"),
            title,
            resolved_kb
        );
    }
    Ok(())
}

pub async fn ls(
    kb: Option<&str>,
    archived: bool,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let body = send_json(client.get(lists_url(daemon)), "list index").await?;
    let empty = Vec::new();
    let rows: Vec<&Value> = body["lists"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|l| kb.is_none_or(|k| l["kb"].as_str() == Some(k)))
        .filter(|l| archived || !l["archived"].as_bool().unwrap_or(false))
        .collect();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({"lists": rows}))?);
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "no reading lists{}",
            kb.map(|k| format!(" in {k}")).unwrap_or_default()
        );
        return Ok(());
    }
    println!(
        "{:<10} {:<15} {:<32} {:>4}  {:>5}  {:>6}  FLAGS",
        "KB", "ID", "TITLE", "N", "READ", "~LEFT"
    );
    for l in rows {
        println!("{}", fmt_list_row(l));
    }
    Ok(())
}

pub async fn show(
    list: &str,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let id = row["id"].as_str().context("list row missing id")?;
    let detail = fetch_detail(daemon, bearer, &resolved_kb, id).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&detail)?);
    } else {
        print_detail_human(&detail);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn add(
    list: &str,
    target: &str,
    section: Option<&str>,
    note: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let (_, artifact_id) =
        resolve_artifact_target(Some(&resolved_kb), target, daemon, bearer).await?;

    let mut body = Map::new();
    body.insert("artifact_id".into(), json!(artifact_id));
    if let Some(s) = section {
        body.insert("anchor".into(), section_anchor(s));
    }
    if let Some(n) = note {
        body.insert("note".into(), json!(n));
    }
    // before/after accept an le_… id or a 1-based index.
    if before.is_some() || after.is_some() {
        let detail = fetch_detail(daemon, bearer, &resolved_kb, list_id).await?;
        let empty = Vec::new();
        let entries = detail["entries"].as_array().unwrap_or(&empty);
        if let Some(b) = before {
            let sibling = resolve_entry(entries, b)?;
            body.insert("before".into(), sibling["id"].clone());
        } else if let Some(a) = after {
            let sibling = resolve_entry(entries, a)?;
            body.insert("after".into(), sibling["id"].clone());
        }
    }

    let client = client_with_timeout_and_bearer(15, bearer)?;
    let out = send_json(
        client
            .post(list_url(daemon, &resolved_kb, list_id, "/entries"))
            .json(&Value::Object(body)),
        "list add",
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "✓ added {}  {} at position {}{}",
            out["id"].as_str().unwrap_or("?"),
            out["title"]
                .as_str()
                .or(out["source_relative"].as_str())
                .unwrap_or(target),
            out["position"].as_u64().map(|p| p + 1).unwrap_or(0),
            section.map(|s| format!(" (§{s})")).unwrap_or_default()
        );
    }
    Ok(())
}

pub async fn rm(
    list: &str,
    entry: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let detail = fetch_detail(daemon, bearer, &resolved_kb, list_id).await?;
    let empty = Vec::new();
    let entries = detail["entries"].as_array().unwrap_or(&empty);
    let target = resolve_entry(entries, entry)?;
    let eid = target["id"].as_str().context("entry missing id")?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(
        client.delete(list_url(
            daemon,
            &resolved_kb,
            list_id,
            &format!("/entries/{}", encode_path_segment(eid)),
        )),
        "list rm",
    )
    .await?;
    println!("✓ removed {eid}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    list: &str,
    entry: &str,
    note: Option<&str>,
    clear_note: bool,
    section: Option<&str>,
    clear_section: bool,
    read: bool,
    unread: bool,
    clear_read: bool,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let detail = fetch_detail(daemon, bearer, &resolved_kb, list_id).await?;
    let empty = Vec::new();
    let entries = detail["entries"].as_array().unwrap_or(&empty);
    let target = resolve_entry(entries, entry)?;
    let eid = target["id"].as_str().context("entry missing id")?;

    let mut body = Map::new();
    if let Some(n) = note {
        body.insert("note".into(), json!(n));
    } else if clear_note {
        body.insert("note".into(), Value::Null);
    }
    if let Some(s) = section {
        body.insert("anchor".into(), section_anchor(s));
    } else if clear_section {
        body.insert("anchor".into(), Value::Null);
    }
    if read {
        body.insert("read_override".into(), json!("read"));
    } else if unread {
        body.insert("read_override".into(), json!("unread"));
    } else if clear_read {
        body.insert("read_override".into(), json!("clear"));
    }
    if body.is_empty() {
        bail!("nothing to update — pass --note/--section/--read/… (see --help)");
    }

    let client = client_with_timeout_and_bearer(15, bearer)?;
    let out = send_json(
        client
            .patch(list_url(
                daemon,
                &resolved_kb,
                list_id,
                &format!("/entries/{}", encode_path_segment(eid)),
            ))
            .json(&Value::Object(body)),
        "list update",
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("✓ updated {eid}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn mv(
    list: &str,
    entry: &str,
    before: Option<&str>,
    after: Option<&str>,
    to: Option<usize>,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let detail = fetch_detail(daemon, bearer, &resolved_kb, list_id).await?;
    let empty = Vec::new();
    let entries = detail["entries"].as_array().unwrap_or(&empty);
    let moving = resolve_entry(entries, entry)?;
    let eid = moving["id"]
        .as_str()
        .context("entry missing id")?
        .to_string();

    let mut body = Map::new();
    if let Some(b) = before {
        let sibling = resolve_entry(entries, b)?;
        body.insert("before".into(), sibling["id"].clone());
    } else if let Some(a) = after {
        let sibling = resolve_entry(entries, a)?;
        body.insert("after".into(), sibling["id"].clone());
    } else if let Some(n) = to {
        let (key, sibling) = position_body_for_to(entries, &eid, n)?;
        body.insert(key, json!(sibling));
    } else {
        bail!("pass one of --before <entry> | --after <entry> | --to <n>");
    }

    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(
        client
            .patch(list_url(
                daemon,
                &resolved_kb,
                list_id,
                &format!("/entries/{}", encode_path_segment(&eid)),
            ))
            .json(&Value::Object(body)),
        "list move",
    )
    .await?;
    println!("✓ moved {eid}");
    Ok(())
}

pub async fn rename(
    list: &str,
    new_title: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(
        client
            .patch(list_url(daemon, &resolved_kb, list_id, ""))
            .json(&json!({ "title": new_title })),
        "list rename",
    )
    .await?;
    println!("✓ renamed {list_id} → {new_title:?}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn edit(
    list: &str,
    description: Option<&str>,
    clear_description: bool,
    pin: bool,
    unpin: bool,
    archive: bool,
    unarchive: bool,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let mut body = Map::new();
    if let Some(d) = description {
        body.insert("description".into(), json!(d));
    } else if clear_description {
        body.insert("description".into(), Value::Null);
    }
    if pin {
        body.insert("pinned".into(), json!(true));
    } else if unpin {
        body.insert("pinned".into(), json!(false));
    }
    if archive {
        body.insert("archived".into(), json!(true));
    } else if unarchive {
        body.insert("archived".into(), json!(false));
    }
    if body.is_empty() {
        bail!("nothing to edit — pass --description/--pin/--archive/… (see --help)");
    }
    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(
        client
            .patch(list_url(daemon, &resolved_kb, list_id, ""))
            .json(&Value::Object(body)),
        "list edit",
    )
    .await?;
    println!("✓ updated {list_id}");
    Ok(())
}

pub async fn delete(
    list: &str,
    yes: bool,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !yes {
        bail!("deleting a list removes all its entries — re-run with --yes");
    }
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    send_json(
        client.delete(list_url(daemon, &resolved_kb, list_id, "")),
        "list delete",
    )
    .await?;
    println!(
        "✓ deleted {} ({:?})",
        list_id,
        row["title"].as_str().unwrap_or("?")
    );
    Ok(())
}

/// v0.33 X3 — drop tombstoned entries (artifact no longer in the index).
/// Without `--yes`, lists the tombstones and asks for re-run with `--yes`
/// (mirrors `list delete`).
pub async fn prune(
    list: &str,
    yes: bool,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let client = client_with_timeout_and_bearer(15, bearer)?;

    // Preview tombstones via the detail endpoint (read path already
    // stamps `tombstone: true` on missing artifacts).
    let detail = send_json(
        client.get(list_url(daemon, &resolved_kb, list_id, "")),
        "list show",
    )
    .await?;
    let empty = Vec::new();
    let entries = detail["entries"].as_array().unwrap_or(&empty);
    let tombs: Vec<&Value> = entries
        .iter()
        .filter(|e| e["tombstone"].as_bool() == Some(true))
        .collect();
    if tombs.is_empty() {
        println!("removed 0 entries");
        return Ok(());
    }
    if !yes {
        let mut msg = format!(
            "pruning would remove {} tombstoned entr{} from {} ({:?}):\n",
            tombs.len(),
            if tombs.len() == 1 { "y" } else { "ies" },
            list_id,
            row["title"].as_str().unwrap_or("?")
        );
        for e in &tombs {
            let title = e["title"].as_str().unwrap_or("(untitled)");
            let rel = e["source_relative"]
                .as_str()
                .or_else(|| e["artifact_id"].as_str())
                .unwrap_or("?");
            msg.push_str(&format!("  - {title}  ({rel})\n"));
        }
        msg.push_str("re-run with --yes to confirm");
        bail!("{msg}");
    }
    let body = send_json(
        client.post(list_url(daemon, &resolved_kb, list_id, "/prune")),
        "list prune",
    )
    .await?;
    let n = body["removed"].as_u64().unwrap_or(0);
    println!("removed {n} entries");
    Ok(())
}

pub async fn reanchor(
    list: &str,
    entry: &str,
    section: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    update(
        list,
        entry,
        None,
        false,
        Some(section),
        false,
        false,
        false,
        false,
        kb,
        false,
        daemon,
        bearer,
    )
    .await
}

// --- import / export ----------------------------------------------------------

/// Cheap client-side peeks at the document for TARGETING + --dry-run.
/// The daemon's codec is authoritative for the actual parse.
fn doc_title(format: &str, body: &str) -> Option<String> {
    if format == "json" {
        serde_json::from_str::<Value>(body)
            .ok()?
            .get("title")?
            .as_str()
            .map(str::to_string)
    } else {
        body.lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(|t| t.trim().to_string())
    }
}

/// Description: the blockquote right after the title (md) / the
/// `description` field (json). Sent when import CREATES the list — the
/// daemon's import endpoint deliberately touches entries only.
fn doc_description(format: &str, body: &str) -> Option<String> {
    if format == "json" {
        return serde_json::from_str::<Value>(body)
            .ok()?
            .get("description")?
            .as_str()
            .map(str::to_string);
    }
    let mut lines = body.lines().skip_while(|l| !l.starts_with("# "));
    lines.next()?; // the title
    let mut out: Vec<String> = Vec::new();
    for l in lines {
        let t = l.trim_start();
        if let Some(q) = t.strip_prefix('>') {
            out.push(q.strip_prefix(' ').unwrap_or(q).to_string());
        } else if t.is_empty() {
            if !out.is_empty() {
                break;
            }
        } else {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

fn doc_list_id(format: &str, body: &str) -> Option<String> {
    if format == "json" {
        return serde_json::from_str::<Value>(body)
            .ok()?
            .get("list_id")?
            .as_str()
            .map(str::to_string);
    }
    // <!-- kb-list {"schema":"kb-list/1","kb":"…","list_id":"l_…"} -->
    let line = body
        .lines()
        .find(|l| l.trim_start().starts_with("<!-- kb-list "))?;
    let start = line.find("<!-- kb-list ")? + "<!-- kb-list ".len();
    let end = line.find(" -->")?;
    serde_json::from_str::<Value>(line.get(start..end)?.trim())
        .ok()?
        .get("list_id")?
        .as_str()
        .map(str::to_string)
}

fn doc_entry_count(format: &str, body: &str) -> usize {
    if format == "json" {
        return serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("entries").and_then(|e| e.as_array()).map(Vec::len))
            .unwrap_or(0);
    }
    body.lines()
        .filter(|l| {
            let s = l.trim_start();
            let digits = s.chars().take_while(char::is_ascii_digit).count();
            digits > 0
                && s[digits..].starts_with(['.', ')'])
                && s[digits + 1..].trim_start().starts_with('[')
        })
        .count()
}

fn detect_format(explicit: Option<&str>, file: &str) -> Result<String> {
    if let Some(f) = explicit {
        return match f {
            "md" | "json" => Ok(f.to_string()),
            other => bail!("invalid --format {other:?} (expected md | json)"),
        };
    }
    if file.ends_with(".json") {
        Ok("json".into())
    } else {
        // .md/.markdown and stdin default to markdown.
        Ok("md".into())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn import(
    file: &str,
    kb: Option<&str>,
    into: Option<&str>,
    mode: &str,
    format: Option<&str>,
    dry_run: bool,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !matches!(mode, "replace" | "append") {
        bail!("invalid --mode {mode:?} (expected replace | append)");
    }
    let body = if file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading stdin")?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
    };
    let format = detect_format(format, file)?;
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;

    // Targeting: --into > the doc's round-trip list_id (when it exists
    // on the daemon) > create a fresh list named by the doc title.
    let mut target: Option<(String, String)> = None; // (list_id, how)
    if let Some(into) = into {
        let (_, row) = resolve_list(Some(&resolved_kb), into, daemon, bearer).await?;
        target = Some((
            row["id"]
                .as_str()
                .context("list row missing id")?
                .to_string(),
            format!("into {:?}", row["title"].as_str().unwrap_or("?")),
        ));
    } else if let Some(doc_id) = doc_list_id(&format, &body) {
        if let Ok((_, row)) = resolve_list(Some(&resolved_kb), &doc_id, daemon, bearer).await {
            target = Some((
                doc_id,
                format!("round-trip into {:?}", row["title"].as_str().unwrap_or("?")),
            ));
        }
    }
    let title = doc_title(&format, &body);
    let n_entries = doc_entry_count(&format, &body);

    if dry_run {
        match &target {
            Some((id, how)) => println!(
                "would import {n_entries} entries ({mode}) {how} ({id}) in kb {resolved_kb}"
            ),
            None => println!(
                "would create list {:?} in kb {resolved_kb} and import {n_entries} entries",
                title.as_deref().unwrap_or("(untitled — import would fail)")
            ),
        }
        return Ok(());
    }

    let client = client_with_timeout_and_bearer(30, bearer)?;
    let (list_id, created) = match target {
        Some((id, _)) => (id, false),
        None => {
            let title = title
                .filter(|t| !t.is_empty())
                .context("document has no title (`# Heading` / \"title\") and no --into target")?;
            let url = format!(
                "{}/api/kb/{}/lists",
                base_url(daemon),
                encode_path_segment(&resolved_kb)
            );
            let description = doc_description(&format, &body);
            let row = send_json(
                client
                    .post(&url)
                    .json(&json!({ "title": title, "description": description })),
                "list create",
            )
            .await?;
            (
                row["id"].as_str().context("create missing id")?.to_string(),
                true,
            )
        }
    };

    let out = send_json(
        client
            .post(list_url(
                daemon,
                &resolved_kb,
                &list_id,
                &format!("/import?format={format}&mode={mode}"),
            ))
            .body(body),
        "list import",
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!(
        "✓ imported {} entries into {}{}",
        out["imported"].as_u64().unwrap_or(0),
        list_id,
        if created { " (created)" } else { "" }
    );
    let empty = Vec::new();
    for s in out["skipped"].as_array().unwrap_or(&empty) {
        println!(
            "  skipped {}: {}",
            s["ref"].as_str().unwrap_or("?"),
            s["reason"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

pub async fn export(
    list: &str,
    format: Option<&str>,
    out: Option<&str>,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let format = match format.unwrap_or("md") {
        f @ ("md" | "json") => f,
        other => bail!("invalid --format {other:?} (expected md | json)"),
    };
    let (resolved_kb, row) = resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"].as_str().context("list row missing id")?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let resp = client
        .get(list_url(
            daemon,
            &resolved_kb,
            list_id,
            &format!("/export?format={format}"),
        ))
        .send()
        .await
        .context("list export")?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        bail!("list export failed: HTTP {status} — {text}");
    }
    match out {
        Some(path) => {
            std::fs::write(path, &text).with_context(|| format!("writing {path}"))?;
            eprintln!("✓ wrote {path}");
        }
        None => print!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(ids: &[&str]) -> Vec<Value> {
        ids.iter().map(|id| json!({ "id": id })).collect()
    }

    #[test]
    fn resolve_entry_by_id_and_index() {
        let es = entries(&["le_a", "le_b", "le_c"]);
        assert_eq!(resolve_entry(&es, "le_b").unwrap()["id"], "le_b");
        assert_eq!(resolve_entry(&es, "1").unwrap()["id"], "le_a");
        assert_eq!(resolve_entry(&es, "3").unwrap()["id"], "le_c");
        assert!(resolve_entry(&es, "0").is_err());
        assert!(resolve_entry(&es, "4").is_err());
        assert!(resolve_entry(&es, "le_nope").is_err());
        assert!(resolve_entry(&es, "first").is_err());
    }

    #[test]
    fn to_position_translates_to_before_after() {
        let es = entries(&["le_a", "le_b", "le_c"]);
        // Move le_c to position 1 → before le_a.
        let (k, v) = position_body_for_to(&es, "le_c", 1).unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("before", "le_a"));
        // Move le_a to position 3 (the end of a 3-list) → after le_c.
        let (k, v) = position_body_for_to(&es, "le_a", 3).unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("after", "le_c"));
        // Move le_a to position 2 → before le_c (reduced order is b,c).
        let (k, v) = position_body_for_to(&es, "le_a", 2).unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("before", "le_c"));
        // Overshoot clamps to the end.
        let (k, v) = position_body_for_to(&es, "le_a", 99).unwrap();
        assert_eq!((k.as_str(), v.as_str()), ("after", "le_c"));
        assert!(position_body_for_to(&es, "le_a", 0).is_err());
    }

    // invariant:25 kb-list-grammar
    #[test]
    fn doc_peeks_extract_title_id_and_count() {
        let md = "# My list\n\n> desc\n\n<!-- kb-list {\"schema\":\"kb-list/1\",\"list_id\":\"l_abc\"} -->\n\n1. [ ] [a](x.html)\n2) [x] [b](y.html#s)\n   note\n";
        assert_eq!(doc_title("md", md).as_deref(), Some("My list"));
        assert_eq!(doc_list_id("md", md).as_deref(), Some("l_abc"));
        assert_eq!(doc_entry_count("md", md), 2);

        assert_eq!(
            doc_description("md", md).as_deref(),
            Some("desc"),
            "blockquote after the title is the description"
        );
        let two = "# T\n\n> line one\n> line two\n\n1. [ ] [a](x.html)\n";
        assert_eq!(
            doc_description("md", two).as_deref(),
            Some("line one\nline two")
        );
        assert_eq!(doc_description("md", "# T\n\n1. [ ] [a](x.html)\n"), None);

        let j = r#"{"schema":"kb-list/1","title":"J","list_id":"l_j","description":"D","entries":[{"path":"a"},{"path":"b"},{"path":"c"}]}"#;
        assert_eq!(doc_title("json", j).as_deref(), Some("J"));
        assert_eq!(doc_description("json", j).as_deref(), Some("D"));
        assert_eq!(doc_list_id("json", j).as_deref(), Some("l_j"));
        assert_eq!(doc_entry_count("json", j), 3);
        assert_eq!(doc_title("md", "no heading\n"), None);
    }

    #[test]
    fn format_detection_prefers_flag_then_extension() {
        assert_eq!(detect_format(Some("json"), "x.md").unwrap(), "json");
        assert_eq!(detect_format(None, "x.json").unwrap(), "json");
        assert_eq!(detect_format(None, "x.md").unwrap(), "md");
        assert_eq!(detect_format(None, "-").unwrap(), "md");
        assert!(detect_format(Some("xml"), "x").is_err());
    }
}
