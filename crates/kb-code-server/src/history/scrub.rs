//! V76-R3d — `scrub/1`: file-scoped time stops and nearest-prior `?at=`.
//!
//! Stops are the commits that touched one path (`git log --follow`), newest
//! first. The pick is always nearest-prior (`exact` only on a same-second
//! hit). An instant older than the oldest stop is a MISS naming the floor
//! — never a silent degrade to the oldest (root invariant #14's
//! `resolve_as_of` posture). `--follow` renames are captioned on the stop
//! that performed them (`renamed_from`). D18 `author_kind` is the same
//! ladder [`super::facts::agent_class_for`] uses for a branch tip.
//!
//! This module never constructs `Command::new("git")`: every spawn goes
//! through [`super::run_git_raw`]. The caller-supplied path is always
//! passed after `--`. Caps refuse with numbers (`limit` above
//! [`MAX_LIMIT`], a file with more than [`HARD_CAP`] stops) rather than
//! truncating silently.

use super::{run_git_raw, HistoryError};
use crate::entities::RouteContract;
use crate::history::facts::{agent_class_for, AgentClass};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA: &str = "scrub/1";
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;
/// Walk cap for one file's `--follow` history. A file with more stops than
/// this is refused with the number, not silently trimmed.
pub const HARD_CAP: usize = 2000;
pub const ERR_BEFORE_FLOOR: &str = "urn:kb:errors:before-floor";

/// `git log --follow --format=` — record separator then six fields.
/// `%x1e` starts each commit so `--numstat`/`--name-status` lines that
/// follow cannot be confused with the next header.
const STOP_FMT: &str = "%x1e%H%x1f%s%x1f%at%x1f%ae%x1f%(trailers:key=Kb-Session,valueonly,separator=%x2c)%x1f%(trailers:key=Kb-Agent,valueonly,separator=%x2c)";

#[derive(Debug, Deserialize)]
pub struct StopsParams {
    pub repo: String,
    pub path: String,
    pub limit: Option<usize>,
    pub before: Option<i64>,
    /// Walk start (`git log <ref> -- <path>`), default HEAD. A file that
    /// lives only on a side branch is scrubbed by passing that branch.
    /// Validated into a [`Revspec`] by the route before it reaches argv.
    #[serde(rename = "ref", default)]
    pub rev: Option<String>,
}

fn stops_params_accept_without(omit: &str) -> bool {
    let mut q =
        serde_json::json!({ "repo": "r", "path": "a.rs", "limit": 10, "before": 1, "ref": "main" });
    q.as_object_mut().expect("object").remove(omit);
    serde_json::from_value::<StopsParams>(q).is_ok()
}

pub const STOPS_ROUTE: RouteContract = RouteContract {
    path: "/api/file/stops",
    handler: "routes::file_stops_route",
    required_params: &["repo", "path"],
    params_accept_without: stops_params_accept_without,
};

#[derive(Debug, Deserialize)]
pub struct AtParams {
    pub repo: String,
    pub path: String,
    pub at: i64,
    /// Walk start for the stop lookup (default HEAD); the blob is read at
    /// the resolved stop's sha either way. Validated into a [`Revspec`]
    /// by the route before it reaches argv.
    #[serde(rename = "ref", default)]
    pub rev: Option<String>,
}

fn at_params_accept_without(omit: &str) -> bool {
    let mut q = serde_json::json!({ "repo": "r", "path": "a.rs", "at": 1, "ref": "main" });
    q.as_object_mut().expect("object").remove(omit);
    serde_json::from_value::<AtParams>(q).is_ok()
}

pub const AT_ROUTE: RouteContract = RouteContract {
    path: "/api/file/at",
    handler: "routes::file_at_route",
    required_params: &["repo", "path", "at"],
    params_accept_without: at_params_accept_without,
};

pub const V76_R3D_ROUTES: &[RouteContract] = &[STOPS_ROUTE, AT_ROUTE];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    Exact,
    NearestPrior,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Floor {
    pub sha: String,
    pub when: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stop {
    pub sha: String,
    pub when: i64,
    pub author_kind: AgentClass,
    pub subject: String,
    pub insertions: u32,
    pub deletions: u32,
    /// Path at this commit. Differs from the request path after a rename
    /// `--follow` walked through.
    pub path: String,
    /// Present on the commit that performed the rename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScrubError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("this file has at least {seen} stops; cap is {cap}")]
    TooMany { cap: usize, seen: usize },
    #[error("limit {limit} is outside 1..={cap}")]
    Limit { limit: usize, cap: usize },
}

pub type ScrubResult<T> = std::result::Result<T, ScrubError>;

/// Newest stop with `when <= at`. `stops` is newest-first. `None` when
/// every stop is newer than `at` (the coordinate predates the floor).
pub fn resolve_as_of(stops: &[Stop], at: i64) -> Option<&Stop> {
    stops.iter().find(|s| s.when <= at)
}

pub fn resolution_of(stop: &Stop, at: i64) -> Resolution {
    if stop.when == at {
        Resolution::Exact
    } else {
        Resolution::NearestPrior
    }
}

pub fn clamp_limit(limit: Option<usize>) -> ScrubResult<usize> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(n) => Err(ScrubError::Limit {
            limit: n,
            cap: MAX_LIMIT,
        }),
    }
}

/// One file's `--follow` history, newest-first, optionally bounded by
/// `before` (unix seconds, git `--before=@<unix>`). `limit` is the already
/// validated page size. `agent_emails` is the D18 likely-rung set.
pub fn file_stops(
    repo_root: &Path,
    path: &str,
    limit: usize,
    before_unix: Option<i64>,
    agent_emails: &[String],
) -> ScrubResult<StopsPageInner> {
    let all = collect_stops(repo_root, path, HARD_CAP, before_unix, agent_emails)?;
    let floor = file_floor(repo_root, path)?;
    let total = all.len();
    let truncated = total > limit;
    let mut stops = all;
    stops.truncate(limit);
    Ok(StopsPageInner {
        stops,
        total,
        truncated,
        floor,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopsPageInner {
    pub stops: Vec<Stop>,
    pub total: usize,
    pub truncated: bool,
    pub floor: Option<Floor>,
}

pub enum AtHit {
    Hit {
        stop: Stop,
        resolution: Resolution,
        floor: Option<Floor>,
    },
    BeforeFloor {
        floor: Option<Floor>,
    },
}

/// Resolve `at` against the file's full (capped) stop list.
pub fn file_at(
    repo_root: &Path,
    path: &str,
    at: i64,
    agent_emails: &[String],
) -> ScrubResult<AtHit> {
    let all = collect_stops(repo_root, path, HARD_CAP, None, agent_emails)?;
    let floor = file_floor(repo_root, path)?;
    match resolve_as_of(&all, at).cloned() {
        Some(stop) => {
            let resolution = resolution_of(&stop, at);
            Ok(AtHit::Hit {
                stop,
                resolution,
                floor,
            })
        }
        None => Ok(AtHit::BeforeFloor { floor }),
    }
}

pub fn before_floor_message(path: &str, floor: Option<&Floor>) -> String {
    match floor {
        Some(f) => format!(
            "no stop of {path} is that old — the floor is {} at unix {}",
            &f.sha[..f.sha.len().min(12)],
            f.when
        ),
        None => format!("no stop of {path} is that old — this file has no recorded stops"),
    }
}

fn collect_stops(
    repo_root: &Path,
    path: &str,
    cap: usize,
    before_unix: Option<i64>,
    agent_emails: &[String],
) -> ScrubResult<Vec<Stop>> {
    let fmt_arg = format!("--format={STOP_FMT}");
    let n = (cap + 1).to_string();
    let before_arg = before_unix.map(|t| format!("--before=@{t}"));
    let mut args: Vec<&str> = vec![
        "log",
        "--follow",
        "--numstat",
        "--name-status",
        &fmt_arg,
        "-n",
        &n,
    ];
    if let Some(b) = before_arg.as_deref() {
        args.push(b);
    }
    args.push("--");
    args.push(path);
    let out = run_git_raw(repo_root, &args)?;
    let text = String::from_utf8_lossy(&out);
    let stops = parse_log_output(&text, path, agent_emails);
    if stops.len() > cap {
        return Err(ScrubError::TooMany {
            cap,
            seen: stops.len(),
        });
    }
    Ok(stops)
}

fn file_floor(repo_root: &Path, path: &str) -> ScrubResult<Option<Floor>> {
    let out = run_git_raw(
        repo_root,
        &[
            "log",
            "--follow",
            "--reverse",
            "--format=%H%x1f%at",
            "-n",
            "1",
            "--",
            path,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut f = line.splitn(2, '\u{1f}');
    let sha = f.next().unwrap_or("").to_string();
    let when: i64 = f.next().unwrap_or("").parse().unwrap_or(0);
    if sha.is_empty() {
        return Ok(None);
    }
    Ok(Some(Floor { sha, when }))
}

fn split_trailers(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse `git log --follow --numstat --name-status --format=STOP_FMT`.
pub fn parse_log_output(text: &str, request_path: &str, agent_emails: &[String]) -> Vec<Stop> {
    let mut out = Vec::new();
    for rec in text.split('\u{1e}') {
        let rec = rec.trim();
        if rec.is_empty() {
            continue;
        }
        let mut lines = rec.lines();
        let Some(header) = lines.next() else { continue };
        let mut fields = header.splitn(6, '\u{1f}');
        let Some(sha) = fields.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let subject = fields.next().unwrap_or("").to_string();
        let when: i64 = fields.next().unwrap_or("").parse().unwrap_or(0);
        let email = fields.next().unwrap_or("").to_string();
        let session = split_trailers(fields.next().unwrap_or(""));
        let agent = split_trailers(fields.next().unwrap_or(""));
        let mut insertions = 0u32;
        let mut deletions = 0u32;
        let mut renamed_from = None;
        let mut path_at = request_path.to_string();
        for line in lines {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let bits: Vec<&str> = line.split('\t').collect();
            if bits.len() >= 3 && (bits[0].starts_with('R') || bits[0].starts_with('C')) {
                // `R100\told\tnew` / `C100\told\tnew`
                renamed_from = Some(bits[1].to_string());
                path_at = bits[2].to_string();
                continue;
            }
            if bits.len() >= 3 {
                let (a, b, rest_path) = (bits[0], bits[1], bits[2]);
                let numstat = (a == "-" && b == "-")
                    || (a.parse::<i64>().is_ok() && b.parse::<i64>().is_ok());
                if numstat {
                    insertions = a.parse().unwrap_or(0);
                    deletions = b.parse().unwrap_or(0);
                    let p = if let Some((old, new)) = rest_path.split_once(" => ") {
                        renamed_from = renamed_from.or_else(|| Some(old.to_string()));
                        new
                    } else {
                        rest_path
                    };
                    if !p.is_empty() {
                        path_at = p.to_string();
                    }
                    continue;
                }
            }
            // `M\tpath` / `A\tpath` / `D\tpath`
            if bits.len() >= 2 && bits[0].len() == 1 {
                path_at = bits[1].to_string();
            }
        }
        let author_kind = agent_class_for(&email, &session, &agent, agent_emails);
        out.push(Stop {
            sha: sha.to_string(),
            when,
            author_kind,
            subject,
            insertions,
            deletions,
            path: path_at,
            renamed_from,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit_at(dir: &Path, file: &str, contents: &str, message: &str, unix_secs: i64) {
        std::fs::write(dir.join(file), contents).unwrap();
        git(dir, &["add", "-A"]);
        let date = format!("{unix_secs} +0000");
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        tmp
    }

    fn emails() -> Vec<String> {
        vec!["noreply@anthropic.com".into()]
    }

    #[test]
    fn parse_log_output_reads_header_numstat_and_rename() {
        let text = "\
\u{1e}aaa\u{1f}rename it\u{1f}1700002000\u{1f}test@example.com\u{1f}\u{1f}\n\
R100\told.txt\tnew.txt\n\
3\t1\told.txt => new.txt\n\
\u{1e}bbb\u{1f}c2\u{1f}1700001000\u{1f}noreply@anthropic.com\u{1f}\u{1f}\n\
M\told.txt\n\
2\t0\told.txt\n\
\u{1e}ccc\u{1f}c1\u{1f}1700000000\u{1f}bot@x\u{1f}sess-1\u{1f}\n\
A\told.txt\n\
1\t0\told.txt\n";
        let stops = parse_log_output(text, "new.txt", &emails());
        assert_eq!(stops.len(), 3);
        assert_eq!(stops[0].sha, "aaa");
        assert_eq!(stops[0].renamed_from.as_deref(), Some("old.txt"));
        assert_eq!(stops[0].path, "new.txt");
        assert_eq!(stops[0].insertions, 3);
        assert_eq!(stops[0].deletions, 1);
        assert_eq!(stops[0].author_kind, AgentClass::None);
        assert_eq!(stops[1].author_kind, AgentClass::Likely);
        assert_eq!(stops[2].author_kind, AgentClass::Exact);
        assert_eq!(stops[2].subject, "c1");
    }

    #[test]
    fn resolve_as_of_is_nearest_prior_exact_on_same_second_and_miss_before_floor() {
        let stops = parse_log_output(
            "\
\u{1e}c3\u{1f}n3\u{1f}300\u{1f}a@b\u{1f}\u{1f}\n\
\u{1e}c2\u{1f}n2\u{1f}200\u{1f}a@b\u{1f}\u{1f}\n\
\u{1e}c1\u{1f}n1\u{1f}100\u{1f}a@b\u{1f}\u{1f}\n",
            "a.txt",
            &[],
        );
        assert_eq!(resolve_as_of(&stops, 300).unwrap().sha, "c3");
        assert_eq!(
            resolution_of(resolve_as_of(&stops, 300).unwrap(), 300),
            Resolution::Exact
        );
        assert_eq!(resolve_as_of(&stops, 250).unwrap().sha, "c2");
        assert_eq!(
            resolution_of(resolve_as_of(&stops, 250).unwrap(), 250),
            Resolution::NearestPrior
        );
        assert_eq!(resolve_as_of(&stops, 100).unwrap().sha, "c1");
        assert!(resolve_as_of(&stops, 99).is_none());
        assert!(resolve_as_of(&[], 1).is_none());
    }

    #[test]
    fn file_stops_follows_a_rename_and_names_the_floor() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "one\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "one\ntwo\n", "c2", 1_700_001_000);
        git(dir, &["mv", "a.txt", "renamed.txt"]);
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", "rename it"])
            .env("GIT_AUTHOR_DATE", "1700002000 +0000")
            .env("GIT_COMMITTER_DATE", "1700002000 +0000")
            .status()
            .unwrap();
        assert!(status.success());

        let page = file_stops(dir, "renamed.txt", 100, None, &emails()).unwrap();
        assert!(!page.truncated);
        assert_eq!(page.total, 3);
        let subjects: Vec<&str> = page.stops.iter().map(|s| s.subject.as_str()).collect();
        assert_eq!(subjects, vec!["rename it", "c2", "c1"]);
        assert_eq!(page.stops[0].renamed_from.as_deref(), Some("a.txt"));
        let floor = page.floor.expect("floor");
        assert_eq!(floor.sha, page.stops[2].sha);
        assert_eq!(floor.when, 1_700_000_000);
    }

    #[test]
    fn file_stops_limit_truncates_with_true_total() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "1\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "2\n", "c2", 1_700_001_000);
        commit_at(dir, "a.txt", "3\n", "c3", 1_700_002_000);
        let page = file_stops(dir, "a.txt", 2, None, &emails()).unwrap();
        assert_eq!(page.stops.len(), 2);
        assert!(page.truncated);
        assert_eq!(page.total, 3);
        assert_eq!(page.stops[0].subject, "c3");
        assert_eq!(page.floor.as_ref().unwrap().when, 1_700_000_000);
    }

    #[test]
    fn file_at_exact_nearest_prior_and_before_floor() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "1\n", "c1", 1_700_000_000);
        commit_at(dir, "a.txt", "2\n", "c2", 1_700_001_000);

        match file_at(dir, "a.txt", 1_700_001_000, &emails()).unwrap() {
            AtHit::Hit {
                stop, resolution, ..
            } => {
                assert_eq!(stop.subject, "c2");
                assert_eq!(resolution, Resolution::Exact);
            }
            AtHit::BeforeFloor { .. } => panic!("expected hit"),
        }
        match file_at(dir, "a.txt", 1_700_000_500, &emails()).unwrap() {
            AtHit::Hit {
                stop, resolution, ..
            } => {
                assert_eq!(stop.subject, "c1");
                assert_eq!(resolution, Resolution::NearestPrior);
            }
            AtHit::BeforeFloor { .. } => panic!("expected hit"),
        }
        match file_at(dir, "a.txt", 1_699_999_999, &emails()).unwrap() {
            AtHit::BeforeFloor { floor } => {
                assert_eq!(floor.unwrap().when, 1_700_000_000);
            }
            AtHit::Hit { .. } => panic!("expected miss"),
        }
    }

    #[test]
    fn file_at_follows_rename_to_the_historical_path() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit_at(dir, "a.txt", "one\n", "c1", 1_700_000_000);
        git(dir, &["mv", "a.txt", "renamed.txt"]);
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", "rename it"])
            .env("GIT_AUTHOR_DATE", "1700002000 +0000")
            .env("GIT_COMMITTER_DATE", "1700002000 +0000")
            .status()
            .unwrap();
        assert!(status.success());

        match file_at(dir, "renamed.txt", 1_700_000_000, &emails()).unwrap() {
            AtHit::Hit { stop, .. } => {
                assert_eq!(stop.subject, "c1");
                assert_eq!(stop.path, "a.txt");
            }
            AtHit::BeforeFloor { .. } => panic!("expected hit at the pre-rename path"),
        }
    }

    #[test]
    fn clamp_limit_refuses_zero_and_over_cap() {
        assert_eq!(clamp_limit(None).unwrap(), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(1)).unwrap(), 1);
        assert_eq!(clamp_limit(Some(MAX_LIMIT)).unwrap(), MAX_LIMIT);
        match clamp_limit(Some(0)) {
            Err(ScrubError::Limit { limit: 0, cap }) => assert_eq!(cap, MAX_LIMIT),
            other => panic!("{other:?}"),
        }
        match clamp_limit(Some(MAX_LIMIT + 1)) {
            Err(ScrubError::Limit { limit, cap }) => {
                assert_eq!(limit, MAX_LIMIT + 1);
                assert_eq!(cap, MAX_LIMIT);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stops_params_require_repo_and_path() {
        assert!(stops_params_accept_without(""));
        assert!(stops_params_accept_without("limit"));
        assert!(stops_params_accept_without("before"));
        assert!(!stops_params_accept_without("repo"));
        assert!(!stops_params_accept_without("path"));
    }

    #[test]
    fn at_params_require_repo_path_at() {
        assert!(at_params_accept_without(""));
        assert!(!at_params_accept_without("repo"));
        assert!(!at_params_accept_without("path"));
        assert!(!at_params_accept_without("at"));
    }
}
