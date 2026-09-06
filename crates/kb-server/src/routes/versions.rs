//! `versions` — Track V artifact version timeline + text/raw diff.
//!
//! ```text
//! GET /api/kb/{kb}/artifacts/{id}/versions[?at=<unix>]
//!     → { mode, versions: [{ref, source, label, author, ts_unix, short}] }
//!     → …plus `memento` when `at` is given (CT-F6)
//! GET /api/kb/{kb}/artifacts/{id}/diff?from=<ref>&to=<ref>&mode=text|raw
//!     → { from, to, mode, hunks: [{old_start, new_start, lines: [...]}] }
//! ```
//!
//! Versions come from git commits, kb index snapshots, and the working tree
//! per the kb's `[kb.*] versions` mode (resolved onto `KbContext`). The diff
//! defaults to "most recent prior version → working tree", so the bare call
//! shows what changed last. HTML/markdown diffs default to rendered prose
//! (markup-agnostic); `mode=raw` diffs the source bytes verbatim.
//!
//! Read-only; lives in the plain `api` tree (loopback bypass + bearer auth,
//! no rate bucket) alongside the other per-kb reads.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::vcs::{self, DiffHunk};
use kb_core::versions::{Version, VersionCtx, WORKING_REF};
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct VersionsResponse {
    /// The effective `[kb.*] versions` mode for this kb.
    pub mode: &'static str,
    pub versions: Vec<Version>,
}

/// CT-F6 — `?at=<unix>` on the timeline route. Taken as a STRING so a
/// malformed value becomes an explicit problem+json 400 naming the
/// expectation, rather than axum's generic query-rejection body.
#[derive(Debug, Default, Deserialize)]
pub struct VersionsParams {
    pub at: Option<String>,
}

/// CT-F6 — the Memento resolution for a `?at=<unix>` request: which version
/// the instant landed on, and enough context that the answer can't be
/// misread. Deliberately explicit rather than implicit:
///
/// - `relation` is ALWAYS `"nearest-prior"` — the resolver never claims an
///   exact match by omission; `exact` says whether the hit happened to land
///   on the version's own second.
/// - a MISS (`found: false`) carries `note` + `oldest_ts_unix` instead of
///   quietly degrading to the oldest version.
///
/// NOT ts-exported on purpose: the SPA resolves the memento client-side from
/// the timeline it already holds in the `["versions", kb, id]` query cache
/// (`web/src/lib/memento.ts`, golden-pinned in lock-step with
/// `kb_core::versions::resolve_memento`), so it never reads this shape off
/// the wire — exporting a binding no SPA code imports would be pure drift.
#[derive(Debug, Serialize)]
pub struct MementoOut {
    /// Echo of the requested instant (unix seconds).
    pub at_unix: i64,
    /// Always `"nearest-prior"` — the resolution rule, stated on every
    /// response so a client never has to infer it.
    pub relation: &'static str,
    /// Did anything resolve at all?
    pub found: bool,
    /// True only when the resolved version's own `ts_unix == at_unix`.
    pub exact: bool,
    /// The resolved version, verbatim from the timeline above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
    /// Oldest known version's timestamp — the floor a miss ran past.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_ts_unix: Option<i64>,
    /// Honest "nothing is that old" sentence; absent when `found`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The `?at=`-flavoured timeline body: byte-for-byte
/// [`VersionsResponse`]'s fields in the same order, plus one trailing
/// `memento` key. A separate struct (rather than an `Option` field on
/// `VersionsResponse`) so the no-`at` response is produced by the SAME code
/// path it always was — additivity by construction, not by
/// `skip_serializing_if` discipline.
#[derive(Debug, Serialize)]
pub struct VersionsAtResponse {
    pub mode: &'static str,
    pub versions: Vec<Version>,
    pub memento: MementoOut,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiffParams {
    /// Older side. Defaults to the most recent version *before* `to`.
    pub from: Option<String>,
    /// Newer side. Defaults to the working tree (current on-disk file).
    pub to: Option<String>,
    /// `text` (rendered prose, the default) or `raw` (source bytes).
    pub mode: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct DiffResponse {
    pub from: String,
    pub to: String,
    /// Echo of the resolved diff mode (`text` | `raw`).
    #[cfg_attr(feature = "ts-export", ts(type = "\"text\" | \"raw\""))]
    pub mode: &'static str,
    pub hunks: Vec<DiffHunk>,
}

/// STRICTER than the shared `routes::is_safe_id` on purpose: version
/// lookups only ever receive the canonical artifact id (the 12-hex
/// SHA-256 prefix of the source-relative path), so anything else is
/// rejected outright before touching storage or git.
fn is_hex_artifact_id(id: &str) -> bool {
    id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve `{kb}` + `{id}` to `(ctx, abs_path, rel_to_git)`, or a problem+json
/// error. `rel_to_git` is the file's path relative to the git root (a possible
/// ancestor of the corpus) — the base `git log`/`git show` expect, NOT the
/// corpus-relative path.
async fn resolve<'a>(
    state: &'a Arc<KbHandles>,
    kb: &str,
    id: &str,
) -> Result<(&'a KbContext, PathBuf, Option<String>), Response<Body>> {
    let (kb_name, ctx) = crate::routes::resolve_kb(state, kb)?;
    if !is_hex_artifact_id(id) {
        return Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
            "artifact {id}"
        ))));
    }
    let doc = match ctx.storage.get_by_id(id.to_string()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            ))))
        }
        Err(e) => return Err(error_to_problem_json(&e)),
    };
    let abs = PathBuf::from(&doc.path);
    let rel_to_git = ctx.git_root.as_ref().and_then(|root| {
        abs.strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    });
    Ok((ctx, abs, rel_to_git))
}

fn version_ctx<'a>(
    ctx: &'a KbContext,
    abs: &'a FsPath,
    rel_to_git: Option<String>,
    id: &'a str,
) -> VersionCtx<'a> {
    VersionCtx {
        mode: ctx.versions_mode,
        git_root: ctx.git_root.as_deref(),
        rel_to_git,
        abs_path: abs,
        storage: &ctx.storage,
        artifact_id: id,
    }
}

/// `GET /api/kb/{kb}/artifacts/{id}/versions[?at=<unix>]` — the version
/// timeline, optionally resolved at an instant (CT-F6, RFC 7089's
/// per-resource "memento" idea).
///
/// Without `at` this is byte-identical to what it has always returned (same
/// struct, same code path) — `at` is purely additive.
///
/// With `at`, the same body gains a `memento` object naming the version the
/// instant resolved to, plus a `Memento-Datetime` response header (RFC
/// 7089 §2.1) carrying that version's OWN datetime — never the requested
/// one, which is exactly the confusion the header exists to prevent.
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<VersionsParams>,
) -> Response<Body> {
    let at_unix = match params.at.as_deref().map(parse_at) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => return error_to_problem_json(&kb_core::Error::BadRequest(e)),
        None => None,
    };
    let (ctx, abs, rel) = match resolve(&state, &kb, &id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let vctx = version_ctx(ctx, &abs, rel, &id);
    let versions = vctx.list().await;
    let Some(at_unix) = at_unix else {
        return Json(VersionsResponse {
            mode: ctx.versions_mode.as_str(),
            versions,
        })
        .into_response();
    };

    let m = kb_core::versions::resolve_memento(&versions, at_unix);
    let memento = MementoOut {
        at_unix: m.at_unix,
        relation: NEAREST_PRIOR,
        found: m.found(),
        exact: m.exact,
        version: m.version.cloned(),
        oldest_ts_unix: m.oldest_ts_unix,
        note: m.miss_note(),
    };
    let resolved_ts = m.version.map(|v| v.ts_unix);
    let mut resp = Json(VersionsAtResponse {
        mode: ctx.versions_mode.as_str(),
        versions,
        memento,
    })
    .into_response();
    // RFC 7089 §2.1.1 — `Memento-Datetime` is the archived resource's own
    // datetime, i.e. WHEN this version is, not when it was asked for. Only
    // on a hit; a miss archives nothing.
    if let Some(ts) = resolved_ts {
        if let Some(value) = http_date(ts) {
            resp.headers_mut().insert("memento-datetime", value);
        }
    }
    resp
}

/// The one spelling of the resolution rule, shared by the wire body and
/// (via the README) the docs.
const NEAREST_PRIOR: &str = "nearest-prior";

/// Parse `?at=` — unix SECONDS, the same coordinate `ts_unix` is measured
/// in. Deliberately not a date grammar: the CLI owns human dates
/// (`kb versions --at 2026-07-30` / `kb diff --between`) and resolves them
/// to seconds before calling, so the HTTP surface has exactly one
/// unambiguous, timezone-free form.
fn parse_at(raw: &str) -> Result<i64, String> {
    raw.trim().parse::<i64>().map_err(|_| {
        format!("invalid `at` {raw:?}: expected unix seconds (an integer), e.g. at=1700000000")
    })
}

/// RFC 1123 / IMF-fixdate in GMT — the format RFC 7089 requires for
/// `Memento-Datetime`. `None` for a timestamp chrono can't represent or a
/// rendering that somehow isn't a valid header value (never in practice;
/// the format is pure ASCII).
fn http_date(ts: i64) -> Option<axum::http::HeaderValue> {
    let s = chrono::DateTime::from_timestamp(ts, 0)?
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    axum::http::HeaderValue::from_str(&s).ok()
}

/// `GET /api/kb/{kb}/artifacts/{id}/diff` — text (prose) or raw diff between
/// two version refs.
pub async fn diff(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<DiffParams>,
) -> Response<Body> {
    let (ctx, abs, rel) = match resolve(&state, &kb, &id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let vctx = version_ctx(ctx, &abs, rel, &id);
    let raw = matches!(params.mode.as_deref(), Some("raw"));
    let mode_str = if raw { "raw" } else { "text" };

    let to = params.to.clone().unwrap_or_else(|| WORKING_REF.to_string());
    let from = match params.from.clone() {
        Some(f) => f,
        None => {
            // Default `from` = the most recent version strictly older than
            // `to`. No older version (single revision) → empty diff.
            match default_from(&vctx.list().await, &to) {
                Some(f) => f,
                None => {
                    return Json(DiffResponse {
                        from: to.clone(),
                        to,
                        mode: mode_str,
                        hunks: Vec::new(),
                    })
                    .into_response()
                }
            }
        }
    };

    let old_text = match fetch(&vctx, &from, raw).await {
        Ok(t) => t,
        Err(e) => return error_to_problem_json(&kb_core::Error::BadRequest(e)),
    };
    let new_text = match fetch(&vctx, &to, raw).await {
        Ok(t) => t,
        Err(e) => return error_to_problem_json(&kb_core::Error::BadRequest(e)),
    };
    Json(DiffResponse {
        from,
        to,
        mode: mode_str,
        hunks: vcs::diff_lines(&old_text, &new_text),
    })
    .into_response()
}

/// Fetch one side of a diff: prose (default) or raw bytes.
async fn fetch(vctx: &VersionCtx<'_>, r: &str, raw: bool) -> Result<String, String> {
    if raw {
        vctx.raw_at(r).await
    } else {
        vctx.prose_at(r).await
    }
}

/// Pick the default `from` ref: the version one step older than `to` in the
/// newest-first timeline. Falls back to the second entry overall when `to`
/// isn't found (e.g. an arbitrary sha the caller passed).
fn default_from(timeline: &[Version], to: &str) -> Option<String> {
    match timeline.iter().position(|v| v.r#ref == to) {
        Some(pos) => timeline.get(pos + 1).map(|v| v.r#ref.clone()),
        None => timeline.get(1).map(|v| v.r#ref.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hex_artifact_id_accepts_only_12_hex() {
        assert!(is_hex_artifact_id("f5919686f042"));
        assert!(!is_hex_artifact_id("kitchen-sink"));
        assert!(!is_hex_artifact_id("f5919686f04")); // 11
        assert!(!is_hex_artifact_id(".."));
    }

    fn v(r: &str, ts: i64) -> Version {
        Version {
            r#ref: r.to_string(),
            source: "git",
            label: String::new(),
            author: String::new(),
            ts_unix: ts,
            short: r.to_string(),
        }
    }

    #[test]
    fn default_from_is_one_step_older_than_to() {
        let tl = vec![v("WORKING", 30), v("aaa", 20), v("bbb", 10)];
        // to = working → from = the newest committed version.
        assert_eq!(default_from(&tl, "WORKING").as_deref(), Some("aaa"));
        // to = aaa → from = bbb.
        assert_eq!(default_from(&tl, "aaa").as_deref(), Some("bbb"));
        // to = oldest → nothing older.
        assert_eq!(default_from(&tl, "bbb"), None);
        // unknown `to` → second entry overall.
        assert_eq!(default_from(&tl, "zzz").as_deref(), Some("aaa"));
    }

    #[test]
    fn default_from_single_version_is_none() {
        let tl = vec![v("WORKING", 30)];
        assert_eq!(default_from(&tl, "WORKING"), None);
    }

    // ---- CT-F6 — ?at= (Memento) ----------------------------------------

    #[test]
    fn parse_at_accepts_unix_seconds_and_rejects_anything_else() {
        assert_eq!(parse_at("1700000000"), Ok(1_700_000_000));
        assert_eq!(parse_at(" 1700000000 "), Ok(1_700_000_000));
        assert_eq!(parse_at("0"), Ok(0));
        assert_eq!(parse_at("-5"), Ok(-5));
        // A date string is NOT the wire grammar (the CLI resolves those).
        let err = parse_at("2026-07-30").unwrap_err();
        assert!(err.contains("expected unix seconds"), "{err}");
        assert!(parse_at("").is_err());
        assert!(parse_at("1.5").is_err());
    }

    #[test]
    fn http_date_is_rfc1123_gmt() {
        let v = http_date(1_700_000_000).unwrap();
        assert_eq!(v.to_str().unwrap(), "Tue, 14 Nov 2023 22:13:20 GMT");
    }

    /// The `?at=` body is the plain body PLUS one trailing key — pinned by
    /// serialising both and comparing the shared prefix. This is the
    /// "absent `at` ⇒ byte-identical" contract, checked from the response
    /// side rather than trusted to the two structs staying in sync.
    #[test]
    fn at_body_is_the_plain_body_plus_a_trailing_memento_key() {
        let versions = vec![v("WORKING", 300), v("deadbeef", 200)];
        let plain = serde_json::to_string(&VersionsResponse {
            mode: "auto",
            versions: versions.clone(),
        })
        .unwrap();
        let m = kb_core::versions::resolve_memento(&versions, 250);
        let with_at = serde_json::to_string(&VersionsAtResponse {
            mode: "auto",
            versions: versions.clone(),
            memento: MementoOut {
                at_unix: m.at_unix,
                relation: NEAREST_PRIOR,
                found: m.found(),
                exact: m.exact,
                version: m.version.cloned(),
                oldest_ts_unix: m.oldest_ts_unix,
                note: m.miss_note(),
            },
        })
        .unwrap();
        // Everything up to the plain body's closing brace is identical.
        let prefix = &plain[..plain.len() - 1];
        assert!(
            with_at.starts_with(prefix),
            "?at= body diverged from the plain body:\n plain: {plain}\n at:    {with_at}"
        );
        assert!(with_at[prefix.len()..].starts_with(",\"memento\":{"));
    }

    /// A hit says nearest-prior and names the version; `exact` stays false
    /// when the instant merely fell after it.
    #[test]
    fn memento_out_hit_is_honest_about_being_approximate() {
        let versions = vec![v("WORKING", 300), v("deadbeef", 200)];
        let m = kb_core::versions::resolve_memento(&versions, 250);
        let out = MementoOut {
            at_unix: m.at_unix,
            relation: NEAREST_PRIOR,
            found: m.found(),
            exact: m.exact,
            version: m.version.cloned(),
            oldest_ts_unix: m.oldest_ts_unix,
            note: m.miss_note(),
        };
        let j = serde_json::to_value(&out).unwrap();
        assert_eq!(j["relation"], "nearest-prior");
        assert_eq!(j["found"], true);
        assert_eq!(j["exact"], false);
        assert_eq!(j["version"]["ref"], "deadbeef");
        assert_eq!(j["at_unix"], 250);
        assert!(j.get("note").is_none(), "a hit carries no miss note");
    }

    /// A miss omits `version` entirely and carries the floor + note —
    /// there is no way to read it as "here is the oldest, close enough".
    #[test]
    fn memento_out_miss_omits_version_and_names_the_floor() {
        let versions = vec![v("WORKING", 300), v("deadbeef", 200)];
        let m = kb_core::versions::resolve_memento(&versions, 50);
        let out = MementoOut {
            at_unix: m.at_unix,
            relation: NEAREST_PRIOR,
            found: m.found(),
            exact: m.exact,
            version: m.version.cloned(),
            oldest_ts_unix: m.oldest_ts_unix,
            note: m.miss_note(),
        };
        let j = serde_json::to_value(&out).unwrap();
        assert_eq!(j["found"], false);
        assert!(j.get("version").is_none());
        assert_eq!(j["oldest_ts_unix"], 200);
        assert!(j["note"].as_str().unwrap().contains("that old"));
    }
}
