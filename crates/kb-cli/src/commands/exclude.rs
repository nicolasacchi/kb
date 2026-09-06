//! `kb exclude <target> [--kb NAME] [--rm] [--list] [--note TEXT] [--json]`
//! — per-file index exclusion (v0.24 X3; CLI parity with the SPA was an
//! operator directive).
//!
//! Add: resolves `<target>` (12-hex id, source-relative path, or unique
//! filename suffix) through the daemon's `/lookup` ladder, then POSTs the
//! resolved `source_relative` to `/api/kb/{kb}/exclusions`. A target that
//! matches no indexed artifact but looks like a path is excluded verbatim
//! (pre-emptive exclusion of a not-yet-indexed or quarantined file) with a
//! stderr note. Exclusion keeps user data: `.review` comments + reading
//! history survive and re-anchor on re-include (D3).
//!
//! Rm: excluded files are gone from the index, so `/lookup` can't see them
//! — `--rm` resolves against the CURRENT exclusion list instead (exact
//! rel-path match, else unique basename/suffix match), then DELETEs
//! `/api/kb/{kb}/exclusions/{path}` (path percent-encoded as ONE segment).
//!
//! List: `--list` prints the exclusion table (raw JSON with `--json`).

use anyhow::{Context, Result};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

#[allow(clippy::too_many_arguments)]
pub async fn run(
    target: Option<&str>,
    kb: Option<&str>,
    rm: bool,
    list: bool,
    note: Option<String>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    if list {
        return run_list(base, &resolved_kb, bearer, json).await;
    }
    let Some(target) = target else {
        anyhow::bail!("pass a target to exclude, or --list to show current exclusions");
    };
    if rm {
        run_rm(base, &resolved_kb, target, bearer, json).await
    } else {
        run_add(base, &resolved_kb, target, note, bearer, json, daemon).await
    }
}

fn exclusions_url(base: &str, kb: &str) -> String {
    format!(
        "{base}/api/kb/{}/exclusions",
        crate::http::encode_path_segment(kb),
    )
}

async fn run_add(
    base: &str,
    kb: &str,
    target: &str,
    note: Option<String>,
    bearer: Option<&str>,
    json: bool,
    daemon: Option<&str>,
) -> Result<()> {
    // Resolve through the daemon's lookup ladder (id → path → unique
    // suffix). Unlike `resolve_artifact_target` we need the PATH, not the
    // id — the exclusion store keys on source-relative paths so a file can
    // stay excluded across delete/recreate cycles.
    let lookup = crate::http::get_lookup(daemon, kb, target, bearer).await?;
    let kind = lookup
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let rel = match kind {
        "exact" | "unique_suffix" => lookup
            .get("source_relative")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .context("lookup response missing `source_relative`")?
            .to_string(),
        "ambiguous" => {
            let candidates = lookup
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target:?} matched {} artifacts in {kb}; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {rel}\n"));
            }
            anyhow::bail!(msg)
        }
        "not_found" => {
            // Pre-emptive / quarantined-file exclusion: a path-shaped
            // target is excluded verbatim (the store accepts paths that
            // were never indexed); anything else is almost certainly a
            // typo'd id or title — refuse rather than record junk.
            if target.contains('/') || target.contains('.') {
                if !json {
                    eprintln!(
                        "  ({target} matched no indexed artifact — excluding by literal \
                         source-relative path)"
                    );
                }
                target.to_string()
            } else {
                anyhow::bail!(
                    "{target:?} matched no artifact in {kb} and doesn't look like a \
                     source-relative path"
                )
            }
        }
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {lookup}"),
    };

    let client = crate::http::client_with_timeout_and_bearer(15, bearer)?;
    let url = exclusions_url(base, kb);
    let body = crate::http::send_json(
        client
            .post(&url)
            .json(&serde_json::json!({ "path": rel, "note": note })),
        "exclude",
    )
    .await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    let path = body["path"].as_str().unwrap_or(&rel);
    if body["newly_excluded"].as_bool().unwrap_or(false) {
        eprintln!("✓ excluded {path} from kb {kb}");
        eprintln!("  comments + reading history kept; restore with: kb exclude {path} --rm");
    } else {
        eprintln!("• {path} was already excluded in kb {kb} (no change)");
    }
    Ok(())
}

async fn run_rm(
    base: &str,
    kb: &str,
    target: &str,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = crate::http::client_with_timeout_and_bearer(15, bearer)?;
    let url = exclusions_url(base, kb);
    let rows = crate::http::send_json(client.get(&url), "list exclusions").await?;
    let rows = rows
        .as_array()
        .cloned()
        .context("GET exclusions: expected an array")?;
    let paths: Vec<String> = rows
        .iter()
        .filter_map(|r| r["path"].as_str().map(str::to_string))
        .collect();

    // Resolution against the exclusion list (the index no longer knows
    // these files): exact normalised match first, else a unique
    // basename/suffix match — the same spirit as the daemon's /lookup
    // ladder, over the only table that still names them.
    let norm = target
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/');
    let resolved = if let Some(exact) = paths.iter().find(|p| p.as_str() == norm) {
        exact.clone()
    } else {
        let suffix: Vec<&String> = paths
            .iter()
            .filter(|p| p.ends_with(&format!("/{norm}")))
            .collect();
        match suffix.len() {
            1 => suffix[0].clone(),
            0 => anyhow::bail!(
                "{target:?} is not excluded in {kb} — see `kb exclude --list --kb {kb}`"
            ),
            n => {
                let mut msg = format!("{target:?} matched {n} excluded paths in {kb}; pick one:\n");
                for p in suffix {
                    msg.push_str(&format!("  {p}\n"));
                }
                anyhow::bail!(msg)
            }
        }
    };

    let del_url = format!(
        "{url}/{}",
        crate::http::encode_path_segment(&resolved), // '/' → %2F: one segment
    );
    let body = crate::http::send_json(client.delete(&del_url), "re-include").await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    if body["was_excluded"].as_bool().unwrap_or(false) {
        eprintln!("✓ re-included {resolved} in kb {kb} — reindexing now");
        eprintln!("  watch progress with: kb push --filter artifact.indexed");
    } else {
        eprintln!("• {resolved} was not excluded in kb {kb} (no change)");
    }
    Ok(())
}

async fn run_list(base: &str, kb: &str, bearer: Option<&str>, json: bool) -> Result<()> {
    let client = crate::http::client_with_timeout_and_bearer(15, bearer)?;
    let body =
        crate::http::send_json(client.get(exclusions_url(base, kb)), "list exclusions").await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    let rows = body
        .as_array()
        .cloned()
        .context("GET exclusions: expected an array")?;
    if rows.is_empty() {
        println!("no exclusions in kb {kb}");
        return Ok(());
    }
    println!("{} exclusion(s) in kb {kb}:", rows.len());
    for r in rows {
        let path = r["path"].as_str().unwrap_or("?");
        let when = r["excluded_at"]
            .as_i64()
            .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "?".to_string());
        let gone = if r["present_on_disk"].as_bool() == Some(false) {
            "  [gone from disk]"
        } else {
            ""
        };
        let note = r["note"]
            .as_str()
            .map(|n| format!("  note: {n}"))
            .unwrap_or_default();
        println!("  {path}  excluded {when}{gone}{note}");
    }
    Ok(())
}
