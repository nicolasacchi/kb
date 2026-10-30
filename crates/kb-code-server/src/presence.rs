//! OL PRES2 — `GET /api/kb/presence[?repo=NAME]`: which files agents are
//! editing right now, as a repo-relative, transcript-free payload
//! (`kbc-presence/1`) for the SPA's presence marks.
//!
//! The data is kb's: PRES1 keeps the last touches of every live session in
//! memory and serves them scrubbed at `GET /api/sessions/live-touches` (a
//! project label plus a path relative to a root kb never kept). This module
//! only MAPS those project labels onto kb-code's configured repos and
//! reshapes the rows. Invariant #2 holds: the one live call direction is
//! kb-code -> kb, through [`KbClient::live_touches`] (kb-sibling/1
//! handshake, fail closed on a mismatch); nothing is cached or persisted
//! and no new SSE kind exists.
//!
//! **Mapping, per kb project label (case-insensitive), first rule that
//! matches wins:** (1) a repo's `[[repos]] projects = [...]` alias, (2) a
//! repo's `name`, (3) the basename of a repo's `path`. A rule that matches
//! more than one repo makes the label ambiguous: its touches are skipped and
//! counted ("N edits in ambiguous projects"), never guessed. A label no rule
//! matches counts toward "N edits outside configured repos". The alias rule
//! exists because the hook reports the HOST directory name: a repo
//! bind-mounted into a container under another name (kb-archive mounted from
//! `kb-archive-checkout`) matches neither its name nor its container-side
//! basename.
//!
//! **Worktrees:** a touch made in a linked worktree carries the worktree's
//! directory BASENAME. Such a row is reported with `worktree` set and is a
//! separate row from the same relative path in the main checkout; a consumer
//! must never draw it on the main checkout's file.
//!
//! **Degradation** is part of the contract: every failure is a `200` with
//! `available:false` and a caption from the closed vocabulary below, never a
//! 5xx. The route is an ordinary `auth_bearer` read under the three
//! crate-wide guards and is NOT on either doc-lens CORS sub-router.

use crate::config::RepoEntry;
use crate::join::kb_client::{
    KbClientError, LiveTouch, LiveTouchesRelay, LIVE_TOUCHES_MAX_PROJECTS,
};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA: &str = "kbc-presence/1";
/// Most rows one answer carries.
pub const MAX_FILES: usize = 200;
/// Touch window asked of kb (kb clamps it to 60..=3600 itself).
pub const MAX_AGE_SECS: i64 = 900;
/// Longest repo-relative path a row may carry (bytes).
const MAX_REL_BYTES: usize = 1024;

// The closed caption vocabulary (the two edit-count captions are built from
// `N` and the phrases below).
pub const CAP_DISABLED: &str = "kb disabled";
pub const CAP_MISMATCH: &str = "kb_sibling_mismatch";
pub const CAP_TIMEOUT: &str = "kb timed out";
pub const CAP_UNREACHABLE: &str = "kb unreachable";
pub const CAP_TOO_OLD: &str = "kb too old for presence";
pub const CAP_UNKNOWN_REPO: &str = "unknown repo";
pub const CAP_TRUNCATED: &str = "kb list truncated";

#[derive(Debug, Deserialize)]
pub struct PresenceParams {
    #[serde(default)]
    pub repo: Option<String>,
}

// One row. `worktree` is the basename of a linked worktree and absent for the
// main checkout; a row with it set is NOT the main checkout's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresenceFile {
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub path: String,
    pub session_id: String,
    pub harness: String,
    pub state: String,
    pub age_secs: i64,
}

// `GET /api/kb/presence`. `caption` is absent when there is nothing to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresenceOut {
    pub schema: &'static str,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub now_unix: i64,
    pub files: Vec<PresenceFile>,
}

/// Where one kb project label lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapped {
    Repo(usize),
    Ambiguous,
    Unmatched,
}

fn basename(r: &RepoEntry) -> Option<String> {
    r.path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

fn pick(hits: &[usize]) -> Option<Mapped> {
    match hits {
        [] => None,
        [one] => Some(Mapped::Repo(*one)),
        _ => Some(Mapped::Ambiguous),
    }
}

/// Map a kb project label onto a configured repo: alias > name > path
/// basename, case-insensitive; a tier matching several repos is ambiguous
/// (it does NOT fall through to the next tier).
pub fn map_project(repos: &[RepoEntry], project: &str) -> Mapped {
    type Tier = fn(&RepoEntry, &str) -> bool;
    let tiers: [Tier; 3] = [
        |r, p| r.projects.iter().any(|a| a.eq_ignore_ascii_case(p)),
        |r, p| r.name.eq_ignore_ascii_case(p),
        |r, p| basename(r).is_some_and(|b| b.eq_ignore_ascii_case(p)),
    ];
    for tier in tiers {
        let hits: Vec<usize> = repos
            .iter()
            .enumerate()
            .filter(|(_, r)| tier(r, project))
            .map(|(i, _)| i)
            .collect();
        if let Some(m) = pick(&hits) {
            return m;
        }
    }
    Mapped::Unmatched
}

/// The distinct project labels (alias, name, path basename) that can map to
/// the given repos, in that order per repo, deduplicated case-insensitively.
pub fn labels_for(repos: &[&RepoEntry]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for r in repos {
        let all = r
            .projects
            .iter()
            .cloned()
            .chain(std::iter::once(r.name.clone()))
            .chain(basename(r));
        for l in all {
            let l = l.trim().to_string();
            if !l.is_empty() && seen.insert(l.to_ascii_lowercase()) {
                out.push(l);
            }
        }
    }
    out
}

/// The `project=` filters to send kb: the scoped repo's labels when a repo is
/// named (and they fit kb's cap of 32), else none. Unfiltered on purpose
/// without a repo: the unmatched / ambiguous captions need the rows kb would
/// otherwise filter away.
pub fn project_filters(repos: &[RepoEntry], scope: Option<usize>) -> Vec<String> {
    let Some(i) = scope else { return Vec::new() };
    let labels = labels_for(&[&repos[i]]);
    if labels.len() > LIVE_TOUCHES_MAX_PROJECTS {
        Vec::new()
    } else {
        labels
    }
}

/// A repo-relative path a browser may be handed: non-empty, bounded, not
/// absolute, no control characters, no empty/`.`/`..` segment.
pub fn rel_ok(rel: &str) -> bool {
    !rel.is_empty()
        && rel.len() <= MAX_REL_BYTES
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.chars().any(char::is_control)
        && rel
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn worktree_ok(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 128
        && !w.contains('/')
        && !w.contains('\\')
        && !w.chars().any(char::is_control)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        "edit"
    } else {
        "edits"
    }
}

fn row(t: &LiveTouch, repo: &RepoEntry) -> Option<PresenceFile> {
    if !rel_ok(&t.rel) {
        return None;
    }
    let worktree = match t.worktree.as_deref() {
        None => None,
        Some(w) if worktree_ok(w) => Some(w.to_string()),
        // A label we cannot show must not turn into "the main checkout".
        Some(_) => return None,
    };
    Some(PresenceFile {
        repo: repo.name.clone(),
        worktree,
        path: t.rel.clone(),
        session_id: t.session_id.clone(),
        harness: t.harness.clone(),
        state: t.state.clone(),
        age_secs: t.age_secs.max(0),
    })
}

/// Compose a healthy answer from kb's rows. Pure. `scope` limits the output
/// to one repo (rows mapping elsewhere are dropped silently, not counted).
pub fn compose(
    repos: &[RepoEntry],
    scope: Option<usize>,
    relay: &LiveTouchesRelay,
    now_unix: i64,
) -> PresenceOut {
    let mut unmatched = 0usize;
    let mut ambiguous = 0usize;
    let mut files: Vec<PresenceFile> = Vec::new();
    for t in &relay.touches {
        match map_project(repos, &t.project) {
            Mapped::Unmatched => {
                if scope.is_none() {
                    unmatched += 1;
                }
            }
            Mapped::Ambiguous => ambiguous += 1,
            Mapped::Repo(i) => {
                if scope.is_some_and(|s| s != i) {
                    continue;
                }
                if let Some(f) = row(t, &repos[i]) {
                    files.push(f);
                }
            }
        }
    }
    files.sort_by(|a, b| {
        (a.age_secs, &a.repo, &a.worktree, &a.path, &a.session_id).cmp(&(
            b.age_secs,
            &b.repo,
            &b.worktree,
            &b.path,
            &b.session_id,
        ))
    });
    let mut seen: HashSet<(String, Option<String>, String, String)> = HashSet::new();
    files.retain(|f| {
        seen.insert((
            f.repo.clone(),
            f.worktree.clone(),
            f.path.clone(),
            f.session_id.clone(),
        ))
    });
    files.truncate(MAX_FILES);

    let mut caps: Vec<String> = Vec::new();
    if unmatched > 0 {
        caps.push(format!(
            "{unmatched} {} outside configured repos",
            plural(unmatched)
        ));
    }
    if ambiguous > 0 {
        caps.push(format!(
            "{ambiguous} {} in ambiguous projects",
            plural(ambiguous)
        ));
    }
    if relay.truncated {
        caps.push(CAP_TRUNCATED.to_string());
    }
    PresenceOut {
        schema: SCHEMA,
        available: true,
        caption: (!caps.is_empty()).then(|| caps.join("; ")),
        now_unix,
        files,
    }
}

/// The caption for a failed relay call (closed vocabulary).
pub fn failure_caption(err: &KbClientError) -> &'static str {
    match err {
        KbClientError::Disabled => CAP_DISABLED,
        KbClientError::SiblingMismatch(_) => CAP_MISMATCH,
        KbClientError::Timeout(_) => CAP_TIMEOUT,
        KbClientError::BadStatus(s) if s.as_u16() == 404 => CAP_TOO_OLD,
        KbClientError::BadStatus(_)
        | KbClientError::Unreachable(_, _)
        | KbClientError::Parse(_)
        | KbClientError::ClientBuild(_)
        | KbClientError::InvalidPath(_) => CAP_UNREACHABLE,
    }
}

fn unavailable(caption: &str, now_unix: i64) -> PresenceOut {
    PresenceOut {
        schema: SCHEMA,
        available: false,
        caption: Some(caption.to_string()),
        now_unix,
        files: Vec::new(),
    }
}

/// The whole answer for one request, given kb's result. Pure.
pub fn body(
    repos: &[RepoEntry],
    scope: Option<usize>,
    result: Result<LiveTouchesRelay, KbClientError>,
    now_unix: i64,
) -> PresenceOut {
    match result {
        Ok(relay) => compose(repos, scope, &relay, now_unix),
        Err(e) => unavailable(failure_caption(&e), now_unix),
    }
}

/// `GET /api/kb/presence[?repo=NAME]`.
pub async fn kb_presence_route(
    State(state): State<SharedState>,
    Query(params): Query<PresenceParams>,
) -> impl IntoResponse {
    let now_unix = chrono::Utc::now().timestamp();
    let repos = &state.repos;
    let out = match params
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(name) => match repos.iter().position(|r| r.name == name) {
            None => unavailable(CAP_UNKNOWN_REPO, now_unix),
            Some(i) => {
                let filters = project_filters(repos, Some(i));
                body(
                    repos,
                    Some(i),
                    state.kb_client.live_touches(&filters, MAX_AGE_SECS).await,
                    now_unix,
                )
            }
        },
        None => body(
            repos,
            None,
            state.kb_client.live_touches(&[], MAX_AGE_SECS).await,
            now_unix,
        ),
    };
    ([(header::CACHE_CONTROL, "no-store")], Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str, path: &str, aliases: &[&str]) -> RepoEntry {
        RepoEntry {
            name: name.to_string(),
            path: PathBuf::from(path),
            projects: aliases.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn touch(project: &str, wt: Option<&str>, rel: &str, sid: &str, age: i64) -> LiveTouch {
        LiveTouch {
            project: project.to_string(),
            worktree: wt.map(str::to_string),
            rel: rel.to_string(),
            session_id: sid.to_string(),
            harness: "claude".to_string(),
            state: "working".to_string(),
            age_secs: age,
        }
    }

    fn relay(ts: Vec<LiveTouch>) -> LiveTouchesRelay {
        LiveTouchesRelay {
            touches: ts,
            truncated: false,
        }
    }

    #[test]
    fn maps_by_name_then_by_path_basename() {
        let repos = vec![repo("kb", "/srv/checkouts/kb-main", &[])];
        assert_eq!(map_project(&repos, "kb"), Mapped::Repo(0));
        assert_eq!(
            map_project(&repos, "KB"),
            Mapped::Repo(0),
            "case-insensitive"
        );
        assert_eq!(map_project(&repos, "kb-main"), Mapped::Repo(0));
        assert_eq!(map_project(&repos, "kb-other"), Mapped::Unmatched);
    }

    /// The prod case: repo `kb-archive` is bind-mounted from a host directory
    /// `kb-archive-checkout`, so the hook's project matches neither the name
    /// nor the container-side basename (`/repos/kb-archive`) - only the alias.
    #[test]
    fn the_kb_archive_checkout_alias_lands_the_touch_in_its_repo() {
        let plain = vec![repo("kb-archive", "/repos/kb-archive", &[])];
        assert_eq!(
            map_project(&plain, "kb-archive-checkout"),
            Mapped::Unmatched
        );
        let out = compose(
            &plain,
            None,
            &relay(vec![touch("kb-archive-checkout", None, "a.rs", "s", 3)]),
            100,
        );
        assert!(out.files.is_empty());
        assert_eq!(
            out.caption.as_deref(),
            Some("1 edit outside configured repos")
        );

        let aliased = vec![repo(
            "kb-archive",
            "/repos/kb-archive",
            &["kb-archive-checkout"],
        )];
        assert_eq!(
            map_project(&aliased, "kb-archive-checkout"),
            Mapped::Repo(0)
        );
        let out = compose(
            &aliased,
            None,
            &relay(vec![touch("kb-archive-checkout", None, "a.rs", "s", 3)]),
            100,
        );
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].repo, "kb-archive");
        assert_eq!(out.caption, None);
    }

    #[test]
    fn order_is_alias_then_name_then_path_basename() {
        // `x` is repo A's alias, repo B's name and repo C's basename: A wins.
        let repos = vec![
            repo("a", "/r/a", &["x"]),
            repo("x", "/r/b", &[]),
            repo("c", "/r/x", &[]),
        ];
        assert_eq!(map_project(&repos, "x"), Mapped::Repo(0));
        // Without the alias, the name beats the basename.
        let repos = vec![repo("x", "/r/b", &[]), repo("c", "/r/x", &[])];
        assert_eq!(map_project(&repos, "x"), Mapped::Repo(0));
        // Basename only.
        let repos = vec![repo("a", "/r/a", &[]), repo("c", "/r/x", &[])];
        assert_eq!(map_project(&repos, "x"), Mapped::Repo(1));
    }

    #[test]
    fn a_tier_matching_two_repos_is_ambiguous_and_does_not_fall_through() {
        let repos = vec![repo("a", "/one/app", &[]), repo("b", "/two/app", &[])];
        assert_eq!(map_project(&repos, "app"), Mapped::Ambiguous);
        let out = compose(
            &repos,
            None,
            &relay(vec![
                touch("app", None, "x.rs", "s1", 1),
                touch("app", None, "y.rs", "s1", 2),
            ]),
            100,
        );
        assert!(out.files.is_empty());
        assert_eq!(
            out.caption.as_deref(),
            Some("2 edits in ambiguous projects")
        );
        // Two aliases claiming one label are ambiguous too, even though a
        // repo is also NAMED that way (alias tier decides first).
        let repos = vec![
            repo("a", "/r/a", &["shared"]),
            repo("b", "/r/b", &["shared"]),
            repo("shared", "/r/s", &[]),
        ];
        assert_eq!(map_project(&repos, "shared"), Mapped::Ambiguous);
    }

    #[test]
    fn unmatched_and_ambiguous_are_counted_separately() {
        let repos = vec![
            repo("a", "/one/app", &[]),
            repo("b", "/two/app", &[]),
            repo("kb", "/r/kb", &[]),
        ];
        let out = compose(
            &repos,
            None,
            &relay(vec![
                touch("kb", None, "ok.rs", "s", 1),
                touch("nope", None, "z.rs", "s", 2),
                touch("app", None, "q.rs", "s", 3),
            ]),
            100,
        );
        assert_eq!(out.files.len(), 1);
        assert_eq!(
            out.caption.as_deref(),
            Some("1 edit outside configured repos; 1 edit in ambiguous projects")
        );
    }

    #[test]
    fn worktree_touches_are_separate_rows_never_the_main_file() {
        let repos = vec![repo("kb", "/r/kb", &[])];
        let out = compose(
            &repos,
            None,
            &relay(vec![
                touch("kb", None, "src/lib.rs", "s1", 10),
                touch("kb", Some("v80-x"), "src/lib.rs", "s2", 5),
                touch("kb", Some("v80-x"), "src/lib.rs", "s2", 9),
            ]),
            100,
        );
        assert_eq!(out.files.len(), 2, "same path, main vs worktree");
        assert_eq!(out.files[0].worktree.as_deref(), Some("v80-x"));
        assert_eq!(out.files[0].age_secs, 5, "newest first, repeat deduped");
        assert_eq!(out.files[1].worktree, None);
        let v = serde_json::to_value(&out).unwrap();
        assert!(v["files"][1].get("worktree").is_none(), "main: key absent");
        assert_eq!(v["files"][0]["worktree"], "v80-x");
        // A worktree label we cannot show drops the row; it never becomes main.
        let out = compose(
            &repos,
            None,
            &relay(vec![touch("kb", Some("a/b"), "src/lib.rs", "s", 1)]),
            100,
        );
        assert!(out.files.is_empty());
    }

    #[test]
    fn rows_are_deduped_per_path_and_session_newest_first_and_capped() {
        let repos = vec![repo("kb", "/r/kb", &["kb-alias"])];
        let mut ts = vec![
            touch("kb", None, "a.rs", "s1", 30),
            touch("kb-alias", None, "a.rs", "s1", 10),
            touch("kb", None, "a.rs", "s2", 20),
        ];
        let out = compose(&repos, None, &relay(ts.clone()), 100);
        let got: Vec<_> = out
            .files
            .iter()
            .map(|f| (f.session_id.as_str(), f.age_secs))
            .collect();
        assert_eq!(got, [("s1", 10), ("s2", 20)]);

        ts = (0..MAX_FILES + 50)
            .map(|i| touch("kb", None, &format!("f{i:04}.rs"), "s", i as i64))
            .collect();
        let out = compose(&repos, None, &relay(ts), 100);
        assert_eq!(out.files.len(), MAX_FILES);
        assert_eq!(out.files[0].path, "f0000.rs");
    }

    #[test]
    fn a_repo_scope_keeps_only_that_repo_and_does_not_count_the_rest() {
        let repos = vec![repo("a", "/r/a", &[]), repo("b", "/r/b", &[])];
        let out = compose(
            &repos,
            Some(1),
            &relay(vec![
                touch("a", None, "x.rs", "s", 1),
                touch("b", None, "y.rs", "s", 2),
                touch("zzz", None, "q.rs", "s", 3),
            ]),
            100,
        );
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].repo, "b");
        assert_eq!(out.caption, None);
    }

    #[test]
    fn unsafe_relative_paths_are_dropped() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a\\b",
            "a\u{7}b",
        ] {
            assert!(!rel_ok(bad), "{bad:?}");
        }
        assert!(!rel_ok(&"a".repeat(MAX_REL_BYTES + 1)));
        for ok in ["a.rs", "src/lib.rs", "..hidden/x", "a b/c"] {
            assert!(rel_ok(ok), "{ok:?}");
        }
        let repos = vec![repo("kb", "/r/kb", &[])];
        let out = compose(
            &repos,
            None,
            &relay(vec![touch("kb", None, "../../etc/passwd", "s", 1)]),
            100,
        );
        assert!(out.files.is_empty());
    }

    #[test]
    fn filters_are_the_scoped_repos_labels_and_respect_kbs_cap() {
        let repos = vec![
            repo(
                "kb-archive",
                "/repos/kb-archive",
                &["kb-archive-checkout", "KB-ARCHIVE"],
            ),
            repo("b", "/r/b", &[]),
        ];
        assert_eq!(project_filters(&repos, None), Vec::<String>::new());
        assert_eq!(
            project_filters(&repos, Some(0)),
            ["kb-archive-checkout", "KB-ARCHIVE"],
            "alias first; the name and basename duplicate the alias case-insensitively"
        );
        let many: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let repos = vec![repo("big", "/r/big", &refs)];
        assert!(
            project_filters(&repos, Some(0)).is_empty(),
            "over kb's cap: unfiltered"
        );
    }

    #[test]
    fn every_failure_has_a_caption_from_the_closed_vocabulary() {
        use reqwest::StatusCode;
        let cases: Vec<(KbClientError, &str)> = vec![
            (KbClientError::Disabled, CAP_DISABLED),
            (KbClientError::SiblingMismatch("x".into()), CAP_MISMATCH),
            (KbClientError::Timeout("u".into()), CAP_TIMEOUT),
            (KbClientError::BadStatus(StatusCode::NOT_FOUND), CAP_TOO_OLD),
            (
                KbClientError::BadStatus(StatusCode::FORBIDDEN),
                CAP_UNREACHABLE,
            ),
            (
                KbClientError::BadStatus(StatusCode::UNAUTHORIZED),
                CAP_UNREACHABLE,
            ),
            (
                KbClientError::Unreachable("u".into(), "e".into()),
                CAP_UNREACHABLE,
            ),
            (KbClientError::Parse("x".into()), CAP_UNREACHABLE),
        ];
        for (err, want) in cases {
            let out = body(&[], None, Err(err), 7);
            assert!(!out.available);
            assert_eq!(out.caption.as_deref(), Some(want));
            assert!(out.files.is_empty());
            assert_eq!(out.now_unix, 7);
        }
    }

    /// The exact wire key set, and nothing that could carry a transcript or
    /// an absolute path.
    #[test]
    fn presence_response_key_set_pinned() {
        let repos = vec![repo("kb", "/home/someone/kb", &[])];
        let out = compose(
            &repos,
            None,
            &relay(vec![
                touch("kb", None, "src/a.rs", "s1", 1),
                touch("kb", Some("wt"), "src/a.rs", "s2", 2),
                touch("other", None, "x.rs", "s3", 3),
            ]),
            100,
        );
        let v = serde_json::to_value(&out).unwrap();
        let mut top: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        top.sort();
        assert_eq!(top, ["available", "caption", "files", "now_unix", "schema"]);
        let mut main: Vec<_> = v["files"][0].as_object().unwrap().keys().cloned().collect();
        main.sort();
        assert_eq!(
            main,
            ["age_secs", "harness", "path", "repo", "session_id", "state"]
        );
        let mut wt: Vec<_> = v["files"][1].as_object().unwrap().keys().cloned().collect();
        wt.sort();
        assert_eq!(
            wt,
            [
                "age_secs",
                "harness",
                "path",
                "repo",
                "session_id",
                "state",
                "worktree"
            ]
        );
        let text = v.to_string();
        for banned in ["/home/", "cwd", "title", "last_line", "model", "transcript"] {
            assert!(!text.contains(banned), "{banned}: {text}");
        }
        assert_eq!(v["schema"], "kbc-presence/1");
        // A healthy, quiet answer carries no caption key at all.
        let quiet = serde_json::to_value(compose(&repos, None, &relay(vec![]), 1)).unwrap();
        assert!(quiet.get("caption").is_none());
        assert_eq!(quiet["available"], true);
    }

    #[test]
    fn a_truncated_kb_list_is_captioned() {
        let repos = vec![repo("kb", "/r/kb", &[])];
        let out = compose(
            &repos,
            None,
            &LiveTouchesRelay {
                touches: vec![],
                truncated: true,
            },
            1,
        );
        assert_eq!(out.caption.as_deref(), Some(CAP_TRUNCATED));
    }
}
