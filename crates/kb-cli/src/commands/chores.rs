//! `kb chores` (v0.44 F10) — one derived agenda of due agent-layer upkeep.
//!
//! A THIN COMPOSITION of reads that already exist: the distill queue
//! (`GET /api/sessions?undistilled=1`), the project slate's counts
//! (`GET /api/slates`), the memory triage queue (`GET /api/memory/triage`),
//! the resurface queue (`GET /api/kb/{kb}/resurface`) and a local scan of
//! the hook marker directory. It prints one line per DUE chore, naming the
//! skill to run and why, and stores nothing: the daemon schedules and runs
//! nothing, the skills stay agent-layer (no in-daemon LLM, #10/#26), and a
//! lane that cannot be read is reported as unavailable rather than guessed.
//! Thresholds are the constants below (config comes later).
//!
//! `--line` prints a single counts-only line (nothing when nothing is due)
//! and prints it at most once per UTC day, so a SessionStart hook can call it
//! without becoming noise. `kb-wake.sh` (SessionStart) calls it; a daemon that
//! is down or a CLI without the verb stays silent there.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::http;

/// Undistilled sessions above this are a chore (any debt at all).
pub const DISTILL_DUE_MIN: usize = 1;
/// Page cap for the distill queue read; a full page prints as `N+`.
const DISTILL_PAGE: usize = 50;
/// `found + idea` posts that make a tidy due (mirrors the slate's own
/// per-session nudge, `kb_core::slate::NUDGE_THRESHOLD`).
pub const SLATE_TIDY_FOUNDS: u64 = 8;
/// Posts in a slate's CURRENT generation at which a distill + rotate is due.
pub const SLATE_ROTATE_POSTS: u64 = 300;
/// Memory triage items (the queue is bounded ~10) at which a pass is due.
pub const TRIAGE_DUE_MIN: usize = 5;
/// Resurface items at which a look is due.
pub const RESURFACE_DUE_MIN: usize = 10;
/// Hook marker files older than this many days are stale...
pub const STALE_MARKER_DAYS: u64 = 14;
/// ...and this many of them make a prune due.
pub const STALE_MARKER_DUE_MIN: usize = 2_000;

/// Days since the newest `weekly-review` note (what `/kb-weekly` writes) at
/// which another review is due. Only a note that EXISTS can age: a user who
/// never ran the skill is not nagged to start.
pub const WEEKLY_NOTE_DUE_DAYS: u64 = 10;
/// Tag `/kb-weekly` stamps on the note it saves.
pub const WEEKLY_NOTE_TAG: &str = "weekly-review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlateFacts {
    pub slug: String,
    pub found_idea: u64,
    pub generation_posts: u64,
    pub hand_unack: u64,
}

/// What each lane read, `None` = the lane could not be read.
#[derive(Debug, Clone, Default)]
pub struct ChoreInputs {
    /// Undistilled sessions on the first page.
    pub undistilled: Option<usize>,
    /// The cwd's slate, when one exists (lane readable, no slate = `Some(None)`).
    pub slate: Option<Option<SlateFacts>>,
    pub triage_items: Option<usize>,
    pub resurface_items: Option<usize>,
    pub stale_markers: Option<usize>,
    /// Age in days of the newest weekly-review note (lane readable, no such
    /// note = `Some(None)`).
    pub weekly_note_age_days: Option<Option<u64>>,
    /// `(cli_sha, daemon_sha)` when both stamps are known and name different
    /// commits (lane readable, no skew = `Some(None)`).
    pub cli_skew: Option<Option<(String, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chore {
    pub id: &'static str,
    pub skill: String,
    pub reason: String,
}

/// Pure: inputs in, due chores out (stable order). Every threshold above is
/// exercised by a unit test.
pub fn decide(i: &ChoreInputs) -> Vec<Chore> {
    let mut out = Vec::new();
    if let Some(n) = i.undistilled.filter(|n| *n >= DISTILL_DUE_MIN) {
        let shown = if n >= DISTILL_PAGE {
            format!("{DISTILL_PAGE}+")
        } else {
            n.to_string()
        };
        out.push(Chore {
            id: "distill",
            skill: "/kb-distill --pending".into(),
            reason: format!("{shown} session(s) committed work but wrote no memory"),
        });
    }
    if let Some(Some(s)) = &i.slate {
        if s.found_idea >= SLATE_TIDY_FOUNDS {
            out.push(Chore {
                id: "slate-tidy",
                skill: "/kb-slate-tidy".into(),
                reason: format!(
                    "slate {}: {} undropped found/idea posts",
                    s.slug, s.found_idea
                ),
            });
        }
        if s.generation_posts >= SLATE_ROTATE_POSTS {
            out.push(Chore {
                id: "slate-rotate",
                skill: "/kb-slate-distill, then `kb slate rotate`".into(),
                reason: format!(
                    "slate {}: {} posts in the current generation",
                    s.slug, s.generation_posts
                ),
            });
        }
        if s.hand_unack > 0 {
            out.push(Chore {
                id: "slate-hand",
                skill: "`kb slate open`".into(),
                reason: format!("slate {}: {} unacknowledged hand(s)", s.slug, s.hand_unack),
            });
        }
    }
    if let Some(n) = i.triage_items.filter(|n| *n >= TRIAGE_DUE_MIN) {
        out.push(Chore {
            id: "memory-triage",
            skill: "`kb memory triage`".into(),
            reason: format!("{n} memories queued for a hygiene pass"),
        });
    }
    if let Some(n) = i.resurface_items.filter(|n| *n >= RESURFACE_DUE_MIN) {
        out.push(Chore {
            id: "resurface",
            skill: "`kb resurface`".into(),
            reason: format!("{n} artifacts waiting (open comments / unfinished reads)"),
        });
    }
    if let Some(n) = i.stale_markers.filter(|n| *n >= STALE_MARKER_DUE_MIN) {
        out.push(Chore {
            id: "stale-markers",
            skill: format!("`find <cache>/kb -type f -mtime +{STALE_MARKER_DAYS} -delete`"),
            reason: format!("{n} hook marker files older than {STALE_MARKER_DAYS}d"),
        });
    }
    if let Some(Some(days)) = i
        .weekly_note_age_days
        .filter(|d| d.is_some_and(|d| d >= WEEKLY_NOTE_DUE_DAYS))
    {
        out.push(Chore {
            id: "weekly-note",
            skill: "/kb-weekly".into(),
            reason: format!("the newest weekly review is {days} day(s) old"),
        });
    }
    if let Some(Some((cli, daemon))) = &i.cli_skew {
        out.push(Chore {
            id: "cli-skew",
            skill: "`kb doctor --hooks`".into(),
            reason: format!("this CLI ({cli}) and the daemon ({daemon}) are different builds"),
        });
    }
    out
}

/// Pure: do two build stamps name different commits? `None` when either is
/// unknown (a missing stamp is reported by `kb doctor --hooks`, not guessed
/// at here). Prefix-equal shas and a `-dirty` suffix count as the same commit.
pub fn skew_between(cli_sha: &str, daemon_sha: &str) -> Option<(String, String)> {
    use super::doctor::sha_unknown;
    if sha_unknown(cli_sha) || sha_unknown(daemon_sha) {
        return None;
    }
    let c = cli_sha.strip_suffix("-dirty").unwrap_or(cli_sha);
    let d = daemon_sha.strip_suffix("-dirty").unwrap_or(daemon_sha);
    if c.starts_with(d) || d.starts_with(c) {
        None
    } else {
        Some((cli_sha.to_string(), daemon_sha.to_string()))
    }
}

/// Pure: age in whole days of the newest note tagged [`WEEKLY_NOTE_TAG`] in
/// a `/api/notes` body; `None` when no such note carries a timestamp.
pub fn weekly_note_age_days(notes: &Value, now_unix: i64) -> Option<u64> {
    let newest = notes["notes"]
        .as_array()?
        .iter()
        .filter(|n| {
            n["tags"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t.as_str() == Some(WEEKLY_NOTE_TAG)))
        })
        .filter_map(|n| n["updated_at"].as_i64())
        .max()?;
    Some((now_unix - newest).max(0) as u64 / 86_400)
}

/// The `--line` form: counts only, one line, nothing when nothing is due.
pub fn count_line(due: &[Chore]) -> Option<String> {
    if due.is_empty() {
        return None;
    }
    let ids: Vec<&str> = due.iter().map(|c| c.id).collect();
    Some(format!(
        "kb chores: {} due ({}) — run `kb chores`",
        due.len(),
        ids.join(", ")
    ))
}

/// Rate limit for `--line`: emit unless the last emission was today (UTC).
pub fn should_emit(last_day: Option<&str>, today: &str) -> bool {
    last_day.map(str::trim) != Some(today)
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("kb"))
}

/// Count regular files under `dir` whose mtime is older than `days`.
pub fn count_stale_markers(dir: &Path, days: u64, now: std::time::SystemTime) -> Option<usize> {
    let cutoff = now.checked_sub(std::time::Duration::from_secs(days * 86_400))?;
    let rd = std::fs::read_dir(dir).ok()?;
    Some(
        rd.filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .filter(|m| m.modified().map(|t| t < cutoff).unwrap_or(false))
            .count(),
    )
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

async fn gather(daemon: Option<&str>, bearer: Option<&str>, cwd: &Path) -> Result<ChoreInputs> {
    let base = http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;

    let undistilled = get_json(
        &client,
        &format!("{base}/api/sessions?undistilled=1&limit={DISTILL_PAGE}"),
    )
    .await
    .and_then(|v| v["sessions"].as_array().map(Vec::len));

    let slug = super::memory::current_repo_slug_in(cwd);
    let slate = get_json(&client, &format!("{base}/api/slates"))
        .await
        .and_then(|v| v.as_array().cloned())
        .map(|rows| {
            rows.iter()
                .find(|r| r["slug"].as_str() == Some(slug.as_str()))
                .map(|r| SlateFacts {
                    slug: slug.clone(),
                    found_idea: r["counts"]["found"].as_u64().unwrap_or(0)
                        + r["counts"]["idea"].as_u64().unwrap_or(0),
                    generation_posts: r["generation_posts"].as_u64().unwrap_or(0),
                    hand_unack: r["counts"]["hand_unack"].as_u64().unwrap_or(0),
                })
        });

    let triage_items = get_json(&client, &format!("{base}/api/memory/triage"))
        .await
        .and_then(|v| v["items"].as_array().map(Vec::len));

    let resurface_items = match http::resolve_default_kb(None, daemon, bearer).await {
        Ok(kb) => get_json(
            &client,
            &format!(
                "{base}/api/kb/{}/resurface?limit=50",
                http::encode_path_segment(&kb)
            ),
        )
        .await
        .and_then(|v| v["items"].as_array().map(Vec::len)),
        Err(_) => None,
    };

    let now_unix = chrono::Utc::now().timestamp();
    let weekly_note_age_days = get_json(&client, &format!("{base}/api/notes"))
        .await
        .map(|v| weekly_note_age_days(&v, now_unix));

    let cli_skew = get_json(&client, &format!("{base}/api/identity"))
        .await
        .map(|v| {
            let (describe, sha) = super::version::stamp();
            if super::version::stamp_missing(describe, sha) {
                return None;
            }
            v["build_sha"].as_str().and_then(|d| skew_between(sha, d))
        });

    let stale_markers = cache_dir()
        .and_then(|d| count_stale_markers(&d, STALE_MARKER_DAYS, std::time::SystemTime::now()));
    Ok(ChoreInputs {
        undistilled,
        slate,
        triage_items,
        resurface_items,
        stale_markers,
        weekly_note_age_days,
        cli_skew,
    })
}

fn unavailable(i: &ChoreInputs) -> Vec<&'static str> {
    let mut v = Vec::new();
    if i.undistilled.is_none() {
        v.push("distill");
    }
    if i.slate.is_none() {
        v.push("slate");
    }
    if i.triage_items.is_none() {
        v.push("memory-triage");
    }
    if i.resurface_items.is_none() {
        v.push("resurface");
    }
    if i.stale_markers.is_none() {
        v.push("stale-markers");
    }
    if i.weekly_note_age_days.is_none() {
        v.push("weekly-note");
    }
    if i.cli_skew.is_none() {
        v.push("cli-skew");
    }
    v
}

pub async fn run(
    json_out: bool,
    line: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let inputs = match gather(daemon, bearer, &cwd).await {
        Ok(i) => i,
        // `--line` runs from a session-start hook: an unreachable daemon
        // must stay silent, not print an error into the session.
        Err(_) if line => return Ok(()),
        Err(e) => return Err(e),
    };
    let due = decide(&inputs);
    if line {
        let Some(text) = count_line(&due) else {
            return Ok(());
        };
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let marker = cache_dir().map(|d| d.join("chores-line-day"));
        let last = marker
            .as_ref()
            .and_then(|m| std::fs::read_to_string(m).ok());
        if !should_emit(last.as_deref(), &today) {
            return Ok(());
        }
        if let Some(m) = &marker {
            if let Some(parent) = m.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(m, &today);
        }
        println!("{text}");
        return Ok(());
    }
    if json_out {
        let rows: Vec<Value> = due
            .iter()
            .map(|c| json!({"id": c.id, "skill": c.skill, "reason": c.reason}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "due": rows,
                "unavailable": unavailable(&inputs),
            }))?
        );
        return Ok(());
    }
    if due.is_empty() {
        println!("nothing due");
    }
    for c in &due {
        println!("{} → {} ({})", c.id, c.skill, c.reason);
    }
    let un = unavailable(&inputs);
    if !un.is_empty() {
        println!("unavailable: {}", un.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slate(found_idea: u64, generation_posts: u64, hand_unack: u64) -> ChoreInputs {
        ChoreInputs {
            slate: Some(Some(SlateFacts {
                slug: "kb".into(),
                found_idea,
                generation_posts,
                hand_unack,
            })),
            ..ChoreInputs::default()
        }
    }

    #[test]
    fn nothing_known_or_nothing_over_threshold_is_nothing_due() {
        assert!(decide(&ChoreInputs::default()).is_empty());
        let i = ChoreInputs {
            undistilled: Some(0),
            triage_items: Some(TRIAGE_DUE_MIN - 1),
            resurface_items: Some(RESURFACE_DUE_MIN - 1),
            stale_markers: Some(STALE_MARKER_DUE_MIN - 1),
            slate: Some(None),
            weekly_note_age_days: Some(Some(WEEKLY_NOTE_DUE_DAYS - 1)),
            cli_skew: Some(None),
        };
        assert!(decide(&i).is_empty());
    }

    #[test]
    fn each_threshold_flips_exactly_its_own_chore() {
        let ids = |i: &ChoreInputs| decide(i).iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(
            ids(&ChoreInputs {
                undistilled: Some(7),
                ..ChoreInputs::default()
            }),
            vec!["distill"]
        );
        assert_eq!(ids(&slate(SLATE_TIDY_FOUNDS, 1, 0)), vec!["slate-tidy"]);
        assert!(ids(&slate(SLATE_TIDY_FOUNDS - 1, 1, 0)).is_empty());
        assert_eq!(ids(&slate(0, SLATE_ROTATE_POSTS, 0)), vec!["slate-rotate"]);
        assert_eq!(ids(&slate(0, 1, 2)), vec!["slate-hand"]);
        assert_eq!(
            ids(&ChoreInputs {
                triage_items: Some(TRIAGE_DUE_MIN),
                resurface_items: Some(RESURFACE_DUE_MIN),
                stale_markers: Some(STALE_MARKER_DUE_MIN),
                ..ChoreInputs::default()
            }),
            vec!["memory-triage", "resurface", "stale-markers"]
        );
    }

    #[test]
    fn weekly_note_is_due_only_once_an_existing_note_ages_out() {
        let ids = |i: &ChoreInputs| decide(i).iter().map(|c| c.id).collect::<Vec<_>>();
        let age = |a| ChoreInputs {
            weekly_note_age_days: Some(a),
            ..ChoreInputs::default()
        };
        assert_eq!(ids(&age(Some(WEEKLY_NOTE_DUE_DAYS))), vec!["weekly-note"]);
        assert!(ids(&age(Some(WEEKLY_NOTE_DUE_DAYS - 1))).is_empty());
        assert!(
            ids(&age(None)).is_empty(),
            "no note ever written is not a nag"
        );
    }

    #[test]
    fn weekly_note_age_reads_the_newest_tagged_note() {
        let now = 100 * 86_400;
        let body = json!({"notes": [
            {"tags": ["weekly-review"], "updated_at": now - 12 * 86_400},
            {"tags": ["weekly-review", "kb"], "updated_at": now - 3 * 86_400},
            {"tags": ["other"], "updated_at": now},
            {"tags": ["weekly-review"]},
        ]});
        assert_eq!(weekly_note_age_days(&body, now), Some(3));
        assert_eq!(
            weekly_note_age_days(&json!({"notes": [{"tags": ["x"], "updated_at": now}]}), now),
            None
        );
        assert_eq!(weekly_note_age_days(&json!({}), now), None);
    }

    #[test]
    fn cli_skew_needs_two_known_stamps_naming_different_commits() {
        assert_eq!(skew_between("abc1234", "abc1234ffff"), None);
        assert_eq!(skew_between("abc1234-dirty", "abc1234"), None);
        assert_eq!(skew_between("unknown", "abc1234"), None);
        assert_eq!(skew_between("abc1234", ""), None);
        assert_eq!(
            skew_between("abc1234", "def5678"),
            Some(("abc1234".into(), "def5678".into()))
        );
        let d = decide(&ChoreInputs {
            cli_skew: Some(skew_between("abc1234", "def5678")),
            ..ChoreInputs::default()
        });
        assert_eq!(d[0].id, "cli-skew");
        assert!(d[0].reason.contains("abc1234") && d[0].reason.contains("def5678"));
    }

    #[test]
    fn a_full_distill_page_prints_as_a_floor() {
        let d = decide(&ChoreInputs {
            undistilled: Some(DISTILL_PAGE),
            ..ChoreInputs::default()
        });
        assert!(d[0].reason.starts_with("50+ session(s)"), "{:?}", d[0]);
    }

    #[test]
    fn the_count_line_is_one_line_counts_only_and_empty_when_nothing_is_due() {
        assert_eq!(count_line(&[]), None);
        let due = decide(&ChoreInputs {
            undistilled: Some(3),
            triage_items: Some(9),
            ..ChoreInputs::default()
        });
        let line = count_line(&due).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(
            line,
            "kb chores: 2 due (distill, memory-triage) — run `kb chores`"
        );
        assert!(
            !line.contains("session(s)"),
            "no episodic detail in the line"
        );
    }

    #[test]
    fn the_line_is_rate_limited_to_once_per_utc_day() {
        assert!(should_emit(None, "2026-10-03"));
        assert!(should_emit(Some("2026-10-02"), "2026-10-03"));
        assert!(!should_emit(Some("2026-10-03"), "2026-10-03"));
        assert!(!should_emit(Some("2026-10-03\n"), "2026-10-03"));
    }

    #[test]
    fn stale_markers_counts_only_old_regular_files() {
        let dir = std::env::temp_dir().join(format!("kb-chores-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("fresh"), "x").unwrap();
        std::fs::write(dir.join("old"), "x").unwrap();
        // "Now" 30 days ahead makes both files older than the 14d cutoff;
        // "now" itself makes neither stale.
        let now = std::time::SystemTime::now();
        assert_eq!(count_stale_markers(&dir, 14, now), Some(0));
        let later = now + std::time::Duration::from_secs(30 * 86_400);
        assert_eq!(count_stale_markers(&dir, 14, later), Some(2));
        assert_eq!(count_stale_markers(&dir.join("missing"), 14, later), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
