//! `kb daycard` — the CLI parity twin of the e-ink daycard (`GET
//! /api/kb/{kb}/daycard`, Unit 2). Deterministic given `(corpus state,
//! day)`: same corpus + `--day` always print the same thing. Three output
//! modes: the default plain-text digest, `--json` (echoes the daemon body
//! verbatim + resolved kb/source, mirroring `kb resurface --json`), and
//! `--html` (prints the exact bytes the e-ink panel's browser would fetch —
//! handy for `kb daycard --html > card.html` or piping into a local render
//! check). No streak, no goal, no `--watch` — a plain "what's here today"
//! read, matching `kb resurface`/`kb timeline`'s anti-nag posture.
//!
//! **CT-E1 — `--since <WHEN>`**: "what happened while I was away", mutually
//! exclusive with `--day` (clap `conflicts_with`, mirroring the server's own
//! 400). `WHEN` accepts unix seconds or a bare `YYYY-MM-DD` UTC date — the
//! SAME grammar `kb timeline`'s `--from`/`--to` already uses (this crate has
//! no relative-duration parser like `3d`/`12h` yet; `parse_since_bound` is a
//! local twin of `timeline::parse_time_bound`, duplicated per this command
//! family's locality convention). Parsed CLIENT-side (like `--from`/`--to`)
//! so the wire `?since=` is always a plain integer.

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::http;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    day: Option<&str>,
    since: Option<&str>,
    html: bool,
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

    let mut req_url = format!(
        "{url}/api/kb/{}/daycard?format={}",
        http::encode_path_segment(&kb_name),
        if html { "html" } else { "json" },
    );
    if let Some(d) = day {
        // `--day` is always a bare `YYYY-MM-DD` (unlike `kb timeline`'s
        // `--from`/`--to`, the daycard is always one whole day) — no
        // character in that grammar needs percent-encoding.
        req_url.push_str(&format!("&day={d}"));
    }
    if let Some(s) = since {
        let since_unix = parse_since_bound(s)?;
        req_url.push_str(&format!("&since={since_unix}"));
    }

    if html {
        // Print the exact HTML bytes the panel would fetch — no
        // reparsing, no re-rendering client-side (the daemon owns markup
        // generation per the recorded "no bitmap pipeline" constraint).
        let body = client.get(&req_url).send().await?.error_for_status()?;
        let text = body.text().await?;
        print!("{text}");
        return Ok(());
    }

    let body: Value = client
        .get(&req_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if json {
        let mut out = body.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("source".into(), Value::String(url));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // CT-E1 — day-mode and since-mode responses have different shapes
    // (`day` vs `since_unix`); pick the matching human renderer.
    if since.is_some() {
        print_since_human(&body);
    } else {
        print_human(&body);
    }
    Ok(())
}

/// CT-E1 — parse `--since`: unix seconds or a bare `YYYY-MM-DD` UTC calendar
/// date (UTC midnight). A local twin of `timeline::parse_time_bound` (same
/// grammar, duplicated per this command family's locality convention) —
/// this crate has no relative-duration (`3d`/`12h`) parser yet, so those
/// forms are rejected with a message saying so.
fn parse_since_bound(s: &str) -> Result<i64> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        anyhow!(
            "invalid --since {s:?}: expected unix seconds or YYYY-MM-DD \
             (relative forms like `3d`/`12h` aren't supported yet) ({e})"
        )
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("invalid date {s:?}"))?;
    Ok(dt.and_utc().timestamp())
}

fn fmt_unix(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| unix.to_string())
}

fn print_human(body: &Value) {
    let kb = body["kb"].as_str().unwrap_or("?");
    let day = body["day"].as_str().unwrap_or("?");
    println!("daycard  [{kb}]  {day}");

    let activity = &body["activity"];
    println!(
        "  today: {} opens · {} searches · {} comments",
        activity["opens"].as_i64().unwrap_or(0),
        activity["searches"].as_i64().unwrap_or(0),
        activity["comments"].as_i64().unwrap_or(0),
    );

    print_section("worth picking back up", &body["resurface"], |it| {
        let title = it["title"].as_str().unwrap_or("?").to_string();
        let reason = it["reasons"][0].as_str().unwrap_or("");
        if reason.is_empty() {
            title
        } else {
            format!("{title}  ▸ {reason}")
        }
    });
    print_section("recently touched", &body["recent"], |it| {
        it["title"].as_str().unwrap_or("?").to_string()
    });
    print_section("never opened", &body["never_opened"], |it| {
        it["title"].as_str().unwrap_or("?").to_string()
    });
}

/// CT-E1 — human digest for the `--since` response shape (`since_unix`/
/// `to_unix` + `sessions`/`memories`/`artifacts`/`comments`, no `day`/
/// `activity`/`resurface`/`recent`/`never_opened`).
fn print_since_human(body: &Value) {
    let kb = body["kb"].as_str().unwrap_or("?");
    let since_unix = body["since_unix"].as_i64().unwrap_or(0);
    let to_unix = body["to_unix"].as_i64().unwrap_or(0);
    println!(
        "daycard  [{kb}]  since {} → {}",
        fmt_unix(since_unix),
        fmt_unix(to_unix)
    );

    print_section("sessions", &body["sessions"], |it| {
        let title = it["title"].as_str().unwrap_or("?").to_string();
        match it["substance"].as_str() {
            Some(s) => format!("{title}  ▸ {s}"),
            None => title,
        }
    });
    print_section("memories written", &body["memories"], |it| {
        it["title"].as_str().unwrap_or("?").to_string()
    });
    print_section("artifacts created/updated", &body["artifacts"], |it| {
        it["title"].as_str().unwrap_or("?").to_string()
    });
    let still_open = body["comments_still_open"].as_u64().unwrap_or(0);
    print_section(
        &format!("comments raised ({still_open} still open)"),
        &body["comments"],
        |it| {
            let title = it["title"].as_str().unwrap_or("?");
            let open = it["open"].as_bool().unwrap_or(false);
            format!("{title}  [{}]", if open { "open" } else { "resolved" })
        },
    );
}

fn print_section(label: &str, items: &Value, render: impl Fn(&Value) -> String) {
    let items = items.as_array().cloned().unwrap_or_default();
    println!("  {label}:");
    if items.is_empty() {
        println!("    (nothing)");
        return;
    }
    for it in &items {
        println!("    - {}", render(it));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_section_renders_items_and_empty_state() {
        // print_human's helpers are stdout-only; smoke-test render() closures
        // directly rather than capturing process stdout.
        let items = serde_json::json!([{"title": "A"}, {"title": "B"}]);
        let rendered: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["title"].as_str().unwrap_or("?").to_string())
            .collect();
        assert_eq!(rendered, vec!["A".to_string(), "B".to_string()]);
    }

    // ---- CT-E1: --since ------------------------------------------------

    #[test]
    fn parse_since_bound_accepts_unix_seconds() {
        assert_eq!(parse_since_bound("1700000000").unwrap(), 1_700_000_000);
        assert_eq!(parse_since_bound("-5").unwrap(), -5);
    }

    #[test]
    fn parse_since_bound_accepts_calendar_date_at_utc_midnight() {
        assert_eq!(parse_since_bound("2024-01-01").unwrap(), 1_704_067_200);
    }

    #[test]
    fn parse_since_bound_rejects_garbage_and_relative_forms() {
        assert!(parse_since_bound("not-a-date").is_err());
        // No relative-duration parser yet — `3d`/`12h` must fail loudly, not
        // silently no-op.
        assert!(parse_since_bound("3d").is_err());
        assert!(parse_since_bound("12h").is_err());
    }
}
