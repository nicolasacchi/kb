//! `kb sessions read <sid>` — the interpreted terminal transcript reader
//! (sessions-rethink W4, cli-grok Proposal 1 / synthesis memo R1/R13). The
//! CLI's presenter over the SAME `session-view/1` engine the HTML renderer
//! (W1) and the `GET /api/sessions/{sid}/view` wire (W2) already consume —
//! ONE interpretation, three presenters; nothing here re-derives turn/item
//! semantics, only formats the wire the daemon already built.
//!
//! Default output (PF-R1): header (title/harness/project/asked/closed/
//! honest metrics) + the last `--tail` turns + an outcome footer, fetched
//! as exactly that TAIL WINDOW over the wire (`?turns=<n>`) rather than the
//! whole transcript sliced client-side — the default read no longer pays
//! to transfer turns it won't show. `--full` is the one escape hatch (no
//! separate `--all` — it already means "no windowing", so a second flag
//! would just duplicate it): disables windowing both on the wire
//! (`?turns=all`) and in the renderer. `--turn` selects an explicit ordinal
//! range (also fetches `?turns=all`, since the requested ordinal may sit
//! outside the default tail window); `--grep` filters to matching turns ±
//! `--context` (same — fetches everything so an earlier match is never
//! silently missed). `--raw` streams the decoded JSONL (`GET /{sid}/raw`,
//! unaffected by turn windowing — a separate route); `--json` prints the
//! `session-view/1` wire verbatim, windowed exactly like the default text
//! render unless combined with `--full`/`--turn`/`--grep`. A `$PAGER`
//! (default `less -RFX`) auto-spawns on a tty when the rendered output is
//! long.
//!
//! The formatter (`render_view`) is a PURE function over the wire's JSON
//! shape (mirrors `commands::sessions::render_replay`'s convention) so it is
//! directly golden-testable without a daemon — see the `tests` module, which
//! builds its fixtures by running `kb_core::sessions::view::session_view`
//! over the `crates/kb-core/tests/session_fixtures/*.jsonl` corpus (never
//! hand-written view JSON) and pins the rendered TEXT. Those fixtures are
//! SYNTHETIC transcripts — real capture SHAPES, invented content.
//!
//! **W7 (R15/LF-6) `--live`/`--follow`** — a SECOND, direct-disk entry point
//! composing with the above rather than forking it: `--live` renders the
//! LIVE JSONL (`[sessions] live_transcripts_dir`, resolved via the SAME
//! `kb_core::sessions::tail::resolve_live_transcript` the daemon's `/live`
//! route uses, LF-4) through the identical `session_view` engine +
//! `render_view`/`render_turn` presenter — one interpretation, one
//! presenter, two data sources. Zero daemon required (R9a's construction);
//! `--follow` (implies `--live`) additionally polls the file every 750ms
//! (`TailReader` + `view_append`) and prints each newly-closed turn as it
//! lands, Ctrl-C to exit. `--json --follow` prints one `ViewEvent` per
//! line (NDJSON) instead of the formatted text — the scripting surface.

use crate::commands::sessions::{require_daemon, truncate};
use crate::http;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

/// PF-R1 — default tail-window turn count requested from the server
/// (`?turns=<n>`) when no `--full`/`--turn`/`--grep` was given; `--tail N`
/// overrides it. Used to build the fetch URL in `run` — the wire arrives
/// already windowed, so `render_view` never slices client-side anymore
/// (the old fixed 3-turn HEAD window this const's doc comment used to pair
/// with is retired along with it: a head window needs the FULL transcript
/// to compute, which is exactly the fetch this const exists to avoid).
const DEFAULT_TAIL_TURNS: usize = 10;
const DEFAULT_WIDTH: usize = 100;

/// Presentation options — everything `render_view` needs, decoupled from
/// clap so the formatter stays testable with plain values. PF-R1: no
/// `tail` field — the tail-window SIZE only matters when building the
/// fetch URL (`run`, before the wire is even parsed); `render_view` just
/// renders whatever turns the (already-windowed) wire handed it.
pub struct ReadOpts {
    pub full: bool,
    pub turn: Option<String>,
    pub grep: Option<String>,
    pub context: u32,
    pub no_color: bool,
    pub width: usize,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Option<&PathBuf>,
    session_id: &str,
    full: bool,
    tail: Option<u32>,
    turn: Option<&str>,
    grep: Option<&str>,
    context: u32,
    raw: bool,
    live: bool,
    follow: bool,
    json: bool,
    no_color: bool,
    width: Option<usize>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if live {
        return run_live(
            config, session_id, follow, grep, raw, json, no_color, width, daemon, bearer,
        )
        .await;
    }

    let url = require_daemon(daemon, bearer).await?;
    let seg = http::encode_path_segment(session_id);

    if raw {
        // `--raw` — the least-mediated surface: the decoded transcript
        // JSONL, verbatim (scrub-floored non-loopback exactly like the
        // route itself — nothing extra to do CLI-side).
        if let Some(turn_spec) = turn {
            // `--raw --turn N`: slice the raw transcript to only the
            // JSONL lines belonging to the specified turn(s).
            return run_raw_with_turn(&url, &seg, turn_spec, bearer).await;
        }
        let client = http::client_with_timeout_and_bearer(60, bearer)?;
        let text = client
            .get(format!("{url}/api/sessions/{seg}/raw"))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        return print_paged(&text);
    }

    let client = http::client_with_timeout_and_bearer(60, bearer)?;
    // PF-R1: only the true default (no --full/--turn/--grep) benefits from
    // the server's windowed default — those three modes need to see turns
    // outside the tail (an explicit ordinal, an earlier grep match, or
    // literally everything), so they ask for `?turns=all` explicitly (the
    // wire-safety rule: the full transcript now requires an explicit ask).
    let turns_param = if full || turn.is_some() || grep.is_some() {
        "all".to_string()
    } else {
        tail.unwrap_or(DEFAULT_TAIL_TURNS as u32).to_string()
    };
    let body = client
        .get(format!("{url}/api/sessions/{seg}/view?turns={turns_param}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    if json {
        // The `session-view/1` wire, verbatim — the universal rule (memo
        // R13/cli.md §c): `--json` always means "the server's own bytes"
        // (windowed exactly like the text render above, per `turns_param`).
        return print_paged(&body);
    }

    let v: serde_json::Value = serde_json::from_str(&body).context("view response was not JSON")?;
    let opts = ReadOpts {
        full,
        turn: turn.map(str::to_string),
        grep: grep.map(str::to_string),
        context,
        no_color,
        width: width.unwrap_or(DEFAULT_WIDTH).max(40),
    };
    let out = render_view(&v, &opts);
    print_paged(&out)
}

// --- `--raw --turn` handling -----------------------------------------------

/// Handle `--raw --turn N` (or `--turn A..B`): fetch the view to get turn
/// raw_lines indices, fetch the raw transcript, slice to those lines, and
/// print verbatim.
async fn run_raw_with_turn(
    url: &str,
    seg: &str,
    turn_spec: &str,
    bearer: Option<&str>,
) -> Result<()> {
    let client = http::client_with_timeout_and_bearer(60, bearer)?;

    // Fetch the view to get the turns' raw_lines indices. PF-R1: explicit
    // `turns=all` — the requested `--turn` ordinal may sit outside the
    // server's default tail window, and silently missing it would be
    // exactly the wire-safety footgun the windowed default is meant to
    // avoid elsewhere.
    let view_body = client
        .get(format!(
            "{url}/api/sessions/{seg}/view?fields=turns&turns=all"
        ))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let v: serde_json::Value =
        serde_json::from_str(&view_body).context("view response was not JSON")?;
    let turns = v["turns"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("view response missing turns array"))?;

    // Parse the turn range (identical to the render_view logic).
    let (range_a, range_b) = match parse_turn_range(turn_spec, turns) {
        Some((a, b)) => (a as usize, b as usize),
        None => {
            eprintln!("(no turn matches --turn {turn_spec:?})");
            return Ok(());
        }
    };

    // Collect raw_lines indices from all turns in the range.
    let mut indices: Vec<u32> = Vec::new();
    for t in turns {
        if let Some(ordinal) = t["ordinal"].as_u64() {
            let ordinal = ordinal as usize;
            if ordinal >= range_a && ordinal <= range_b {
                if let Some(raw_lines) = t["raw_lines"].as_array() {
                    for line_val in raw_lines {
                        if let Some(line_idx) = line_val.as_u64() {
                            indices.push(line_idx as u32);
                        }
                    }
                }
            }
        }
    }

    // Sort and deduplicate.
    indices.sort_unstable();
    indices.dedup();

    if indices.is_empty() {
        // No lines found for this turn range.
        return Ok(());
    }

    // Fetch the raw transcript (all lines).
    let text = client
        .get(format!("{url}/api/sessions/{seg}/raw"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // Split into lines and slice to the requested indices.
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    for &idx in &indices {
        if (idx as usize) < lines.len() {
            out.push_str(lines[idx as usize]);
            out.push('\n');
        }
    }

    print_paged(&out)
}

// --- W7 (R15/LF-6) — `--live`/`--follow`, direct-disk ------------------------

/// Pure function: check if capture mtime advanced from prev to cur.
/// Returns true only if the file appeared (None->Some) or was modified (Some(a)->Some(b) where b>a).
fn capture_mtime_advanced(prev: Option<SystemTime>, cur: Option<SystemTime>) -> bool {
    match (prev, cur) {
        (None, Some(_)) => true,     // First observation of a file
        (Some(p), Some(c)) => c > p, // Mtime moved forward
        _ => false,                  // No change or file disappeared
    }
}

/// Resolve the capture file for a session, if KB_SESSIONS_DIR is set.
/// Sanitizes session_id (non-alphanumeric -> '-', max 80 chars) and looks for
/// `session-*-<sanitized>.html` in the directory. Returns None if env var is
/// unset or no matching file is found (graceful degradation).
fn resolve_capture_file(session_id: &str) -> Option<PathBuf> {
    let dir = std::env::var("KB_SESSIONS_DIR").ok()?;

    // Sanitize session_id: replace every non-alphanumeric with '-', truncate to 80 chars
    let sanitized: String = session_id
        .chars()
        .take(80)
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Scan directory for session-*-<sanitized>.html files
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                if name.ends_with(".html")
                    && name.contains(&sanitized)
                    && name.starts_with("session-")
                {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// Get the current mtime of a file; returns None if the file doesn't exist or mtime is unavailable.
fn get_capture_mtime(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// `--live` / `--live --follow`. Zero daemon required: resolves `kb.toml`
/// locally (`config` — the SAME `--config` global flag every other command
/// respects) to read `[sessions]`, then calls the SHARED
/// `kb_core::sessions::tail::resolve_live_transcript` resolver directly
/// against the local disk — no HTTP at all for the resolve + one-shot
/// render. `daemon`/`bearer` are used ONLY for two best-effort polish bits
/// that degrade silently when no daemon is reachable: (1) the "session not
/// live" hint's staleness figure, (2) a `session.captured` divider during
/// `--follow` when KB_SESSIONS_DIR is set.
#[allow(clippy::too_many_arguments)]
async fn run_live(
    config: Option<&PathBuf>,
    session_id: &str,
    follow: bool,
    grep: Option<&str>,
    raw: bool,
    json: bool,
    no_color: bool,
    width: Option<usize>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let cfg_path = crate::commands::resolve_config_path(config)?;
    let cfg = crate::commands::load_config_or_default(&cfg_path)?;

    let Some(path) =
        kb_core::sessions::tail::resolve_live_transcript(session_id, &cfg.sessions, None)
    else {
        let hint = live_not_found_hint(session_id, daemon, bearer).await;
        eprintln!("{hint}");
        std::process::exit(2);
    };

    if raw {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read live transcript {}", path.display()))?;
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        if follow {
            return follow_raw_loop(&path, session_id).await;
        }
        return Ok(());
    }

    // One-shot: the FULL engine over the live file's current bytes — same
    // grammar as a captured read, just a different (unwrapped, no `<pre>`
    // envelope) source. `TailBlocks::default()` — a live file has no
    // captured commits/subagents tail block yet.
    let jsonl = std::fs::read_to_string(&path)
        .with_context(|| format!("read live transcript {}", path.display()))?;
    let view = kb_core::sessions::view::session_view(
        &jsonl,
        &kb_core::sessions::view::TailBlocks::default(),
        &kb_core::sessions::view::ViewOptions::default(),
    );
    let sid8: String = session_id.chars().take(8).collect();
    let wire = live_wire_json(session_id, &view);
    let harness = view.header.harness.clone();

    if json && !follow {
        return print_paged(&serde_json::to_string(&wire)?);
    }

    if json {
        // NDJSON from the start — the scripting surface stays one uniform
        // shape whether a turn came from the catch-up render or a later
        // poll.
        for t in &view.turns {
            println!(
                "{}",
                serde_json::to_string(&kb_core::sessions::view::ViewEvent::TurnClosed(t.clone()))?
            );
        }
    } else {
        let opts = ReadOpts {
            full: false,
            turn: None,
            grep: grep.map(str::to_string),
            context: 0,
            no_color,
            width: width.unwrap_or(DEFAULT_WIDTH).max(40),
        };
        let out = render_view(&wire, &opts);
        print!("{out}");
        if follow {
            let banner = format!("── live: {sid8} — following (Ctrl-C to exit) ──");
            println!("{}", if no_color { banner } else { dim(&banner) });
        }
    }

    if !follow {
        return Ok(());
    }

    follow_interpreted_loop(&path, json, harness, session_id).await
}

/// `--live --follow --raw`: `tail -f`-equivalent over the decoded JSONL,
/// complete-line discipline via `TailReader` (no interpretation at all).
/// Optionally prints "── capture landed ──" when the KB_SESSIONS_DIR
/// capture file's mtime advances.
async fn follow_raw_loop(path: &std::path::Path, session_id: &str) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    let mut reader = kb_core::sessions::tail::TailReader::new(path.to_path_buf(), size);

    // Optionally resolve and seed capture file mtime for divider printing
    let capture_path = resolve_capture_file(session_id);
    let mut prev_capture_mtime = capture_path.as_ref().and_then(get_capture_mtime);

    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(750)) => {}
            _ = tokio::signal::ctrl_c() => { return Ok(()); }
        }
        let delta = reader.read_delta(kb_core::sessions::tail::LIVE_DELTA_MAX_BYTES)?;

        // Check if capture file's mtime advanced
        if let Some(ref cpath) = capture_path {
            let cur_mtime = get_capture_mtime(cpath);
            if capture_mtime_advanced(prev_capture_mtime, cur_mtime) {
                println!("── capture landed ──");
                prev_capture_mtime = cur_mtime;
            }
        }

        if !delta.complete_lines.is_empty() {
            print!("{}", delta.complete_lines);
        }
        reader.offset = delta.next_offset;
    }
}

/// `--live --follow` (interpreted): poll every 750ms, print each newly
/// CLOSED turn (`ViewEvent::TurnClosed`) as it lands — `TaskUpdated` deltas
/// are silently absorbed for now (no task-board presenter in the terminal
/// reader; the captured render is still the full-fidelity surface, per
/// LF-4's stated scope). A truncation/rotation mid-follow resets the carry
/// and re-bootstraps rather than erroring out.
///
/// Also prints:
/// - "── capture landed ──" when KB_SESSIONS_DIR capture file's mtime advances
/// - A TTY status line on stderr (if stderr is a terminal) after each poll
///   with no new turns, showing the last emitted item's headline and elapsed time.
async fn follow_interpreted_loop(
    path: &std::path::Path,
    json: bool,
    harness: String,
    session_id: &str,
) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    let mut reader = kb_core::sessions::tail::TailReader::new(path.to_path_buf(), size);
    let mut carry = kb_core::sessions::view::ViewCarry::default();

    // Optionally resolve and seed capture file mtime for divider printing
    let capture_path = resolve_capture_file(session_id);
    let mut prev_capture_mtime = capture_path.as_ref().and_then(get_capture_mtime);

    // TTY status line state (interpreted loop only, non-JSON)
    let stderr_is_tty = std::io::stderr().is_terminal();
    let mut last_emit_time = Instant::now();
    let mut last_headline = "waiting".to_string();
    let sid8: String = session_id.chars().take(8).collect();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(750)) => {}
            _ = tokio::signal::ctrl_c() => { return Ok(()); }
        }
        let delta = reader.read_delta(kb_core::sessions::tail::LIVE_DELTA_MAX_BYTES)?;
        if delta.truncated_restart {
            // The file was truncated/rotated/rewritten out from under us —
            // reset and keep following from the new byte 0 rather than
            // erroring the whole loop out. Clear status line if present.
            if stderr_is_tty && !json {
                eprint!("\r\x1b[2K");
            }
            carry = kb_core::sessions::view::ViewCarry::default();
            reader.offset = 0;
            continue;
        }
        if !delta.complete_lines.is_empty() {
            // Clear status line before printing new content
            if stderr_is_tty && !json {
                eprint!("\r\x1b[2K");
            }

            // Check if capture file's mtime advanced
            if let Some(ref cpath) = capture_path {
                let cur_mtime = get_capture_mtime(cpath);
                if capture_mtime_advanced(prev_capture_mtime, cur_mtime) {
                    println!("── capture landed ──");
                    prev_capture_mtime = cur_mtime;
                }
            }

            let (events, new_carry) =
                kb_core::sessions::view::view_append(carry, &delta.complete_lines);
            carry = new_carry;
            for ev in events {
                match ev {
                    kb_core::sessions::view::ViewEvent::TurnClosed(t) => {
                        last_emit_time = Instant::now();
                        let v = serde_json::to_value(&t)?;
                        // Update headline from the turn's first Prose item
                        if let Some(items) = v["items"].as_array() {
                            if let Some(first_item) = items.first() {
                                if first_item["item"].as_str() == Some("Prose") {
                                    if let Some(text) = first_item["text"].as_str() {
                                        last_headline = text.chars().take(60).collect();
                                    }
                                } else if let Some(kind) = first_item["item"].as_str() {
                                    last_headline = kind.to_string();
                                }
                            }
                        }
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(
                                    &kb_core::sessions::view::ViewEvent::TurnClosed(t)
                                )?
                            );
                        } else {
                            print!("{}", render_turn(&v, &harness));
                        }
                    }
                    kb_core::sessions::view::ViewEvent::TaskUpdated(_) => {}
                }
            }
        } else if stderr_is_tty && !json {
            // No new turns this poll: show or update status line on stderr
            let elapsed = last_emit_time.elapsed().as_secs();
            eprint!("\rfollowing {sid8} · last: {last_headline} · {elapsed}s ago");
        }
        reader.offset = delta.next_offset;
    }
}

/// Build the `SessionViewResponse`-shaped JSON `render_view`/`render_turn`
/// expect, from a directly-parsed live `SessionView` — mirrors
/// `tests::view_wire_for_fixture`'s wrapper shape exactly (the three
/// route-added wrapper fields are placeholders here: a live file has no
/// `artifact_id`/`kb` yet since it hasn't been captured). PF-R1:
/// `turns_total`/`turns_returned` are always equal here — the live path
/// always renders the FULL current transcript, no windowing — so
/// `render_view`'s default branch never prints a "turns hidden" note for
/// `--live` output.
fn live_wire_json(
    session_id: &str,
    view: &kb_core::sessions::view::SessionView,
) -> serde_json::Value {
    let turns_total = view.turns.len();
    serde_json::json!({
        "grammar": view.grammar,
        "session_id": session_id,
        "kb": "-",
        "artifact_id": "-",
        "header": view.header,
        "turns": view.turns,
        "side_lanes": view.side_lanes,
        "outline": view.outline,
        "tasks": view.tasks_final,
        "subagents": view.subagents,
        "minimap": view.minimap,
        "stats": view.stats,
        "turns_total": turns_total,
        "turns_returned": turns_total,
        "scrubbed": false,
        "redactions": 0,
    })
}

/// The honest exit-2 hint for a sid that isn't resolvably live (LF-6):
/// best-effort daemon lookup for how stale the newest CAPTURE is, if a
/// daemon happens to be reachable; a plain hint otherwise. Never blocks on
/// daemon unavailability — `--live` stays a zero-daemon verb.
async fn live_not_found_hint(
    session_id: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> String {
    let sid8: String = session_id.chars().take(8).collect();
    if let Some(url) = http::detect_daemon(daemon, bearer).await {
        if let Ok(client) = http::client_with_timeout_and_bearer(10, bearer) {
            let seg = http::encode_path_segment(session_id);
            if let Ok(resp) = client.get(format!("{url}/api/sessions/{seg}")).send().await {
                if let Ok(v) = resp.json::<serde_json::Value>().await {
                    if let Some(ended_at) = v["ended_at"].as_i64() {
                        let now = chrono::Utc::now().timestamp();
                        let age_min = ((now - ended_at).max(0)) / 60;
                        return format!(
                            "session not live; newest capture is {age_min}m old — kb sessions read {sid8}"
                        );
                    }
                }
            }
        }
    }
    format!("session not live (no resolvable live transcript) — kb sessions read {sid8}")
}

/// Print `text` through `$PAGER` (default `less -RFX`) when stdout is a tty
/// and the text is longer than a screenful; otherwise print directly. Best-
/// effort: any pager-spawn failure falls back to a plain print rather than
/// erroring the whole command.
fn print_paged(text: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        let is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } != 0;
        let long_enough = text.lines().count() > 60;
        if is_tty && long_enough {
            let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -RFX".to_string());
            let mut parts = pager.split_whitespace();
            if let Some(cmd) = parts.next() {
                let args: Vec<&str> = parts.collect();
                if let Ok(mut child) = std::process::Command::new(cmd)
                    .args(&args)
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
                    if let Some(stdin) = child.stdin.as_mut() {
                        // A closed pager (user quit early) is not an error —
                        // ignore the write failure and just wait on the child.
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                    return Ok(());
                }
            }
        }
    }
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    Ok(())
}

// --- the pure presenter ------------------------------------------------------

/// Render the `session-view/1` wire (a `SessionViewResponse`-shaped
/// `serde_json::Value` — `{grammar, session_id, kb, artifact_id, header,
/// turns, side_lanes, outline, tasks, subagents, minimap, stats, ...}`) as
/// deterministic terminal text. Pure — no I/O, no clock beyond what's already
/// baked into the wire — so it is directly golden-testable.
pub fn render_view(v: &serde_json::Value, opts: &ReadOpts) -> String {
    let mut out = String::new();
    let header = render_header(v);
    // The `--no-color`/color axis: bold the ONE session-identity line (the
    // header's first line) when color is on — minimal, deterministic under
    // `--no-color` (the pinned golden posture), never applied to any line
    // whose exact text a test asserts on.
    match header.split_once('\n') {
        Some((first, rest)) if !opts.no_color => {
            out.push_str(&bold(first));
            out.push('\n');
            out.push_str(rest);
        }
        _ => out.push_str(&header),
    }
    out.push('\n');

    let empty: Vec<serde_json::Value> = Vec::new();
    let turns = v["turns"].as_array().unwrap_or(&empty);
    let harness = v["header"]["harness"].as_str().unwrap_or("claude");
    let rule = "─".repeat(opts.width.min(120));

    if let Some(pat) = opts.grep.as_deref().filter(|p| !p.is_empty()) {
        render_grep(&mut out, turns, pat, opts.context as usize, harness, &rule);
    } else if let Some(spec) = opts.turn.as_deref() {
        match parse_turn_range(spec, turns) {
            Some((a, b)) => {
                out.push_str(&rule);
                out.push('\n');
                for t in turns {
                    let n = t["ordinal"].as_u64().unwrap_or(0);
                    if n >= a as u64 && n <= b as u64 {
                        out.push_str(&render_turn(t, harness));
                    }
                }
            }
            None => {
                out.push_str(&format!("(no turn matches --turn {spec:?})\n"));
            }
        }
    } else if opts.full {
        out.push_str(&rule);
        out.push('\n');
        for t in turns {
            out.push_str(&render_turn(t, harness));
        }
    } else {
        // PF-R1: the wire is already windowed (the fetch in `run` asked
        // for exactly this many turns) — just render what came back, with
        // an honest note when `turns_total`/`turns_returned` (additive
        // PF-R1 wire fields) say something was left off.
        render_tail_window(&mut out, v, turns, harness, &rule, opts.no_color);
    }

    out.push_str(&rule);
    out.push('\n');
    out.push_str(&render_outcome_footer(v, harness));
    let sid8: String = v["session_id"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(8)
        .collect();
    if !sid8.is_empty() {
        let hint =
            format!("raw: kb sessions read {sid8} --raw · full: --full · one turn: --turn N");
        out.push_str(&if opts.no_color { hint } else { dim(&hint) });
        out.push('\n');
    }
    out
}

fn render_header(v: &serde_json::Value) -> String {
    let mut out = String::new();
    let sid = v["session_id"].as_str().unwrap_or("-");
    let sid8: String = sid.chars().take(8).collect();
    let kb = v["kb"].as_str().unwrap_or("-");
    let h = &v["header"];
    let harness = h["harness"].as_str().unwrap_or("claude");
    let glyph = harness_glyph(harness);
    let cwd = h["cwd"].as_str();
    let folder = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .map(|c| c.rsplit(['/', '\\']).next().unwrap_or(c))
        .filter(|s| !s.is_empty());
    let started = h["started_at"].as_str().unwrap_or("-");
    let ended = h["ended_at"].as_str().unwrap_or("-");

    let where_ = match folder {
        Some(f) => format!("{kb}/{f}"),
        None => kb.to_string(),
    };
    out.push_str(&format!(
        "session {sid8} · {where_} · {glyph} {harness} · {started} → {ended}"
    ));
    if let Some(span) = h["span_secs"].as_i64() {
        let active = h["active_secs"].as_i64().unwrap_or(0);
        out.push_str(&format!(
            " ({} wall · {} active)",
            fmt_duration(span),
            fmt_duration(active)
        ));
    }
    out.push('\n');
    if let Some(title) = h["title"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(title);
        out.push('\n');
    }
    if let Some(asked) = h["opening"]["text"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!("  asked   {}\n", truncate(asked, 100)));
    }
    if let Some(closed) = h["outcome"]["text"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!("  closed  {}\n", truncate(closed, 100)));
    }
    let turns_h = h["turns_human"].as_u64().unwrap_or(0);
    let turns_a = h["turns_assistant"].as_u64().unwrap_or(0);
    let tool_calls = h["tool_calls"].as_u64().unwrap_or(0);
    let errors = h["error_count"].as_u64().unwrap_or(0);
    let thinking_empty = v["stats"]["thinking_empty"].as_u64().unwrap_or(0);
    out.push_str(&format!(
        "  {turns_h} asked · {turns_a} replies · {tool_calls} tool calls · {errors} errors"
    ));
    if thinking_empty > 0 {
        out.push_str(&format!(" · {thinking_empty} empty thinking (hidden)"));
    }
    out.push('\n');
    out
}

/// Harness glyph — a lightweight, ASCII-superset marker (not a font-
/// dependent icon). cli-grok's Proposal-5 glyph-avoidance policy is about
/// the repeated FACET rows (list/rollup breakdowns, `sessions.rs`); this
/// verb's header is a single prominent line, the one place the work order
/// explicitly asks for a glyph alongside the (always-shown) harness name.
/// SGR bold — the `--no-color` axis's one structural touch (the header's
/// identity line). No terminal-capability detection: `run()` only calls this
/// when color is wanted (see `render_view`), so a plain function suffices.
fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}

/// SGR dim — used for the elision marker and the trailing hint line, the two
/// "meta, not content" lines in the default view.
fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

fn harness_glyph(harness: &str) -> &'static str {
    match harness {
        "claude" => "◆",
        "codex" => "▲",
        "opencode" => "■",
        "grok" => "✶",
        "kimi" => "☾",
        _ => "○",
    }
}

fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else {
        format!("{mins}m")
    }
}

/// `HH:MM` from an ISO-8601 UTC timestamp string (`view.rs` emits
/// RFC3339-ish strings) — a cheap slice, not a full parse (mirrors the
/// existing `fmt_clock`'s "no chrono for one call site" discipline).
fn short_time(ts: &str) -> String {
    // Expect `YYYY-MM-DDTHH:MM:SS...`; fall back to the raw string when it
    // doesn't match (never panics on a foreign-harness timestamp shape).
    ts.get(11..16).unwrap_or(ts).to_string()
}

/// `A` or `A..B` → an inclusive `(min, max)` ordinal window. Delegates to
/// the shared kb-core grammar so CLI and server cannot drift.
fn parse_turn_range(spec: &str, _turns: &[serde_json::Value]) -> Option<(u32, u32)> {
    kb_core::sessions::view::parse_turn_window(spec)
}

/// PF-R1 — the new default render: the wire has ALREADY been windowed
/// server-side (the fetch in `run` asks for exactly the tail window it
/// wants), so this just prints every turn it was given, plus — when
/// `v["turns_total"] > v["turns_returned"]` — an honest note up front
/// about how many EARLIER turns were left off (never in the middle: with
/// only a tail window, everything missing is necessarily before what's
/// shown, unlike the old head+tail scheme's middle elision). Both fields
/// are best-effort reads (`.as_u64()` on a missing/foreign key is `None`,
/// e.g. the `--live` wire before PF-R1 or a hand-built test fixture), so a
/// wire that doesn't carry them just renders with no note — never a panic.
/// Replaces the old client-side head+tail elision (`render_windowed`,
/// retired) now that the SERVER does the windowing — one fewer place
/// turn-count logic can drift.
fn render_tail_window(
    out: &mut String,
    v: &serde_json::Value,
    turns: &[serde_json::Value],
    harness: &str,
    rule: &str,
    no_color: bool,
) {
    out.push_str(rule);
    out.push('\n');
    let total = v["turns_total"].as_u64();
    let returned = v["turns_returned"].as_u64();
    if let (Some(total), Some(returned)) = (total, returned) {
        if total > returned {
            let hidden = total - returned;
            let note = format!(
                "   … {hidden} earlier turn(s) not shown — --full to see all, or --tail N for more"
            );
            out.push_str(&if no_color { note } else { dim(&note) });
            out.push('\n');
        }
    }
    for t in turns {
        out.push_str(&render_turn(t, harness));
    }
}

fn render_grep(
    out: &mut String,
    turns: &[serde_json::Value],
    pat: &str,
    context: usize,
    harness: &str,
    rule: &str,
) {
    out.push_str(rule);
    out.push('\n');
    let pat_lc = pat.to_lowercase();
    let hits: Vec<usize> = turns
        .iter()
        .enumerate()
        .filter(|(_, t)| turn_text(t).to_lowercase().contains(&pat_lc))
        .map(|(i, _)| i)
        .collect();
    if hits.is_empty() {
        out.push_str(&format!("(no turn matches --grep {pat:?})\n"));
        return;
    }
    // Merge hit±context into contiguous ranges so overlapping windows print
    // once, with a "⋯" separator between non-adjacent groups.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &i in &hits {
        let lo = i.saturating_sub(context);
        let hi = (i + context).min(turns.len() - 1);
        match ranges.last_mut() {
            Some((_, prev_hi)) if lo <= *prev_hi + 1 => *prev_hi = hi.max(*prev_hi),
            _ => ranges.push((lo, hi)),
        }
    }
    for (idx, (lo, hi)) in ranges.iter().enumerate() {
        if idx > 0 {
            out.push_str("   ⋯\n");
        }
        for t in &turns[*lo..=*hi] {
            out.push_str(&render_turn(t, harness));
        }
    }
}

/// The searchable text for one turn (grep target): every `Prose` item's text
/// joined, falling back to the turn's `id` (never empty — a turn with no
/// prose still has SOMETHING to not-match against).
fn turn_text(t: &serde_json::Value) -> String {
    let empty: Vec<serde_json::Value> = Vec::new();
    let items = t["items"].as_array().unwrap_or(&empty);
    let mut parts = Vec::new();
    for it in items {
        if let Some(text) = it["text"].as_str() {
            parts.push(text.to_string());
        }
        if let Some(h) = it["headline"].as_str() {
            parts.push(h.to_string());
        }
    }
    if parts.is_empty() {
        t["id"].as_str().unwrap_or("").to_string()
    } else {
        parts.join(" ")
    }
}

/// Render one turn: an ordinal/who/time headline (the first `Prose` item's
/// text, or a compact descriptor of the first item when there is none), then
/// every remaining item as an indented compact line.
fn render_turn(t: &serde_json::Value, harness: &str) -> String {
    let n = t["ordinal"].as_u64().unwrap_or(0);
    let role = t["role"].as_str().unwrap_or("assistant");
    let who = if role == "human" { "you" } else { harness };
    let ts = t["ts"]
        .as_str()
        .map(short_time)
        .unwrap_or_else(|| "--:--".into());
    let empty: Vec<serde_json::Value> = Vec::new();
    let items = t["items"].as_array().unwrap_or(&empty);

    let (headline, rest): (String, &[serde_json::Value]) = match items.first() {
        Some(first) if first["item"].as_str() == Some("Prose") => (
            truncate(first["text"].as_str().unwrap_or(""), 80),
            &items[1..],
        ),
        Some(_) => (String::new(), items),
        None => (String::new(), items),
    };

    let mut out = format!("{n:>4} {who:<7}{ts}  {headline}\n");
    for it in rest {
        if let Some(line) = render_item(it) {
            out.push_str("      ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// One [`kb_core::sessions::view::Item`] variant → one compact terminal
/// line, or `None` for a variant that renders as nothing by default (empty
/// thinking — counted in the header only, per the work order).
fn render_item(it: &serde_json::Value) -> Option<String> {
    let kind = it["item"].as_str()?;
    match kind {
        "Prose" => Some(truncate(it["text"].as_str().unwrap_or(""), 90)),
        "Thinking" => {
            if it["empty"].as_bool().unwrap_or(true) {
                None
            } else {
                Some(format!(
                    "· thinking ({} chars)",
                    it["len"].as_u64().unwrap_or(0)
                ))
            }
        }
        "ToolCall" => {
            let name = it["name"].as_str().unwrap_or("Tool");
            let headline = truncate(it["headline"].as_str().unwrap_or(""), 64);
            let err = it["result"]["is_error"].as_bool().unwrap_or(false);
            let badge = if err {
                "  ✗"
            } else if it["unpaired"].as_bool().unwrap_or(false) {
                "  …"
            } else {
                ""
            };
            Some(format!("{name:<8}{headline}{badge}"))
        }
        "Command" => {
            let name = it["name"].as_str().unwrap_or("");
            let args = it["args"].as_str().unwrap_or("");
            let mut s = format!("⌁ /{name} {args}");
            if let Some(out) = it["stdout"].as_str().filter(|s| !s.is_empty()) {
                s.push_str(" → ");
                s.push_str(&truncate(out, 60));
            }
            Some(s)
        }
        "TaskEvent" => Some(format!(
            "◔ task {} {} → {}",
            it["id"].as_str().unwrap_or("?"),
            truncate(it["subject"].as_str().unwrap_or(""), 40),
            it["transition"].as_str().unwrap_or("?"),
        )),
        "WorkflowCard" => {
            let name = it["name"]
                .as_str()
                .or_else(|| it["task_id"].as_str())
                .unwrap_or("workflow");
            let phases = it["phases"].as_array().map(|a| a.len()).unwrap_or(0);
            Some(format!("⛭ workflow {name} · {phases} phase(s)"))
        }
        "KbCommand" => {
            let verb = it["verb"].as_str().unwrap_or("");
            let args = it["args"].as_str().unwrap_or("");
            let mut s = format!("kb {verb} {}", truncate(args, 50));
            if let Some(rv) = it["result_view"].as_str().filter(|s| !s.is_empty()) {
                s.push_str(" — ");
                s.push_str(&truncate(rv, 40));
            }
            Some(s)
        }
        "MemoryInjection" => Some(format!(
            "⌁ memories: {} injected",
            it["items"].as_array().map(|a| a.len()).unwrap_or(0)
        )),
        "SystemReminder" => Some(format!(
            "ⓘ {}",
            truncate(it["preview"].as_str().unwrap_or(""), 60)
        )),
        "Decision" => {
            let prompt = truncate(it["prompt"].as_str().unwrap_or(""), 46);
            match it["answer"].as_str().filter(|a| !a.is_empty()) {
                Some(a) => Some(format!("◆ {prompt} → {}", truncate(a, 24))),
                None => Some(format!("◆ {prompt}")),
            }
        }
        "ModeChange" => Some(format!("⌁ mode → {}", it["mode"].as_str().unwrap_or("?"))),
        "TimeGap" => {
            let secs = it["secs"].as_i64().unwrap_or(0);
            Some(format!("⋯ {} gap", fmt_duration(secs)))
        }
        "Raw" => Some(format!(
            "? unparsed ({})",
            it["reason"].as_str().unwrap_or("?")
        )),
        _ => None,
    }
}

fn render_outcome_footer(v: &serde_json::Value, harness: &str) -> String {
    let mut out = String::new();
    let outcome = &v["header"]["outcome"];
    if let Some(text) = outcome["text"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!("outcome {harness}  {}\n", truncate(text, 140)));
    }
    if let Some(commits) = outcome["commits"].as_array().filter(|a| !a.is_empty()) {
        let first = &commits[0];
        let sha = first["sha"].as_str().unwrap_or("-------");
        let subject = truncate(first["subject"].as_str().unwrap_or(""), 56);
        let resolved = if first["resolved"].as_bool().unwrap_or(false) {
            "resolved"
        } else {
            "unresolved"
        };
        let more = if commits.len() > 1 {
            format!(" · +{} more", commits.len() - 1)
        } else {
            String::new()
        };
        out.push_str(&format!("commits {sha} {subject} ({resolved}){more}\n"));
    }
    if let Some(tasks) = v["tasks"].as_array().filter(|a| !a.is_empty()) {
        out.push_str(&format!("tasks   {} task(s):\n", tasks.len()));
        for t in tasks {
            out.push_str(&format!(
                "  ◔ {} → {}\n",
                truncate(t["subject"].as_str().unwrap_or(""), 60),
                t["status"].as_str().unwrap_or("?"),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::sessions::view::{session_view, TailBlocks, ViewOptions};

    /// Test-support helper (work-order-mandated: fixtures come from RUNNING
    /// the kb-core engine over the shared fixture corpus, never hand-written
    /// view JSON). Reads a `crates/kb-core/tests/session_fixtures/*.jsonl`
    /// prefix, runs it through the SAME `session_view` engine the daemon's
    /// `/view` route calls, then wraps it in the `SessionViewResponse` shape
    /// (constant test-only `session_id`/`kb`/`artifact_id` — those three are
    /// wrapper fields the route adds, not part of the engine's own output)
    /// so `render_view` sees exactly what it would over HTTP.
    /// PF-R1: `turns_total`/`turns_returned` are set EQUAL (the full turn
    /// count) — this helper simulates an unwindowed `?turns=all` fetch, the
    /// only shape that makes sense for a fixture built by calling
    /// `session_view` directly rather than going through the server's
    /// `?turns=` handling. Tests that want to exercise a WINDOWED wire
    /// build one explicitly (see `default_render_shows_the_hidden_count_
    /// when_the_wire_was_windowed`).
    fn view_wire_for_fixture(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kb-core/tests/session_fixtures")
            .join(format!("{name}.jsonl"));
        let jsonl = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
        let view = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let turns_total = view.turns.len();
        serde_json::json!({
            "grammar": view.grammar,
            "session_id": "0123456789abcdef",
            "kb": "test-kb",
            "artifact_id": "testartifact",
            "header": view.header,
            "turns": view.turns,
            "side_lanes": view.side_lanes,
            "outline": view.outline,
            "tasks": view.tasks_final,
            "subagents": view.subagents,
            "minimap": view.minimap,
            "stats": view.stats,
            "turns_total": turns_total,
            "turns_returned": turns_total,
            "scrubbed": false,
            "redactions": 0,
        })
    }

    fn opts() -> ReadOpts {
        ReadOpts {
            full: false,
            turn: None,
            grep: None,
            context: 0,
            no_color: true,
            width: 100,
        }
    }

    #[test]
    fn renders_something_for_every_digest_fixture() {
        // Determinism + no-panic over the whole transcript corpus (the
        // synthetic GC-C1 set `session_digest_snapshots.rs` also pins).
        for name in [
            "tiny-noop",
            "tiny-rate-limited",
            "subagent-delegation",
            "ask-user-question",
            "tool-heavy-research",
        ] {
            let wire = view_wire_for_fixture(name);
            let out1 = render_view(&wire, &opts());
            let out2 = render_view(&wire, &opts());
            assert!(!out1.is_empty(), "{name}: empty render");
            assert_eq!(out1, out2, "{name}: render is not deterministic");
        }
    }

    #[test]
    fn renders_the_session_header_line() {
        let wire = view_wire_for_fixture("ask-user-question");
        let out = render_view(&wire, &opts());
        assert!(out.starts_with("session 01234567"), "{out}");
        assert!(out.contains("claude"), "{out}");
    }

    #[test]
    fn full_flag_renders_every_turn_no_elision() {
        let wire = view_wire_for_fixture("tool-heavy-research");
        let mut o = opts();
        o.full = true;
        let out = render_view(&wire, &o);
        assert!(!out.contains("turn(s) hidden"), "{out}");
        let turn_count = wire["turns"].as_array().unwrap().len();
        // Every turn's ordinal marker should appear.
        for t in wire["turns"].as_array().unwrap() {
            let n = t["ordinal"].as_u64().unwrap();
            assert!(
                out.contains(&format!("{n:>4} ")),
                "turn {n} missing from --full output"
            );
        }
        assert!(turn_count > 0);
    }

    /// PF-R1: `render_view`'s default branch no longer slices client-side —
    /// it trusts the wire's `turns` array is already the window (the fetch
    /// in `run` did the slicing) and only ADDS an honest note when the
    /// wire's own `turns_total`/`turns_returned` metadata says something
    /// was left off. Simulate a real windowed `?turns=<k>` response by
    /// hand: take a fixture's full turns, keep only its last few, and set
    /// the counts the server would have sent alongside them.
    #[test]
    fn default_render_shows_the_hidden_count_when_the_wire_was_windowed() {
        let full = view_wire_for_fixture("tool-heavy-research");
        let all_turns = full["turns"].as_array().unwrap().clone();
        if all_turns.len() < 5 {
            return; // fixture too small to exercise windowing at all
        }
        let kept: Vec<_> = all_turns[all_turns.len() - 3..].to_vec();
        let mut wire = full.clone();
        wire["turns"] = serde_json::Value::Array(kept.clone());
        wire["turns_total"] = serde_json::json!(all_turns.len());
        wire["turns_returned"] = serde_json::json!(kept.len());

        let out = render_view(&wire, &opts());
        let hidden = all_turns.len() - kept.len();
        assert!(
            out.contains(&format!("{hidden} earlier turn(s) not shown")),
            "{out}"
        );
        // Only the KEPT (last 3) turns' ordinals should appear as markers —
        // the elided earlier turns must not leak into the render.
        for t in &kept {
            let n = t["ordinal"].as_u64().unwrap();
            assert!(out.contains(&format!("{n:>4} ")), "{out}");
        }
        let first_elided_ordinal = all_turns[all_turns.len() - 4]["ordinal"].as_u64().unwrap();
        assert!(
            !out.contains(&format!("{first_elided_ordinal:>4} ")),
            "{out}"
        );
    }

    /// A wire whose `turns_total`/`turns_returned` are equal (or absent, as
    /// on the `--live` path pre-PF-R1) must never claim anything was
    /// hidden — `view_wire_for_fixture` always represents an unwindowed
    /// (`?turns=all`-equivalent) fetch.
    #[test]
    fn default_render_prints_no_note_when_the_wire_is_unwindowed() {
        let wire = view_wire_for_fixture("tiny-noop");
        let out = render_view(&wire, &opts());
        assert!(!out.contains("not shown"), "{out}");
    }

    #[test]
    fn turn_range_selects_an_explicit_window() {
        let wire = view_wire_for_fixture("ask-user-question");
        let turns = wire["turns"].as_array().unwrap();
        let first_n = turns[0]["ordinal"].as_u64().unwrap();
        let mut o = opts();
        o.turn = Some(first_n.to_string());
        let out = render_view(&wire, &o);
        assert!(out.contains(&format!("{first_n:>4} ")), "{out}");
        assert!(!out.contains("turn(s) hidden"), "{out}");
    }

    #[test]
    fn turn_range_includes_raw_lines_in_view() {
        // Verify that the view_wire_for_fixture includes raw_lines
        // in each Turn, which is required for --turn --raw slicing.
        let wire = view_wire_for_fixture("ask-user-question");
        let turns = wire["turns"].as_array().unwrap();
        for t in turns {
            assert!(
                t.get("raw_lines").is_some(),
                "Turn {} missing raw_lines field",
                t["ordinal"]
            );
            if let Some(raw_lines) = t["raw_lines"].as_array() {
                // Each raw_lines entry should be a u32 index.
                for line_val in raw_lines {
                    assert!(line_val.is_u64(), "raw_lines entry is not a number");
                }
            }
        }
    }

    #[test]
    fn grep_filters_to_matching_turns_only() {
        let wire = view_wire_for_fixture("ask-user-question");
        // Every ask-user-question fixture has a Decision item; grep on a
        // word from the outline's first entry, which must always match.
        let outline = wire["outline"].as_array().unwrap();
        let Some(needle) = outline
            .first()
            .and_then(|r| r["preview"].as_str())
            .and_then(|p| p.split_whitespace().next())
        else {
            return; // no outline text in this fixture shape — nothing to grep
        };
        let mut o = opts();
        o.grep = Some(needle.to_string());
        let out = render_view(&wire, &o);
        assert!(!out.contains("no turn matches"), "{out}");
    }

    #[test]
    fn grep_with_no_match_says_so_rather_than_rendering_nothing() {
        let wire = view_wire_for_fixture("tiny-noop");
        let mut o = opts();
        o.grep = Some("zzz_definitely_not_present_zzz".to_string());
        let out = render_view(&wire, &o);
        assert!(out.contains("no turn matches --grep"), "{out}");
    }

    #[test]
    fn thinking_empty_count_is_header_only_never_per_item() {
        // Every Thinking item with empty=true must render NO per-item line
        // (the work order: "empty-thinking count in header only").
        let wire = view_wire_for_fixture("tool-heavy-research");
        let mut o = opts();
        o.full = true;
        let out = render_view(&wire, &o);
        assert!(!out.contains("· thinking (0 chars)"), "{out}");
    }

    // ---- W7 (R15/LF-6) — `--live`/`--follow` ----

    #[test]
    fn live_wire_json_matches_the_session_view_response_shape() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kb-core/tests/session_fixtures")
            .join("tiny-noop.jsonl");
        let jsonl = std::fs::read_to_string(&path).unwrap();
        let view = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let wire = live_wire_json("sid-live-test", &view);
        assert_eq!(wire["session_id"], "sid-live-test");
        assert_eq!(wire["kb"], "-");
        assert_eq!(wire["artifact_id"], "-");
        assert_eq!(wire["grammar"], view.grammar);
        // render_view must accept this shape without panicking — the whole
        // point of reusing the wire wrapper.
        let out = render_view(&wire, &opts());
        assert!(!out.is_empty());
    }

    #[test]
    fn resolve_live_transcript_then_full_render_round_trips_end_to_end() {
        // Direct-disk smoke: config → resolver → full engine render, with
        // NO daemon involved (R9a's construction) — exercises the exact
        // path `run_live` takes for the one-shot (non-follow) case.
        use kb_core::config::SessionsSection;
        use kb_core::sessions::tail::resolve_live_transcript;

        let tmp = tempfile::tempdir().unwrap();
        let slug = kb_core::session_bundle::claude_project_slug("/proj/live-cli-test");
        let dir = tmp.path().join(&slug);
        std::fs::create_dir_all(&dir).unwrap();
        let sid = "cli-live-smoke-1";
        let jsonl = concat!(
            r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"hello back"}]}}"#,
            "\n",
        );
        std::fs::write(dir.join(format!("{sid}.jsonl")), jsonl).unwrap();

        let cfg = SessionsSection {
            live_transcripts_dir: Some(tmp.path().to_path_buf()),
            live_window_secs: None,
        };
        let resolved =
            resolve_live_transcript(sid, &cfg, Some("/proj/live-cli-test")).expect("resolves");
        let text = std::fs::read_to_string(&resolved).unwrap();
        let view = session_view(&text, &TailBlocks::default(), &ViewOptions::default());
        let wire = live_wire_json(sid, &view);
        let out = render_view(&wire, &opts());
        assert!(out.contains("session cli-live"), "{out}");
    }

    // ---- W7 capture-landed divider tests ----

    #[test]
    fn capture_mtime_advanced_first_observation_prints() {
        let prev = None;
        let cur = Some(SystemTime::now());
        assert!(
            capture_mtime_advanced(prev, cur),
            "first observation should trigger print"
        );
    }

    #[test]
    fn capture_mtime_advanced_when_time_moves_forward() {
        let now = SystemTime::now();
        let later = now + std::time::Duration::from_secs(1);
        assert!(
            capture_mtime_advanced(Some(now), Some(later)),
            "mtime advance should trigger print"
        );
    }

    #[test]
    fn capture_mtime_advanced_when_time_unchanged() {
        let now = SystemTime::now();
        assert!(
            !capture_mtime_advanced(Some(now), Some(now)),
            "unchanged mtime should not trigger print"
        );
    }
}
