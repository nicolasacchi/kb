//! `kb timeline` — four synchronized, day-bucketed lanes (created / read /
//! session / comment) over `GET /api/kb/{kb}/timeline` (C-a). Modeled on
//! `kb resurface`: HTTP-only (no offline mode — the lanes are storage-actor
//! reads), `--json` echoes the daemon body verbatim + the resolved kb/source.
//!
//! Deliberately NOT a dashboard: no streak line, no "best day", no goal, no
//! `--follow` (the codebase's recorded non-goal is no streaks/badges/
//! gamification — see root `CLAUDE.md`). Human output is one fixed-width
//! density sparkline per track plus a plain count, e.g.
//!
//! ```text
//! created  ▁▂▅█▃▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁  142
//! ```
//!
//! `--ids` dumps the resolved artifact-id set (one id per line) for the
//! selected tracks, so an agent can pipe it straight into another `kb`
//! verb — mirroring the server's own pivot-into-gallery intent (`?ids=`,
//! invariant #35) without the CLI reaching into the gallery itself.

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::http;

/// Terminal width for the density sparkline — independent of the window
/// length (the default server-side window is a full trailing year, invariant
/// #C-a §W2.10 parity); wide windows bin down, narrow windows still render
/// one bar per day up to this width.
const SPARKLINE_WIDTH: usize = 40;

/// Every track the server returns, in the fixed display order the server's
/// `lanes` array uses (`created, read, session, comment`).
const ALL_TRACKS: [&str; 4] = ["created", "read", "session", "comment"];

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    tracks: Option<&str>,
    ids: bool,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })?;
    let kb_name = http::resolve_default_kb(kb, daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;

    let selected = parse_tracks(tracks)?;

    let from_unix = from.map(parse_time_bound).transpose()?;
    let to_unix = to.map(parse_time_bound).transpose()?;
    let mut req_url = format!(
        "{url}/api/kb/{}/timeline",
        http::encode_path_segment(&kb_name),
    );
    let mut qs = Vec::new();
    if let Some(f) = from_unix {
        qs.push(format!("from={f}"));
    }
    if let Some(t) = to_unix {
        qs.push(format!("to={t}"));
    }
    if !qs.is_empty() {
        req_url.push('?');
        req_url.push_str(&qs.join("&"));
    }

    let body: Value = client
        .get(&req_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if json {
        // Echo the daemon body verbatim + the resolved kb/source, mirroring
        // `kb resurface --json`.
        let mut out = body.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("kb".into(), Value::String(kb_name));
            obj.insert("source".into(), Value::String(url));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if ids {
        print_ids(&body, &selected);
        return Ok(());
    }

    print_human(&body, &kb_name, &selected);
    Ok(())
}

/// Parse `--tracks created,read,session,comment` (csv, any subset, any
/// order) into the fixed server display order; `None`/empty means "all
/// four". Errors on an unrecognised track name rather than silently
/// dropping it.
fn parse_tracks(s: Option<&str>) -> Result<Vec<&'static str>> {
    let Some(s) = s.filter(|s| !s.trim().is_empty()) else {
        return Ok(ALL_TRACKS.to_vec());
    };
    let requested: Vec<String> = s
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    for r in &requested {
        if !ALL_TRACKS.contains(&r.as_str()) {
            return Err(anyhow!(
                "unknown --tracks value {r:?}: expected any of {}",
                ALL_TRACKS.join(",")
            ));
        }
    }
    Ok(ALL_TRACKS
        .into_iter()
        .filter(|t| requested.iter().any(|r| r == t))
        .collect())
}

/// Parse a `--from`/`--to` bound: either a bare unix-seconds integer or a
/// `YYYY-MM-DD` calendar date (UTC midnight) — the same grammar
/// `commands::search`'s (private) `parse_time_bound` uses for
/// `--read-from`/`--read-to`; duplicated here rather than exposed
/// cross-module (a small pure helper — this crate already has several such
/// per-command twins, e.g. `commands::search`'s own).
fn parse_time_bound(s: &str) -> Result<i64> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        anyhow!("invalid --from/--to {s:?}: expected unix seconds or YYYY-MM-DD ({e})")
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("invalid date {s:?}"))?;
    Ok(dt.and_utc().timestamp())
}

fn fmt_date(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| unix.to_string())
}

/// Bin `counts` (one entry per UTC day, ascending) down to `width` bars via
/// contiguous-chunk sums, then render an 8-level unicode density bar per
/// bin scaled to THIS series' own max (never a cross-track scale — each
/// track's bar is independently legible). A count series shorter than
/// `width` renders one bar per day (no binning needed).
fn sparkline(counts: &[i64], width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if counts.is_empty() {
        return String::new();
    }
    let bins = bin_sums(counts, width.max(1));
    let max = bins.iter().copied().max().unwrap_or(0);
    bins.iter()
        .map(|&v| {
            if max == 0 {
                LEVELS[0]
            } else {
                let idx = ((v as f64 / max as f64) * (LEVELS.len() - 1) as f64).round() as usize;
                LEVELS[idx.min(LEVELS.len() - 1)]
            }
        })
        .collect()
}

/// Split `counts` into at most `width` contiguous chunks (as even as
/// integer division allows) and sum each — pure, so the binning is
/// unit-testable independent of rendering.
fn bin_sums(counts: &[i64], width: usize) -> Vec<i64> {
    let n = counts.len();
    if n <= width {
        return counts.to_vec();
    }
    let mut out = Vec::with_capacity(width);
    for i in 0..width {
        let start = i * n / width;
        let end = ((i + 1) * n / width).max(start + 1);
        out.push(counts[start..end].iter().sum());
    }
    out
}

fn lane_counts(lane: &Value) -> Vec<i64> {
    lane["days"]
        .as_array()
        .map(|d| d.iter().map(|x| x["count"].as_i64().unwrap_or(0)).collect())
        .unwrap_or_default()
}

fn print_human(body: &Value, kb: &str, selected: &[&str]) {
    let lanes = body["lanes"].as_array().cloned().unwrap_or_default();
    let from = body["from"].as_i64().unwrap_or(0);
    let to = body["to"].as_i64().unwrap_or(0);
    println!(
        "timeline  [{kb}]  {} .. {} (UTC)",
        fmt_date(from),
        fmt_date(to)
    );
    for lane in &lanes {
        let track = lane["track"].as_str().unwrap_or("?");
        if !selected.contains(&track) {
            continue;
        }
        let total = lane["total"].as_i64().unwrap_or(0);
        let counts = lane_counts(lane);
        let spark = sparkline(&counts, SPARKLINE_WIDTH);
        println!("{track:<8} {spark}  {total:>4}");
    }
    for lane in &lanes {
        let track = lane["track"].as_str().unwrap_or("?");
        if !selected.contains(&track) {
            continue;
        }
        if lane["truncated"].as_bool().unwrap_or(false) {
            println!(
                "  ({track}: resolved id set truncated at the daemon's cap — use --from/--to to narrow)"
            );
        }
    }
}

fn print_ids(body: &Value, selected: &[&str]) {
    let lanes = body["lanes"].as_array().cloned().unwrap_or_default();
    for lane in &lanes {
        let track = lane["track"].as_str().unwrap_or("?");
        if !selected.contains(&track) {
            continue;
        }
        if let Some(ids) = lane["ids"].as_array() {
            for id in ids {
                if let Some(s) = id.as_str() {
                    println!("{s}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_bound_accepts_unix_seconds() {
        assert_eq!(parse_time_bound("1700000000").unwrap(), 1_700_000_000);
        assert_eq!(parse_time_bound("-5").unwrap(), -5);
    }

    #[test]
    fn parse_time_bound_accepts_calendar_date_at_utc_midnight() {
        assert_eq!(parse_time_bound("2024-01-01").unwrap(), 1_704_067_200);
    }

    #[test]
    fn parse_time_bound_rejects_garbage() {
        assert!(parse_time_bound("not-a-date").is_err());
        assert!(parse_time_bound("2024-13-99").is_err());
    }

    #[test]
    fn parse_tracks_defaults_to_all_four_in_fixed_order() {
        assert_eq!(
            parse_tracks(None).unwrap(),
            vec!["created", "read", "session", "comment"]
        );
        assert_eq!(
            parse_tracks(Some("")).unwrap(),
            vec!["created", "read", "session", "comment"]
        );
    }

    #[test]
    fn parse_tracks_reorders_to_fixed_display_order() {
        assert_eq!(
            parse_tracks(Some("comment,created")).unwrap(),
            vec!["created", "comment"]
        );
    }

    #[test]
    fn parse_tracks_rejects_unknown_track() {
        assert!(parse_tracks(Some("created,bogus")).is_err());
    }

    #[test]
    fn bin_sums_passthrough_when_shorter_than_width() {
        assert_eq!(bin_sums(&[1, 2, 3], 40), vec![1, 2, 3]);
    }

    #[test]
    fn bin_sums_splits_evenly_and_sums_each_chunk() {
        // 8 days into 4 bins → 2 days per bin.
        let counts = vec![1, 1, 2, 2, 3, 3, 4, 4];
        assert_eq!(bin_sums(&counts, 4), vec![2, 4, 6, 8]);
    }

    #[test]
    fn sparkline_all_zero_renders_lowest_level_everywhere() {
        let s = sparkline(&[0, 0, 0], 40);
        assert_eq!(s, "▁▁▁");
    }

    #[test]
    fn sparkline_scales_to_series_own_max() {
        let s = sparkline(&[0, 10], 40);
        assert_eq!(s.chars().next().unwrap(), '▁');
        assert_eq!(s.chars().nth(1).unwrap(), '█');
    }
}
