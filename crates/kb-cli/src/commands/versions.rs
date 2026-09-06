//! `kb versions <target>` + `kb diff <target>` — Track V artifact version
//! timeline and text/raw diff.
//!
//! Both resolve `<target>` (a 12-hex id, source-relative path, or unique
//! filename) via `/api/kb/{kb}/lookup`, then GET the daemon's
//! `/api/kb/{kb}/artifacts/{id}/{versions,diff}` routes. `--kb` defaults to
//! the single configured kb. `kb diff` defaults to "most recent prior
//! version → working tree" and renders rendered-prose changes (`--raw` for
//! the source-bytes diff); `--json` passes the route payload through.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::IsTerminal;

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// Resolve `(kb?, target)` to `(kb_name, artifact_id)` via the daemon's
/// `/lookup`. Mirrors `http::resolve_artifact_target`.
async fn resolve_target(
    kb: Option<&str>,
    target: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let looks_like_id = target.len() == 12 && target.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_id {
        return Ok((resolved_kb, target.to_string()));
    }
    let body = crate::http::get_lookup(daemon, &resolved_kb, target, bearer).await?;
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match kind {
        "exact" | "unique_suffix" => {
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("lookup response missing `id`"))?
                .to_string();
            Ok((resolved_kb, id))
        }
        "ambiguous" => {
            let candidates = body
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target:?} matched {} artifacts in {resolved_kb}; pick one:\n",
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
        "not_found" => anyhow::bail!("{target:?} matched no artifact in {resolved_kb}"),
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {body}"),
    }
}

/// Format a unix timestamp as `YYYY-MM-DD HH:MM` (UTC), or empty when 0.
fn fmt_ts(ts: i64) -> String {
    if ts == 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

// --- versions (timeline) --------------------------------------------------

/// `kb versions <target> [--at <DATE>]`.
///
/// CT-F6 — `--at` resolves a Memento coordinate (RFC 7089's per-resource
/// "as it stood at this instant") through the daemon's `?at=<unix>` query,
/// reusing `--between`'s date grammar so the two temporal flags can't
/// disagree about what "2026-07-30" means. The daemon answers with the
/// NEAREST PRIOR version — never an exact-match claim, never a silent
/// fallback to the oldest — and the resolved row is marked `→` in the
/// printed timeline so the answer is visible in context, not just asserted
/// in a header line.
pub async fn versions(
    target: &str,
    at: Option<&str>,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let at_unix = at.map(parse_as_of).transpose()?;
    let (resolved_kb, id) = resolve_target(kb, target, daemon, bearer).await?;
    let mut url = format!(
        "{}/api/kb/{}/artifacts/{}/versions",
        base_url(daemon),
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id),
    );
    if let Some(ts) = at_unix {
        url.push_str(&format!("?at={ts}"));
    }
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .context("GET versions")?
        .error_for_status()?
        .json()
        .await
        .context("parse versions")?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
    let versions = body
        .get("versions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if versions.is_empty() {
        println!("no versions (mode={mode})");
        return Ok(());
    }
    println!("mode: {mode}");
    // CT-F6 — the resolution line, printed ABOVE the timeline it explains.
    // `memento` is only in the body when `--at` was passed (the daemon
    // never volunteers it), so a plain `kb versions` skips all of this and
    // prints exactly what it printed before.
    let memento = body.get("memento");
    let hit_ref = memento
        .filter(|m| m.get("found").and_then(|f| f.as_bool()) == Some(true))
        .and_then(|m| m.get("version"))
        .and_then(|v| v.get("ref"))
        .and_then(|r| r.as_str())
        .map(str::to_string);
    if let (Some(m), Some(requested)) = (memento, at) {
        println!("{}", memento_line(m, requested));
    }
    for v in &versions {
        let source = v.get("source").and_then(|x| x.as_str()).unwrap_or("?");
        let short = v.get("short").and_then(|x| x.as_str()).unwrap_or("");
        let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("");
        let author = v.get("author").and_then(|x| x.as_str()).unwrap_or("");
        let when = fmt_ts(v.get("ts_unix").and_then(|x| x.as_i64()).unwrap_or(0));
        let glyph = match source {
            "working" => "✎",
            "git" => "●",
            "index" => "○",
            _ => " ",
        };
        let desc = match source {
            "working" => "working tree (uncommitted)".to_string(),
            "index" => "indexed snapshot".to_string(),
            _ if author.is_empty() => label.to_string(),
            _ => format!("{label}  — {author}"),
        };
        // The `--at` hit is marked in place. Without `--at` there is no
        // gutter at all, so a plain `kb versions` prints byte-for-byte what
        // it printed before CT-F6.
        let mark = row_mark(
            at.is_some(),
            hit_ref.as_deref(),
            v.get("ref").and_then(|x| x.as_str()),
        );
        println!("{mark}{glyph} {short:<9} {when:<16}  {desc}");
    }
    Ok(())
}

/// CT-F6 — the per-row `--at` gutter. Empty (no gutter at all) without
/// `--at`, so the default timeline output is byte-unchanged; `"→ "` on the
/// resolved row, `"  "` on the rest so the marked row stays aligned.
fn row_mark(at_given: bool, hit_ref: Option<&str>, row_ref: Option<&str>) -> &'static str {
    if !at_given {
        return "";
    }
    match (hit_ref, row_ref) {
        (Some(h), Some(r)) if h == r => "→ ",
        _ => "  ",
    }
}

/// CT-F6 — render the daemon's `memento` object as one human line. A HIT
/// always names the relation (`nearest prior` / `exact`) so an approximate
/// answer can never read as an exact one; a MISS prints the daemon's own
/// note (the floor it ran past) rather than any version at all.
fn memento_line(m: &Value, requested: &str) -> String {
    let found = m.get("found").and_then(|f| f.as_bool()).unwrap_or(false);
    if !found {
        let note = m
            .get("note")
            .and_then(|n| n.as_str())
            .unwrap_or("no version resolved");
        return format!("as of {requested}: {note}");
    }
    let exact = m.get("exact").and_then(|e| e.as_bool()).unwrap_or(false);
    let v = m.get("version");
    let short = v
        .and_then(|v| v.get("short"))
        .and_then(|s| s.as_str())
        .unwrap_or("?");
    let source = v
        .and_then(|v| v.get("source"))
        .and_then(|s| s.as_str())
        .unwrap_or("?");
    let ts = v
        .and_then(|v| v.get("ts_unix"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);
    let relation = if exact { "exact" } else { "nearest prior" };
    format!(
        "as of {requested} → {short} @ {} ({source}, {relation})",
        fmt_ts(ts)
    )
}

// --- diff -----------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn diff(
    target: &str,
    from: Option<&str>,
    to: Option<&str>,
    raw: bool,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, id) = resolve_target(kb, target, daemon, bearer).await?;
    diff_with_refs(
        &resolved_kb,
        &id,
        from,
        to,
        raw,
        json_out,
        daemon,
        bearer,
        None,
    )
    .await
}

/// The GET-and-render core of `diff`, factored out so MI-W2.4b's
/// `diff_between` can call it a SECOND time — once with explicit
/// `--from`/`--to` refs, once with the two refs `resolve_as_of` picked —
/// without duplicating the HTTP fetch / hunk-rendering logic.
///
/// `epoch` is `Some((tombstone_era_started_unix, epoch_caveat))` when the
/// caller (`diff_between`) has an MI-W2.4c epoch-honesty verdict to
/// surface; `None` for the plain `diff` command, which has no `--between`
/// window to evaluate and so merges nothing. When `Some` and `json_out`,
/// both values are merged into the printed JSON body via
/// [`merge_epoch_fields`] — the review-fix half of the caveat: it must
/// reach the agent-facing `--json` surface, not just the human-facing
/// stdout note.
#[allow(clippy::too_many_arguments)]
async fn diff_with_refs(
    resolved_kb: &str,
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    raw: bool,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    epoch: Option<(i64, bool)>,
) -> Result<()> {
    let mut url = format!(
        "{}/api/kb/{}/artifacts/{}/diff",
        base_url(daemon),
        encode_path_segment(resolved_kb),
        encode_path_segment(id),
    );
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(f) = from {
        params.push(("from", f));
    }
    if let Some(t) = to {
        params.push(("to", t));
    }
    if raw {
        params.push(("mode", "raw"));
    }
    if !params.is_empty() {
        let qs = params
            .iter()
            .map(|(k, v)| format!("{k}={}", encode_path_segment(v)))
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&qs);
    }
    let client = client_with_timeout_and_bearer(20, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .context("GET diff")?
        .error_for_status()?
        .json()
        .await
        .context("parse diff")?;
    if json_out {
        let body = match epoch {
            Some((era_started_unix, epoch_caveat)) => {
                merge_epoch_fields(body, era_started_unix, epoch_caveat)
            }
            None => body,
        };
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let from_ref = body.get("from").and_then(|v| v.as_str()).unwrap_or("");
    let to_ref = body.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("text");
    println!("diff {from_ref} → {to_ref}  ({mode})");
    let hunks = body
        .get("hunks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if hunks.is_empty() {
        println!("(no changes)");
        return Ok(());
    }
    let color = std::io::stdout().is_terminal();
    for h in &hunks {
        let os = h.get("old_start").and_then(|v| v.as_i64()).unwrap_or(0);
        let ns = h.get("new_start").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("{}", hunk_header(os, ns, color));
        if let Some(lines) = h.get("lines").and_then(|v| v.as_array()) {
            for line in lines {
                let tag = line.get("tag").and_then(|v| v.as_str()).unwrap_or("equal");
                let text = line.get("text").and_then(|v| v.as_str()).unwrap_or("");
                println!("{}", format_diff_line(tag, text, color));
            }
        }
    }
    Ok(())
}

/// Parse the daemon's `GET …/versions` wire array back into
/// `kb_core::versions::Version` — can't `derive(Deserialize)` on `Version`
/// itself (its `source` field is `&'static str`, which can't satisfy the
/// fully-generic `Deserialize<'de>` bound `serde_json::from_value` needs),
/// so this maps the wire `source` STRING onto the matching `&'static str`
/// literal by hand. An entry with an unrecognized `source` is skipped
/// (best-effort forward-compat with a future daemon adding a fourth
/// source) rather than failing the whole parse.
fn parse_versions_wire(arr: Option<&Value>) -> Result<Vec<kb_core::versions::Version>> {
    let arr = arr
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("versions response missing a `versions` array"))?;
    Ok(arr
        .iter()
        .filter_map(|v| {
            let source = match v.get("source").and_then(|s| s.as_str())? {
                "working" => "working",
                "git" => "git",
                "index" => "index",
                _ => return None,
            };
            Some(kb_core::versions::Version {
                r#ref: v.get("ref")?.as_str()?.to_string(),
                source,
                label: v
                    .get("label")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                author: v
                    .get("author")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                ts_unix: v.get("ts_unix").and_then(|t| t.as_i64())?,
                short: v
                    .get("short")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect())
}

/// MI-W2.4b — the shared date grammar (2026-07 temporal-query design):
/// `YYYY-MM-DD` means END of that calendar day, `23:59:59Z` ("as of June
/// 5" = after June 5's changes land); a full RFC 3339 instant is exact.
/// Always UTC, never local time — the same argument must resolve
/// identically regardless of the caller's timezone (determinism).
///
/// CT-F6: `kb versions --at` reuses this verbatim, so `--at` and
/// `--between` can never disagree about what a given date means.
fn parse_as_of(s: &str) -> Result<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp());
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        anyhow!("invalid date {s:?}: expected YYYY-MM-DD or an RFC 3339 timestamp ({e})")
    })?;
    let dt = date
        .and_hms_opt(23, 59, 59)
        .ok_or_else(|| anyhow!("invalid date {s:?}"))?;
    Ok(dt.and_utc().timestamp())
}

/// MI-W2.4c — the human-readable EPOCH HONESTY note, shared by both
/// `diff_between` return paths (the "no recorded change" short-circuit and
/// the full diff) so the wording can't drift between them.
fn epoch_honesty_note(era_started_unix: i64) -> String {
    format!(
        "NOTE (epoch honesty): this window starts before {} — the moment this daemon \
         became able to soft-forget (MI-W2.3) instead of hard-deleting. Anything hard-\
         deleted before then left no trace and cannot appear in this diff.",
        fmt_ts(era_started_unix)
    )
}

/// MI-W2.R (review fix) — merge the epoch-honesty verdict into a `--json`
/// response body as explicit fields, mirroring `kb memory log --json`'s
/// `tombstone_era_started_unix` embed (`commands::memory::log`): one
/// vocabulary for the same caveat across both agent-facing temporal-query
/// surfaces. Inserted unconditionally (not just when the caveat fires) so
/// a caller can always tell how far back this daemon's honest window
/// reaches, the same way `kb memory log --json` always carries the field.
fn merge_epoch_fields(mut body: Value, era_started_unix: i64, epoch_caveat: bool) -> Value {
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "tombstone_era_started_unix".into(),
            serde_json::json!(era_started_unix),
        );
        obj.insert("epoch_caveat".into(), serde_json::json!(epoch_caveat));
    }
    body
}

/// `kb diff --between <D1> <D2>` — MI-W2.4b. Resolves both dates to the
/// nearest version at-or-before each (`kb_core::versions::resolve_as_of`,
/// a PURE fn over the same timeline `kb versions` already lists — zero
/// server delta), echoes what each date resolved to, then diffs those two
/// refs exactly like an explicit `--from`/`--to` would. MI-W2.4c: surfaces
/// an EPOCH HONESTY caveat when the REQUESTED window (not the resolved
/// versions' own timestamps — the user's intent) reaches back before this
/// daemon's tombstone era — in BOTH human (`NOTE (epoch honesty): …`) and
/// `--json` (`tombstone_era_started_unix`/`epoch_caveat` fields) output.
/// MI-W2.R (review fix): the tombstone-era fetch + window comparison now
/// runs immediately after `d1_unix`/`d2_unix` are computed, AHEAD of every
/// early return below (the empty-versions bail, the resolve failures, and
/// the "same version both sides" short-circuit) — `--json` is the
/// agent-facing mode, and a temporal answer that silently omits the
/// caveat there is worse than no temporal answer at all; the short-circuit
/// used to skip the caveat check entirely, in EITHER output mode.
#[allow(clippy::too_many_arguments)]
pub async fn diff_between(
    target: &str,
    d1: &str,
    d2: &str,
    raw: bool,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, id) = resolve_target(kb, target, daemon, bearer).await?;
    let d1_unix = parse_as_of(d1)?;
    let d2_unix = parse_as_of(d2)?;

    let client = client_with_timeout_and_bearer(15, bearer)?;

    // MI-W2.4c / MI-W2.R — fetched + evaluated BEFORE any early return
    // (see the fn doc for why: JSON must carry this too, and the
    // same-version short-circuit further down must not skip it).
    let era: Value = client
        .get(format!("{}/api/memory/tombstone-era", base_url(daemon)))
        .send()
        .await
        .context("GET tombstone-era")?
        .error_for_status()?
        .json()
        .await
        .context("parse tombstone-era")?;
    let era_started = era
        .get("started_unix")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let window_start = d1_unix.min(d2_unix);
    let epoch_caveat = window_start < era_started;

    let versions_url = format!(
        "{}/api/kb/{}/artifacts/{}/versions",
        base_url(daemon),
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id),
    );
    let vbody: Value = client
        .get(&versions_url)
        .send()
        .await
        .context("GET versions")?
        .error_for_status()?
        .json()
        .await
        .context("parse versions")?;
    let versions = parse_versions_wire(vbody.get("versions"))?;
    if versions.is_empty() {
        let mode = vbody.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        anyhow::bail!(
            "{target:?} has no recorded versions (mode={mode}) — nothing to resolve --between against"
        );
    }
    let oldest_ts = versions.iter().map(|v| v.ts_unix).min().unwrap();

    let resolve_named = |label: &str, at_unix: i64| -> Result<&kb_core::versions::Version> {
        kb_core::versions::resolve_as_of(&versions, at_unix).ok_or_else(|| {
            anyhow!(
                "{label:?} ({at_unix} unix) predates the oldest known version of {target:?} \
                 ({}, unix {oldest_ts}) — nothing to diff",
                fmt_ts(oldest_ts)
            )
        })
    };
    let v1 = resolve_named(d1, d1_unix)?;
    let v2 = resolve_named(d2, d2_unix)?;

    if !json_out {
        println!(
            "{d1} → {}  @ {}  ({})",
            v1.r#ref,
            fmt_ts(v1.ts_unix),
            v1.source
        );
        println!(
            "{d2} → {}  @ {}  ({})",
            v2.r#ref,
            fmt_ts(v2.ts_unix),
            v2.source
        );
    }

    if v1.r#ref == v2.r#ref {
        if json_out {
            let body = merge_epoch_fields(
                serde_json::json!({
                    "from": v1.r#ref,
                    "to": v2.r#ref,
                    "message": "no recorded change between d1 and d2",
                }),
                era_started,
                epoch_caveat,
            );
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            println!("no recorded change between {d1} and {d2}");
            if epoch_caveat {
                println!("{}", epoch_honesty_note(era_started));
            }
        }
        return Ok(());
    }
    let (from_ref, to_ref) = (v1.r#ref.clone(), v2.r#ref.clone());

    if !json_out && epoch_caveat {
        println!("{}", epoch_honesty_note(era_started));
    }

    diff_with_refs(
        &resolved_kb,
        &id,
        Some(&from_ref),
        Some(&to_ref),
        raw,
        json_out,
        daemon,
        bearer,
        Some((era_started, epoch_caveat)),
    )
    .await
}

/// `@@ -<old> +<new> @@` hunk header, dimmed on a TTY.
fn hunk_header(old_start: i64, new_start: i64, color: bool) -> String {
    let h = format!("@@ -{old_start} +{new_start} @@");
    if color {
        format!("\x1b[36m{h}\x1b[0m")
    } else {
        h
    }
}

/// Render one diff line: `+`/green for inserts, `-`/red for deletes, two
/// leading spaces for context. ANSI only when `color` (a TTY).
fn format_diff_line(tag: &str, text: &str, color: bool) -> String {
    let (sign, code) = match tag {
        "insert" => ('+', "\x1b[32m"),
        "delete" => ('-', "\x1b[31m"),
        _ => (' ', ""),
    };
    if color && !code.is_empty() {
        format!("{code}{sign} {text}\x1b[0m")
    } else {
        format!("{sign} {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_line_rendering_plain() {
        assert_eq!(format_diff_line("equal", "ctx", false), "  ctx");
        assert_eq!(format_diff_line("insert", "added", false), "+ added");
        assert_eq!(format_diff_line("delete", "gone", false), "- gone");
    }

    #[test]
    fn diff_line_rendering_color() {
        assert_eq!(
            format_diff_line("insert", "added", true),
            "\x1b[32m+ added\x1b[0m"
        );
        assert_eq!(
            format_diff_line("delete", "gone", true),
            "\x1b[31m- gone\x1b[0m"
        );
        // Context lines stay uncolored even with color on.
        assert_eq!(format_diff_line("equal", "ctx", true), "  ctx");
    }

    #[test]
    fn hunk_header_format() {
        assert_eq!(hunk_header(3, 5, false), "@@ -3 +5 @@");
        assert_eq!(hunk_header(3, 5, true), "\x1b[36m@@ -3 +5 @@\x1b[0m");
    }

    #[test]
    fn fmt_ts_zero_is_empty() {
        assert_eq!(fmt_ts(0), "");
        assert_eq!(fmt_ts(1_700_000_000), "2023-11-14 22:13");
    }

    // ---- MI-W2.4b — --between date grammar -----------------------------

    #[test]
    fn parse_as_of_calendar_date_means_end_of_day_utc() {
        let ts = parse_as_of("2023-11-14").unwrap();
        // 2023-11-14T23:59:59Z
        assert_eq!(ts, 1_700_006_399);
    }

    #[test]
    fn parse_as_of_accepts_rfc3339_exact_instant() {
        let ts = parse_as_of("2023-11-14T22:13:20Z").unwrap();
        assert_eq!(ts, 1_700_000_000);
    }

    #[test]
    fn parse_as_of_rejects_garbage() {
        assert!(parse_as_of("not-a-date").is_err());
        assert!(parse_as_of("2023-13-99").is_err());
    }

    // ---- MI-W2.R — JSON epoch-honesty fields ----------------------------

    #[test]
    fn merge_epoch_fields_inserts_both_fields_unconditionally() {
        let body = serde_json::json!({ "from": "abc", "to": "def", "mode": "text" });
        let merged = merge_epoch_fields(body, 1_700_000_000, true);
        assert_eq!(merged["tombstone_era_started_unix"], 1_700_000_000);
        assert_eq!(merged["epoch_caveat"], true);
        // Original fields survive untouched.
        assert_eq!(merged["from"], "abc");
        assert_eq!(merged["to"], "def");
        assert_eq!(merged["mode"], "text");
    }

    #[test]
    fn merge_epoch_fields_inserts_false_caveat_when_window_is_inside_the_era() {
        let body = serde_json::json!({ "from": "abc", "to": "def" });
        let merged = merge_epoch_fields(body, 1_700_000_000, false);
        assert_eq!(merged["tombstone_era_started_unix"], 1_700_000_000);
        assert_eq!(merged["epoch_caveat"], false);
    }

    #[test]
    fn merge_epoch_fields_is_a_no_op_on_a_non_object_body() {
        // Defensive: a malformed/non-object body (should never happen in
        // practice — the daemon always returns a JSON object) must not
        // panic; it's returned unchanged.
        let body = serde_json::json!("not an object");
        let merged = merge_epoch_fields(body.clone(), 1_700_000_000, true);
        assert_eq!(merged, body);
    }

    // ---- CT-F6 — `kb versions --at` rendering ---------------------------

    #[test]
    fn memento_line_hit_says_nearest_prior_and_names_the_version() {
        let m = serde_json::json!({
            "at_unix": 1_700_000_100_i64,
            "relation": "nearest-prior",
            "found": true,
            "exact": false,
            "version": { "ref": "deadbeef", "source": "git", "short": "deadbee",
                         "ts_unix": 1_700_000_000_i64 },
        });
        let line = memento_line(&m, "2023-11-14");
        assert_eq!(
            line,
            "as of 2023-11-14 → deadbee @ 2023-11-14 22:13 (git, nearest prior)"
        );
    }

    #[test]
    fn memento_line_exact_hit_is_labelled_exact_not_nearest_prior() {
        let m = serde_json::json!({
            "found": true,
            "exact": true,
            "version": { "ref": "deadbeef", "source": "git", "short": "deadbee",
                         "ts_unix": 1_700_000_000_i64 },
        });
        let line = memento_line(&m, "2023-11-14T22:13:20Z");
        assert!(line.ends_with("(git, exact)"), "{line}");
        assert!(!line.contains("nearest prior"));
    }

    /// A miss prints the daemon's floor-naming note and NO version — the
    /// "never silently return the oldest" rule, at the render layer.
    #[test]
    fn memento_line_miss_prints_the_note_and_no_version() {
        let m = serde_json::json!({
            "found": false,
            "exact": false,
            "oldest_ts_unix": 1_700_000_000_i64,
            "note": "no version of this artifact is that old — the oldest known version is 2023-11-14 22:13 (unix 1700000000)",
        });
        let line = memento_line(&m, "2020-01-01");
        assert!(line.starts_with("as of 2020-01-01: "), "{line}");
        assert!(line.contains("is that old"));
        assert!(
            !line.contains("→"),
            "a miss never points at a version: {line}"
        );
    }

    /// Without `--at` there is no gutter at all — `kb versions` prints
    /// byte-for-byte what it did before CT-F6.
    #[test]
    fn row_mark_is_empty_without_at() {
        assert_eq!(row_mark(false, Some("abc"), Some("abc")), "");
        assert_eq!(row_mark(false, None, Some("abc")), "");
    }

    #[test]
    fn row_mark_points_at_the_resolved_row_only() {
        assert_eq!(row_mark(true, Some("abc"), Some("abc")), "→ ");
        assert_eq!(row_mark(true, Some("abc"), Some("zzz")), "  ");
        // A miss marks nothing, but still pads so the timeline stays aligned.
        assert_eq!(row_mark(true, None, Some("abc")), "  ");
    }

    #[test]
    fn epoch_honesty_note_names_the_era_boundary() {
        let note = epoch_honesty_note(1_700_000_000);
        assert!(note.contains("NOTE (epoch honesty)"));
        assert!(note.contains(&fmt_ts(1_700_000_000)));
    }
}
