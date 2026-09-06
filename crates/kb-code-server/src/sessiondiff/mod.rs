//! W3.5 — the SESSION DIFF: everything session S changed, presented as one
//! narrative review unit. [`session_diff`] is the entrypoint — `routes::
//! session_diff_route` (`GET /api/session-diff`) and `kb-code session-diff`
//! both call it directly, mirroring `join::ladder::resolve_commit`'s own
//! signature convention (explicit narrow deps, not `state: &SharedState` —
//! see that fn's doc): this keeps the whole assembly directly testable
//! against a fixture `Store` + fixture repo + mock kb daemon, with no need
//! to construct a full `AppState`.
//!
//! # The change-set, and why it's assembled this way
//!
//! A session's change-set is the UNION of two sources, presented in the
//! session's OWN narrative order (transcript order) — never algorithmically
//! re-sorted by file, author, or commit hash:
//!
//! - **Commits** — kb's own `session_commits` capture
//!   (`join::kb_client::KbClient::session_commits`, `GET /api/sessions/
//!   {session_id}/commits`), each optionally enriched with a REAL local
//!   diff (`git show --numstat`, [`git_diff`]) when the commit resolved
//!   (V0025's `sha_full`/`repo_root`) to one of THIS daemon's configured
//!   repos.
//! - **Uncommitted evidence** — this daemon's own local transcript index
//!   (`store::Store::transcript_turns_for_session`): every `tool_use` turn
//!   naming an edit/write tool ([`EDIT_TOOL_NAMES`]) with a non-empty
//!   `file_paths`, MINUS whatever a resolved commit already covers (see
//!   "Uncommitted evidence" below).
//!
//! # Assembly
//!
//! 1. Walk the session's transcript turns (oldest first) into ANCHORS:
//!    a top-level (non-sidechain) `user` turn is a `Prompt` anchor; every
//!    run of edit/write `tool_use` turns BETWEEN two prompts collapses into
//!    one `EditGroup` anchor. Anything else (assistant text, thinking,
//!    tool_result, a non-edit tool_use, any sidechain/subagent turn) is
//!    skipped — this is a v1 scope limit (subagent edits are indexed
//!    locally but excluded from this top-level narrative view), not an
//!    oversight.
//! 2. Each commit that resolved to a configured repo slots in at its
//!    AUTHOR-TIME position among those anchors (before the first anchor
//!    whose own ts is later) — "commits slot into the order at their
//!    author-time position among turns," per the design brief. Commits
//!    landing in the same slot merge into ONE `commits` segment. A commit
//!    that couldn't be diffed locally (no matching configured repo, or the
//!    sha didn't actually resolve there) carries no author-time to slot
//!    by — it's appended in one trailing `commits` segment at the very end,
//!    clearly out of narrative position rather than silently dropped (a
//!    documented deviation — see the module's own tests for the exact
//!    shape).
//! 3. Every `EditGroup` anchor becomes an `uncommitted` segment, filtered
//!    to just the files NOT covered by any diffed commit's file list
//!    (matched by repo-relative path — a transcript `tool_use` turn's
//!    `file_paths` are absolute, so each is mapped to its owning configured
//!    repo first). The filter is GLOBAL (any commit in the session, not
//!    just ones positioned after the group) — "was this edit ever
//!    committed within the session," not "committed later than this
//!    point." A file whose owning repo isn't configured at all (so it
//!    can't be cross-checked) is conservatively kept as uncommitted rather
//!    than silently dropped. A group that ends up with zero uncommitted
//!    files is dropped entirely — the diff already accounts for it via its
//!    `commits` segment.
//!
//! Hunk text is NOT reconstructed for uncommitted evidence in v1 — each
//! `uncommitted` segment's turns carry `src_file`/`byte_offset`/`byte_len`
//! (the SAME re-read coordinates `transcripts::search::read_turn_text`
//! uses), so a future SPA/CLI affordance can fetch surrounding context
//! on demand without this module ever growing an unbounded payload.
//! [`git_diff::commit_patch`] is the equivalent on-demand companion for a
//! COMMITTED file's actual patch text — also never called by
//! [`session_diff`] itself, for the same "don't inline an unbounded diff"
//! reason.
//!
//! # Degradation
//!
//! An unreachable/disabled kb daemon degrades ONLY the commits half —
//! [`CommitsStatus::Degraded`] on the response, `segments` still built from
//! the local transcript alone (prompts + uncommitted evidence, with NO
//! commits to subtract, so every edit group's files stay listed). An
//! session id this daemon's OWN transcript index has never seen (zero
//! turns) is [`SessionDiffError::UnknownSession`] — decided locally, never
//! from kb's own (empty-on-unknown, not-an-error) `session_commits` reply.

pub mod git_diff;

use crate::config::RepoEntry;
use crate::git::GitRepo;
use crate::join::kb_client::{KbClient, SessionCommitEntry};
use crate::store::{Store, StoreBlocking, StoreError};
use crate::transcripts::search::read_turn_text;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

/// session-diff/1 — bump this (and the golden test below) only alongside a
/// deliberate, documented shape change.
pub const SCHEMA: &str = "session-diff/1";

/// `tool_use` names treated as "this turn wrote to disk" — the uncommitted-
/// evidence filter's input set. Deliberately a small fixed list (mirrors
/// `transcripts::parse::FILE_PATH_KEYS`'s own "curated, not heuristic"
/// convention): `Read`/`Glob`/`Grep`/etc. also carry `file_path`-shaped
/// input but never MUTATE anything, so they're not evidence of an
/// uncommitted change.
pub const EDIT_TOOL_NAMES: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Prompt/display-name truncation length — "truncated ~200 chars" per the
/// design brief.
const PROMPT_TRUNCATE_CHARS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum SessionDiffError {
    /// This daemon's own transcript index has zero turns for the session —
    /// see the module doc's "Degradation" section for why this is decided
    /// locally rather than from kb's `session_commits` reply.
    #[error("unknown session: {0:?}")]
    UnknownSession(String),
    #[error("no such repo: {0:?}")]
    UnknownRepo(String),
    #[error("kb-code store error: {0}")]
    Store(#[from] StoreError),
}

pub type Result<T> = std::result::Result<T, SessionDiffError>;

/// Whether the commits half of the payload is real (`Ok`) or degraded (kb
/// daemon disabled/unreachable/erroring — `reason` is the underlying
/// `KbClientError`'s own message). Internally tagged (`{"status": "ok"}` /
/// `{"status": "degraded", "reason": "..."}`) — see the module doc.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommitsStatus {
    Ok,
    Degraded { reason: String },
}

/// One file's line-delta within a [`CommitEntryOut`] — mirrors
/// [`git_diff::FileStat`] field for field (a distinct wire type so
/// `git_diff` itself carries no `serde` dependency of its own).
#[derive(Debug, Serialize)]
pub struct CommitFileOut {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
    pub binary: bool,
}

/// One commit in the session's change-set. `diffed = false` means kb knows
/// about this commit but this daemon could not locally compute its file
/// stats (no configured repo matched `repo_root`, or the sha didn't
/// actually resolve there) — `files`/`author_time` are then absent, and the
/// entry appears in the TRAILING `commits` segment (see the module doc).
#[derive(Debug, Serialize)]
pub struct CommitEntryOut {
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trailers: Vec<String>,
    pub diffed: bool,
    /// Unix seconds — only present when `diffed` (this is what positioned
    /// the commit among the transcript's own turns; see the module doc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<CommitFileOut>,
    pub insertions: u32,
    pub deletions: u32,
}

/// One edit/write `tool_use` turn backing an `uncommitted` segment — carries
/// the SAME `(src_file, byte_offset, byte_len)` re-read coordinates
/// `transcripts::search::read_turn_text` uses, so a future consumer can
/// fetch surrounding hunk context on demand (see the module doc — hunk text
/// is NOT reconstructed here).
#[derive(Debug, Serialize)]
pub struct UncommittedTurnOut {
    pub ts: i64,
    pub tool_name: String,
    pub file_paths: Vec<String>,
    pub uuid: String,
    pub src_file: String,
    pub byte_offset: i64,
    pub byte_len: i64,
}

/// One narrative unit, in the session's OWN transcript order. Internally
/// tagged (`{"kind": "prompt"|"commits"|"uncommitted", ...}`) per the
/// design brief.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Segment {
    Prompt {
        ts: i64,
        uuid: String,
        text: String,
    },
    Commits {
        commits: Vec<CommitEntryOut>,
    },
    Uncommitted {
        /// Absolute paths still lacking an owning commit — see the module
        /// doc's step 3 for the exact filter.
        files: Vec<String>,
        turns: Vec<UncommittedTurnOut>,
    },
}

#[derive(Debug, Default, Serialize)]
pub struct Totals {
    /// Every `"commit"`-kind row kb reported for the session (diffed +
    /// unresolved).
    pub commits: usize,
    /// The subset actually diffed locally.
    pub commits_diffed: usize,
    /// Distinct files touched — diffed commits' numstat files, unioned with
    /// whatever `uncommitted` segments still list.
    pub files: usize,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Serialize)]
pub struct SessionDiff {
    pub version: &'static str,
    pub session_id: String,
    /// The session's own first top-level prompt, truncated — best-effort,
    /// `None` when the transcript had no prompt turn at all (every turn was
    /// e.g. a lone tool_result, which shouldn't happen in practice but
    /// isn't treated as an error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub segments: Vec<Segment>,
    pub repos_touched: Vec<String>,
    pub totals: Totals,
    pub commits_status: CommitsStatus,
}

/// The join ladder's own repo-alignment rule (`join::ladder::repo_aligned`),
/// duplicated here rather than imported: that fn is private to `join::
/// ladder` and takes a `join::kb_client::CommitMapRow`, a different type
/// than [`SessionCommitEntry`] — same one-line comparison either way
/// (canonical path equality, trailing slash tolerated).
fn repo_root_matches(repo: &RepoEntry, repo_root: &str) -> bool {
    repo.path.to_string_lossy().trim_end_matches('/') == repo_root.trim_end_matches('/')
}

/// `file_path` (absolute) → the configured repo that owns it (respecting
/// `filter_repo` when set) plus its repo-relative, forward-slash path —
/// `None` when no configured (in-scope) repo is a prefix of it.
fn owning_repo_rel<'a>(
    file_path: &str,
    repos: &'a [RepoEntry],
    filter_repo: Option<&RepoEntry>,
) -> Option<(&'a RepoEntry, String)> {
    repos
        .iter()
        .filter(|r| filter_repo.is_none_or(|f| f.name == r.name))
        .find_map(|r| {
            Path::new(file_path)
                .strip_prefix(&r.path)
                .ok()
                .map(|rel| (r, rel.to_string_lossy().replace('\\', "/")))
        })
}

/// `true` when `file_path` maps to a configured repo AND that repo+relpath
/// pair is in `covered` (a diffed commit's own file list) — see the module
/// doc's step 3. A path that maps to NO configured repo is conservatively
/// treated as NOT committed (kept as uncommitted evidence) — we simply have
/// no commit data to check it against.
fn is_committed(
    file_path: &str,
    repos: &[RepoEntry],
    filter_repo: Option<&RepoEntry>,
    covered: &HashSet<(String, String)>,
) -> bool {
    match owning_repo_rel(file_path, repos, filter_repo) {
        Some((repo, rel)) => covered.contains(&(repo.name.clone(), rel)),
        None => false,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// One successfully-diffed commit — the blocking git work's own output,
/// paired back up with its `commit_rows` index once `spawn_blocking`
/// returns (see [`session_diff`]).
struct DiffedCommit {
    row_idx: usize,
    repo_name: String,
    sha_full: String,
    /// Unix seconds.
    author_time_unix: i64,
    files: Vec<git_diff::FileStat>,
}

/// Resolve one commit's local diff: open `repo` fresh (gix, `!Send` — see
/// `crate::git`'s own module doc), read author-time via `commit_info`
/// (in-process ODB), then shell out for `--numstat`. BLOCKING — only ever
/// called from inside a `spawn_blocking` closure. `None` on ANY failure
/// (the sha doesn't actually resolve in this repo, a transient git error,
/// ...) — the caller falls back to treating the commit as unresolved rather
/// than failing the whole request.
fn resolve_one_commit(row_idx: usize, sha: &str, repo: &RepoEntry) -> Option<DiffedCommit> {
    let git = GitRepo::open(&repo.path).ok()?;
    let info = git.commit_info(sha).ok()?;
    let numstat = git_diff::commit_numstat(&repo.path, &info.sha).ok()?;
    Some(DiffedCommit {
        row_idx,
        repo_name: repo.name.clone(),
        sha_full: info.sha,
        author_time_unix: info.author_time_unix,
        files: numstat.files,
    })
}

fn commit_entry_diffed(row: &SessionCommitEntry, d: &DiffedCommit) -> CommitEntryOut {
    let insertions: u32 = d.files.iter().map(|f| f.insertions).sum();
    let deletions: u32 = d.files.iter().map(|f| f.deletions).sum();
    CommitEntryOut {
        sha: d.sha_full.clone(),
        repo: Some(d.repo_name.clone()),
        subject: row.subject.clone(),
        author: row.author.clone(),
        trailers: row.trailers.clone(),
        diffed: true,
        author_time: Some(d.author_time_unix),
        files: d
            .files
            .iter()
            .map(|f| CommitFileOut {
                path: f.path.clone(),
                insertions: f.insertions,
                deletions: f.deletions,
                binary: f.binary,
            })
            .collect(),
        insertions,
        deletions,
    }
}

fn commit_entry_unresolved(row: &SessionCommitEntry) -> CommitEntryOut {
    let sha = row
        .sha_full
        .clone()
        .or_else(|| row.sha.clone())
        .unwrap_or_default();
    CommitEntryOut {
        sha,
        repo: None,
        subject: row.subject.clone(),
        author: row.author.clone(),
        trailers: row.trailers.clone(),
        diffed: false,
        author_time: None,
        files: Vec::new(),
        insertions: 0,
        deletions: 0,
    }
}

/// One consecutive-between-prompts run of edit/write `tool_use` turns.
struct EditGroup {
    /// The group's first turn's `ts` (unix ms) — its position among the
    /// transcript's other anchors.
    ts: i64,
    turns: Vec<UncommittedTurnOut>,
}

enum Anchor {
    Prompt { ts: i64, uuid: String, text: String },
    Edits(EditGroup),
}

/// See the module doc for the full design. `repos`/`store`/
/// `transcripts_root`/`kb_client` are the narrow deps [`session_diff`]
/// needs, mirroring `join::ladder::resolve_commit`'s own signature
/// convention (never `state: &SharedState` — see that fn's doc) so this is
/// directly testable against a fixture `Store` + fixture repo + mock kb
/// daemon.
pub async fn session_diff(
    session_id: &str,
    repo_filter: Option<&str>,
    repos: &[RepoEntry],
    store: &Arc<Store>,
    transcripts_root: &Path,
    kb_client: &KbClient,
) -> Result<SessionDiff> {
    let filter_repo: Option<&RepoEntry> = match repo_filter {
        Some(name) => Some(
            repos
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| SessionDiffError::UnknownRepo(name.to_string()))?,
        ),
        None => None,
    };

    let session_id_c = session_id.to_string();
    let turns = store
        .run_blocking(move |store| store.transcript_turns_for_session(&session_id_c))
        .await?;
    if turns.is_empty() {
        return Err(SessionDiffError::UnknownSession(session_id.to_string()));
    }

    let (commit_rows, commits_status) = match kb_client.session_commits(session_id).await {
        Ok(rows) => (rows, CommitsStatus::Ok),
        Err(e) => (
            Vec::new(),
            CommitsStatus::Degraded {
                reason: e.to_string(),
            },
        ),
    };
    let commit_rows: Vec<SessionCommitEntry> = commit_rows
        .into_iter()
        .filter(|c| c.kind == "commit")
        .collect();

    // Split into "diffable locally" (resolved repo_root matches a
    // configured repo, and — when `repo_filter` is set — that repo) vs
    // "unresolved" (kb knows about it, we can't diff it).
    let mut to_resolve: Vec<(usize, String, RepoEntry)> = Vec::new();
    let mut unresolved_idx: Vec<usize> = Vec::new();
    for (i, c) in commit_rows.iter().enumerate() {
        let Some(sha) = c.sha_full.clone().or_else(|| c.sha.clone()) else {
            continue; // no sha at all — nothing to show or diff
        };
        let local_repo = c
            .repo_root
            .as_deref()
            .and_then(|root| repos.iter().find(|r| repo_root_matches(r, root)));
        match (local_repo, filter_repo) {
            (Some(r), Some(f)) if r.name == f.name => to_resolve.push((i, sha, r.clone())),
            (Some(_), Some(_)) => {} // resolved to a DIFFERENT repo than the filter — excluded
            (Some(r), None) => to_resolve.push((i, sha, r.clone())),
            (None, Some(_)) => {} // can't confirm membership in the filtered repo — excluded
            (None, None) => unresolved_idx.push(i),
        }
    }

    let resolved: Vec<(usize, Option<DiffedCommit>)> = if to_resolve.is_empty() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || {
            to_resolve
                .into_iter()
                .map(|(idx, sha, repo)| (idx, resolve_one_commit(idx, &sha, &repo)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default()
    };
    let mut diffed: Vec<DiffedCommit> = Vec::new();
    for (idx, r) in resolved {
        match r {
            Some(d) => diffed.push(d),
            None => unresolved_idx.push(idx),
        }
    }

    // Files any diffed commit already covers — (repo name, repo-relative
    // path) pairs — the uncommitted-evidence filter's input (module doc
    // step 3).
    let covered: HashSet<(String, String)> = diffed
        .iter()
        .flat_map(|d| {
            let repo_name = d.repo_name.clone();
            d.files
                .iter()
                .map(move |f| (repo_name.clone(), f.path.clone()))
        })
        .collect();

    // --- step 1: walk transcript turns into ordered anchors -------------
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut pending: Vec<UncommittedTurnOut> = Vec::new();
    for t in &turns {
        if t.is_sidechain {
            continue; // subagent-internal — out of scope for v1 (module doc)
        }
        if t.kind == "user" {
            if !pending.is_empty() {
                let ts = pending[0].ts;
                anchors.push(Anchor::Edits(EditGroup {
                    ts,
                    turns: std::mem::take(&mut pending),
                }));
            }
            let text = read_turn_text(
                transcripts_root,
                &t.src_file,
                &t.uuid,
                &t.kind,
                t.tool_name.as_deref(),
                t.byte_offset,
                t.byte_len,
            )
            .unwrap_or_default();
            anchors.push(Anchor::Prompt {
                ts: t.ts,
                uuid: t.uuid.clone(),
                text: truncate_chars(text.trim(), PROMPT_TRUNCATE_CHARS),
            });
        } else if t.kind == "tool_use" {
            if let Some(name) = &t.tool_name {
                if EDIT_TOOL_NAMES.contains(&name.as_str()) && !t.file_paths.is_empty() {
                    pending.push(UncommittedTurnOut {
                        ts: t.ts,
                        tool_name: name.clone(),
                        file_paths: t.file_paths.clone(),
                        uuid: t.uuid.clone(),
                        src_file: t.src_file.clone(),
                        byte_offset: t.byte_offset,
                        byte_len: t.byte_len,
                    });
                }
            }
        }
    }
    if !pending.is_empty() {
        let ts = pending[0].ts;
        anchors.push(Anchor::Edits(EditGroup { ts, turns: pending }));
    }

    // --- step 2: position diffed commits by author-time (ms), trailing
    // bucket for anything with no local resolution -----------------------
    let mut positioned: VecDeque<(i64, CommitEntryOut)> = diffed
        .iter()
        .map(|d| {
            (
                d.author_time_unix * 1000,
                commit_entry_diffed(&commit_rows[d.row_idx], d),
            )
        })
        .collect();
    positioned.make_contiguous().sort_by_key(|(ts, _)| *ts);
    let trailing: Vec<CommitEntryOut> = unresolved_idx
        .iter()
        .map(|&idx| commit_entry_unresolved(&commit_rows[idx]))
        .collect();

    // --- step 3: merge anchors + positioned commits into segments -------
    let mut segments: Vec<Segment> = Vec::new();
    let mut repos_touched: HashSet<String> = diffed.iter().map(|d| d.repo_name.clone()).collect();
    let mut uncommitted_files: HashSet<(String, String)> = HashSet::new();

    let mut commit_buf: Vec<CommitEntryOut> = Vec::new();
    let flush_commits = |segments: &mut Vec<Segment>, buf: &mut Vec<CommitEntryOut>| {
        if !buf.is_empty() {
            segments.push(Segment::Commits {
                commits: std::mem::take(buf),
            });
        }
    };

    for anchor in anchors {
        let anchor_ts = match &anchor {
            Anchor::Prompt { ts, .. } => *ts,
            Anchor::Edits(g) => g.ts,
        };
        while let Some((ts, _)) = positioned.front() {
            if *ts > anchor_ts {
                break;
            }
            let (_, c) = positioned.pop_front().unwrap();
            commit_buf.push(c);
        }
        flush_commits(&mut segments, &mut commit_buf);

        match anchor {
            Anchor::Prompt { ts, uuid, text } => segments.push(Segment::Prompt { ts, uuid, text }),
            Anchor::Edits(group) => {
                let mut files: Vec<String> = Vec::new();
                for turn in &group.turns {
                    for fp in &turn.file_paths {
                        if files.contains(fp) {
                            continue;
                        }
                        if is_committed(fp, repos, filter_repo, &covered) {
                            continue;
                        }
                        files.push(fp.clone());
                        if let Some((repo, rel)) = owning_repo_rel(fp, repos, filter_repo) {
                            repos_touched.insert(repo.name.clone());
                            uncommitted_files.insert((repo.name.clone(), rel));
                        } else {
                            uncommitted_files.insert((String::new(), fp.clone()));
                        }
                    }
                }
                if !files.is_empty() {
                    segments.push(Segment::Uncommitted {
                        files,
                        turns: group.turns,
                    });
                }
            }
        }
    }
    while let Some((_, c)) = positioned.pop_front() {
        commit_buf.push(c);
    }
    flush_commits(&mut segments, &mut commit_buf);
    if !trailing.is_empty() {
        segments.push(Segment::Commits { commits: trailing });
    }

    let insertions: u32 = diffed
        .iter()
        .flat_map(|d| d.files.iter())
        .map(|f| f.insertions)
        .sum();
    let deletions: u32 = diffed
        .iter()
        .flat_map(|d| d.files.iter())
        .map(|f| f.deletions)
        .sum();
    let mut distinct_files = covered;
    distinct_files.extend(uncommitted_files);

    let totals = Totals {
        commits: diffed.len() + unresolved_idx.len(),
        commits_diffed: diffed.len(),
        files: distinct_files.len(),
        insertions,
        deletions,
    };

    let display_name = turns
        .iter()
        .find(|t| t.kind == "user" && !t.is_sidechain)
        .and_then(|t| {
            read_turn_text(
                transcripts_root,
                &t.src_file,
                &t.uuid,
                &t.kind,
                t.tool_name.as_deref(),
                t.byte_offset,
                t.byte_len,
            )
        })
        .map(|s| truncate_chars(s.trim(), PROMPT_TRUNCATE_CHARS));

    let mut repos_touched: Vec<String> = repos_touched.into_iter().collect();
    repos_touched.sort();

    Ok(SessionDiff {
        version: SCHEMA,
        session_id: session_id.to_string(),
        display_name,
        segments,
        repos_touched,
        totals,
        commits_status,
    })
}

#[cfg(test)]
#[path = "session_diff_tests.rs"]
mod tests;
