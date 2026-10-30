//! `GET /api/changes` — "since you left": what landed ACROSS PROJECTS
//! (sessions that ran and how they closed, the commits they recorded, what
//! was posted on each project's slate) since a time. (Provenance: OL IDEA5.)
//!
//! ## What it is, and what it deliberately is not
//!
//! The CORPUS axis (artifacts, memory notes, comments raised) already ships
//! as `GET /api/kb/{kb}/daycard?since=&all=1` and the `/ambient` page; this
//! route does not rebuild it. It is the PROJECT axis, composed from reads
//! that already exist:
//!
//! | lane | source | invariant it must not bend |
//! |------|--------|----------------------------|
//! | sessions | `Db::sessions_active_since` (activity, not start) | #11 newest capture, logical counts, pointers only |
//! | commits | `session_commits_for_sessions` (one batch per corpus) | #11 newest capture |
//! | slate | the append-only ledger read BY `at`, hidden map of `slate::project` | #6 read-only, never written by a read |
//!
//! Fan-out is `buffered_join` in `state.kbs` order and a failing corpus
//! degrades its own lane (`degraded`), never the response (#28). Nothing is
//! scored: groups are ordered by time (#10). Nothing is persisted, no new
//! SSE kind exists (#23/#24), kb never calls kb-code (#2).
//!
//! ## Honesty rules
//!
//! * The window is wall-clock (`to_unix` = now) and not reproducible across
//!   requests. These lanes are "activity since", never "events since".
//! * kb stores no commit time. Commits are listed under the session that
//!   recorded them, so a session with `started_before_since` may list
//!   commits made before the watermark.
//! * The wire NEVER carries `cwd`, `repo_root`, transcript text, or a slate
//!   post's `body`/`refs`/`prov` — only the closing line (<= 160 chars), the
//!   commit subjects (<= 120 chars), slate lines and the author tag.
//!
//! ## Posture
//!
//! Plain `/api` tree + `auth_bearer` + loopback bypass, exactly like
//! `/context`: every field is already bearer-visible through
//! `/api/sessions*` and `/api/slates*`, and this route narrows rather than
//! widens. The slate read always uses the strictest scrub floor.

use crate::middleware::error_to_problem_json;
use crate::routes::context::{classify_query_error, degraded_of, DegradedLane, QueryErrorClass};
use crate::routes::sessions::{display_name_of, project_identity, ProjectIdentity};
use crate::routes::slates::{facts_opts, list_slugs, load_for_read, presence_slice};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::paths::KbPaths;
use kb_core::sessions::live::LivePolicy;
use kb_core::sessions::ResolvedProject;
use kb_core::slate::{self, HideReason, Kind, Post, Presence, SlateSlug};
use kb_core::storage::sqlite::{SessionCommitRow, SessionRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Newest sessions kept per corpus (one extra row is fetched to detect the cap).
pub const SESSIONS_SCAN_CAP: usize = 500;
/// Project groups on the wire.
pub const MAX_PROJECTS: usize = 20;
/// Sessions listed per group.
pub const MAX_SESSIONS_PER_PROJECT: usize = 5;
/// Commits listed per session.
pub const MAX_COMMITS_PER_SESSION: usize = 5;
/// Slate posts listed per group.
pub const MAX_SLATE_POSTS: usize = 8;
/// Oldest honoured `since`, in seconds before now (31 days).
pub const MAX_WINDOW_SECS: i64 = 31 * 86_400;
/// Client clock skew tolerated before a future `since` is a 400.
pub const SKEW_SECS: i64 = 300;
const CLOSING_MAX_CHARS: usize = 160;
const SUBJECT_MAX_CHARS: usize = 120;
const SLATE_LINE_MAX_CHARS: usize = 200;
const NAME_MAX_CHARS: usize = 120;
const UNATTRIBUTED_LABEL: &str = "(unattributed)";

// ---------------------------------------------------------------------------
// Wire types (never carry paths, transcript text, or slate bodies)
// ---------------------------------------------------------------------------

// One commit a session recorded.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesCommit {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subject: Option<String>,
}

// One session with activity inside the window.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesSession {
    pub kb: String,
    pub session_id: String,
    pub display_name: String,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub substance: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    pub started_before_since: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub closing: Option<String>,
    pub commits: Vec<ChangesCommit>,
    pub commits_total: u64,
}

// One slate post that landed inside the window.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesSlatePost {
    pub seq: u64,
    pub kind: String,
    pub line: String,
    pub at: i64,
    pub who: String,
    pub closed: bool,
}

// A project's slate activity inside the window.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesSlate {
    pub slug: String,
    pub head_seq: u64,
    pub closed: bool,
    pub posts: Vec<ChangesSlatePost>,
    pub posts_total: u64,
    pub posts_truncated: bool,
    pub other_total: u64,
    pub retracted_total: u64,
    pub archived_in_window: bool,
}

// One project group.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesProject {
    pub project: String,
    pub label: String,
    pub source: String,
    pub latest_unix: i64,
    pub sessions: Vec<ChangesSession>,
    pub sessions_total: u64,
    pub sessions_truncated: bool,
    pub commit_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub slate: Option<ChangesSlate>,
}

// The whole response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesResponse {
    pub since_unix: i64,
    pub to_unix: i64,
    pub window_clamped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project: Option<String>,
    pub projects: Vec<ChangesProject>,
    pub projects_total: u64,
    pub projects_truncated: bool,
    pub sessions_scan_capped: bool,
    pub trivial_sessions_hidden: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub slates_skipped: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub degraded: Option<Vec<DegradedLane>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChangesParams {
    /// Required unix seconds.
    pub since: Option<String>,
    /// Keep groups whose `project` or `label` equals this exactly.
    pub project: Option<String>,
    /// The caller's own session id (it and its segments are excluded).
    pub session: Option<String>,
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub since: i64,
    pub clamped: bool,
}

/// Validate and normalise `since`. `Err` is the 400 detail.
pub(crate) fn resolve_window(raw: Option<&str>, now: i64) -> Result<Window, String> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "since is required (unix seconds; `kb changes --since 2h` converts for you)".to_string()
        })?;
    let mut since: i64 = raw
        .parse()
        .map_err(|_| format!("since must be unix seconds, got {raw:?}"))?;
    if since > now + SKEW_SECS {
        return Err(format!(
            "since {since} is in the future (server clock {now}, tolerance {SKEW_SECS}s)"
        ));
    }
    if since > now {
        // Client clock skew: nothing can have happened after "now".
        since = now;
    }
    let floor = now - MAX_WINDOW_SECS;
    if since < floor {
        return Ok(Window {
            since: floor,
            clamped: true,
        });
    }
    Ok(Window {
        since,
        clamped: false,
    })
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    kb_core::sessions::truncate_chars_ellipsis(&flat, max)
}

// ---------------------------------------------------------------------------
// Sessions lane (pure fold over per-corpus results)
// ---------------------------------------------------------------------------

/// One corpus's raw reads, errors already classified (no text, no paths).
pub(crate) struct CorpusSessions {
    pub kb: String,
    pub rows: Result<Vec<SessionRow>, QueryErrorClass>,
    pub commits: Result<HashMap<String, Vec<SessionCommitRow>>, QueryErrorClass>,
}

/// A group under construction.
pub(crate) struct Group {
    project: String,
    label: String,
    source: &'static str,
    latest_unix: i64,
    /// `(activity, session)` — sorted at finalize.
    sessions: Vec<(i64, ChangesSession)>,
    logical: HashSet<(String, String)>,
    commit_count: u64,
    slate: Option<ChangesSlate>,
}

impl Group {
    fn new(project: String, label: String, source: &'static str) -> Self {
        Group {
            project,
            label,
            source,
            latest_unix: 0,
            sessions: Vec::new(),
            logical: HashSet::new(),
            commit_count: 0,
            slate: None,
        }
    }
}

pub(crate) struct SessionsFold {
    pub groups: BTreeMap<String, Group>,
    pub trivial_hidden: u64,
    pub scan_capped: bool,
    pub degraded: Vec<DegradedLane>,
}

type Resolver<'a> = &'a dyn Fn(Option<&str>, Option<&str>) -> Option<ResolvedProject>;

fn min_opt(a: Option<String>, b: Option<&str>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y < x.as_str() { y.to_string() } else { x }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y.to_string()),
        (None, None) => None,
    }
}

/// Fold per-corpus session reads into project groups. Pure: no I/O, no
/// clock, an injected registry resolver.
pub(crate) fn fold_sessions(
    inputs: Vec<CorpusSessions>,
    since: i64,
    caller: Option<&str>,
    resolver: Resolver<'_>,
) -> SessionsFold {
    struct Item {
        key: String,
        activity: i64,
        logical: (String, String),
        commit_rows: u64,
        session: ChangesSession,
    }
    let mut degraded: Vec<DegradedLane> = Vec::new();
    let mut scan_capped = false;
    let mut items: Vec<Item> = Vec::new();
    // Per key: (min repo_root, min cwd) over EVERY in-window row, trivial
    // included — the inputs the projects facet's SQL MIN() sees.
    let mut key_roots: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut trivial_logical: HashSet<(String, String)> = HashSet::new();

    for input in inputs {
        let CorpusSessions { kb, rows, commits } = input;
        let mut rows = match rows {
            Ok(r) => r,
            Err(class) => {
                degraded.push(degraded_of(&kb, "sessions", class));
                continue;
            }
        };
        if rows.len() > SESSIONS_SCAN_CAP {
            scan_capped = true;
            rows.truncate(SESSIONS_SCAN_CAP);
        }
        let mut commit_map = match commits {
            Ok(m) => m,
            Err(class) => {
                degraded.push(degraded_of(&kb, "commits", class));
                HashMap::new()
            }
        };
        for row in rows {
            if let Some(c) = caller {
                if row.session_id == c || row.segment_of.as_deref() == Some(c) {
                    continue;
                }
            }
            let key = row
                .project_key
                .clone()
                .or_else(|| row.cwd.clone().filter(|c| !c.is_empty()))
                .unwrap_or_default();
            let roots = key_roots.entry(key.clone()).or_default();
            roots.0 = min_opt(roots.0.take(), row.repo_root.as_deref());
            roots.1 = min_opt(roots.1.take(), row.cwd.as_deref());
            let logical = (
                kb.clone(),
                row.segment_of
                    .clone()
                    .unwrap_or_else(|| row.session_id.clone()),
            );
            if row.substance.as_deref() == Some("trivial") {
                trivial_logical.insert(logical);
                continue;
            }
            let commit_rows = commit_map.remove(&row.session_id).unwrap_or_default();
            let commit_n = commit_rows.iter().filter(|c| c.kind == "commit").count() as u64;
            let commits_total = commit_rows.len() as u64;
            // Rows arrive seq ASC; the wire is newest first.
            let listed: Vec<ChangesCommit> = commit_rows
                .iter()
                .rev()
                .take(MAX_COMMITS_PER_SESSION)
                .map(|c| ChangesCommit {
                    kind: c.kind.clone(),
                    sha: c.sha.clone(),
                    subject: c
                        .subject
                        .as_deref()
                        .map(|s| one_line(s, SUBJECT_MAX_CHARS))
                        .filter(|s| !s.is_empty()),
                })
                .collect();
            let activity = row.ended_at.max(row.started_at);
            let session = ChangesSession {
                kb: kb.clone(),
                display_name: one_line(
                    &display_name_of(
                        row.title.as_deref(),
                        row.first_user_prompt.as_deref(),
                        &row.session_id,
                    ),
                    NAME_MAX_CHARS,
                ),
                session_id: row.session_id.clone(),
                harness: row.harness.clone(),
                substance: row.substance.clone(),
                started_at: row.started_at,
                ended_at: row.ended_at,
                started_before_since: row.started_at <= since,
                closing: row
                    .last_assistant_text
                    .as_deref()
                    .map(|t| one_line(t, CLOSING_MAX_CHARS))
                    .filter(|t| !t.is_empty()),
                commits: listed,
                commits_total,
            };
            items.push(Item {
                key,
                activity,
                logical,
                commit_rows: commit_n,
                session,
            });
        }
    }

    // Resolve each KEY once, from the min repo_root / min cwd the facet sees.
    let mut key_ident: HashMap<String, ProjectIdentity> = HashMap::new();
    for (key, (repo_root, cwd)) in &key_roots {
        let ident = if key.is_empty() {
            ProjectIdentity {
                id: String::new(),
                label: UNATTRIBUTED_LABEL.to_string(),
                source: "derived",
            }
        } else {
            let resolved = resolver(repo_root.as_deref(), cwd.as_deref());
            project_identity(key, repo_root.as_deref(), cwd.as_deref(), resolved.as_ref())
        };
        key_ident.insert(key.clone(), ident);
    }

    // Groups merge by RESOLVED id (two keys under one registry entry merge).
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut eligible: HashSet<(String, String)> = HashSet::new();
    for it in items {
        let ident = &key_ident[&it.key];
        let g = groups
            .entry(ident.id.clone())
            .or_insert_with(|| Group::new(ident.id.clone(), ident.label.clone(), ident.source));
        g.latest_unix = g.latest_unix.max(it.activity);
        g.commit_count += it.commit_rows;
        g.logical.insert(it.logical.clone());
        eligible.insert(it.logical);
        g.sessions.push((it.activity, it.session));
    }
    let trivial_hidden = trivial_logical.difference(&eligible).count() as u64;
    SessionsFold {
        groups,
        trivial_hidden,
        scan_capped,
        degraded,
    }
}

// ---------------------------------------------------------------------------
// Slate lane
// ---------------------------------------------------------------------------

pub(crate) struct SlateLane {
    pub slate: ChangesSlate,
    pub latest_at: i64,
}

/// Build one slate's window entry from its (already scrub-floored) ledger.
/// `archive_mtime` is the mtime of generation-1's archive when this slate
/// has rotated. `None` when nothing landed and nothing rotated in-window.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_slate(
    slug: &SlateSlug,
    head_seq: u64,
    closed: bool,
    posts: &[Post],
    since: i64,
    now: i64,
    presence: &[Presence],
    archive_mtime: Option<i64>,
) -> Option<SlateLane> {
    let archived_in_window = archive_mtime.map(|m| m > since).unwrap_or(false);
    let mut fresh: Vec<&Post> = posts.iter().filter(|p| p.at > since).collect();
    if fresh.is_empty() && !archived_in_window {
        return None;
    }
    fresh.sort_by_key(|p| std::cmp::Reverse(p.seq));
    let digest = slate::project(
        posts,
        now,
        &LivePolicy::default(),
        presence,
        &facts_opts(slug),
    );
    let mut listed: Vec<ChangesSlatePost> = Vec::new();
    let mut other_total = 0u64;
    let mut retracted_total = 0u64;
    for p in &fresh {
        match p.kind {
            Kind::Drop | Kind::Mark | Kind::Take | Kind::Idea | Kind::Tried => other_total += 1,
            _ => {
                let hide = digest.hidden.get(&p.seq).map(|h| h.reason);
                if matches!(hide, Some(HideReason::Dropped | HideReason::Superseded)) {
                    retracted_total += 1;
                } else {
                    listed.push(ChangesSlatePost {
                        seq: p.seq,
                        kind: p.kind.as_str().to_string(),
                        line: one_line(&p.line, SLATE_LINE_MAX_CHARS),
                        at: p.at,
                        who: slate::author_tag(p),
                        closed: hide == Some(HideReason::Done),
                    });
                }
            }
        }
    }
    let posts_total = listed.len() as u64;
    let posts_truncated = listed.len() > MAX_SLATE_POSTS;
    listed.truncate(MAX_SLATE_POSTS);
    let fresh_max = fresh.iter().map(|p| p.at).max().unwrap_or(0);
    let latest_at = if archived_in_window {
        fresh_max.max(archive_mtime.unwrap_or(0))
    } else {
        fresh_max
    };
    Some(SlateLane {
        slate: ChangesSlate {
            slug: slug.to_string(),
            head_seq,
            closed,
            posts: listed,
            posts_total,
            posts_truncated,
            other_total,
            retracted_total,
            archived_in_window,
        },
        latest_at,
    })
}

fn file_mtime_secs(path: &std::path::Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// One blocking walk of every slate. A slate whose meta or ledger is
/// unreadable is named in the second tuple member, never a 500.
pub(crate) fn slate_lane(
    paths: &KbPaths,
    since: i64,
    now: i64,
    presence: &[Presence],
) -> (Vec<SlateLane>, Vec<String>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for slug in list_slugs(paths) {
        // Cheap pre-check: an append bumps the ledger mtime and a rotate or
        // close rewrites the meta, so both older than `since` means no post
        // can have `at > since` and no rotation landed in the window. Skip
        // before reading and scrubbing the whole ledger. Any stat failure
        // falls through to the full load.
        let m_ledger = file_mtime_secs(&paths.slate_ledger_file(&slug));
        let m_meta = file_mtime_secs(&paths.slate_meta_file(&slug));
        if matches!((m_ledger, m_meta), (Some(a), Some(b)) if a < since && b < since) {
            continue;
        }
        // `loopback = false`: the strictest scrub floor, always.
        let loaded = match load_for_read(paths, &slug, false) {
            Ok(Some(l)) => l,
            Ok(None) => continue,
            Err(_) => {
                skipped.push(slug.to_string());
                continue;
            }
        };
        // Rotate renames the ledger, so the archive's mtime is that
        // generation's last append. Archived posts are named, not read.
        let archive_mtime = if loaded.meta.generation > 1 {
            std::fs::metadata(paths.slate_archive_file(&slug, loaded.meta.generation - 1))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        } else {
            None
        };
        if let Some(lane) = build_slate(
            &slug,
            loaded.meta.head_seq,
            loaded.meta.closed(),
            &loaded.posts,
            since,
            now,
            presence,
            archive_mtime,
        ) {
            out.push(lane);
        }
    }
    (out, skipped)
}

/// Join slate entries to groups: exact `project` id, else exact `label`
/// (lowest project id wins), else the slate is its own `source: "slate"` group.
pub(crate) fn join_slates(groups: &mut BTreeMap<String, Group>, slates: Vec<SlateLane>) {
    for lane in slates {
        let slug = lane.slate.slug.clone();
        let target = if groups.contains_key(&slug) {
            slug.clone()
        } else {
            // BTreeMap iterates ids ascending, so the first label hit is the
            // lowest project id.
            groups
                .iter()
                .find(|(_, g)| g.label == slug)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| {
                    groups.insert(
                        slug.clone(),
                        Group::new(slug.clone(), slug.clone(), "slate"),
                    );
                    slug.clone()
                })
        };
        let g = groups.get_mut(&target).expect("target group exists");
        g.latest_unix = g.latest_unix.max(lane.latest_at);
        g.slate = Some(lane.slate);
    }
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

/// Turn groups into the capped, ordered wire list.
/// Returns `(projects, projects_total, truncated)`.
pub(crate) fn finalize_groups(
    groups: BTreeMap<String, Group>,
    project_filter: Option<&str>,
    cap: usize,
) -> (Vec<ChangesProject>, u64, bool) {
    let mut out: Vec<ChangesProject> = groups
        .into_values()
        .filter(|g| match project_filter {
            Some(f) => g.project == f || g.label == f,
            None => true,
        })
        .filter(|g| !g.sessions.is_empty() || g.slate.is_some())
        .map(|mut g| {
            g.sessions.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.session_id.cmp(&b.1.session_id))
                    .then_with(|| a.1.kb.cmp(&b.1.kb))
            });
            let sessions_total = g.logical.len() as u64;
            let sessions_truncated = g.sessions.len() > MAX_SESSIONS_PER_PROJECT;
            let sessions: Vec<ChangesSession> = g
                .sessions
                .into_iter()
                .take(MAX_SESSIONS_PER_PROJECT)
                .map(|(_, s)| s)
                .collect();
            ChangesProject {
                project: g.project,
                label: g.label,
                source: g.source.to_string(),
                latest_unix: g.latest_unix,
                sessions,
                sessions_total,
                sessions_truncated,
                commit_count: g.commit_count,
                slate: g.slate,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.latest_unix
            .cmp(&a.latest_unix)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.project.cmp(&b.project))
    });
    let total = out.len() as u64;
    let truncated = out.len() > cap;
    out.truncate(cap);
    (out, total, truncated)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compose(
    window: Window,
    now: i64,
    project_filter: Option<&str>,
    mut fold: SessionsFold,
    slates: Vec<SlateLane>,
    slates_skipped: Vec<String>,
) -> ChangesResponse {
    join_slates(&mut fold.groups, slates);
    let (projects, projects_total, projects_truncated) =
        finalize_groups(fold.groups, project_filter, MAX_PROJECTS);
    ChangesResponse {
        since_unix: window.since,
        to_unix: now,
        window_clamped: window.clamped,
        project: project_filter.map(str::to_string),
        projects,
        projects_total,
        projects_truncated,
        sessions_scan_capped: fold.scan_capped,
        trivial_sessions_hidden: fold.trivial_hidden,
        slates_skipped: (!slates_skipped.is_empty()).then_some(slates_skipped),
        degraded: (!fold.degraded.is_empty()).then_some(fold.degraded),
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `GET /api/changes?since=<unix>&project=&session=`
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<ChangesParams>,
) -> Response<Body> {
    let now = chrono::Utc::now().timestamp();
    let window = match resolve_window(params.since.as_deref(), now) {
        Ok(w) => w,
        Err(detail) => return error_to_problem_json(&kb_core::Error::BadRequest(detail)),
    };
    let project_filter = params
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let caller = params
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let since = window.since;

    // Sessions + commits: one fan-out, submission order (#28).
    let mut futs: Vec<super::CorpusFut<'_, CorpusSessions>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let rows = ctx
                .storage
                .sessions_active_since(since, (SESSIONS_SCAN_CAP + 1) as u32)
                .await
                .map_err(|e| {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_active_since failed in changes");
                    classify_query_error(&e.to_string())
                });
            let commits = match &rows {
                Ok(r) if !r.is_empty() => {
                    let mut ids: Vec<String> = r.iter().map(|s| s.session_id.clone()).collect();
                    ids.sort();
                    ids.dedup();
                    ctx.storage
                        .session_commits_for_sessions(ids)
                        .await
                        .map_err(|e| {
                            tracing::warn!(kb = %kb_name, error = %e, "session_commits_for_sessions failed in changes");
                            classify_query_error(&e.to_string())
                        })
                }
                _ => Ok(HashMap::new()),
            };
            CorpusSessions {
                kb: kb_name.to_string(),
                rows,
                commits,
            }
        }));
    }
    let inputs = super::buffered_join(futs, state.fanout_cap).await;
    let fold = fold_sessions(
        inputs,
        since,
        caller,
        &kb_core::sessions::resolve_registry_project,
    );

    // Slate lane: one blocking walk.
    let presence = presence_slice(&state, now);
    let paths = state.paths.clone();
    let (slates, skipped) =
        tokio::task::spawn_blocking(move || slate_lane(&paths, since, now, &presence))
            .await
            .unwrap_or_default();

    let body = compose(window, now, project_filter, fold, slates, skipped);
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_registry(_: Option<&str>, _: Option<&str>) -> Option<ResolvedProject> {
        None
    }

    fn row(sid: &str, started: i64, ended: i64) -> SessionRow {
        SessionRow {
            artifact_id: format!("art-{sid}"),
            session_id: sid.into(),
            started_at: started,
            ended_at: ended,
            harness: "claude".into(),
            project_key: Some("-srv-abs-kb".into()),
            repo_root: Some("/srv/abs/kb".into()),
            cwd: Some("/srv/abs/kb/sub".into()),
            ..Default::default()
        }
    }

    fn commit(sid: &str, seq: i64, kind: &str, sha: &str, subject: &str) -> SessionCommitRow {
        SessionCommitRow {
            artifact_id_session: format!("art-{sid}"),
            session_id: sid.into(),
            seq,
            kind: kind.into(),
            sha: Some(sha.into()),
            subject: Some(subject.into()),
            ..Default::default()
        }
    }

    fn corpus(kb: &str, rows: Vec<SessionRow>) -> CorpusSessions {
        CorpusSessions {
            kb: kb.into(),
            rows: Ok(rows),
            commits: Ok(HashMap::new()),
        }
    }

    fn fold(inputs: Vec<CorpusSessions>, since: i64, caller: Option<&str>) -> SessionsFold {
        fold_sessions(inputs, since, caller, &no_registry)
    }

    fn only_group(f: SessionsFold) -> ChangesProject {
        let (mut v, total, _) = finalize_groups(f.groups, None, MAX_PROJECTS);
        assert_eq!(total, 1, "expected exactly one group");
        v.remove(0)
    }

    fn post(seq: u64, at: i64, kind: &str, line: &str, extra: serde_json::Value) -> Post {
        let mut v = json!({
            "seq": seq,
            "id": format!("e_{seq:012x}"),
            "at": at,
            "kind": kind,
            "line": line,
            "prov": {"harness": "claude", "session_id": "abcd1234"},
        });
        if let Some(o) = extra.as_object() {
            for (k, val) in o {
                v[k] = val.clone();
            }
        }
        serde_json::from_value(v).expect("post fixture parses")
    }

    #[test]
    fn window_clamps_to_31_days_and_flags() {
        let now = 10_000_000;
        let w = resolve_window(Some("9999000"), now).unwrap();
        assert_eq!(
            w,
            Window {
                since: 9_999_000,
                clamped: false
            }
        );
        let old = resolve_window(Some("1"), now).unwrap();
        assert_eq!(old.since, now - MAX_WINDOW_SECS);
        assert!(old.clamped);
        // Exactly at the floor is not clamped.
        let at_floor = resolve_window(Some(&(now - MAX_WINDOW_SECS).to_string()), now).unwrap();
        assert!(!at_floor.clamped);
        // Small skew clamps to now; large skew and junk are refused.
        let skew = resolve_window(Some(&(now + 100).to_string()), now).unwrap();
        assert_eq!(skew.since, now);
        assert!(resolve_window(Some(&(now + SKEW_SECS + 1).to_string()), now).is_err());
        assert!(resolve_window(Some("yesterday"), now).is_err());
        assert!(resolve_window(None, now).is_err());
        assert!(resolve_window(Some("  "), now).is_err());
    }

    #[test]
    fn project_identity_derived_basename_ladder() {
        let id = project_identity("-k", Some("/a/b/repo/"), Some("/c/d"), None);
        assert_eq!(
            (id.id.as_str(), id.label.as_str(), id.source),
            ("-k", "repo", "derived")
        );
        let id = project_identity("-k", None, Some("/c/d"), None);
        assert_eq!(id.label, "d");
        let id = project_identity("plain", None, None, None);
        assert_eq!(id.label, "plain");
    }

    fn registry_hit(_: Option<&str>, cwd: Option<&str>) -> Option<ResolvedProject> {
        cwd.filter(|c| c.starts_with("/srv/abs"))?;
        Some(ResolvedProject {
            id: "kb".into(),
            label: "Knowledge Base".into(),
            source: "registry",
            kb: None,
            code_url: None,
            code_repo: None,
        })
    }

    #[test]
    fn project_identity_registry_wins() {
        let rp = registry_hit(None, Some("/srv/abs/kb")).unwrap();
        let id = project_identity("-srv-abs-kb", Some("/srv/abs/kb"), None, Some(&rp));
        assert_eq!(
            (id.id.as_str(), id.label.as_str(), id.source),
            ("kb", "Knowledge Base", "registry")
        );
    }

    #[test]
    fn two_keys_resolving_to_one_registry_id_merge() {
        let mut a = row("s1", 100, 200);
        a.project_key = Some("-key-a".into());
        a.cwd = Some("/srv/abs/a".into());
        let mut b = row("s2", 100, 300);
        b.project_key = Some("-key-b".into());
        b.cwd = Some("/srv/abs/b".into());
        let f = fold_sessions(vec![corpus("k", vec![a, b])], 50, None, &registry_hit);
        let g = only_group(f);
        assert_eq!(g.project, "kb");
        assert_eq!(g.label, "Knowledge Base");
        assert_eq!(g.source, "registry");
        assert_eq!(g.sessions_total, 2);
    }

    #[test]
    fn same_key_in_two_corpora_is_one_group() {
        let f = fold(
            vec![
                corpus("one", vec![row("s1", 100, 200)]),
                corpus("two", vec![row("s2", 100, 300)]),
            ],
            50,
            None,
        );
        let g = only_group(f);
        assert_eq!(g.project, "-srv-abs-kb");
        assert_eq!(g.sessions_total, 2);
        // Activity desc: s2 (300) before s1 (200).
        assert_eq!(g.sessions[0].session_id, "s2");
    }

    #[test]
    fn unattributed_rows_go_to_one_group() {
        let mut r = row("s1", 100, 200);
        r.project_key = None;
        r.cwd = None;
        r.repo_root = None;
        let g = only_group(fold(vec![corpus("k", vec![r])], 50, None));
        assert_eq!(g.project, "");
        assert_eq!(g.label, "(unattributed)");
    }

    #[test]
    fn straddler_listed_with_started_before_since() {
        let g = only_group(fold(
            vec![corpus("k", vec![row("s1", 100, 900)])],
            500,
            None,
        ));
        assert_eq!(g.sessions.len(), 1);
        assert!(g.sessions[0].started_before_since);
        let g = only_group(fold(
            vec![corpus("k", vec![row("s2", 600, 900)])],
            500,
            None,
        ));
        assert!(!g.sessions[0].started_before_since);
    }

    #[test]
    fn caller_session_and_its_segments_excluded() {
        let mut part = row("me-p02", 100, 200);
        part.segment_of = Some("me".into());
        let f = fold(
            vec![corpus(
                "k",
                vec![row("me", 100, 200), part, row("other", 100, 150)],
            )],
            50,
            Some("me"),
        );
        let g = only_group(f);
        let ids: Vec<&str> = g.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["other"]);
    }

    #[test]
    fn trivial_counted_not_listed_none_substance_listed() {
        let mut t = row("trivial-1", 100, 200);
        t.substance = Some("trivial".into());
        let n = row("none-1", 100, 210); // substance None
        let mut s = row("subst-1", 100, 220);
        s.substance = Some("substantive".into());
        let f = fold(vec![corpus("k", vec![t, n, s])], 50, None);
        assert_eq!(f.trivial_hidden, 1);
        let g = only_group(f);
        let ids: Vec<&str> = g.sessions.iter().map(|x| x.session_id.as_str()).collect();
        assert_eq!(ids, ["subst-1", "none-1"]);
        assert_eq!(g.sessions_total, 2);
    }

    #[test]
    fn segment_parts_listed_but_counted_once() {
        let p1 = row("chain", 100, 200);
        let mut p2 = row("chain-p02", 200, 300);
        p2.segment_of = Some("chain".into());
        let mut t1 = row("tchain", 100, 150);
        t1.substance = Some("trivial".into());
        let mut t2 = row("tchain-p02", 150, 160);
        t2.segment_of = Some("tchain".into());
        t2.substance = Some("trivial".into());
        let f = fold(vec![corpus("k", vec![p1, p2, t1, t2])], 50, None);
        assert_eq!(
            f.trivial_hidden, 1,
            "two trivial parts count as one logical session"
        );
        let g = only_group(f);
        assert_eq!(g.sessions.len(), 2, "both parts are listed");
        assert_eq!(g.sessions_total, 1, "but counted once");
    }

    #[test]
    fn commits_newest_first_capped_with_total() {
        let mut map = HashMap::new();
        let rows: Vec<SessionCommitRow> = (0..7)
            .map(|i| {
                commit(
                    "s1",
                    i,
                    "commit",
                    &format!("sha{i}"),
                    &format!("subject {i}\nsecond line"),
                )
            })
            .collect();
        map.insert("s1".to_string(), rows);
        let f = fold(
            vec![CorpusSessions {
                kb: "k".into(),
                rows: Ok(vec![row("s1", 100, 200)]),
                commits: Ok(map),
            }],
            50,
            None,
        );
        let g = only_group(f);
        let s = &g.sessions[0];
        assert_eq!(s.commits_total, 7);
        assert_eq!(s.commits.len(), MAX_COMMITS_PER_SESSION);
        let shas: Vec<&str> = s
            .commits
            .iter()
            .map(|c| c.sha.as_deref().unwrap())
            .collect();
        assert_eq!(shas, ["sha6", "sha5", "sha4", "sha3", "sha2"]);
        assert_eq!(
            s.commits[0].subject.as_deref(),
            Some("subject 6 second line")
        );
    }

    #[test]
    fn commit_count_is_kind_commit_across_all_in_window_sessions() {
        let mut map = HashMap::new();
        map.insert(
            "s1".to_string(),
            vec![
                commit("s1", 0, "commit", "a", "x"),
                commit("s1", 1, "push", "a", "x"),
                commit("s1", 2, "commit", "b", "y"),
            ],
        );
        // Seven further sessions with one commit each: only five are listed
        // but every one of them counts.
        let mut rows = vec![row("s1", 100, 900)];
        for i in 0..7 {
            let sid = format!("x{i}");
            map.insert(sid.clone(), vec![commit(&sid, 0, "commit", "c", "z")]);
            rows.push(row(&sid, 100, 100 + i));
        }
        let f = fold(
            vec![CorpusSessions {
                kb: "k".into(),
                rows: Ok(rows),
                commits: Ok(map),
            }],
            50,
            None,
        );
        let g = only_group(f);
        assert_eq!(g.sessions.len(), MAX_SESSIONS_PER_PROJECT);
        assert!(g.sessions_truncated);
        assert_eq!(g.sessions_total, 8);
        assert_eq!(g.commit_count, 2 + 7, "push rows do not count");
    }

    #[test]
    fn one_corpus_error_degrades_other_corpus_still_listed() {
        let bad = CorpusSessions {
            kb: "bad".into(),
            rows: Err(QueryErrorClass::Storage),
            commits: Ok(HashMap::new()),
        };
        let bad_commits = CorpusSessions {
            kb: "nocommits".into(),
            rows: Ok(vec![row("s2", 100, 250)]),
            commits: Err(QueryErrorClass::Timeout),
        };
        let f = fold(
            vec![bad, bad_commits, corpus("good", vec![row("s1", 100, 200)])],
            50,
            None,
        );
        assert_eq!(f.degraded.len(), 2);
        assert_eq!(f.degraded[0].kb, "bad");
        assert_eq!(f.degraded[0].lane, "sessions");
        assert_eq!(f.degraded[1].kb, "nocommits");
        assert_eq!(f.degraded[1].lane, "commits");
        let g = only_group(f);
        assert_eq!(
            g.sessions.len(),
            2,
            "sessions of the commits-degraded corpus stay"
        );
    }

    #[test]
    fn scan_cap_sets_sessions_scan_capped() {
        let rows: Vec<SessionRow> = (0..=SESSIONS_SCAN_CAP)
            .map(|i| row(&format!("s{i:04}"), 100, 10_000 - i as i64))
            .collect();
        assert_eq!(rows.len(), SESSIONS_SCAN_CAP + 1);
        let f = fold(vec![corpus("k", rows)], 50, None);
        assert!(f.scan_capped);
        let g = only_group(f);
        assert_eq!(g.sessions_total as usize, SESSIONS_SCAN_CAP);
        let f = fold(vec![corpus("k", vec![row("a", 100, 200)])], 50, None);
        assert!(!f.scan_capped);
    }

    fn group_with(project: &str, label: &str, latest: i64) -> Group {
        let mut g = Group::new(project.into(), label.into(), "derived");
        g.latest_unix = latest;
        g.slate = Some(ChangesSlate {
            slug: project.into(),
            head_seq: 1,
            closed: false,
            posts: vec![],
            posts_total: 0,
            posts_truncated: false,
            other_total: 1,
            retracted_total: 0,
            archived_in_window: false,
        });
        g
    }

    #[test]
    fn groups_sorted_latest_desc_label_tiebreak() {
        let mut m = BTreeMap::new();
        for (id, label, latest) in [
            ("p1", "beta", 100),
            ("p2", "alpha", 100),
            ("p3", "zeta", 200),
        ] {
            m.insert(id.to_string(), group_with(id, label, latest));
        }
        let (v, total, trunc) = finalize_groups(m, None, 20);
        let labels: Vec<&str> = v.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["zeta", "alpha", "beta"]);
        assert_eq!((total, trunc), (3, false));
    }

    #[test]
    fn projects_cap_truncates_with_total() {
        let mut m = BTreeMap::new();
        for i in 0..25 {
            let id = format!("p{i:02}");
            m.insert(id.clone(), group_with(&id, &id, i));
        }
        let (v, total, trunc) = finalize_groups(m, None, MAX_PROJECTS);
        assert_eq!(v.len(), MAX_PROJECTS);
        assert_eq!(total, 25);
        assert!(trunc);
        assert_eq!(v[0].project, "p24", "newest first");
    }

    #[test]
    fn project_filter_matches_id_or_label_exactly() {
        let mut m = BTreeMap::new();
        m.insert("p1".to_string(), group_with("p1", "alpha", 1));
        m.insert("p2".to_string(), group_with("p2", "beta", 2));
        let (v, total, _) = finalize_groups(m, Some("alpha"), 20);
        assert_eq!((v.len(), total), (1, 1));
        assert_eq!(v[0].project, "p1");
    }

    fn slug(s: &str) -> SlateSlug {
        SlateSlug::new(s).unwrap()
    }

    #[test]
    fn slate_partition_sums_to_fresh_count() {
        let posts = vec![
            post(1, 10, "now", "old status", json!({})),
            post(2, 100, "now", "fresh status", json!({})),
            post(3, 101, "take", "taking x", json!({"subject": "x"})),
            post(4, 102, "found", "retracted finding", json!({})),
            post(5, 103, "drop", "drop it", json!({"re": 4})),
            post(6, 104, "warn", "careful", json!({})),
            post(7, 105, "done", "finished", json!({"re": 6})),
            post(8, 106, "idea", "an idea", json!({})),
        ];
        let lane = build_slate(&slug("proj"), 8, false, &posts, 50, 1_000, &[], None).unwrap();
        let s = &lane.slate;
        let fresh = 7u64;
        assert_eq!(s.posts_total + s.other_total + s.retracted_total, fresh);
        assert_eq!(s.other_total, 3, "take + drop + idea");
        assert_eq!(s.retracted_total, 1, "the dropped found");
        let kinds: Vec<(&str, bool)> = s
            .posts
            .iter()
            .map(|p| (p.kind.as_str(), p.closed))
            .collect();
        assert_eq!(kinds, [("done", false), ("warn", true), ("now", false)]);
        assert_eq!(lane.latest_at, 106);
        assert_eq!(s.posts[0].who, "claude/abcd");
        // Nothing fresh and no archive: no entry.
        assert!(build_slate(&slug("proj"), 8, false, &posts, 500, 1_000, &[], None).is_none());
        // An archive in the window alone makes an entry.
        let only_archive =
            build_slate(&slug("proj"), 8, false, &posts, 500, 1_000, &[], Some(600)).unwrap();
        assert!(only_archive.slate.archived_in_window);
        assert!(only_archive.slate.posts.is_empty());
    }

    #[test]
    fn slate_joins_group_by_id_then_label_else_own_group() {
        let mk = |slug_: &str| SlateLane {
            slate: ChangesSlate {
                slug: slug_.into(),
                head_seq: 1,
                closed: false,
                posts: vec![],
                posts_total: 0,
                posts_truncated: false,
                other_total: 1,
                retracted_total: 0,
                archived_in_window: false,
            },
            latest_at: 500,
        };
        let mut groups = BTreeMap::new();
        groups.insert(
            "-key-b".to_string(),
            Group::new("-key-b".into(), "shared".into(), "derived"),
        );
        groups.insert(
            "-key-a".to_string(),
            Group::new("-key-a".into(), "shared".into(), "derived"),
        );
        groups.insert(
            "exact".to_string(),
            Group::new("exact".into(), "Other".into(), "registry"),
        );
        join_slates(&mut groups, vec![mk("exact"), mk("shared"), mk("loner")]);
        assert!(groups["exact"].slate.is_some(), "exact id match");
        assert!(
            groups["-key-a"].slate.is_some(),
            "label match: lowest id wins"
        );
        assert!(groups["-key-b"].slate.is_none());
        let own = &groups["loner"];
        assert_eq!((own.source, own.label.as_str()), ("slate", "loner"));
        assert_eq!(own.latest_unix, 500);
    }

    #[test]
    fn changes_json_never_carries_paths_or_bodies() {
        let mut r = row("s1", 100, 200);
        r.cwd = Some("/srv/abs/secret".into());
        r.repo_root = Some("/srv/abs/secret".into());
        r.all_cwds = Some("/srv/abs/secret\n/srv/abs/other".into());
        r.last_assistant_text = Some(format!("LINE-ONE\nLINE-TWO {}", "x".repeat(700)));
        let f = fold(vec![corpus("k", vec![r])], 50, None);
        let posts = vec![post(
            1,
            100,
            "hand",
            "a handoff",
            json!({
                "body": "BODY-MARKER first\nsecond",
                "refs": ["file:/srv/abs/secret/x.rs"],
                "prov": {"harness": "claude", "session_id": "abcd1234", "cwd": "/srv/abs/secret", "model": "m-secret"},
            }),
        )];
        let lane = build_slate(&slug("kb"), 1, false, &posts, 50, 1_000, &[], None).unwrap();
        let resp = compose(
            Window {
                since: 50,
                clamped: false,
            },
            1_000,
            None,
            f,
            vec![lane],
            vec![],
        );
        let text = serde_json::to_string(&resp).unwrap();
        for needle in ["/srv/abs", "BODY-MARKER", "m-secret", "second", "\\n"] {
            assert!(!text.contains(needle), "wire leaked {needle:?}: {text}");
        }
        let closing = resp.projects[0].sessions[0].closing.as_deref().unwrap();
        assert!(
            closing.chars().count() <= CLOSING_MAX_CHARS,
            "closing is capped"
        );
        assert!(!closing.contains('\n'));
    }

    #[test]
    fn empty_compose_has_every_flag_false_and_no_degraded() {
        let f = fold(vec![corpus("k", vec![])], 50, None);
        let resp = compose(
            Window {
                since: 50,
                clamped: false,
            },
            100,
            None,
            f,
            vec![],
            vec![],
        );
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["projects"], json!([]));
        for flag in [
            "window_clamped",
            "projects_truncated",
            "sessions_scan_capped",
        ] {
            assert_eq!(v[flag], json!(false), "{flag}");
        }
        assert_eq!(v["trivial_sessions_hidden"], json!(0));
        assert!(v.get("degraded").is_none());
        assert!(v.get("slates_skipped").is_none());
        assert!(v.get("project").is_none());
    }

    #[test]
    fn slate_lane_skips_untouched_slates_before_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke".to_string());
        let slug = SlateSlug::new("stale-one").unwrap();
        // A corrupt meta: a full load would name it in `skipped`.
        std::fs::create_dir_all(paths.slate_dir(&slug)).unwrap();
        std::fs::write(paths.slate_meta_file(&slug), "not json").unwrap();
        std::fs::write(paths.slate_ledger_file(&slug), "not json\n").unwrap();
        let now = chrono::Utc::now().timestamp();
        // Files were touched before `since`: skipped without a load.
        let (lanes, skipped) = slate_lane(&paths, now + 3600, now + 3600, &[]);
        assert!(lanes.is_empty());
        assert!(skipped.is_empty(), "the pre-check must precede the load");
        // Files touched inside the window: the load runs and the corrupt
        // slate is named.
        let (_, skipped) = slate_lane(&paths, 0, now, &[]);
        assert_eq!(skipped, vec!["stale-one".to_string()]);
    }
}
