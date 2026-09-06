//! `kb atlas recompute --kb NAME` — trigger an atlas recompute on the
//! daemon and tail SSE for the `atlas.recompute.complete` frame so the
//! command exits with the report (`<N> points in <K> clusters in <ms>`).
//!
//! `kb atlas recluster --kb NAME [--k N]` — fast path that only re-runs
//! k-means on existing coords (no UMAP). Same waiting-for-SSE shape but
//! targets `atlas.recluster.{start,complete}`.
//!
//! `kb atlas labels --kb NAME [--json]` — read-only: the deterministic
//! c-TF-IDF top terms per cluster (W1.B), last refreshed at the most
//! recent recompute/recluster. No SSE tail — one `GET
//! /api/kb/{kb}/atlas/labels` round-trip.
//!
//! `kb atlas points --kb NAME [--json]` — read-only: the FULL-corpus atlas
//! point set (M-a), fixing the paged gallery `?projection=atlas` route's
//! silent truncation past its 200-doc default `limit` on large corpora.
//! Same single-GET shape as `labels` — one `GET /api/kb/{kb}/atlas/points`
//! round-trip, no SSE tail.
//!
//! `kb atlas history --kb NAME [--json]` — read-only: the corpus time-lapse
//! frame list (W3 T-b, V0028), newest first. **Frames start EMPTY** — kb
//! retains no past layout or embedding, so there is nothing to show until
//! recomputes accumulate going forward; the human-readable empty state says
//! so instead of rendering a blank table.
//!
//! `kb atlas show <id> --kb NAME [--align-to ID] [--json]` — read-only: one
//! frame's points, Procrustes-aligned **server-side** against `--align-to`
//! (default: the newest frame) — one `GET /api/kb/{kb}/atlas/history/{id}`
//! round-trip. Every frame this prints is labelled `recorded` or
//! `reconstructed` (`kb_core::storage::sqlite::FrameProvenance`) — never
//! left implicit.
//!
//! `kb atlas prune --kb NAME --keep N [--json]` — explicit operator
//! retention: `POST /api/kb/{kb}/atlas/history/prune?keep=N`, honest about
//! how many frames it actually removed (never `N` itself).
//!
//! `kb atlas backfill --kb NAME [--frames N] [--json]` — seed the time-lapse
//! with **RECONSTRUCTED** frames (W3 T-d): for each of N evenly-spaced
//! `mtime_unix` cut points, today's embeddings laid out over the docs that
//! existed then. It prints the plan (the cut points + the doc count at each)
//! BEFORE the work runs, then the result. **A reconstruction is not
//! history** — nothing in kb retains a past layout or a past embedding — and
//! the word `reconstructed` appears in the output for exactly that reason.
//!
//! Uses the shared `crate::sse` helper for the byte-stream parse.

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};
use crate::sse::{open_events_stream, FrameReader};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::time::{Duration, Instant};

const RECOMPUTE_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn recompute(kb: &str, daemon: Option<&str>, bearer: Option<&str>) -> Result<()> {
    let path = format!("/api/kb/{}/atlas/recompute", encode_path_segment(kb));
    run_and_tail(daemon, bearer, &path, "recompute").await
}

pub async fn recluster(
    kb: &str,
    k: Option<usize>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let mut path = format!("/api/kb/{}/atlas/recluster", encode_path_segment(kb));
    if let Some(k) = k {
        path.push_str(&format!("?k={k}"));
    }
    run_and_tail(daemon, bearer, &path, "recluster").await
}

/// Subscribe to /api/events, POST `path`, then wait for the matching
/// `atlas.{phase}.complete` frame. `phase` is "recompute" or "recluster"
/// and chooses both the user-facing log line and the SSE event name.
async fn run_and_tail(
    daemon: Option<&str>,
    bearer: Option<&str>,
    path: &str,
    phase: &str,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!("{base}{path}");
    let complete_evt = format!("atlas.{phase}.complete");

    // K10: subscribe to /api/events FIRST, then POST. Pre-fix the POST
    // raced the subscription — for small kbs the work finished before
    // SSE was connected and the command hung to its 60s timeout.
    let resp_stream = open_events_stream(base, bearer, None, "").await?;
    let mut reader = FrameReader::from_response(resp_stream);

    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let body: serde_json::Value = resp
        .error_for_status()
        .with_context(|| format!("POST {url}"))?
        .json()
        .await?;
    let run = body["run"].as_str().unwrap_or("?").to_string();
    eprintln!("{phase} queued · run {run} · tailing /api/events …");

    let started = Instant::now();
    loop {
        // v0.7.1 P2 — wrap `next_frame()` in a timeout. The pre-P2 code
        // only checked the deadline *between* frames, so a stream that
        // went silent (no keep-alives, work wedged) hung the command
        // forever — the 60 s budget never fired.
        let Some(remaining) = RECOMPUTE_TIMEOUT.checked_sub(started.elapsed()) else {
            return Err(anyhow!("timed out waiting for {complete_evt} (run {run})"));
        };
        let frame = match tokio::time::timeout(remaining, reader.next_frame()).await {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => {
                return Err(anyhow!("/api/events closed before {phase} completed"));
            }
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => {
                return Err(anyhow!("timed out waiting for {complete_evt} (run {run})"));
            }
        };
        if frame.event.as_deref() == Some(complete_evt.as_str()) {
            let payload: serde_json::Value =
                serde_json::from_str(frame.data.as_deref().unwrap_or("{}")).unwrap_or_default();
            if payload["run"].as_str() == Some(&run) {
                let points = payload["points"].as_u64().unwrap_or(0);
                let clusters = payload["clusters"].as_u64().unwrap_or(0);
                let ms = payload["duration_ms"].as_u64().unwrap_or(0);
                if let Some(err) = payload["error"].as_str() {
                    return Err(anyhow!("{phase} failed: {err}"));
                }
                println!("✓ {points} points in {clusters} clusters ({ms} ms)");
                return Ok(());
            }
        }
    }
}

/// `kb atlas labels --kb NAME [--json]` — one round-trip against `GET
/// /api/kb/{kb}/atlas/labels`. Unlike recompute/recluster there's no work
/// to tail: this route reads the label set the last recompute/recluster
/// already stamped.
pub async fn labels(
    kb: &str,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!("{base}/api/kb/{}/atlas/labels", encode_path_segment(kb));
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_labels_human(kb, &body);
    Ok(())
}

fn print_labels_human(kb: &str, body: &Value) {
    let clusters = body["clusters"].as_array().cloned().unwrap_or_default();
    if clusters.is_empty() {
        println!("atlas labels  [{kb}]  no labels yet — run `kb atlas recompute --kb {kb}` first");
        return;
    }
    let avg_tokens = body["avg_tokens"].as_f64().unwrap_or(0.0);
    println!(
        "atlas labels  [{kb}]  {} cluster{}  (A = {avg_tokens:.1} mean tokens/cluster)",
        clusters.len(),
        if clusters.len() == 1 { "" } else { "s" }
    );
    for c in &clusters {
        let cluster = c["cluster"].as_i64().unwrap_or(0);
        let terms = c["terms"].as_array().cloned().unwrap_or_default();
        let names: Vec<&str> = terms
            .iter()
            .map(|t| t["term"].as_str().unwrap_or("?"))
            .collect();
        println!("  cluster {cluster:>2}  {}", names.join(", "));
        for t in &terms {
            let term = t["term"].as_str().unwrap_or("?");
            let tf = t["tf"].as_f64().unwrap_or(0.0);
            let ft = t["ft"].as_f64().unwrap_or(0.0);
            let score = t["score"].as_f64().unwrap_or(0.0);
            println!("      {term:<20} tf={tf:>5.0}  ft={ft:>5.0}  score={score:.3}");
        }
    }
}

/// `kb atlas points --kb NAME [--json]` — one round-trip against `GET
/// /api/kb/{kb}/atlas/points` (M-a). Unlike the gallery's paged
/// `?projection=atlas`, this always returns EVERY doc in the kb (server-
/// memoised per index generation) — the honest count for the map, not
/// whatever page size the caller happened to ask for.
pub async fn points(
    kb: &str,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!("{base}/api/kb/{}/atlas/points", encode_path_segment(kb));
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_points_human(kb, &body);
    Ok(())
}

fn print_points_human(kb: &str, body: &Value) {
    let total = body["total"].as_u64().unwrap_or(0);
    let clusters = body["clusters"].as_array().cloned().unwrap_or_default();
    println!(
        "atlas points  [{kb}]  {total} point{}  ·  {} cluster{}",
        if total == 1 { "" } else { "s" },
        clusters.len(),
        if clusters.len() == 1 { "" } else { "s" }
    );
    if clusters.is_empty() {
        if total > 0 {
            println!("  (no atlas recompute yet — run `kb atlas recompute --kb {kb}`)");
        }
        return;
    }
    for c in &clusters {
        let cluster = c["cluster"].as_i64().unwrap_or(0);
        let count = c["count"].as_u64().unwrap_or(0);
        println!(
            "  cluster {cluster:>2}  {count} point{}",
            if count == 1 { "" } else { "s" }
        );
    }
}

/// `kb atlas history --kb NAME [--json]` — one round-trip against `GET
/// /api/kb/{kb}/atlas/history` (W3 T-b). No SSE tail — like `labels`/
/// `points`, this reads what the last recompute/recluster already wrote.
pub async fn history(
    kb: &str,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!("{base}/api/kb/{}/atlas/history", encode_path_segment(kb));
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_history_human(kb, &body);
    Ok(())
}

fn print_history_human(kb: &str, body: &Value) {
    print!("{}", format_history_human(kb, body));
}

/// Pure (Value in, String out) so the empty state's wording — the part that
/// carries the honesty contract — is unit-testable.
fn format_history_human(kb: &str, body: &Value) -> String {
    let frames = body["frames"].as_array().cloned().unwrap_or_default();
    if frames.is_empty() {
        // Frames start EMPTY, honestly — see the module doc. No blank
        // table; say WHY there's nothing to show, and what the two options
        // are (one of which is explicitly NOT history).
        return format!(
            "atlas history  [{kb}]  no frames recorded yet\n  \
             kb retains no past layout or embedding, so TRUE history is unrecoverable:\n  \
             recorded frames only accumulate going forward. Run `kb atlas recompute\n  \
             --kb {kb}` (or `recluster`) to record the first one.\n  \
             `kb atlas backfill --kb {kb}` can seed the timeline instead — but those\n  \
             frames are RECONSTRUCTED (today's embeddings over each cut point's docs),\n  \
             not recorded history.\n"
        );
    }
    let mut out = format!(
        "atlas history  [{kb}]  {} frame{}\n",
        frames.len(),
        if frames.len() == 1 { "" } else { "s" }
    );
    out.push_str(&format!(
        "  {:<6} {:<17} {:>8} {:>10} {:<10} provenance\n",
        "id", "when (UTC)", "points", "clusters", "layout"
    ));
    for f in &frames {
        let id = f["id"].as_i64().unwrap_or(0);
        let when = fmt_ts(f["created_at_unix"].as_i64().unwrap_or(0));
        let points = f["point_count"].as_i64().unwrap_or(0);
        let clusters = f["cluster_count"].as_i64().unwrap_or(0);
        let layout = f["layout"].as_str().unwrap_or("?");
        // Every frame is labelled 'recorded' or 'reconstructed' — never
        // left implicit (see the module doc).
        let provenance = f["provenance"].as_str().unwrap_or("?");
        out.push_str(&format!(
            "  {id:<6} {when:<17} {points:>8} {clusters:>10} {layout:<10} {provenance}\n"
        ));
    }
    out
}

/// Format a unix timestamp as `YYYY-MM-DD HH:MM` (UTC). Mirrors
/// `commands::versions::fmt_ts` (private to that module, so duplicated
/// rather than threaded through a shared helper for one two-line fn).
fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

/// `kb atlas show <id> --kb NAME [--align-to ID] [--json]` — one frame's
/// points, Procrustes-aligned SERVER-SIDE against `--align-to` (default:
/// the newest frame) — one `GET /api/kb/{kb}/atlas/history/{id}` round-trip
/// (W3 T-b). Doing the fit on the daemon means this and the SPA's atlas
/// time-lapse view always agree on the same aligned coordinates.
pub async fn show(
    kb: &str,
    id: i64,
    align_to: Option<i64>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let mut url = format!(
        "{base}/api/kb/{}/atlas/history/{id}",
        encode_path_segment(kb)
    );
    if let Some(a) = align_to {
        url.push_str(&format!("?align_to={a}"));
    }
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_show_human(kb, &body);
    Ok(())
}

fn print_show_human(kb: &str, body: &Value) {
    let frame = &body["frame"];
    let align_to = &body["align_to"];
    let frame_id = frame["id"].as_i64().unwrap_or(0);
    let frame_provenance = frame["provenance"].as_str().unwrap_or("?");
    let align_id = align_to["id"].as_i64().unwrap_or(0);
    let align_provenance = align_to["provenance"].as_str().unwrap_or("?");

    println!(
        "atlas frame {frame_id}  [{kb}]  ({frame_provenance})  aligned onto frame {align_id} ({align_provenance})"
    );
    let points = body["points"].as_array().cloned().unwrap_or_default();
    let matched = body["matched"].as_u64().unwrap_or(0);
    let residual = body["residual"].as_f64().unwrap_or(0.0);
    println!(
        "  {} point{} · {matched} matched against frame {align_id} · residual {residual:.6}",
        points.len(),
        if points.len() == 1 { "" } else { "s" }
    );
    // The residual honesty note (see the server route's doc comment): a
    // similarity transform can never fully undo the atlas layout's
    // independent x/y min-max normalisation, so a nonzero number here is
    // expected, not a bug.
    println!(
        "  (residual won't hit zero even for identical geometry — the atlas layout's x/y\n   normalisation is independently scaled per axis, which no rotate+scale+translate\n   fit can fully undo)"
    );
    for p in &points {
        let id = p["artifact_id"].as_str().unwrap_or("?");
        let x = p["x"].as_f64().unwrap_or(0.0);
        let y = p["y"].as_f64().unwrap_or(0.0);
        let cluster = p["cluster"].as_i64().unwrap_or(0);
        println!("  {id:<14} x={x:>8.4}  y={y:>8.4}  cluster={cluster}");
    }
}

/// A backfill is N full layout passes (one per cut point), so it gets a far
/// more generous budget than the 60 s recompute tail.
const BACKFILL_TIMEOUT: Duration = Duration::from_secs(600);

/// `kb atlas backfill --kb NAME [--frames N] [--json]` — W3 T-d. Prints the
/// PLAN first (what it will reconstruct: each cut point + the doc count
/// there), then tails `/api/events` for per-frame progress and the run's
/// `atlas.backfill.complete`.
///
/// The plan comes back in the route's own 202 body — the daemon computes it
/// synchronously before spawning the work — so this is one POST, not a
/// separate dry-run call.
///
/// `--json` emits ONE document (`{plan, result}`) at the end rather than
/// interleaving two: a machine consumer wants a single parse, and the
/// plan-before-work ordering it exists to preserve is a human affordance.
pub async fn backfill(
    kb: &str,
    frames: Option<u32>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let mut url = format!(
        "{base}/api/kb/{}/atlas/history/backfill",
        encode_path_segment(kb)
    );
    if let Some(f) = frames {
        url.push_str(&format!("?frames={f}"));
    }

    // K10 — subscribe BEFORE the POST (a small kb can finish the whole
    // backfill before a late subscription connects). Same ordering as
    // `run_and_tail` above.
    let resp_stream = open_events_stream(base, bearer, None, "").await?;
    let mut reader = FrameReader::from_response(resp_stream);

    let client = client_with_timeout_and_bearer(30, bearer)?;
    let plan: Value = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?
        .error_for_status()
        .with_context(|| format!("POST {url}"))?
        .json()
        .await?;

    let run = plan["run"].as_str().unwrap_or("?").to_string();
    if plan["status"].as_str() == Some("already-running") {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            println!(
                "atlas backfill  [{kb}]  another atlas job is already running (run {run}) — \
                 nothing started"
            );
        }
        return Ok(());
    }

    if !json {
        print!("{}", format_backfill_plan(kb, &plan));
    }

    let started = Instant::now();
    let result = loop {
        let Some(remaining) = BACKFILL_TIMEOUT.checked_sub(started.elapsed()) else {
            return Err(anyhow!(
                "timed out waiting for atlas.backfill.complete (run {run})"
            ));
        };
        let frame = match tokio::time::timeout(remaining, reader.next_frame()).await {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => {
                return Err(anyhow!("/api/events closed before the backfill completed"))
            }
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => {
                return Err(anyhow!(
                    "timed out waiting for atlas.backfill.complete (run {run})"
                ))
            }
        };
        let payload: Value =
            serde_json::from_str(frame.data.as_deref().unwrap_or("{}")).unwrap_or_default();
        match frame.event.as_deref() {
            // Per-frame progress rides the SAME event a recorded frame emits
            // — filtered to this kb (it carries no run id).
            Some("atlas.snapshot.recorded") if payload["kb"].as_str() == Some(kb) => {
                if !json {
                    println!(
                        "  wrote reconstructed frame {} · {} points",
                        payload["id"].as_i64().unwrap_or(0),
                        payload["points"].as_i64().unwrap_or(0)
                    );
                }
            }
            Some("atlas.backfill.complete") if payload["run"].as_str() == Some(&run) => {
                break payload;
            }
            _ => {}
        }
    };

    if let Some(err) = result["error"].as_str() {
        return Err(anyhow!("backfill failed: {err}"));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plan": plan,
                "result": result,
            }))?
        );
    } else {
        print!("{}", format_backfill_result(kb, &result));
    }
    Ok(())
}

/// The plan block, printed BEFORE any frame is written. Pure (Value in,
/// String out) so the "the word reconstructed is always there" contract is
/// unit-testable.
fn format_backfill_plan(kb: &str, plan: &Value) -> String {
    let cuts = plan["cuts"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    if cuts.is_empty() {
        out.push_str(&format!(
            "atlas backfill  [{kb}]  nothing to reconstruct — no doc in this kb carries an\n  \
             mtime to place it on the time axis.\n"
        ));
        return out;
    }
    out.push_str(&format!(
        "atlas backfill  [{kb}]  will reconstruct {} frame{} (run {})\n",
        cuts.len(),
        if cuts.len() == 1 { "" } else { "s" },
        plan["run"].as_str().unwrap_or("?")
    ));
    // The honesty sentence comes from the daemon verbatim so the CLI can't
    // paraphrase it into a stronger claim.
    out.push_str(&format!(
        "  RECONSTRUCTED, not recorded: {}\n",
        plan["note"]
            .as_str()
            .unwrap_or("today's embeddings over each cut point's docs — not recorded history")
    ));
    out.push_str(&format!("  {:<17} {:>10}\n", "cut point (UTC)", "docs"));
    for c in &cuts {
        let when = fmt_ts(c["cut_unix"].as_i64().unwrap_or(0));
        let docs = c["doc_count"].as_i64().unwrap_or(0);
        out.push_str(&format!("  {when:<17} {docs:>10}\n"));
    }
    let no_mtime = plan["docs_without_mtime"].as_i64().unwrap_or(0);
    if no_mtime > 0 {
        out.push_str(&format!(
            "  (excluded: {no_mtime} doc{} with no mtime — placed in no frame)\n",
            if no_mtime == 1 { "" } else { "s" }
        ));
    }
    out
}

/// The result block. `skipped` is load-bearing, not noise: it is how the
/// idempotence rule (skip on coord_hash) shows itself — a second run
/// reports every frame skipped and writes nothing.
fn format_backfill_result(kb: &str, result: &Value) -> String {
    let written = result["written"].as_i64().unwrap_or(0);
    let skipped = result["skipped"].as_i64().unwrap_or(0);
    let ms = result["duration_ms"].as_i64().unwrap_or(0);
    let mut out = format!(
        "✓ [{kb}] {written} reconstructed frame{} written · {skipped} skipped ({ms} ms)\n",
        if written == 1 { "" } else { "s" }
    );
    if written == 0 && skipped > 0 {
        out.push_str(
            "  (nothing new — a frame whose geometry the kb already holds is not rewritten;\n   \
             re-running a backfill is idempotent)\n",
        );
    }
    out
}

/// `kb atlas prune --kb NAME --keep N [--json]` — explicit operator
/// retention: `POST /api/kb/{kb}/atlas/history/prune?keep=N` (W3 T-b). The
/// insert path already self-prunes to `DEFAULT_ATLAS_FRAMES_KEEP` (24) on
/// every recompute/recluster; this is for an operator who wants a tighter
/// bound right now.
pub async fn prune(
    kb: &str,
    keep: u32,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!(
        "{base}/api/kb/{}/atlas/history/prune?keep={keep}",
        encode_path_segment(kb)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?
        .error_for_status()
        .with_context(|| format!("POST {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    // Honest about how many it removed — never `keep` itself (a kb with
    // fewer than `keep` frames removes zero).
    let removed = body["removed"].as_u64().unwrap_or(0);
    println!(
        "atlas history  [{kb}]  pruned {removed} frame{} (kept newest {keep})",
        if removed == 1 { "" } else { "s" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- W3 T-d: `kb atlas backfill` output -------------------------------

    fn sample_plan() -> Value {
        json!({
            "run": "run-1",
            "events": "/api/events?filter=run:run-1",
            "kb": "kb1",
            "status": "started",
            "provenance": "reconstructed",
            "cuts": [
                {"cut_unix": 1_700_000_000i64, "doc_count": 3},
                {"cut_unix": 1_700_086_400i64, "doc_count": 7},
            ],
            "docs_without_mtime": 1,
            "note": "reconstructed frames use TODAY's embeddings over the docs that existed at \
                     each cut point (by mtime) — they are not recorded history",
        })
    }

    #[test]
    fn backfill_plan_names_the_cut_points_and_says_reconstructed() {
        let out = format_backfill_plan("kb1", &sample_plan());
        // The plan is printed BEFORE the work: what, how many, and how big.
        assert!(out.contains("will reconstruct 2 frames"), "{out}");
        assert!(out.contains("2023-11-14 22:13"), "{out}");
        assert!(out.contains(" 3"), "{out}");
        assert!(out.contains(" 7"), "{out}");
        // The word itself — never a subtle badge.
        assert!(out.to_lowercase().contains("reconstructed"), "{out}");
        assert!(out.contains("not recorded history"), "{out}");
        // Docs with no mtime are disclosed, not silently dropped.
        assert!(out.contains("excluded: 1 doc with no mtime"), "{out}");
    }

    #[test]
    fn backfill_plan_empty_corpus_says_why_instead_of_printing_a_blank_table() {
        let plan = json!({"run": "r", "cuts": [], "docs_without_mtime": 4});
        let out = format_backfill_plan("kb1", &plan);
        assert!(out.contains("nothing to reconstruct"), "{out}");
        assert!(out.contains("mtime"), "{out}");
    }

    #[test]
    fn backfill_result_reports_written_and_skipped() {
        let out = format_backfill_result(
            "kb1",
            &json!({"written": 2, "skipped": 1, "duration_ms": 1234}),
        );
        assert!(out.contains("2 reconstructed frames written"), "{out}");
        assert!(out.contains("1 skipped"), "{out}");
        assert!(out.contains("1234 ms"), "{out}");
    }

    #[test]
    fn backfill_result_explains_a_fully_skipped_rerun() {
        // The idempotence rule, made visible: a second run writes nothing.
        let out = format_backfill_result(
            "kb1",
            &json!({"written": 0, "skipped": 4, "duration_ms": 90}),
        );
        assert!(out.contains("0 reconstructed frames written"), "{out}");
        assert!(out.contains("idempotent"), "{out}");
    }

    #[test]
    fn history_empty_state_offers_backfill_without_calling_it_history() {
        let out = format_history_human("kb1", &json!({"frames": []}));
        assert!(out.contains("kb atlas backfill --kb kb1"), "{out}");
        assert!(out.contains("RECONSTRUCTED"), "{out}");
        assert!(out.contains("not recorded history"), "{out}");
    }

    #[test]
    fn history_table_labels_every_frames_provenance() {
        let out = format_history_human(
            "kb1",
            &json!({"frames": [
                {"id": 2, "created_at_unix": 1_700_086_400i64, "point_count": 9,
                 "cluster_count": 3, "layout": "umap", "provenance": "recorded"},
                {"id": 1, "created_at_unix": 1_700_000_000i64, "point_count": 5,
                 "cluster_count": 2, "layout": "pca", "provenance": "reconstructed"},
            ]}),
        );
        assert!(out.contains("provenance"), "{out}");
        assert!(out.contains("recorded"), "{out}");
        assert!(out.contains("reconstructed"), "{out}");
    }
}
