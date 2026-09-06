//! `kb doctor` — read-only diagnostics distinct from `kb daemon doctor`
//! (which probes the daemon's own HTTP health: identity/kbs/errors/stats/
//! embedder/bus). v0.38 CT-C6 adds the first mode, `--hooks`: the
//! provenance-chain integrity check. Four pieces wire together to make a
//! memory's provenance trustworthy, and every one of them fails SILENTLY:
//!
//!   1. the session marker files (`~/.cache/kb/current-session` +
//!      `current-session-repo-<slug>`) `kb-recall.sh` writes;
//!   2. the kb-memory plugin hooks themselves (SessionStart/UserPromptSubmit/
//!      Stop — `plugins/kb-memory/hooks/hooks.json`);
//!   3. the git `prepare-commit-msg` trailer dispatcher
//!      (`plugins/kb-memory/hooks/git-dispatch/`) that stamps `Kb-Session:`;
//!   4. the V0035 `memory_recalls` ledger fed by capture;
//!   5. kb-code's why-hook (`plugins/kb-code/hooks/kb-code-why.sh`).
//!
//! v0.42 (D30) folds in one more check that ISN'T about provenance: `kb
//! slate`'s `slate-cursor-<sid>`/`slate-topic-<sid>` markers under the same
//! `~/.cache/kb` never expire on their own, so `--hooks` also flags any
//! older than 30 days — and `--fix` removes THOSE ONLY, every other check
//! here stays read-only.
//!
//! Every check below prints PASS/WARN/SKIP + a one-line fix, and is
//! explicit about what it couldn't verify (a SKIP is never silently
//! upgraded to a PASS — "detection is best-effort" means saying so, not
//! guessing). HTTP-backed checks are split into a thin async fetch + a
//! pure decision fn (`decide_*`) so the interesting logic is unit-tested
//! without a live daemon.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// ============================================================ status/render

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Skip,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Skip => "skip",
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Pass => "✓",
            CheckStatus::Warn => "⚠",
            CheckStatus::Skip => "○",
        }
    }
}

#[derive(Debug, Clone)]
struct HookCheck {
    id: &'static str,
    status: CheckStatus,
    detail: String,
    fix: Option<String>,
}

impl HookCheck {
    fn new(id: &'static str, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            id,
            status,
            detail: detail.into(),
            fix: None,
        }
    }
    fn pass(id: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Pass, detail)
    }
    fn warn(id: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Warn, detail)
    }
    fn skip(id: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Skip, detail)
    }
    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// v1 harness scope: every check below only understands Claude Code's own
/// marker/hook shapes. Printed once, human mode only (also carried in
/// `--json`'s `notes`) — honesty over coverage (WORK ORDER item 4).
const HARNESS_SCOPE_NOTE: &str = "harness scope: v1 checks Claude Code only — \
     the codex/kimi capture + distill-nudge adapters exist but aren't probed \
     by this command";

fn render_human(checks: &[HookCheck]) -> String {
    let mut out = String::new();
    out.push_str("kb doctor --hooks — provenance-chain integrity check\n\n");
    for c in checks {
        out.push_str(&format!(
            "  {} {:<28} {}\n",
            c.status.glyph(),
            c.id,
            c.detail
        ));
        if let Some(fix) = &c.fix {
            out.push_str(&format!("      fix: {fix}\n"));
        }
    }
    let pass = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Pass)
        .count();
    let warn = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let skip = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Skip)
        .count();
    out.push_str(&format!(
        "\n{pass} pass, {warn} warn, {skip} skip ({} checks total)\n",
        checks.len()
    ));
    out.push_str(&format!("\n{HARNESS_SCOPE_NOTE}\n"));
    out
}

fn to_json(checks: &[HookCheck]) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = checks
        .iter()
        .map(|c| {
            let mut v = serde_json::json!({
                "id": c.id,
                "status": c.status.as_str(),
                "detail": c.detail,
            });
            if let Some(fix) = &c.fix {
                v["fix"] = serde_json::Value::String(fix.clone());
            }
            v
        })
        .collect();
    serde_json::json!({
        "checks": rows,
        "notes": [HARNESS_SCOPE_NOTE],
    })
}

// ================================================================ helpers

/// Coarse "how long ago" for an already-computed age in seconds
/// (`s`/`m`/`h`/`d`), clamped at 0 (never a negative age string) — mirrors
/// `commands::comments::fmt_age`'s bucketing but takes a delta rather than
/// a timestamp (every call site here already has one).
fn fmt_age(age_secs: i64) -> String {
    let d = age_secs.max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86_400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86_400)
    }
}

/// `${XDG_CACHE_HOME:-$HOME/.cache}` — same resolution `kb-recall.sh` /
/// `commands::memory::read_session_marker` use. `None` when neither env
/// var is set (can't resolve a cache dir at all).
fn resolve_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
}

/// Same squeeze-and-lowercase slug `kb-recall.sh`/`trailer-logic.sh`'s
/// `kb_slugify()` compute (`tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9'
/// '-'`, then trim leading/trailing `-`): every run of non-alphanumeric
/// bytes collapses to a single `-`. Byte-identical to the bash version for
/// ASCII paths (the overwhelmingly common case — repo paths on this box);
/// non-ASCII paths are NOT guaranteed to match bash's C-locale byte-wise
/// `tr` behaviour (out of scope for a diagnostic tool).
fn slugify_repo_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_dash = false;
    for c in path.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            out.push(lc);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Parse the repo-keyed marker's two-line body (session id, then a unix
/// timestamp) — mirrors `trailer-logic.sh`'s `sed -n '1p'`/`'2p'` read,
/// including its `marker_ts` fallback to `0` on anything non-numeric
/// (which then always reads as maximally stale, never fresh). `None` when
/// the first line (the session id) is blank.
fn parse_repo_marker(contents: &str) -> Option<(String, i64)> {
    let mut lines = contents.lines();
    let sid = lines.next().unwrap_or("").trim();
    if sid.is_empty() {
        return None;
    }
    let ts = lines
        .next()
        .unwrap_or("")
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    Some((sid.to_string(), ts))
}

/// `trailer-logic.sh`'s exact freshness test: `age = now - marker_ts`,
/// fresh iff `0 <= age <= max_age_secs` (a future-dated marker — negative
/// age, clock skew — reads as NOT fresh, same as the bash `-ge 0` guard).
fn marker_is_fresh(now: i64, marker_ts: i64, max_age_secs: i64) -> bool {
    let age = now - marker_ts;
    (0..=max_age_secs).contains(&age)
}

/// ~40 minutes — `trailer-logic.sh`'s `max_age=2400`.
const REPO_MARKER_MAX_AGE_SECS: i64 = 2400;

// ======================================================= a) plain marker

fn plain_marker_check(cache_dir: Option<&Path>, now: i64) -> HookCheck {
    let Some(cache_dir) = cache_dir else {
        return HookCheck::skip(
            "session-marker",
            "could not resolve a cache dir — neither XDG_CACHE_HOME nor HOME is set",
        );
    };
    let path = cache_dir.join("kb").join("current-session");
    match std::fs::read_to_string(&path) {
        Ok(raw) if !raw.trim().is_empty() => {
            let age = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| now - d.as_secs() as i64);
            match age {
                Some(age) => HookCheck::pass(
                    "session-marker",
                    format!(
                        "{} present, age {} (informational)",
                        path.display(),
                        fmt_age(age)
                    ),
                ),
                None => HookCheck::pass(
                    "session-marker",
                    format!("{} present (mtime unavailable)", path.display()),
                ),
            }
        }
        Ok(_) => HookCheck::warn(
            "session-marker",
            format!("{} exists but is empty", path.display()),
        ),
        Err(_) => HookCheck::warn(
            "session-marker",
            format!(
                "{} not found — kb-recall.sh's UserPromptSubmit hook hasn't \
                 written it yet in this environment (no prompt run under the \
                 kb-memory plugin, or ~/.cache was cleared)",
                path.display()
            ),
        ),
    }
}

// ===================================================== b) repo-keyed marker

fn repo_marker_check(cache_dir: Option<&Path>, slug: &str, now: i64) -> HookCheck {
    let Some(cache_dir) = cache_dir else {
        return HookCheck::skip(
            "repo-session-marker",
            "could not resolve a cache dir — neither XDG_CACHE_HOME nor HOME is set",
        );
    };
    let path = cache_dir
        .join("kb")
        .join(format!("current-session-repo-{slug}"));
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return HookCheck::warn(
            "repo-session-marker",
            format!(
                "{} not found (slug `{slug}`) — the git Kb-Session trailer \
                 hook has no fallback session id for this repo until a \
                 prompt runs here",
                path.display()
            ),
        );
    };
    let Some((sid, ts)) = parse_repo_marker(&raw) else {
        return HookCheck::warn(
            "repo-session-marker",
            format!(
                "{} is malformed — couldn't parse a session id off its first line",
                path.display()
            ),
        );
    };
    let age = now - ts;
    if marker_is_fresh(now, ts, REPO_MARKER_MAX_AGE_SECS) {
        HookCheck::pass(
            "repo-session-marker",
            format!(
                "sid={sid} age={} (within the 40m freshness window)",
                fmt_age(age)
            ),
        )
    } else {
        HookCheck::warn(
            "repo-session-marker",
            format!(
                "sid={sid} age={} — stale (past trailer-logic.sh's 40m \
                 freshness window; the next commit here won't get a \
                 Kb-Session trailer from this marker)",
                fmt_age(age)
            ),
        )
    }
}

// ================================================= c) git trailer hook

enum GitHookState {
    NotGitRepo,
    Missing { hooks_dir: PathBuf },
    Other { path: PathBuf },
    Dispatcher { path: PathBuf },
}

fn run_git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    // `--path-format=absolute` needs git >= 2.31; fall back to resolving a
    // possibly-relative `--git-common-dir` against `repo` on older git.
    if let Some(p) = run_git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        return Some(PathBuf::from(p));
    }
    let p = run_git(repo, &["rev-parse", "--git-common-dir"])?;
    let pb = PathBuf::from(&p);
    Some(if pb.is_absolute() { pb } else { repo.join(pb) })
}

/// Resolve the repo's effective hooks dir + inspect `prepare-commit-msg`
/// (following a symlink transparently — `read_to_string` always does),
/// looking for the kb-memory dispatcher's own fingerprints
/// (`dispatch.sh`'s "Kb-Session" comment / its call to `trailer-logic.sh`).
/// Deliberately content-based rather than an exact-path match against
/// `install-git-trailer.sh`'s own idempotency check: the repo may have
/// been wired from a DIFFERENT kb checkout than the one running this
/// doctor, and an exact-path comparison would false-negative that case.
fn detect_git_trailer_hook(repo: &Path) -> GitHookState {
    if run_git(repo, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return GitHookState::NotGitRepo;
    }
    let hooks_dir = match run_git(repo, &["config", "--local", "--get", "core.hooksPath"]) {
        Some(p) if !p.is_empty() => {
            let pb = PathBuf::from(&p);
            if pb.is_absolute() {
                pb
            } else {
                // A relative core.hooksPath resolves against the worktree
                // top-level (githooks(5)).
                run_git(repo, &["rev-parse", "--show-toplevel"])
                    .map(|t| PathBuf::from(t).join(&pb))
                    .unwrap_or_else(|| repo.join(&pb))
            }
        }
        _ => match git_common_dir(repo) {
            Some(common) => common.join("hooks"),
            None => return GitHookState::NotGitRepo,
        },
    };
    let candidate = hooks_dir.join("prepare-commit-msg");
    match std::fs::read_to_string(&candidate) {
        Ok(content) if content.contains("Kb-Session") || content.contains("trailer-logic.sh") => {
            GitHookState::Dispatcher { path: candidate }
        }
        Ok(_) => GitHookState::Other { path: candidate },
        Err(_) => GitHookState::Missing { hooks_dir },
    }
}

const INSTALL_GIT_TRAILER_HINT: &str =
    "plugins/kb-memory/hooks/install-git-trailer.sh <repo-path>  (see hooks/README.md \
     \"Kb-Session commit trailer\")";

fn decide_git_trailer_hook(state: GitHookState, repo: &Path) -> HookCheck {
    match state {
        GitHookState::NotGitRepo => HookCheck::skip(
            "git-trailer-hook",
            format!(
                "{} is not a git repository — nothing to check",
                repo.display()
            ),
        ),
        GitHookState::Missing { hooks_dir } => HookCheck::warn(
            "git-trailer-hook",
            format!(
                "no prepare-commit-msg hook at {} — commits made here won't \
                 carry a Kb-Session trailer",
                hooks_dir.display()
            ),
        )
        .with_fix(INSTALL_GIT_TRAILER_HINT),
        GitHookState::Other { path } => HookCheck::warn(
            "git-trailer-hook",
            format!(
                "a prepare-commit-msg hook exists at {} but doesn't look like \
                 the kb-memory dispatcher (no \"Kb-Session\"/trailer-logic.sh \
                 fingerprint) — Kb-Session trailers won't be stamped",
                path.display()
            ),
        )
        .with_fix(format!(
            "merge git-dispatch/trailer-logic.sh into that hook by hand, or \
             {INSTALL_GIT_TRAILER_HINT} if the existing hook isn't otherwise needed"
        )),
        GitHookState::Dispatcher { path } => HookCheck::pass(
            "git-trailer-hook",
            format!(
                "prepare-commit-msg is wired to the kb-memory dispatcher ({})",
                path.display()
            ),
        ),
    }
}

// ============================================= d/e/g) daemon-backed checks

#[derive(Debug, Clone, serde::Deserialize)]
struct KbLite {
    name: String,
    #[serde(default)]
    memory_scope: Option<String>,
    #[serde(default)]
    default_search_category: Option<String>,
}

async fn fetch_kbs(client: &reqwest::Client, base: &str) -> Result<Vec<KbLite>> {
    let url = format!("{base}/api/kbs");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    resp.json::<Vec<KbLite>>()
        .await
        .with_context(|| format!("parse JSON from {url}"))
}

fn decide_daemon_sessions_kb(kbs: &[KbLite]) -> HookCheck {
    if kbs.is_empty() {
        return HookCheck::warn(
            "daemon-sessions-kb",
            "daemon reachable but no kbs are configured",
        )
        .with_fix("kb add <path>");
    }
    if let Some(k) = kbs
        .iter()
        .find(|k| k.default_search_category.as_deref() == Some("memory-session"))
    {
        return HookCheck::pass(
            "daemon-sessions-kb",
            format!(
                "daemon reachable; sessions corpus = {} (default_search_category=memory-session)",
                k.name
            ),
        );
    }
    if let Some(k) = kbs.iter().find(|k| k.name.eq_ignore_ascii_case("sessions")) {
        return HookCheck::pass(
            "daemon-sessions-kb",
            format!(
                "daemon reachable; found a kb named '{}' (name heuristic — \
                 no default_search_category=memory-session configured to confirm it)",
                k.name
            ),
        );
    }
    let names: Vec<&str> = kbs.iter().map(|k| k.name.as_str()).collect();
    HookCheck::warn(
        "daemon-sessions-kb",
        format!(
            "daemon reachable ({} kb(s): {}) but none looks like a sessions \
             corpus (no default_search_category=memory-session, none named 'sessions')",
            kbs.len(),
            names.join(", ")
        ),
    )
    .with_fix("add `default_search_category = \"memory-session\"` to the [kb.<name>] stanza capturing transcripts")
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SessionLite {
    session_id: String,
    started_at: i64,
}
#[derive(Debug, Clone, serde::Deserialize)]
struct SessionsListLite {
    #[serde(default)]
    sessions: Vec<SessionLite>,
}
#[derive(Debug, Clone, serde::Deserialize)]
struct RecallsLite {
    #[serde(default)]
    recalls: Vec<serde_json::Value>,
}

/// A session started within this window counts as "recent" for the
/// parse-gap heuristic below — tight enough that a 0-recall reading is
/// actually informative (a long-dead session's ledger tells us nothing
/// about whether recall injection is working NOW).
const LEDGER_RECENCY_SECS: i64 = 6 * 3600;

async fn ledger_liveness_check(client: &reqwest::Client, base: &str) -> HookCheck {
    let url = format!("{base}/api/sessions?limit=1");
    let list: SessionsListLite = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                return HookCheck::warn(
                    "ledger-liveness",
                    format!("GET {url} returned an unparsable body: {e}"),
                )
            }
        },
        Ok(r) => {
            return HookCheck::warn("ledger-liveness", format!("GET {url}: HTTP {}", r.status()))
        }
        Err(e) => {
            return HookCheck::warn(
                "ledger-liveness",
                format!("kb daemon unreachable at {url}: {e}"),
            )
        }
    };
    let Some(latest) = list.sessions.first() else {
        return HookCheck::skip(
            "ledger-liveness",
            "no captured sessions yet (GET /api/sessions is empty) — nothing to check",
        );
    };
    let now = chrono::Utc::now().timestamp();
    let recalls_url = format!(
        "{base}/api/sessions/{}/recalls",
        crate::http::encode_path_segment(&latest.session_id)
    );
    let recall_count = match client.get(&recalls_url).send().await {
        Ok(r) if r.status().is_success() => match r.json::<RecallsLite>().await {
            Ok(v) => Some(v.recalls.len()),
            Err(_) => None,
        },
        _ => None,
    };
    decide_ledger_liveness(now - latest.started_at, recall_count)
}

fn decide_ledger_liveness(age_secs: i64, recall_count: Option<usize>) -> HookCheck {
    let recent = (0..=LEDGER_RECENCY_SECS).contains(&age_secs);
    match recall_count {
        None => HookCheck::warn(
            "ledger-liveness",
            "could not fetch the latest session's /recalls ledger (network or parse failure)",
        ),
        Some(0) if recent => HookCheck::warn(
            "ledger-liveness",
            format!(
                "the latest session (age {}) has 0 memory_recalls rows",
                fmt_age(age_secs)
            ),
        )
        .with_fix(
            "if memories were injected this may be the parse-gap failure — \
             grep daemon logs for \"memory recall injection parse gap\"",
        ),
        Some(0) => HookCheck::skip(
            "ledger-liveness",
            format!(
                "the latest session is {} old (outside the {} recency window) \
                 — 0 recalls is inconclusive this far out, skipping the parse-gap heuristic",
                fmt_age(age_secs),
                fmt_age(LEDGER_RECENCY_SECS)
            ),
        ),
        Some(n) => HookCheck::pass(
            "ledger-liveness",
            format!(
                "the latest session (age {}) recalled {n} memor{} — ledger alive",
                fmt_age(age_secs),
                if n == 1 { "y" } else { "ies" }
            ),
        ),
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CensusRowLite {
    /// Always sent by `GET /api/memory/census`; `Option` only so a shape
    /// change can't make the whole page unparsable for a diagnostic.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// CT-A1 (U3 parse-back) — the origin artifact a highlight-born memory
    /// was lifted from. Absent for the vast majority of the corpus (an
    /// ordinary `kb remember` records no provenance) AND absent entirely on
    /// a pre-CT-A1 daemon — both read as "nothing to check", never a WARN.
    #[serde(default)]
    source_kb: Option<String>,
    #[serde(default)]
    source_artifact: Option<String>,
}
#[derive(Debug, Clone, serde::Deserialize)]
struct CensusLite {
    #[serde(default)]
    rows: Vec<CensusRowLite>,
}

/// g) provenance-lint — cap on how many `session_id`s we probe against
/// `GET /api/sessions/{sid}` per the WORK ORDER's "cap 5 checks".
const ORPHAN_SAMPLE_CAP: usize = 5;

/// One corpus's census page, or `None` when it couldn't be fetched/parsed
/// at all (unreachable daemon, non-2xx, unexpected body). Shared by both
/// halves of the provenance lint (g) so they cost ONE census request per
/// memory corpus between them, and so "couldn't read the census" means the
/// same thing to both.
async fn fetch_census_page(
    client: &reqwest::Client,
    base: &str,
    kb: &str,
) -> Option<Vec<CensusRowLite>> {
    let url = format!(
        "{base}/api/memory/census?kb={}",
        crate::http::encode_path_segment(kb)
    );
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<CensusLite>().await.ok().map(|b| b.rows)
}

async fn session_exists(client: &reqwest::Client, base: &str, sid: &str) -> Option<bool> {
    let url = format!(
        "{base}/api/sessions/{}",
        crate::http::encode_path_segment(sid)
    );
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Some(true),
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => Some(false),
        _ => None,
    }
}

async fn orphan_origin_check(
    client: &reqwest::Client,
    base: &str,
    memory_kbs: &[&str],
) -> HookCheck {
    if memory_kbs.is_empty() {
        return HookCheck::skip(
            "provenance-orphan-origin",
            "no memory-scoped kb configured (no [kb.*] with memory_scope) — nothing to sample",
        );
    }
    let mut candidates: Vec<String> = Vec::new();
    'outer: for kb in memory_kbs {
        let Some(rows) = fetch_census_page(client, base, kb).await else {
            continue;
        };
        for row in rows {
            if let Some(sid) = row.session_id {
                candidates.push(sid);
                if candidates.len() >= ORPHAN_SAMPLE_CAP {
                    break 'outer;
                }
            }
        }
    }
    if candidates.is_empty() {
        return HookCheck::skip(
            "provenance-orphan-origin",
            "no memory rows in the sampled census page(s) carry a kb-session origin — nothing to check",
        );
    }
    let mut results: Vec<(String, bool)> = Vec::new();
    for sid in &candidates {
        if let Some(exists) = session_exists(client, base, sid).await {
            results.push((sid.clone(), exists));
        }
    }
    if results.is_empty() {
        return HookCheck::warn(
            "provenance-orphan-origin",
            format!(
                "sampled {} memory row(s) with a session_id, but every \
                 GET /api/sessions/{{id}} probe failed (network) — inconclusive",
                candidates.len()
            ),
        );
    }
    decide_orphan_origin(&results)
}

fn decide_orphan_origin(results: &[(String, bool)]) -> HookCheck {
    let total = results.len();
    let orphans: Vec<&str> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(id, _)| id.as_str())
        .collect();
    if orphans.is_empty() {
        HookCheck::pass(
            "provenance-orphan-origin",
            format!("{total}/{total} sampled memory origin session_id(s) resolve"),
        )
    } else {
        HookCheck::warn(
            "provenance-orphan-origin",
            format!(
                "{}/{total} sampled memory origin session_id(s) are orphaned \
                 (the session was hard-deleted or never existed): {}",
                orphans.len(),
                orphans.join(", ")
            ),
        )
    }
}

/// The other half of step (g) — dangling highlight origin: a memory whose
/// `kb-source-artifact` names an artifact that no longer resolves.
///
/// CT-C6 shipped this as a permanent SKIP on a stale premise ("the U3
/// provenance metas are write-only, so no read API exposes them"). CT-A1
/// parses all five back and surfaces `source_kb`/`source_artifact` on
/// `GET /api/memory/census`'s rows (and adds the reverse
/// `…/docs/{id}/memories-from`), so the check is implementable with the
/// EXISTING routes: sample census rows that carry an origin, then probe
/// `GET /api/kb/{source_kb}/docs/{artifact}`.
///
/// Cap on how many origins are probed — same "cap 5" discipline as
/// [`ORPHAN_SAMPLE_CAP`]; this is a smoke check, not an audit sweep
/// (`/kb-verify` owns sweeps).
const DANGLING_SAMPLE_CAP: usize = 5;

/// `Some(true)` = the artifact resolves (including via F3b's moves chain —
/// reqwest follows the 301 to the relocated id, so a MOVED artifact is not
/// dangling). `Some(false)` = a clean 404 on a kb this daemon does serve.
/// `None` = anything else (network, 5xx, auth) — inconclusive, and never
/// counted as dangling.
async fn artifact_resolves(
    client: &reqwest::Client,
    base: &str,
    kb: &str,
    id: &str,
) -> Option<bool> {
    let url = format!(
        "{base}/api/kb/{}/docs/{}",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id)
    );
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Some(true),
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => Some(false),
        _ => None,
    }
}

async fn dangling_highlight_check(
    client: &reqwest::Client,
    base: &str,
    memory_kbs: &[&str],
    served_kbs: &[&str],
) -> HookCheck {
    if memory_kbs.is_empty() {
        return HookCheck::skip(
            "provenance-dangling-highlight",
            "no memory-scoped kb configured (no [kb.*] with memory_scope) — nothing to sample",
        );
    }
    // (memory id, source kb, source artifact id) for rows that carry a
    // highlight origin THIS daemon can resolve; rows naming a kb it doesn't
    // serve are counted separately (they may well live on another daemon —
    // "can't check from here" is not "dangling").
    let mut probes: Vec<(String, String, String)> = Vec::new();
    let mut unprobeable = 0usize;
    'outer: for kb in memory_kbs {
        let Some(rows) = fetch_census_page(client, base, kb).await else {
            continue;
        };
        for row in rows {
            let Some(artifact) = row.source_artifact.filter(|s| !s.is_empty()) else {
                continue;
            };
            let mem_id = row.id.unwrap_or_else(|| "?".to_string());
            match row.source_kb.filter(|s| !s.is_empty()) {
                Some(src_kb)
                    if served_kbs
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(src_kb.as_str())) =>
                {
                    probes.push((mem_id, src_kb, artifact));
                }
                _ => unprobeable += 1,
            }
            if probes.len() >= DANGLING_SAMPLE_CAP {
                break 'outer;
            }
        }
    }
    let mut results: Vec<(String, bool)> = Vec::new();
    let mut probe_failures = 0usize;
    for (mem_id, src_kb, artifact) in &probes {
        match artifact_resolves(client, base, src_kb, artifact).await {
            Some(ok) => results.push((format!("{mem_id} → {src_kb}/{artifact}"), ok)),
            None => probe_failures += 1,
        }
    }
    decide_dangling_highlight(&results, unprobeable, probe_failures)
}

/// Pure verdict for [`dangling_highlight_check`]. Every "we couldn't look"
/// branch is a SKIP — an unreadable census, an origin on a kb this daemon
/// doesn't serve, and an inconclusive probe are all silence, never a WARN
/// (a false "dangling" would send an operator hunting a memory that is
/// perfectly intact somewhere else).
fn decide_dangling_highlight(
    results: &[(String, bool)],
    unprobeable: usize,
    probe_failures: usize,
) -> HookCheck {
    let aside = |extra: &str| -> String {
        let mut parts: Vec<String> = Vec::new();
        if unprobeable > 0 {
            parts.push(format!(
                "{unprobeable} more name a source kb this daemon doesn't serve (or none at all) — not checked"
            ));
        }
        if probe_failures > 0 {
            parts.push(format!(
                "{probe_failures} probe(s) were inconclusive (network/HTTP error), NOT counted as dangling"
            ));
        }
        if parts.is_empty() {
            extra.to_string()
        } else if extra.is_empty() {
            parts.join("; ")
        } else {
            format!("{extra}; {}", parts.join("; "))
        }
    };
    if results.is_empty() {
        let detail = if probe_failures > 0 || unprobeable > 0 {
            aside("no highlight origin could actually be probed")
        } else {
            "no memory row in the sampled census page(s) carries a kb-source-artifact \
             origin (an ordinary `kb remember` records none, and a pre-CT-A1 daemon \
             doesn't surface them) — nothing to check"
                .to_string()
        };
        return HookCheck::skip("provenance-dangling-highlight", detail);
    }
    let total = results.len();
    let dangling: Vec<&str> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(label, _)| label.as_str())
        .collect();
    if dangling.is_empty() {
        HookCheck::pass(
            "provenance-dangling-highlight",
            aside(&format!(
                "{total}/{total} sampled highlight origin artifact(s) resolve"
            )),
        )
    } else {
        HookCheck::warn(
            "provenance-dangling-highlight",
            aside(&format!(
                "{}/{total} sampled memory highlight origin(s) no longer resolve \
                 (the artifact was deleted, or moved outside the relocate engine — \
                 invariant #27/F3): {}",
                dangling.len(),
                dangling.join(", ")
            )),
        )
        .with_fix(
            "open the memory and re-anchor or drop its kb-source-artifact meta; if the \
             artifact was moved by hand, `kb mv` is the only path that keeps ids resolvable",
        )
    }
}

// ================================================== f) kb-code why-hook

/// Same case-insensitive kill-switch values `kb-code-why.sh` matches
/// (`case "$kill_switch" in off | 1 | true | yes)`).
fn why_hook_disabled(raw: Option<&str>) -> bool {
    match raw.map(|s| s.to_ascii_lowercase()) {
        Some(v) => matches!(v.as_str(), "off" | "1" | "true" | "yes"),
        None => false,
    }
}

fn claude_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("settings.json"))
}

/// Best-effort: any `"kb-code@…": true` key in the global
/// `enabledPlugins` map. A project-level `.claude/settings.json` could
/// still enable it even when this reads `false` — noted in the WARN text
/// rather than silently assumed.
fn kb_code_plugin_enabled(settings: &serde_json::Value) -> bool {
    settings
        .get("enabledPlugins")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .any(|(k, v)| k.starts_with("kb-code@") && v.as_bool() == Some(true))
        })
        .unwrap_or(false)
}

fn decide_kb_code_why_hook(raw: Option<&str>, plugin_enabled: Option<bool>) -> HookCheck {
    if why_hook_disabled(raw) {
        return HookCheck::warn(
            "kb-code-why-hook",
            format!(
                "KB_CODE_WHY_HOOK={} disables the why-hook kill switch in this process's env",
                raw.unwrap_or_default()
            ),
        )
        .with_fix("unset KB_CODE_WHY_HOOK (or set it to anything other than off/1/true/yes) if this wasn't intentional");
    }
    match plugin_enabled {
        Some(true) => HookCheck::pass(
            "kb-code-why-hook",
            "kill switch not set in this process's env; kb-code is enabled in ~/.claude/settings.json",
        ),
        Some(false) => HookCheck::warn(
            "kb-code-why-hook",
            "kill switch not set, but kb-code isn't in ~/.claude/settings.json's \
             enabledPlugins (global config only — a project-level settings.json could still enable it)",
        )
        .with_fix("enable the kb-code plugin (`/plugin`) or wire hooks.json manually per plugins/kb-code/README.md"),
        None => HookCheck::skip(
            "kb-code-why-hook",
            "kill switch not set in this process's env; couldn't read/parse \
             ~/.claude/settings.json to check plugin enablement (best-effort — \
             installed-plugin state isn't a stable public format)",
        ),
    }
}

fn kb_code_why_hook_check() -> HookCheck {
    let raw = std::env::var("KB_CODE_WHY_HOOK").ok();
    let plugin_enabled = claude_settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| kb_code_plugin_enabled(&v));
    decide_kb_code_why_hook(raw.as_deref(), plugin_enabled)
}

// ============================================ h) slate marker GC (D30, v0.42)
//
// `kb slate open`/`delta` leave two client-side markers per session id
// (`slate-cursor-<sid>`, `slate-topic-<sid>`, `commands::slate`'s module
// doc point 3) that never expire on their own — a long-lived box
// accumulates one pair per session forever. D30 closes the design's open
// question by putting the GC here, beside every other `--hooks` check:
// a lint by default, `--fix` (scoped to THIS check only) removes them.

/// 30 days — D30's marker-GC threshold.
const SLATE_MARKER_MAX_AGE_SECS: i64 = 30 * 86_400;

/// A stale slate marker file plus how old it is.
#[derive(Debug, Clone)]
struct StaleSlateMarker {
    path: PathBuf,
    age_secs: i64,
}

/// `commands::slate`'s own `cursor_file`/`topic_file` naming, mirrored by
/// name rather than imported — this is a read-only filesystem scan in a
/// different module, not a shared contract the two sides must agree on
/// beyond the literal prefix.
fn is_slate_marker_name(name: &str) -> bool {
    name.starts_with("slate-cursor-") || name.starts_with("slate-topic-")
}

/// Scan `kb_dir` (`~/.cache/kb`) for slate cursor/topic markers whose
/// mtime is at least [`SLATE_MARKER_MAX_AGE_SECS`] old. PURE over an
/// already-resolved directory and `now`, so the selection is unit-tested
/// with a tempdir and an explicit clock rather than the real one.
/// Best-effort: an unreadable directory, a non-file entry, or a file whose
/// mtime can't be read is skipped rather than erroring — this is a lint,
/// not a required check. Sorted by path for a stable report.
fn find_stale_slate_markers(kb_dir: &Path, now: i64) -> Vec<StaleSlateMarker> {
    let Ok(entries) = std::fs::read_dir(kb_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_slate_marker_name(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(age_secs) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| now - d.as_secs() as i64)
        else {
            continue;
        };
        if age_secs >= SLATE_MARKER_MAX_AGE_SECS {
            out.push(StaleSlateMarker { path, age_secs });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// `--fix`: remove every marker [`find_stale_slate_markers`] flags.
/// Best-effort per file (an unremovable file — permissions, a race with
/// another process — is skipped, not a hard error: this is GC, not a
/// transaction) and returns the count actually removed.
fn gc_stale_slate_markers(kb_dir: &Path, now: i64) -> usize {
    find_stale_slate_markers(kb_dir, now)
        .iter()
        .filter(|m| std::fs::remove_file(&m.path).is_ok())
        .count()
}

/// The `slate-marker-gc` check. `removed` is `Some(n)` only when `--fix`
/// ran (n may be 0); the caller always re-scans AFTER any removal, so a
/// clean result reads the same whether nothing was ever stale or `--fix`
/// just cleaned it up — the difference is which sentence is printed.
fn slate_marker_gc_check(cache_dir: Option<&Path>, now: i64, removed: Option<usize>) -> HookCheck {
    let Some(cache_dir) = cache_dir else {
        return HookCheck::skip(
            "slate-marker-gc",
            "could not resolve a cache dir — neither XDG_CACHE_HOME nor HOME is set",
        );
    };
    let kb_dir = cache_dir.join("kb");
    let stale = find_stale_slate_markers(&kb_dir, now);
    if let Some(n) = removed.filter(|n| *n > 0) {
        return HookCheck::pass(
            "slate-marker-gc",
            format!("removed {n} stale slate-cursor/slate-topic marker(s) (--fix)"),
        );
    }
    if stale.is_empty() {
        return HookCheck::pass(
            "slate-marker-gc",
            "no slate-cursor/slate-topic marker older than 30d",
        );
    }
    let mut detail = format!(
        "{} stale slate-cursor/slate-topic marker(s) older than 30d in {}:",
        stale.len(),
        kb_dir.display()
    );
    for m in stale.iter().take(5) {
        detail.push_str(&format!(
            "\n      {} ({})",
            m.path.display(),
            fmt_age(m.age_secs)
        ));
    }
    if stale.len() > 5 {
        detail.push_str(&format!("\n      … and {} more", stale.len() - 5));
    }
    HookCheck::warn("slate-marker-gc", detail).with_fix("kb doctor --hooks --fix removes them")
}

// ==================================================================== run

fn git_toplevel(repo: &Path) -> Option<PathBuf> {
    run_git(repo, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

pub async fn hooks(
    repo: Option<PathBuf>,
    daemon: Option<&str>,
    json_out: bool,
    bearer: Option<&str>,
    fix: bool,
) -> Result<()> {
    let repo_path = match repo {
        Some(p) => p,
        None => std::env::current_dir().context("resolve current directory")?,
    };
    let now = chrono::Utc::now().timestamp();
    let cache_dir = resolve_cache_dir();
    let mut checks: Vec<HookCheck> = Vec::new();

    // a) plain session marker.
    checks.push(plain_marker_check(cache_dir.as_deref(), now));

    // b) repo-keyed marker.
    let slug_root = git_toplevel(&repo_path).unwrap_or_else(|| repo_path.clone());
    let slug = slugify_repo_path(&slug_root.to_string_lossy());
    checks.push(repo_marker_check(cache_dir.as_deref(), &slug, now));

    // c) git prepare-commit-msg trailer hook.
    checks.push(decide_git_trailer_hook(
        detect_git_trailer_hook(&repo_path),
        &repo_path,
    ));

    // d/e/g) daemon-backed checks.
    let base = daemon
        .map(str::to_string)
        .unwrap_or_else(|| "http://127.0.0.1:4000".to_string());
    let client = crate::http::client_with_timeout_and_bearer(8, bearer)?;

    let kbs = fetch_kbs(&client, &base).await;
    match &kbs {
        Ok(list) => checks.push(decide_daemon_sessions_kb(list)),
        Err(e) => checks.push(
            HookCheck::warn(
                "daemon-sessions-kb",
                format!("kb daemon unreachable at {base}/api/kbs: {e}"),
            )
            .with_fix("start it (`kb daemon`) or pass --daemon <url>"),
        ),
    }

    checks.push(ledger_liveness_check(&client, &base).await);

    match &kbs {
        Ok(list) => {
            let memory_kbs: Vec<&str> = list
                .iter()
                .filter(|k| k.memory_scope.is_some())
                .map(|k| k.name.as_str())
                .collect();
            let served_kbs: Vec<&str> = list.iter().map(|k| k.name.as_str()).collect();
            checks.push(orphan_origin_check(&client, &base, &memory_kbs).await);
            // CT-C6 follow-up — the other half of (g), now implementable:
            // CT-A1 parses the U3 provenance metas back and surfaces
            // `source_kb`/`source_artifact` on the census rows this scan
            // already reads.
            checks.push(dangling_highlight_check(&client, &base, &memory_kbs, &served_kbs).await);
        }
        Err(_) => {
            checks.push(HookCheck::skip(
                "provenance-orphan-origin",
                "kb daemon unreachable — cannot scan GET /api/memory/census",
            ));
            checks.push(HookCheck::skip(
                "provenance-dangling-highlight",
                "kb daemon unreachable — cannot scan GET /api/memory/census",
            ));
        }
    }

    // f) kb-code why-hook.
    checks.push(kb_code_why_hook_check());

    // h) slate marker GC (D30, v0.42) — the ONLY check `--fix` acts on.
    let removed = fix.then(|| {
        let kb_dir = cache_dir
            .as_deref()
            .map(|d| d.join("kb"))
            .unwrap_or_default();
        gc_stale_slate_markers(&kb_dir, now)
    });
    checks.push(slate_marker_gc_check(cache_dir.as_deref(), now, removed));

    if json_out {
        println!("{}", serde_json::to_string_pretty(&to_json(&checks))?);
    } else {
        print!("{}", render_human(&checks));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- slugify_repo_path ------------------------------------------------

    #[test]
    fn slugify_matches_bash_kb_slugify_for_ascii_paths() {
        assert_eq!(
            slugify_repo_path("/home/user/project/kb"),
            "home-user-project-kb"
        );
        assert_eq!(
            slugify_repo_path("/home/user/.claude/worktrees/wf_c2af9297-edc-4"),
            "home-user-claude-worktrees-wf-c2af9297-edc-4"
        );
        assert_eq!(
            slugify_repo_path("/home/user/My Project_v2.0"),
            "home-user-my-project-v2-0"
        );
        assert_eq!(slugify_repo_path("/a__b--c"), "a-b-c");
    }

    // --- parse_repo_marker / marker_is_fresh -------------------------------

    #[test]
    fn parse_repo_marker_reads_sid_and_timestamp() {
        assert_eq!(
            parse_repo_marker("sess-abc\n1700000000\n"),
            Some(("sess-abc".to_string(), 1_700_000_000))
        );
    }

    #[test]
    fn parse_repo_marker_bad_timestamp_falls_back_to_zero() {
        assert_eq!(
            parse_repo_marker("sess-abc\nnot-a-number\n"),
            Some(("sess-abc".to_string(), 0))
        );
    }

    #[test]
    fn parse_repo_marker_missing_second_line_falls_back_to_zero() {
        assert_eq!(
            parse_repo_marker("sess-abc\n"),
            Some(("sess-abc".to_string(), 0))
        );
    }

    #[test]
    fn parse_repo_marker_blank_sid_is_none() {
        assert_eq!(parse_repo_marker("\n1700000000\n"), None);
        assert_eq!(parse_repo_marker(""), None);
    }

    #[test]
    fn marker_freshness_boundaries() {
        let now = 1_700_100_000;
        assert!(marker_is_fresh(now, now, 2400)); // age 0
        assert!(marker_is_fresh(now, now - 2400, 2400)); // age == max, inclusive
        assert!(!marker_is_fresh(now, now - 2401, 2400)); // one second stale
        assert!(!marker_is_fresh(now, now + 1, 2400)); // future timestamp, clock skew
    }

    // --- plain_marker_check (file fixtures) --------------------------------

    #[test]
    fn plain_marker_check_pass_when_present_and_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("current-session"), "sess-xyz\n").unwrap();
        let c = plain_marker_check(Some(tmp.path()), chrono::Utc::now().timestamp());
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("present"));
    }

    #[test]
    fn plain_marker_check_warns_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let c = plain_marker_check(Some(tmp.path()), chrono::Utc::now().timestamp());
        assert_eq!(c.status, CheckStatus::Warn);
    }

    #[test]
    fn plain_marker_check_warns_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("current-session"), "\n  \n").unwrap();
        let c = plain_marker_check(Some(tmp.path()), chrono::Utc::now().timestamp());
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("empty"));
    }

    #[test]
    fn plain_marker_check_skips_when_no_cache_dir() {
        let c = plain_marker_check(None, 0);
        assert_eq!(c.status, CheckStatus::Skip);
    }

    // --- repo_marker_check (file fixtures) ----------------------------------

    #[test]
    fn repo_marker_check_pass_when_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        let now = 1_700_100_000;
        std::fs::write(
            dir.join("current-session-repo-myslug"),
            format!("sess-1\n{}\n", now - 100),
        )
        .unwrap();
        let c = repo_marker_check(Some(tmp.path()), "myslug", now);
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn repo_marker_check_warns_when_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        let now = 1_700_100_000;
        std::fs::write(
            dir.join("current-session-repo-myslug"),
            format!("sess-1\n{}\n", now - 3000),
        )
        .unwrap();
        let c = repo_marker_check(Some(tmp.path()), "myslug", now);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("stale"));
    }

    #[test]
    fn repo_marker_check_warns_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let c = repo_marker_check(Some(tmp.path()), "myslug", 1_700_100_000);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("not found"));
    }

    // --- git trailer hook (temp git repos, mirrors test-git-trailer.sh) ------

    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .expect("git available");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    #[test]
    fn git_trailer_hook_skips_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = detect_git_trailer_hook(tmp.path());
        let check = decide_git_trailer_hook(state, tmp.path());
        assert_eq!(check.status, CheckStatus::Skip);
    }

    #[test]
    fn git_trailer_hook_warns_when_no_hookspath_set() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let state = detect_git_trailer_hook(tmp.path());
        let check = decide_git_trailer_hook(state, tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.fix.is_some());
    }

    #[test]
    fn git_trailer_hook_passes_when_dispatcher_installed() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let hooks_dir = tmp.path().join("fake-git-dispatch");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("prepare-commit-msg"),
            "#!/bin/sh\n# runs trailer-logic.sh (the Kb-Session commit trailer)\n",
        )
        .unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "--local", "core.hooksPath"])
            .arg(&hooks_dir)
            .status()
            .unwrap();
        assert!(status.success());
        let state = detect_git_trailer_hook(tmp.path());
        let check = decide_git_trailer_hook(state, tmp.path());
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[test]
    fn git_trailer_hook_warns_when_other_hook_installed() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let hooks_dir = tmp.path().join("husky-style");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("prepare-commit-msg"),
            "#!/bin/sh\necho unrelated hook\n",
        )
        .unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "--local", "core.hooksPath"])
            .arg(&hooks_dir)
            .status()
            .unwrap();
        assert!(status.success());
        let state = detect_git_trailer_hook(tmp.path());
        let check = decide_git_trailer_hook(state, tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.detail.contains("doesn't look like"));
    }

    // --- decide_daemon_sessions_kb -----------------------------------------

    #[test]
    fn decide_daemon_sessions_kb_finds_via_category() {
        let kbs = vec![KbLite {
            name: "sessions-corpus".into(),
            memory_scope: None,
            default_search_category: Some("memory-session".into()),
        }];
        let c = decide_daemon_sessions_kb(&kbs);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("sessions-corpus"));
    }

    #[test]
    fn decide_daemon_sessions_kb_finds_via_name_heuristic() {
        let kbs = vec![KbLite {
            name: "Sessions".into(),
            memory_scope: None,
            default_search_category: None,
        }];
        let c = decide_daemon_sessions_kb(&kbs);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("heuristic"));
    }

    #[test]
    fn decide_daemon_sessions_kb_warns_when_no_candidate() {
        let kbs = vec![KbLite {
            name: "notes".into(),
            memory_scope: None,
            default_search_category: None,
        }];
        let c = decide_daemon_sessions_kb(&kbs);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.fix.is_some());
    }

    #[test]
    fn decide_daemon_sessions_kb_warns_when_empty() {
        let c = decide_daemon_sessions_kb(&[]);
        assert_eq!(c.status, CheckStatus::Warn);
    }

    // --- decide_ledger_liveness ---------------------------------------------

    #[test]
    fn decide_ledger_liveness_warns_on_recent_zero_recalls() {
        let c = decide_ledger_liveness(60, Some(0));
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.fix.unwrap().contains("memory recall injection parse gap"));
    }

    #[test]
    fn decide_ledger_liveness_skips_on_stale_zero_recalls() {
        let c = decide_ledger_liveness(LEDGER_RECENCY_SECS + 1, Some(0));
        assert_eq!(c.status, CheckStatus::Skip);
    }

    #[test]
    fn decide_ledger_liveness_passes_on_nonzero_recalls() {
        let c = decide_ledger_liveness(60, Some(3));
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("3 memories"));
    }

    #[test]
    fn decide_ledger_liveness_warns_when_recalls_unfetchable() {
        let c = decide_ledger_liveness(60, None);
        assert_eq!(c.status, CheckStatus::Warn);
    }

    // --- decide_orphan_origin ------------------------------------------------

    #[test]
    fn decide_orphan_origin_passes_when_all_resolve() {
        let c = decide_orphan_origin(&[("s1".into(), true), ("s2".into(), true)]);
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn decide_orphan_origin_warns_on_a_404() {
        let c = decide_orphan_origin(&[("s1".into(), true), ("s2".into(), false)]);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("s2"));
    }

    // --- decide_dangling_highlight (CT-C6 follow-up) --------------------------

    #[test]
    fn decide_dangling_highlight_passes_when_every_origin_resolves() {
        let c = decide_dangling_highlight(
            &[("m1 → docs/a1".into(), true), ("m2 → docs/a2".into(), true)],
            0,
            0,
        );
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("2/2"));
    }

    #[test]
    fn decide_dangling_highlight_warns_and_names_the_dangling_origin() {
        let c = decide_dangling_highlight(
            &[
                ("m1 → docs/a1".into(), true),
                ("m2 → docs/gone".into(), false),
            ],
            0,
            0,
        );
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("m2 → docs/gone"));
        assert!(!c.detail.contains("m1"), "only the broken origin is named");
        assert!(c.fix.is_some());
    }

    /// A daemon that doesn't serve the provenance fields (pre-CT-A1) — or a
    /// corpus where no memory was born from a highlight — must SKIP, never
    /// invent a clean PASS or a WARN.
    #[test]
    fn decide_dangling_highlight_skips_when_nothing_carries_an_origin() {
        let c = decide_dangling_highlight(&[], 0, 0);
        assert_eq!(c.status, CheckStatus::Skip);
        assert!(c.detail.contains("kb-source-artifact"));
    }

    /// Every probe failed (network/5xx) — inconclusive is SKIP, never a
    /// false "dangling" WARN.
    #[test]
    fn decide_dangling_highlight_skips_when_every_probe_was_inconclusive() {
        let c = decide_dangling_highlight(&[], 0, 3);
        assert_eq!(c.status, CheckStatus::Skip);
        assert!(c.detail.contains("inconclusive"));
        assert!(c.detail.contains('3'));
    }

    /// An origin naming a kb this daemon doesn't serve is unreachable from
    /// HERE — reported as an aside on the surviving verdict, never counted
    /// as dangling.
    #[test]
    fn decide_dangling_highlight_reports_unprobeable_origins_as_an_aside() {
        let c = decide_dangling_highlight(&[("m1 → docs/a1".into(), true)], 2, 1);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("1/1"));
        assert!(c.detail.contains("2 more"));
        assert!(c.detail.contains("1 probe(s) were inconclusive"));

        // With nothing probed at all it degrades to SKIP, not PASS.
        let c = decide_dangling_highlight(&[], 4, 0);
        assert_eq!(c.status, CheckStatus::Skip);
        assert!(c.detail.contains("4 more"));
    }

    #[test]
    fn census_row_lite_parses_the_ct_a1_provenance_fields_and_tolerates_their_absence() {
        let with = serde_json::json!({
            "id": "m1", "session_id": "s1",
            "source_kb": "docs", "source_artifact": "a1"
        });
        let row: CensusRowLite = serde_json::from_value(with).unwrap();
        assert_eq!(row.source_kb.as_deref(), Some("docs"));
        assert_eq!(row.source_artifact.as_deref(), Some("a1"));
        // Pre-CT-A1 shape: the fields simply aren't there.
        let without = serde_json::json!({ "id": "m1", "session_id": "s1" });
        let row: CensusRowLite = serde_json::from_value(without).unwrap();
        assert!(row.source_kb.is_none());
        assert!(row.source_artifact.is_none());
        assert_eq!(row.id.as_deref(), Some("m1"));
    }

    // --- kb-code why-hook -----------------------------------------------------

    #[test]
    fn why_hook_disabled_matches_bash_case_values() {
        assert!(why_hook_disabled(Some("off")));
        assert!(why_hook_disabled(Some("OFF")));
        assert!(why_hook_disabled(Some("1")));
        assert!(why_hook_disabled(Some("true")));
        assert!(why_hook_disabled(Some("yes")));
        assert!(!why_hook_disabled(Some("0")));
        assert!(!why_hook_disabled(Some("")));
        assert!(!why_hook_disabled(None));
    }

    #[test]
    fn decide_kb_code_why_hook_warns_when_kill_switch_set() {
        let c = decide_kb_code_why_hook(Some("off"), Some(true));
        assert_eq!(c.status, CheckStatus::Warn);
    }

    #[test]
    fn decide_kb_code_why_hook_pass_when_enabled() {
        let c = decide_kb_code_why_hook(None, Some(true));
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn decide_kb_code_why_hook_warns_when_plugin_disabled() {
        let c = decide_kb_code_why_hook(None, Some(false));
        assert_eq!(c.status, CheckStatus::Warn);
    }

    #[test]
    fn decide_kb_code_why_hook_skips_when_undetectable() {
        let c = decide_kb_code_why_hook(None, None);
        assert_eq!(c.status, CheckStatus::Skip);
    }

    #[test]
    fn kb_code_plugin_enabled_reads_enabled_plugins_map() {
        let v =
            serde_json::json!({"enabledPlugins": {"kb-code@kb-plugins": true, "other@x": true}});
        assert!(kb_code_plugin_enabled(&v));
        let v2 = serde_json::json!({"enabledPlugins": {"kb-code@kb-plugins": false}});
        assert!(!kb_code_plugin_enabled(&v2));
        let v3 = serde_json::json!({"enabledPlugins": {}});
        assert!(!kb_code_plugin_enabled(&v3));
    }

    // --- render/json ------------------------------------------------------

    #[test]
    fn render_human_includes_glyphs_and_summary() {
        let checks = vec![
            HookCheck::pass("a", "ok"),
            HookCheck::warn("b", "meh").with_fix("do x"),
            HookCheck::skip("c", "dunno"),
        ];
        let s = render_human(&checks);
        assert!(s.contains('✓'));
        assert!(s.contains('⚠'));
        assert!(s.contains('○'));
        assert!(s.contains("fix: do x"));
        assert!(s.contains("1 pass, 1 warn, 1 skip (3 checks total)"));
        assert!(s.contains("harness scope"));
    }

    #[test]
    fn to_json_shape_has_id_status_detail_and_optional_fix() {
        let checks = vec![
            HookCheck::pass("a", "ok"),
            HookCheck::warn("b", "meh").with_fix("do x"),
        ];
        let v = to_json(&checks);
        let rows = v["checks"].as_array().unwrap();
        assert_eq!(rows[0]["id"], "a");
        assert_eq!(rows[0]["status"], "pass");
        assert_eq!(rows[0]["detail"], "ok");
        assert!(rows[0].get("fix").is_none());
        assert_eq!(rows[1]["fix"], "do x");
        assert!(v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("Claude Code only")));
    }

    // --- D30 slate marker GC ----------------------------------------------

    /// Backdate a file's mtime by `age_secs` (std only — no `filetime`
    /// dependency needed: `File::set_modified` has been stable since
    /// Rust 1.75, well under this workspace's 1.86 MSRV).
    fn touch_with_age(path: &Path, contents: &str, age_secs: u64) {
        std::fs::write(path, contents).unwrap();
        let ts = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::File::open(path).unwrap().set_modified(ts).unwrap();
    }

    const THIRTY_ONE_DAYS_SECS: u64 = 31 * 86_400;

    #[test]
    fn is_slate_marker_name_matches_only_the_two_slate_prefixes() {
        assert!(is_slate_marker_name("slate-cursor-sess-a"));
        assert!(is_slate_marker_name("slate-topic-sess-a"));
        assert!(!is_slate_marker_name("current-session"));
        assert!(!is_slate_marker_name("current-session-repo-kb"));
        // A mid-write `.tmp` sibling (write_marker's atomic-rename source)
        // still starts with the prefix, so it is swept too once stale —
        // that is the intended behaviour, not an edge case to special-case.
        assert!(is_slate_marker_name("slate-cursor-sess-a.tmp"));
    }

    #[test]
    fn find_stale_slate_markers_selects_old_slate_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = chrono::Utc::now().timestamp();

        // Fresh slate marker — NOT selected.
        touch_with_age(&dir.join("slate-cursor-fresh"), "10\n", 0);
        // Old slate markers, both prefixes — selected.
        touch_with_age(&dir.join("slate-cursor-old"), "5\n", THIRTY_ONE_DAYS_SECS);
        touch_with_age(&dir.join("slate-topic-old"), "v7\n", THIRTY_ONE_DAYS_SECS);
        // Old, but not a slate marker name — NOT selected.
        touch_with_age(
            &dir.join("current-session-repo-kb"),
            "sess-x\n1\n",
            THIRTY_ONE_DAYS_SECS,
        );
        // Old directory that happens to match the name — NOT selected
        // (is_file() gate).
        std::fs::create_dir(dir.join("slate-cursor-adir")).unwrap();

        let stale = find_stale_slate_markers(dir, now);
        let names: Vec<String> = stale
            .iter()
            .map(|m| m.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["slate-cursor-old", "slate-topic-old"]);
        assert!(stale
            .iter()
            .all(|m| m.age_secs >= SLATE_MARKER_MAX_AGE_SECS));
    }

    #[test]
    fn find_stale_slate_markers_is_empty_on_a_missing_or_fresh_dir() {
        let now = chrono::Utc::now().timestamp();
        assert!(find_stale_slate_markers(Path::new("/does/not/exist"), now).is_empty());

        let tmp = tempfile::tempdir().unwrap();
        touch_with_age(&tmp.path().join("slate-cursor-fresh"), "1\n", 0);
        assert!(find_stale_slate_markers(tmp.path(), now).is_empty());
    }

    #[test]
    fn gc_stale_slate_markers_removes_only_the_stale_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = chrono::Utc::now().timestamp();
        let fresh = dir.join("slate-cursor-fresh");
        let old = dir.join("slate-topic-old");
        touch_with_age(&fresh, "1\n", 0);
        touch_with_age(&old, "v7\n", THIRTY_ONE_DAYS_SECS);

        let removed = gc_stale_slate_markers(dir, now);
        assert_eq!(removed, 1);
        assert!(fresh.exists(), "a fresh marker must survive the GC");
        assert!(!old.exists(), "a stale marker must be removed by --fix");

        // Idempotent: nothing left to remove on a second pass.
        assert_eq!(gc_stale_slate_markers(dir, now), 0);
    }

    #[test]
    fn slate_marker_gc_check_passes_on_a_clean_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let c = slate_marker_gc_check(Some(tmp.path()), now, None);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("no slate-cursor"));
    }

    #[test]
    fn slate_marker_gc_check_warns_and_names_the_fix_when_stale_markers_exist() {
        let tmp = tempfile::tempdir().unwrap();
        // `slate_marker_gc_check` joins "kb" onto the cache dir itself
        // (the same `<XDG_CACHE_HOME>/kb` every other check here uses).
        let kb_dir = tmp.path().join("kb");
        std::fs::create_dir_all(&kb_dir).unwrap();
        touch_with_age(
            &kb_dir.join("slate-cursor-old"),
            "5\n",
            THIRTY_ONE_DAYS_SECS,
        );
        let now = chrono::Utc::now().timestamp();
        let c = slate_marker_gc_check(Some(tmp.path()), now, None);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("1 stale"), "{}", c.detail);
        assert_eq!(
            c.fix.as_deref(),
            Some("kb doctor --hooks --fix removes them")
        );
    }

    #[test]
    fn slate_marker_gc_check_reports_the_removed_count_after_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        // After --fix already removed 2, the re-scan is clean; `removed`
        // is what makes the message say "removed" rather than "no markers".
        let c = slate_marker_gc_check(Some(tmp.path()), now, Some(2));
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("removed 2"), "{}", c.detail);
    }

    #[test]
    fn slate_marker_gc_check_skips_with_no_resolvable_cache_dir() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(
            slate_marker_gc_check(None, now, None).status,
            CheckStatus::Skip
        );
    }
}
