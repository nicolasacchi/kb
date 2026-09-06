//! `kb sessions status` (LSC-1/LSC-2/LSC-5, `docs/research/
//! kb-live-sessions-cockpit-2026-08.html` §8 "The CLI and the machine
//! surface") — the live-sessions snapshot: who holds the ball, right now,
//! grouped into the three lanes the operator asked for: IN PROGRESS,
//! WAITING ON YOU, FINISHED / COLD.
//!
//! **LSC-1 shipped `--local`** — direct-disk, zero daemon, the same
//! "offline path" precedent `kb sessions read --live` already established
//! (design §8: "`--local` is not a fallback; it is the honest primitive").
//!
//! **LSC-2 wires the daemon-backed mode** (`GET /api/sessions/live-status`,
//! design §6): [`run_daemon`] fetches the merged cockpit view and maps each
//! wire row back into a [`LiveSession`] so it flows through the EXACT SAME
//! filtering (`parse_state_filter`/project/harness) and presentation
//! (`group_lanes`/`print_lane`) code `--local` already uses — human and
//! `--json` output are identical in shape between the two modes by
//! construction (one filter/sort/render path, two ways to fill
//! `Vec<LiveSession>`), not by parallel re-implementation.
//!
//! **LSC-5 fans `--local` out across every harness**: the pure
//! classification work — the Claude Code adapter (`derive_state`,
//! `classify_claude_transcript`, `scan_claude_projects`) plus its four
//! siblings (codex/grok/kimi/opencode) and the unified `scan_all` fan-out —
//! lives in `kb_core::sessions::{live, live_adapters}`. This module stays
//! presentation only: root resolution, csv filter parsing, lane
//! grouping/sorting, and the two renderers (human text / `--json`).
//! `--root` overrides ONLY the Claude Code root (its pre-LSC-5 meaning,
//! unchanged); the other four harnesses use their real default locations
//! (`live_adapters::LiveRoots::with_defaults`) — no new per-harness root
//! flags in this phase, keeping the CLI surface stable. `print_lane` shows
//! each row's harness glyph (mirrors the SPA's `harnessGlyph` convention in
//! `web/src/lib/sessionChips.ts` exactly) and `group_lanes` breaks a
//! same-`since_secs` tie by `kb_core::sessions::HARNESSES`'s canonical
//! order — both apply to daemon-mode rows too, since `--local` and daemon
//! mode share this one render path.

use anyhow::{bail, Result};
use kb_core::sessions::live::{
    Confidence, Holder, LivePolicy, LiveSession, LiveState, StateSource,
};
use kb_core::sessions::live_adapters::{self, LiveRoots};
use kb_core::sessions::HARNESSES;
use std::path::{Path, PathBuf};

/// The validated Python spike's cheap-gate window (design §12's evidence
/// appendix cites this exact classifier): a transcript untouched for a full
/// week cannot possibly be live, so `scan_claude_projects` never even opens
/// it. Kept identical to the spike's own constant so `kb sessions status
/// --local`'s buckets are directly comparable to it (the work order's
/// cross-check requirement).
const MAX_AGE_SECS: i64 = 7 * 86_400;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Option<&PathBuf>,
    local: bool,
    root: Option<&PathBuf>,
    json: bool,
    state: Option<&str>,
    project: Option<&str>,
    harness: Option<&str>,
    limit: Option<usize>,
    no_color: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !local {
        let rows = fetch_daemon_rows(daemon, bearer).await?;
        return filter_and_render(
            rows,
            json,
            state,
            project,
            harness,
            limit,
            no_color,
            "no live sessions reported by the daemon (POST /api/sessions/beat \
             hasn't landed one yet, and no recent capture qualifies for the \
             Tier-0 degraded view)",
        );
    }

    let root_dir = resolve_root(config, root)?;
    let now_unix = chrono::Utc::now().timestamp();
    let policy = LivePolicy::default();
    // LSC-5: fan out across every harness. `claude` is pinned to the
    // already-resolved root (config/`--root`/default ladder, unchanged);
    // `with_defaults()` fills codex/grok/kimi/opencode with their real
    // default locations — never overwriting an explicit override, and
    // there are none to overwrite here since this phase adds no new
    // per-harness root flags.
    let roots = LiveRoots {
        claude: Some(root_dir.clone()),
        ..Default::default()
    }
    .with_defaults();
    let rows = live_adapters::scan_all(&roots, now_unix, &policy, MAX_AGE_SECS);
    filter_and_render(
        rows,
        json,
        state,
        project,
        harness,
        limit,
        no_color,
        &format!(
            "no live sessions found across claude ({}) / codex / opencode / grok / kimi \
             (touched within {})",
            root_dir.display(),
            fmt_elapsed(MAX_AGE_SECS)
        ),
    )
}

/// LSC-2 — `GET /api/sessions/live-status`, mapped back into `LiveSession`
/// rows (via [`live_session_from_wire`]) so daemon mode shares
/// [`filter_and_render`] byte-for-byte with `--local`. A row that fails to
/// parse (unrecognised `holder`/`state`/`source`/`confidence` — a future
/// daemon version skew) is skipped rather than failing the whole command;
/// an honest partial list beats an error over one bad row.
async fn fetch_daemon_rows(daemon: Option<&str>, bearer: Option<&str>) -> Result<Vec<LiveSession>> {
    let url = crate::commands::sessions::require_daemon(daemon, bearer).await?;
    let client = crate::http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/live-status"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let raw = body["sessions"].as_array().unwrap_or(&empty);
    Ok(raw.iter().filter_map(live_session_from_wire).collect())
}

/// One `kb-server::routes::sessions::LiveStatusRow` JSON object → a
/// `LiveSession`. Deliberately re-parses the wire JSON by hand (rather than
/// importing the server's struct directly) — the established convention in
/// this crate for daemon wire shapes (see `commands/refs.rs`'s doc
/// comments); a hand-parsed mapping degrades gracefully (returns `None`)
/// instead of a hard `serde` error on an unrecognised enum string. Fields
/// with no wire equivalent (`transcript_path`, `version`) get honest
/// placeholders; `why` carries the `blocked`/`detail_reason` flag when set,
/// since it's the only place LSC-1's presenter renders that context.
fn live_session_from_wire(v: &serde_json::Value) -> Option<LiveSession> {
    let session_id = v["session_id"].as_str()?.to_string();
    let harness = v["harness"].as_str().unwrap_or("claude").to_string();
    let holder = match v["holder"].as_str()? {
        "agent" => Holder::Agent,
        "human" => Holder::Human,
        "none" => Holder::Ended,
        _ => return None,
    };
    let state = match v["state"].as_str()? {
        "working" => LiveState::Working,
        "stalled" => LiveState::Stalled,
        "waiting" => LiveState::Waiting,
        "cold" => LiveState::Cold,
        "finished" => LiveState::Finished,
        "presumed_ended" => LiveState::PresumedEnded,
        _ => return None,
    };
    let source = match v["source"].as_str()? {
        "hook" => StateSource::Hook,
        "transcript" => StateSource::Transcript,
        "capture" => StateSource::Capture,
        _ => return None,
    };
    let confidence = match v["confidence"].as_str()? {
        "observed" => Confidence::Observed,
        "inferred" => Confidence::Inferred,
        "presumed" => Confidence::Presumed,
        _ => return None,
    };
    let blocked = v["blocked"].as_bool().unwrap_or(false);
    let detail_reason = v["detail_reason"].as_str().map(str::to_string);
    let why = if blocked {
        Some(detail_reason.unwrap_or_else(|| "blocked".to_string()))
    } else {
        None
    };

    Some(LiveSession {
        session_id,
        harness,
        holder,
        state,
        source,
        confidence,
        since_unix: v["since_unix"].as_i64().unwrap_or(0),
        since_secs: v["since_secs"].as_i64().unwrap_or(0),
        project: v["project"].as_str().map(str::to_string),
        cwd: v["cwd"].as_str().map(str::to_string),
        model: v["model"].as_str().map(str::to_string),
        title: v["title"].as_str().map(str::to_string),
        transcript_path: PathBuf::new(),
        resume: v["resume"].as_str().unwrap_or_default().to_string(),
        why,
        version: None,
    })
}

/// The shared filter → group → render tail of `run` for BOTH `--local` and
/// daemon mode (LSC-2): applies the three CLI filters, then
/// [`group_lanes`]/[`print_lane`] exactly as before — this is the ONE place
/// that decides what a `--json`/human snapshot looks like, so the two
/// source modes can never drift in presentation.
#[allow(clippy::too_many_arguments)]
fn filter_and_render(
    mut rows: Vec<LiveSession>,
    json: bool,
    state: Option<&str>,
    project: Option<&str>,
    harness: Option<&str>,
    limit: Option<usize>,
    no_color: bool,
    empty_message: &str,
) -> Result<()> {
    if let Some(s) = state {
        let wanted = parse_state_filter(s)?;
        rows.retain(|r| wanted.contains(&r.state));
    }
    if let Some(p) = project {
        rows.retain(|r| {
            r.project
                .as_deref()
                .map(|x| x.eq_ignore_ascii_case(p))
                .unwrap_or(false)
        });
    }
    if let Some(h) = harness {
        if !HARNESSES.iter().any(|known| known.eq_ignore_ascii_case(h)) {
            bail!(
                "unknown --harness value {h:?} (expected one of: {})",
                HARNESSES.join(", ")
            );
        }
        rows.retain(|r| r.harness.eq_ignore_ascii_case(h));
    }

    if rows.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("{empty_message}");
        }
        return Ok(());
    }

    let (in_progress, waiting, finished) = group_lanes(&rows, limit);

    if json {
        // The machine surface: one flat array, lane order preserved
        // (IN PROGRESS, WAITING ON YOU, FINISHED/COLD) so a caller that
        // doesn't care about lanes still gets a stable, sensible order.
        let all: Vec<&LiveSession> = in_progress
            .iter()
            .chain(waiting.iter())
            .chain(finished.iter())
            .copied()
            .collect();
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }

    print_lane("IN PROGRESS", &in_progress, no_color);
    print_lane("WAITING ON YOU", &waiting, no_color);
    print_lane("FINISHED / COLD", &finished, no_color);
    Ok(())
}

/// `--root` wins; else `[sessions] live_transcripts_dir` from `kb.toml`
/// (the SAME config `kb sessions read --live` reads, via the SAME
/// `resolve_config_path`/`load_config_or_default` pair); else the
/// universal default `~/.claude/projects`.
fn resolve_root(config: Option<&PathBuf>, explicit_root: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(r) = explicit_root {
        return Ok(r.clone());
    }
    let cfg_path = crate::commands::resolve_config_path(config)?;
    let cfg = crate::commands::load_config_or_default(&cfg_path)?;
    if let Some(dir) = cfg.sessions.resolved_live_dir() {
        return Ok(dir);
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --root explicitly"))?;
    Ok(Path::new(&home).join(".claude").join("projects"))
}

/// csv over the six-state vocabulary. Accepts both `presumed_ended` (the
/// wire spelling) and `presumed-ended` (friendlier to type).
fn parse_state_filter(s: &str) -> Result<Vec<LiveState>> {
    s.split(',')
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(|tok| match tok {
            "working" => Ok(LiveState::Working),
            "stalled" => Ok(LiveState::Stalled),
            "waiting" => Ok(LiveState::Waiting),
            "cold" => Ok(LiveState::Cold),
            "finished" => Ok(LiveState::Finished),
            "presumed_ended" | "presumed-ended" => Ok(LiveState::PresumedEnded),
            other => bail!(
                "unknown --state value {other:?} (expected one of: \
                 working, stalled, waiting, cold, finished, presumed_ended)"
            ),
        })
        .collect()
}

/// Split `rows` into the three operator-facing lanes and apply the
/// design's per-lane ordering (§7 "Ordering per lane"): waiting sorts
/// longest-wait-first (that's the "you are the bottleneck" signal),
/// in-progress sorts most-recently-active first, finished sorts
/// newest-first. `limit`, when present, caps EACH lane independently (so a
/// small `--limit` still shows a balanced snapshot across all three lanes
/// rather than letting a busy IN PROGRESS lane crowd out WAITING ON YOU
/// entirely) — this same per-lane cap is what `--json`'s flat array is
/// built from too, so human and machine output never disagree about which
/// rows survive a `--limit`.
fn group_lanes(
    rows: &[LiveSession],
    limit: Option<usize>,
) -> (Vec<&LiveSession>, Vec<&LiveSession>, Vec<&LiveSession>) {
    let mut in_progress: Vec<&LiveSession> = rows
        .iter()
        .filter(|r| matches!(r.state, LiveState::Working | LiveState::Stalled))
        .collect();
    let mut waiting: Vec<&LiveSession> = rows
        .iter()
        .filter(|r| matches!(r.state, LiveState::Waiting))
        .collect();
    let mut finished: Vec<&LiveSession> = rows
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                LiveState::Cold | LiveState::Finished | LiveState::PresumedEnded
            )
        })
        .collect();

    // Most-recently-active first == smallest since_secs first. A tie
    // (same since_secs, e.g. two rows with second-granularity timestamps
    // landing on the same wall-clock second) breaks by
    // `kb_core::sessions::HARNESSES`'s canonical order (LSC-5) — a
    // deterministic, documented tie-break rather than whatever order the
    // scan happened to discover the two harnesses in.
    in_progress.sort_by_key(|r| (r.since_secs, harness_rank(&r.harness)));
    // Longest-wait first == largest since_secs first; same tie-break.
    waiting.sort_by_key(|r| (std::cmp::Reverse(r.since_secs), harness_rank(&r.harness)));
    // Newest-first == smallest since_secs first; same tie-break.
    finished.sort_by_key(|r| (r.since_secs, harness_rank(&r.harness)));

    if let Some(n) = limit {
        in_progress.truncate(n);
        waiting.truncate(n);
        finished.truncate(n);
    }

    (in_progress, waiting, finished)
}

/// A per-lane debug prefix for states a merged lane doesn't otherwise name
/// (`IN PROGRESS` merges `working`+`stalled`; `FINISHED / COLD` merges
/// `cold`+`finished`+`presumed_ended`) — mirrors the design §3/§12
/// evidence's rendered `stalled? silent >45m` style. `None` when the lane
/// name alone already says everything (`working`, `waiting`).
fn state_prefix(state: LiveState) -> Option<&'static str> {
    match state {
        LiveState::Stalled => Some("stalled?"),
        LiveState::Cold => Some("cold"),
        LiveState::Finished => Some("finished"),
        LiveState::PresumedEnded => Some("presumed_ended?"),
        LiveState::Working | LiveState::Waiting => None,
    }
}

/// A row's position in `kb_core::sessions::HARNESSES`'s canonical order —
/// `usize::MAX` for an unrecognised harness string (a future wire skew),
/// so it sorts last rather than panicking or silently reordering the
/// known set.
fn harness_rank(harness: &str) -> usize {
    HARNESSES
        .iter()
        .position(|h| h.eq_ignore_ascii_case(harness))
        .unwrap_or(usize::MAX)
}

/// One glyph per harness — byte-identical to the SPA's `harnessGlyph`
/// (`web/src/lib/sessionChips.ts`): claude=◆, codex=⬢, opencode=⬡,
/// grok=✦, kimi=☾, omp=π, unrecognised=● (that file's own fallback glyph).
/// Kept in lock-step by inspection (no shared source between a Rust CLI
/// and a TS module) — the SPA's own `harnessGlyph` test pins the same six
/// mappings.
fn harness_glyph(harness: &str) -> &'static str {
    match harness {
        "claude" => "◆",
        "codex" => "⬢",
        "opencode" => "⬡",
        "grok" => "✦",
        "kimi" => "☾",
        "omp" => "π",
        _ => "●",
    }
}

fn print_lane(name: &str, rows: &[&LiveSession], no_color: bool) {
    if rows.is_empty() {
        return;
    }
    let header = format!("=== {name} ({})", rows.len());
    println!("{}", if no_color { header } else { bold(&header) });
    for r in rows {
        let elapsed = fmt_elapsed(r.since_secs);
        let project = r.project.as_deref().unwrap_or("-");
        let title = r.title.as_deref().unwrap_or("(no title)");
        let sid8: String = r.session_id.chars().take(8).collect();
        let glyph = harness_glyph(&r.harness);
        let tag = match (state_prefix(r.state), r.why.as_deref()) {
            (Some(prefix), Some(why)) => format!("{prefix} {why}"),
            (Some(prefix), None) => prefix.to_string(),
            (None, Some(why)) => why.to_string(),
            (None, None) => String::new(),
        };
        let line = format!(
            "  {elapsed:>6}  {glyph} {:<16}  {:<52}  {sid8}  [{tag}]",
            crate::commands::sessions::truncate(project, 16),
            crate::commands::sessions::truncate(title, 52),
        );
        println!(
            "{}",
            if no_color {
                line
            } else {
                dim_tail(&line, &tag)
            }
        );
    }
    println!();
}

/// Humanised elapsed time: `45s` / `12m` / `1h23m` / `2d` — the exact
/// grammar the work order specifies.
fn fmt_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// SGR bold — mirrors `session_read.rs`'s own `bold`/`dim` convention (each
/// terminal presenter in this crate owns its own tiny color helpers rather
/// than sharing one, since the `--no-color` gate is always applied at the
/// call site, not centrally).
fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}

/// Dim JUST the trailing `[...]` debug tag on an already-formatted row
/// line — the columns before it (elapsed/project/title/sid) stay plain so
/// the row is still scannable; the tag is "meta, not content" (same
/// rationale as `session_read.rs`'s elision/hint lines).
fn dim_tail(line: &str, tag: &str) -> String {
    let bracketed = format!("[{tag}]");
    match line.rfind(&bracketed) {
        Some(idx) => format!("{}\x1b[2m{bracketed}\x1b[0m", &line[..idx],),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::sessions::live::{Confidence, Holder, StateSource};
    use std::path::PathBuf;

    fn row(session_id: &str, state: LiveState, since_secs: i64, project: &str) -> LiveSession {
        LiveSession {
            session_id: session_id.to_string(),
            harness: "claude".to_string(),
            holder: match state {
                LiveState::Working | LiveState::Stalled => Holder::Agent,
                LiveState::Waiting | LiveState::Cold => Holder::Human,
                LiveState::Finished | LiveState::PresumedEnded => Holder::Ended,
            },
            state,
            source: StateSource::Transcript,
            confidence: Confidence::Inferred,
            since_unix: 0,
            since_secs,
            project: Some(project.to_string()),
            cwd: None,
            model: None,
            title: Some("t".to_string()),
            transcript_path: PathBuf::from("/tmp/x.jsonl"),
            resume: format!("claude -r {session_id}"),
            why: Some("stop_reason=tool_use, tail=assistant".to_string()),
            version: None,
        }
    }

    #[test]
    fn fmt_elapsed_matches_the_specified_grammar() {
        assert_eq!(fmt_elapsed(45), "45s");
        assert_eq!(fmt_elapsed(12 * 60), "12m");
        assert_eq!(fmt_elapsed(3_600 + 23 * 60), "1h23m");
        assert_eq!(fmt_elapsed(2 * 86_400), "2d");
    }

    #[test]
    fn parse_state_filter_accepts_csv_and_rejects_unknown_tokens() {
        let got = parse_state_filter("waiting, cold").unwrap();
        assert_eq!(got, vec![LiveState::Waiting, LiveState::Cold]);
        assert!(parse_state_filter("bogus").is_err());
    }

    #[test]
    fn group_lanes_sorts_waiting_longest_wait_first() {
        let rows = vec![
            row("a", LiveState::Waiting, 60, "kb"),
            row("b", LiveState::Waiting, 3_000, "kb"),
            row("c", LiveState::Waiting, 600, "kb"),
        ];
        let (_, waiting, _) = group_lanes(&rows, None);
        let ids: Vec<&str> = waiting.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn group_lanes_sorts_in_progress_most_recently_active_first() {
        let rows = vec![
            row("a", LiveState::Working, 600, "kb"),
            row("b", LiveState::Stalled, 3_000, "kb"),
            row("c", LiveState::Working, 30, "kb"),
        ];
        let (in_progress, _, _) = group_lanes(&rows, None);
        let ids: Vec<&str> = in_progress.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn group_lanes_merges_stalled_into_in_progress_and_cold_into_finished() {
        let rows = vec![
            row("a", LiveState::Working, 10, "kb"),
            row("b", LiveState::Stalled, 3_000, "kb"),
            row("c", LiveState::Cold, 40_000, "kb"),
            row("d", LiveState::Waiting, 100, "kb"),
        ];
        let (in_progress, waiting, finished) = group_lanes(&rows, None);
        assert_eq!(in_progress.len(), 2);
        assert_eq!(waiting.len(), 1);
        assert_eq!(finished.len(), 1);
    }

    #[test]
    fn group_lanes_limit_is_applied_per_lane_independently() {
        let rows = vec![
            row("a", LiveState::Working, 10, "kb"),
            row("b", LiveState::Working, 20, "kb"),
            row("c", LiveState::Waiting, 100, "kb"),
        ];
        let (in_progress, waiting, _) = group_lanes(&rows, Some(1));
        assert_eq!(in_progress.len(), 1);
        assert_eq!(waiting.len(), 1);
    }

    #[test]
    fn state_prefix_names_stalled_and_cold_but_not_working_or_waiting() {
        assert_eq!(state_prefix(LiveState::Stalled), Some("stalled?"));
        assert_eq!(state_prefix(LiveState::Cold), Some("cold"));
        assert_eq!(state_prefix(LiveState::Working), None);
        assert_eq!(state_prefix(LiveState::Waiting), None);
    }

    /// LSC-5: `harness_glyph` must be byte-identical to the SPA's
    /// `harnessGlyph` (`web/src/lib/sessionChips.test.ts`'s pinned
    /// mapping) — same six harnesses (OK1 added `omp`), same fallback
    /// glyph.
    #[test]
    fn harness_glyph_matches_the_spa_convention() {
        assert_eq!(harness_glyph("claude"), "◆");
        assert_eq!(harness_glyph("codex"), "⬢");
        assert_eq!(harness_glyph("opencode"), "⬡");
        assert_eq!(harness_glyph("grok"), "✦");
        assert_eq!(harness_glyph("kimi"), "☾");
        assert_eq!(harness_glyph("omp"), "π");
        assert_eq!(harness_glyph("some-future-harness"), "●");
    }

    #[test]
    fn harness_rank_follows_the_closed_set_order_and_is_case_insensitive() {
        assert_eq!(harness_rank("claude"), 0);
        assert_eq!(harness_rank("codex"), 1);
        assert_eq!(harness_rank("opencode"), 2);
        assert_eq!(harness_rank("grok"), 3);
        assert_eq!(harness_rank("kimi"), 4);
        assert_eq!(harness_rank("KIMI"), 4);
        assert_eq!(harness_rank("omp"), 5);
        assert_eq!(harness_rank("bogus"), usize::MAX);
    }

    /// LSC-5: a same-`since_secs` tie breaks by the canonical harness
    /// order, not scan-discovery order.
    #[test]
    fn group_lanes_breaks_a_since_secs_tie_by_canonical_harness_order() {
        let mut a = row("a", LiveState::Working, 100, "kb");
        a.harness = "kimi".to_string();
        let mut b = row("b", LiveState::Working, 100, "kb");
        b.harness = "claude".to_string();
        let mut c = row("c", LiveState::Working, 100, "kb");
        c.harness = "codex".to_string();
        let rows = [a, b, c];
        let (in_progress, _, _) = group_lanes(&rows, None);
        let ids: Vec<&str> = in_progress.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn filter_and_render_rejects_an_unknown_harness_value() {
        let rows = vec![row("a", LiveState::Working, 10, "kb")];
        let err = filter_and_render(rows, true, None, None, Some("bogus"), None, true, "empty")
            .unwrap_err();
        assert!(err.to_string().contains("unknown --harness value"));
    }
}
