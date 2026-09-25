//! RS-U10a — the agent-facing review CLI, part 1 (README §13, D15).
//!
//! # The contract every verb in this module keeps
//!
//! * **Output.** `--json` prints ONE versioned envelope on stdout
//!   ([`envelope::ok_value`]: `{schema, ok, data, warnings, degraded,
//!   empty_reason, next}`); `schema` names the DATA's shape (the daemon's
//!   own `kbc-review-*/1` string where one exists). Diagnostics and
//!   progress go to stderr, never stdout.
//! * **Errors** are typed ([`envelope::err_value`], on stderr):
//!   `{ok:false, error:{code:"urn:kb:errors:<slug>", message, hint, next}}`
//!   — `candidates` is added when an address was ambiguous. `next` is a
//!   list of argv vectors, never shell strings.
//! * **Exit codes** — the shipped table in `envelope.rs` (NOT renumbered):
//!   0 ok · 1 generic · 2 usage (a malformed address, an ambiguous `pr:<N>`,
//!   a daemon 400) · 3 conflict (409/503, and a `verify` that FAILS — the
//!   request was fine, the state refuses it, same slot `review lint` uses)
//!   · 4 refused (401/403, incl. the secret denylist) · 5 unreachable · 6
//!   upstream · 7 partial · 8 not found (no such review / patchset / PR
//!   binding / file on that side).
//! * **Addressing** is uniform: `<id>`, `<id>/ps<n>`, `pr:<N>` (and
//!   `pr:<N>/ps<n>`). `pr:<N>` resolves through `GET /api/reviews/find` —
//!   inferred when exactly one repo has a review bound to PR N, else
//!   `--repo`; ambiguity is a typed error listing the candidates. One
//!   parser, [`parse_review_ref`].
//! * **No client-side guesswork on daemon work.** `diff`/`log`/`cat` are
//!   computed BY THE DAEMON from the patchset's own base/tip
//!   (`crate::review_views` on the server) — never `git -C <clone>` here, so
//!   they keep working once review refs live only in the internal store.
//!
//! Verbs here: `review find`, `diff`, `log`, `cat`, `verify`, and the
//! `compose --slugify` helper ([`slugify_findings`]). The start-pr /
//! snapshot envelopes ([`start_envelope`], [`snapshot_envelope`]) live here
//! too; main.rs calls them.
//!
//! # Extension points for RS-U10b (`review sync`, `review status`)
//!
//! Both need the RS-U6 base model. They plug in here without new plumbing:
//! [`resolve`] already turns any address into `(id, ps)`, [`AgentError`] +
//! [`envelope::ok_value`] are the output contract, [`base_block`] is the
//! one place the `base{mode,branch,source,state,merge_base}` object is
//! assembled (U6 fills `mode`/`state` from the new columns), and
//! `poll_review_job` in main.rs is the generalized `--wait[=SECS]` poller
//! over `GET /api/reviews/jobs/{id}` that a daemon-side `sync` job reuses.

use crate::envelope::{self, NextArgv};
use clap::Args;
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_DAEMON: &str = "http://127.0.0.1:4747";

/// Daemon work (a diff over a large change set on a cold cache) is not
/// timed out client-side at the old 10 s; this is a backstop only.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

pub const VERIFY_SCHEMA: &str = "kbc-review-verify/1";
pub const START_SCHEMA: &str = "kbc-review-start/1";
pub const SNAPSHOT_SCHEMA: &str = "kbc-review-snapshot/1";

fn argv(parts: &[&str]) -> NextArgv {
    parts.iter().map(|s| s.to_string()).collect()
}

// --- addressing ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Id(i64),
    Pr(u32),
}

/// A parsed review address: the review (by id or PR) and an optional
/// patchset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewRef {
    pub target: Target,
    pub ps: Option<i64>,
}

impl std::fmt::Display for ReviewRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.target {
            Target::Id(id) => write!(f, "{id}")?,
            Target::Pr(n) => write!(f, "pr:{n}")?,
        }
        if let Some(ps) = self.ps {
            write!(f, "/ps{ps}")?;
        }
        Ok(())
    }
}

const REF_GRAMMAR: &str = "<id>, <id>/ps<n>, pr:<N> or pr:<N>/ps<n>";

fn positive<T: std::str::FromStr + PartialOrd + Default>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<T>().ok().filter(|n| *n > T::default())
}

/// THE address parser. Digits only — no sign, no whitespace inside, no
/// leading `+` — so an address can never smuggle anything into a URL path.
pub fn parse_review_ref(s: &str) -> Result<ReviewRef, String> {
    let s = s.trim();
    let bad = || format!("invalid review address {s:?} — expected {REF_GRAMMAR}");
    if s.is_empty() {
        return Err(bad());
    }
    let (head, tail) = match s.split_once('/') {
        Some((h, t)) => (h, Some(t)),
        None => (s, None),
    };
    let ps = match tail {
        None => None,
        Some(t) => Some(
            t.strip_prefix("ps")
                .and_then(positive::<i64>)
                .ok_or_else(bad)?,
        ),
    };
    let target = if let Some(n) = head.strip_prefix("pr:") {
        Target::Pr(positive::<u32>(n).ok_or_else(bad)?)
    } else {
        Target::Id(positive::<i64>(head).ok_or_else(bad)?)
    };
    Ok(ReviewRef { target, ps })
}

/// `<ref>/ps<n>` and `--ps N` together must agree.
pub fn merge_ps(from_ref: Option<i64>, flag: Option<i64>) -> Result<Option<i64>, String> {
    match (from_ref, flag) {
        (Some(a), Some(b)) if a != b => Err(format!(
            "the address says ps{a} but --ps says {b} — pass one"
        )),
        (a, b) => Ok(a.or(b)),
    }
}

// --- the typed error -----------------------------------------------------------

/// A verb failure: the typed envelope's fields plus the exit code.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub next: Vec<NextArgv>,
    /// Boxed so `Result<_, AgentError>` stays under clippy's
    /// `result_large_err` threshold.
    pub candidates: Option<Box<Value>>,
    pub exit: i32,
}

impl AgentError {
    pub fn new(code: &str, message: impl Into<String>, exit: i32) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            hint: None,
            next: Vec::new(),
            candidates: None,
            exit,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("usage", message, envelope::EXIT_USAGE)
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_next(mut self, next: Vec<NextArgv>) -> Self {
        self.next = next;
        self
    }

    pub fn to_value(&self) -> Value {
        envelope::err_value(
            &self.code,
            &self.message,
            self.hint.as_deref(),
            &self.next,
            self.candidates.as_deref(),
        )
    }

    /// Map a non-2xx daemon answer. A typed body (`type` URN) keeps its
    /// code; otherwise the status names it.
    pub fn from_http(status: u16, body: &Value, what: &str) -> Self {
        let code = body["type"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                match status {
                    400 => "bad-request",
                    401 | 403 => "refused",
                    404 => "not-found",
                    409 => "conflict",
                    503 => "unavailable",
                    _ => "daemon-error",
                }
                .to_string()
            });
        let message = body["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("{what}: HTTP {status}"));
        let exit = match status {
            400 => envelope::EXIT_USAGE,
            401 | 403 => envelope::EXIT_REFUSED,
            404 => envelope::EXIT_NOT_FOUND,
            409 | 503 => envelope::EXIT_CONFLICT,
            _ => envelope::EXIT_GENERIC,
        };
        let mut e = Self::new(&code, message, exit);
        if status == 404 && body.is_null() {
            e.hint =
                Some("an empty 404 is also what a loopback-only route answers off-loopback".into());
        }
        e
    }

    /// Print (JSON envelope or a human line, both on stderr) and exit.
    pub fn emit(&self, json: bool) -> ! {
        if json {
            let v = self.to_value();
            match serde_json::to_string_pretty(&v) {
                Ok(s) => eprintln!("{s}"),
                Err(_) => eprintln!("{v}"),
            }
        } else {
            eprintln!("error ({}): {}", self.code, self.message);
            if let Some(h) = &self.hint {
                eprintln!("hint: {h}");
            }
            for n in &self.next {
                eprintln!("try: {}", n.join(" "));
            }
        }
        std::process::exit(self.exit)
    }
}

// --- http ------------------------------------------------------------------------

async fn get(
    daemon: &str,
    path: &str,
    query: &[(&'static str, String)],
) -> Result<(u16, Value), AgentError> {
    let client = crate::client_builder()
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(|e| AgentError::new("client", e.to_string(), envelope::EXIT_GENERIC))?;
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client.get(&url).query(query).send().await.map_err(|e| {
        let exit = if e.is_connect() || e.is_timeout() || e.status().is_none() {
            envelope::EXIT_UNREACHABLE
        } else {
            envelope::EXIT_GENERIC
        };
        AgentError::new("unreachable", format!("GET {url}: {e}"), exit)
            .with_hint(format!("is kb-code-server running at {daemon}?"))
            .with_next(vec![argv(&["kb-code", "identity", "--daemon", daemon])])
    })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let body = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text.trim()).unwrap_or_else(|_| json!({ "error": text.trim() }))
    };
    Ok((status, body))
}

async fn get_ok(
    daemon: &str,
    path: &str,
    query: &[(&'static str, String)],
    what: &str,
) -> Result<Value, AgentError> {
    let (status, body) = get(daemon, path, query).await?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(AgentError::from_http(status, &body, what))
    }
}

fn fill_id(template: &str, id: i64) -> String {
    template.replace("{id}", &id.to_string())
}

// --- request builders (walked by main.rs's dead-surface test) ------------------

pub fn review_find_request(
    pr: u32,
    repo: Option<&str>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q = vec![("pr", pr.to_string())];
    if let Some(r) = repo {
        q.push(("repo", r.to_string()));
    }
    (kb_code_server::review_views::REVIEW_FIND_ROUTE.path, q)
}

pub fn review_diff_request(
    ps: Option<i64>,
    mode: &str,
    path: Option<&str>,
    budget: Option<u64>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q = vec![("mode", mode.to_string())];
    if let Some(n) = ps {
        q.push(("ps", n.to_string()));
    }
    if let Some(p) = path {
        q.push(("path", p.to_string()));
    }
    if let Some(b) = budget {
        q.push(("budget", b.to_string()));
    }
    (kb_code_server::review_views::REVIEW_DIFF_ROUTE.path, q)
}

pub fn review_log_request(ps: Option<i64>) -> (&'static str, Vec<(&'static str, String)>) {
    let q = ps.map(|n| vec![("ps", n.to_string())]).unwrap_or_default();
    (kb_code_server::review_views::REVIEW_LOG_ROUTE.path, q)
}

pub fn review_cat_request(
    path: &str,
    ps: Option<i64>,
    side: &str,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q = vec![("path", path.to_string()), ("side", side.to_string())];
    if let Some(n) = ps {
        q.push(("ps", n.to_string()));
    }
    (kb_code_server::review_views::REVIEW_CAT_ROUTE.path, q)
}

// --- resolution ------------------------------------------------------------------

/// An address resolved to a concrete review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub id: i64,
    pub ps: Option<i64>,
    /// Set when the address was `pr:<N>`.
    pub pr: Option<u32>,
}

/// Pick the review `pr:<N>` names from a `kbc-review-find/1` body. Pure.
pub fn pick_from_find(
    body: &Value,
    pr: u32,
    repo: Option<&str>,
) -> Result<(i64, String), AgentError> {
    let reviews = body["reviews"].as_array().cloned().unwrap_or_default();
    let repos: Vec<String> = body["repos"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let n = pr.to_string();
    if reviews.is_empty() || repos.is_empty() {
        let where_ = repo.map(|r| format!(" in {r}")).unwrap_or_default();
        let next = match repo {
            Some(r) => vec![argv(&[
                "kb-code", "review", "start-pr", "--repo", r, "--pr", &n,
            ])],
            None => vec![argv(&[
                "kb-code", "review", "start-pr", "--repo", "<repo>", "--pr", &n,
            ])],
        };
        return Err(AgentError::new(
            "review-not-found",
            format!("no review is bound to PR #{pr}{where_}"),
            envelope::EXIT_NOT_FOUND,
        )
        .with_hint("start one with `kb-code review start-pr`")
        .with_next(next));
    }
    if repos.len() > 1 {
        let candidates: Vec<Value> = reviews
            .iter()
            .map(|r| json!({ "id": r["id"], "repo": r["repo"], "state": r["state"] }))
            .collect();
        let mut e = AgentError::new(
            "ambiguous-ref",
            format!(
                "pr:{pr} matches reviews in {} repos ({}) — pass --repo",
                repos.len(),
                repos.join(", ")
            ),
            envelope::EXIT_USAGE,
        )
        .with_hint("add --repo <name>, or address the review by id")
        .with_next(
            repos
                .iter()
                .map(|r| argv(&["kb-code", "review", "find", "--pr", &n, "--repo", r]))
                .collect(),
        );
        e.candidates = Some(Box::new(Value::Array(candidates)));
        return Err(e);
    }
    let r = &repos[0];
    let id = body["preferred"][r.as_str()]
        .as_i64()
        .or_else(|| reviews[0]["id"].as_i64())
        .ok_or_else(|| {
            AgentError::new(
                "daemon-error",
                "find answer carries no review id",
                envelope::EXIT_GENERIC,
            )
        })?;
    Ok((id, r.clone()))
}

/// Turn any address into `(review id, ps)`. `pr:<N>` costs one
/// `GET /api/reviews/find`; an `<id>` costs nothing (the verb's own read
/// 404s an unknown id with the typed `not-found`).
pub async fn resolve(
    daemon: &str,
    raw: &str,
    repo: Option<&str>,
    ps_flag: Option<i64>,
) -> Result<Resolved, AgentError> {
    let r = parse_review_ref(raw).map_err(AgentError::usage)?;
    let ps = merge_ps(r.ps, ps_flag).map_err(AgentError::usage)?;
    match r.target {
        Target::Id(id) => Ok(Resolved { id, ps, pr: None }),
        Target::Pr(pr) => {
            let (path, q) = review_find_request(pr, repo);
            let body = get_ok(daemon, path, &q, "review find").await?;
            let (id, _repo) = pick_from_find(&body, pr, repo)?;
            Ok(Resolved {
                id,
                ps,
                pr: Some(pr),
            })
        }
    }
}

// --- clap args -----------------------------------------------------------------

#[derive(Args, Debug)]
pub struct FindArgs {
    /// The PR number.
    #[arg(long = "pr")]
    pub pr: u32,
    /// Restrict to one configured repo.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// `<id>`, `<id>/ps<n>`, `pr:<N>`.
    pub target: String,
    #[arg(long)]
    pub ps: Option<i64>,
    /// Per-file counts (the default).
    #[arg(long, conflicts_with_all = ["name_only", "patch"])]
    pub stat: bool,
    /// Paths only.
    #[arg(long = "name-only", conflicts_with = "patch")]
    pub name_only: bool,
    /// Unified diff text.
    #[arg(long)]
    pub patch: bool,
    /// Only this file, or everything under this directory.
    #[arg(long)]
    pub path: Option<String>,
    /// Cut the patch at ~TOKENS (4 bytes each) with a marker line. Implies
    /// `--patch` unless `--stat`/`--name-only` is given.
    #[arg(long, value_name = "TOKENS")]
    pub budget: Option<u64>,
    /// Disambiguates `pr:<N>` when several repos have one.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

impl DiffArgs {
    pub fn mode(&self) -> &'static str {
        if self.patch || (self.budget.is_some() && !self.stat && !self.name_only) {
            "patch"
        } else if self.name_only {
            "name-only"
        } else {
            "stat"
        }
    }
}

#[derive(Args, Debug)]
pub struct LogArgs {
    pub target: String,
    #[arg(long)]
    pub ps: Option<i64>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct CatArgs {
    pub target: String,
    /// Repo-relative path.
    pub path: String,
    #[arg(long)]
    pub ps: Option<i64>,
    /// `new` (the patchset tip, default) or `old` (its base).
    #[arg(long, value_parser = ["old", "new"], default_value = "new")]
    pub side: String,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct VerifyArgs {
    pub target: String,
    #[arg(long)]
    pub ps: Option<i64>,
    /// Fail unless at least N (non-superseded) findings exist.
    #[arg(long = "min-findings")]
    pub min_findings: Option<usize>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

// --- review find -----------------------------------------------------------------

/// The `find` envelope. Pure.
pub fn find_envelope(body: &Value, repo: Option<&str>) -> Value {
    let reviews = body["reviews"].as_array().cloned().unwrap_or_default();
    let pr = body["pr_number"].as_u64().unwrap_or(0).to_string();
    let mut next = Vec::new();
    if let Some(prefs) = body["preferred"].as_object() {
        for id in prefs.values().filter_map(Value::as_i64) {
            next.push(argv(&[
                "kb-code",
                "review",
                "diff",
                &id.to_string(),
                "--stat",
            ]));
        }
    }
    if reviews.is_empty() {
        if let Some(r) = repo {
            next.push(argv(&[
                "kb-code", "review", "start-pr", "--repo", r, "--pr", &pr,
            ]));
        }
    }
    envelope::ok_value(
        kb_code_server::review_views::FIND_SCHEMA,
        body,
        vec![],
        false,
        reviews.is_empty().then_some("no-review-for-pr"),
        next,
    )
}

pub async fn find_cmd(a: FindArgs) -> anyhow::Result<()> {
    let (path, q) = review_find_request(a.pr, a.repo.as_deref());
    let body = match get_ok(&a.daemon, path, &q, "review find").await {
        Ok(b) => b,
        Err(e) => e.emit(a.json),
    };
    if a.json {
        envelope::print_value(&find_envelope(&body, a.repo.as_deref()));
        return Ok(());
    }
    let reviews = body["reviews"].as_array().cloned().unwrap_or_default();
    if reviews.is_empty() {
        println!("(no review bound to PR #{})", a.pr);
        return Ok(());
    }
    for r in &reviews {
        println!(
            "#{:<5} {:<24} {:<6} ps{:<3} {}",
            r["id"].as_i64().unwrap_or(0),
            r["repo"].as_str().unwrap_or("?"),
            r["state"].as_str().unwrap_or("?"),
            r["latest_ps"].as_i64().unwrap_or(0),
            r["title"].as_str().unwrap_or("(untitled)"),
        );
    }
    Ok(())
}

// --- review diff -----------------------------------------------------------------

/// The `diff` envelope. Pure.
pub fn diff_envelope(body: &Value, addr: &str, resolved: &Resolved) -> Value {
    let mut data = body.clone();
    data["ref"] = json!(addr);
    data["pr"] = json!(resolved.pr);
    let id = resolved.id.to_string();
    let mut warnings = Vec::new();
    let mut next = Vec::new();
    if body["truncated"].as_bool() == Some(true) {
        warnings.push(format!(
            "truncated: {} of {} patch bytes shown",
            body["patch"].as_str().map(str::len).unwrap_or(0),
            body["patch_bytes"]
        ));
        next.push(argv(&["kb-code", "review", "diff", &id, "--stat"]));
    }
    if let Some(r) = body["redacted"].as_array().filter(|r| !r.is_empty()) {
        warnings.push(format!(
            "redacted: {} file(s) withheld by the secret denylist",
            r.len()
        ));
    }
    if body["mode"] == "stat" || body["mode"] == "name-only" {
        next.push(argv(&[
            "kb-code", "review", "diff", &id, "--patch", "--budget", "8000",
        ]));
    }
    let empty = body["files"].as_array().is_none_or(|f| f.is_empty());
    envelope::ok_value(
        kb_code_server::review_views::DIFF_SCHEMA,
        data,
        warnings,
        false,
        empty.then_some("no-changed-files"),
        next,
    )
}

pub async fn diff_cmd(a: DiffArgs) -> anyhow::Result<()> {
    let json = a.json;
    match diff_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn diff_run(a: &DiffArgs) -> Result<(), AgentError> {
    let res = resolve(&a.daemon, &a.target, a.repo.as_deref(), a.ps).await?;
    let (tpl, q) = review_diff_request(res.ps, a.mode(), a.path.as_deref(), a.budget);
    let body = get_ok(&a.daemon, &fill_id(tpl, res.id), &q, "review diff").await?;
    if a.json {
        envelope::print_value(&diff_envelope(&body, &a.target, &res));
        return Ok(());
    }
    let files = body["files"].as_array().cloned().unwrap_or_default();
    match a.mode() {
        "patch" => {
            print!("{}", body["patch"].as_str().unwrap_or(""));
            for r in body["redacted"].as_array().into_iter().flatten() {
                eprintln!(
                    "redacted: {} (matches {})",
                    r["path"].as_str().unwrap_or("?"),
                    r["pattern"].as_str().unwrap_or("?")
                );
            }
        }
        "name-only" => {
            for f in &files {
                println!("{}", f["path"].as_str().unwrap_or("?"));
            }
        }
        _ => {
            for f in &files {
                let path = match f["old_path"].as_str() {
                    Some(old) => format!("{old} => {}", f["path"].as_str().unwrap_or("?")),
                    None => f["path"].as_str().unwrap_or("?").to_string(),
                };
                println!(
                    " {} +{:<5} -{:<5} {path}",
                    f["status"].as_str().unwrap_or("?"),
                    f["additions"].as_u64().unwrap_or(0),
                    f["deletions"].as_u64().unwrap_or(0),
                );
            }
            println!(
                "{} file(s), +{} -{}  (ps{} {}..{})",
                files.len(),
                body["additions"],
                body["deletions"],
                body["ps_number"],
                short(body["base_sha"].as_str()),
                short(body["tip_sha"].as_str()),
            );
        }
    }
    Ok(())
}

fn short(s: Option<&str>) -> String {
    s.unwrap_or("?").chars().take(12).collect()
}

// --- review log -----------------------------------------------------------------

pub fn log_envelope(body: &Value, addr: &str, resolved: &Resolved) -> Value {
    let mut data = body.clone();
    data["ref"] = json!(addr);
    data["pr"] = json!(resolved.pr);
    let mut warnings = Vec::new();
    if body["truncated"].as_bool() == Some(true) {
        warnings.push(format!("truncated: first {} commits only", body["count"]));
    }
    let empty = body["commits"].as_array().is_none_or(|c| c.is_empty());
    envelope::ok_value(
        kb_code_server::review_views::LOG_SCHEMA,
        data,
        warnings,
        false,
        empty.then_some("no-commits"),
        vec![argv(&[
            "kb-code",
            "review",
            "diff",
            &resolved.id.to_string(),
            "--stat",
        ])],
    )
}

pub async fn log_cmd(a: LogArgs) -> anyhow::Result<()> {
    let json = a.json;
    match log_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn log_run(a: &LogArgs) -> Result<(), AgentError> {
    let res = resolve(&a.daemon, &a.target, a.repo.as_deref(), a.ps).await?;
    let (tpl, q) = review_log_request(res.ps);
    let body = get_ok(&a.daemon, &fill_id(tpl, res.id), &q, "review log").await?;
    if a.json {
        envelope::print_value(&log_envelope(&body, &a.target, &res));
        return Ok(());
    }
    for c in body["commits"].as_array().into_iter().flatten() {
        println!(
            "{}  {}  {}  ({})",
            short(c["sha"].as_str()),
            c["author_date"].as_str().unwrap_or("?"),
            c["subject"].as_str().unwrap_or(""),
            c["author"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

// --- review cat -----------------------------------------------------------------

pub fn cat_envelope(body: &Value, addr: &str, resolved: &Resolved) -> Value {
    let mut data = body.clone();
    data["ref"] = json!(addr);
    data["pr"] = json!(resolved.pr);
    envelope::ok_value(
        kb_code_server::review_views::CAT_SCHEMA,
        data,
        vec![],
        false,
        None,
        vec![],
    )
}

pub async fn cat_cmd(a: CatArgs) -> anyhow::Result<()> {
    let json = a.json;
    match cat_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn cat_run(a: &CatArgs) -> Result<(), AgentError> {
    let res = resolve(&a.daemon, &a.target, a.repo.as_deref(), a.ps).await?;
    let (tpl, q) = review_cat_request(&a.path, res.ps, &a.side);
    let body = get_ok(&a.daemon, &fill_id(tpl, res.id), &q, "review cat").await?;
    if a.json {
        envelope::print_value(&cat_envelope(&body, &a.target, &res));
        return Ok(());
    }
    let content = body["content"].as_str().unwrap_or("");
    if body["encoding"] == "base64" {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| AgentError::new("decode", e.to_string(), envelope::EXIT_GENERIC))?;
        use std::io::Write as _;
        std::io::stdout()
            .write_all(&bytes)
            .map_err(|e| AgentError::new("io", e.to_string(), envelope::EXIT_GENERIC))?;
    } else {
        print!("{content}");
    }
    Ok(())
}

// --- review verify ----------------------------------------------------------------

/// The four reads `verify` evaluates, as fetched.
#[derive(Debug, Default)]
pub struct VerifyInputs {
    /// `GET /api/reviews/{id}`.
    pub review: Value,
    /// `GET /api/reviews/{id}/doc?ps=` — `None` when it 404'd (no document).
    pub doc: Option<Value>,
    /// `GET /api/reviews/{id}/doc/lint?ps=` — `None` when there is no doc.
    pub lint: Option<Value>,
    /// `GET /api/reviews/{id}/findings?ps=`.
    pub findings: Value,
}

/// The post-compose gate, as a pure function over the four reads: the
/// document is present (and lints without errors), the findings count
/// (optionally `>= min_findings`), every finding anchor resolves at the
/// patchset, and a verdict exists on the LATEST patchset. Returns the
/// `kbc-review-verify/1` data and whether every check passed.
pub fn evaluate_verify(inp: &VerifyInputs, min_findings: Option<usize>) -> (Value, bool) {
    let id = inp.review["id"].as_i64().unwrap_or(0);
    let id_s = id.to_string();
    let latest_ps = inp.review["patchsets"]
        .as_array()
        .and_then(|p| p.last())
        .and_then(|p| p["ps_number"].as_i64());
    let ps = inp.findings["ps"].as_i64().or(latest_ps);
    let mut checks = Vec::new();
    let mut next: Vec<NextArgv> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(json!({ "name": name, "ok": ok, "detail": detail }));
        ok
    };

    let doc_ok = check(
        "document",
        inp.doc.is_some(),
        match &inp.doc {
            Some(d) => format!("revision {} (tier {})", d["revision"], d["tier"]),
            None => "no kbc-review/1 document at this patchset".into(),
        },
    );
    if !doc_ok {
        next.push(argv(&[
            "kb-code",
            "review",
            "compose",
            &id_s,
            "--doc",
            "<review.md>",
        ]));
    }
    let lint_errors = inp
        .lint
        .as_ref()
        .and_then(|l| l["errors"].as_u64())
        .unwrap_or(0);
    let lint_ok = check(
        "document_lint",
        inp.doc.is_some() && lint_errors == 0,
        if inp.doc.is_some() {
            format!("{lint_errors} lint error(s)")
        } else {
            "no document to lint".into()
        },
    );
    if doc_ok && !lint_ok {
        next.push(argv(&["kb-code", "review", "lint", &id_s]));
    }

    let findings = inp.findings["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let live: Vec<&Value> = findings
        .iter()
        .filter(|f| f["superseded"].as_bool() != Some(true))
        .collect();
    let orphaned: Vec<String> = live
        .iter()
        .filter(|f| f["resolution"]["orphaned"].as_bool() == Some(true))
        .map(|f| f["slug"].as_str().unwrap_or("?").to_string())
        .collect();
    let min = min_findings.unwrap_or(0);
    check(
        "findings",
        live.len() >= min,
        format!(
            "{} finding(s){}",
            live.len(),
            if min > 0 {
                format!(", need >= {min}")
            } else {
                String::new()
            }
        ),
    );
    let anchors_ok = check(
        "anchors",
        orphaned.is_empty(),
        if orphaned.is_empty() {
            "every finding anchor resolves".into()
        } else {
            format!("{} orphaned: {}", orphaned.len(), orphaned.join(", "))
        },
    );
    if !anchors_ok {
        next.push(argv(&["kb-code", "review", "findings", "list", &id_s]));
    }

    let verdict = &inp.review["verdict"];
    let verdict_ps = verdict["ps"].as_i64();
    let verdict_ok = check(
        "verdict",
        !verdict.is_null() && verdict_ps.is_some() && verdict_ps == latest_ps,
        match (verdict["state"].as_str(), verdict_ps, latest_ps) {
            (None, _, _) => "no verdict".into(),
            (Some(s), Some(v), Some(l)) if v == l => format!("{s} on ps{v} (latest)"),
            (Some(s), v, l) => format!(
                "{s} on ps{} but the latest is ps{}",
                v.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                l.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
            ),
        },
    );
    if !verdict_ok {
        next.push(argv(&[
            "kb-code",
            "review",
            "verdict",
            &id_s,
            "<approve|request-changes|comment>",
        ]));
    }

    let passed = checks.iter().all(|c| c["ok"] == true);
    (
        json!({
            "result": if passed { "ok" } else { "fail" },
            "review_id": id,
            "ps_number": ps,
            "latest_ps": latest_ps,
            "findings_count": live.len(),
            "orphaned": orphaned,
            "verdict": verdict,
            "checks": checks,
            "next": next,
        }),
        passed,
    )
}

pub async fn verify_cmd(a: VerifyArgs) -> anyhow::Result<()> {
    let json = a.json;
    match verify_run(&a).await {
        Ok(passed) => {
            if !passed {
                std::process::exit(envelope::EXIT_CONFLICT);
            }
            Ok(())
        }
        Err(e) => e.emit(json),
    }
}

async fn verify_run(a: &VerifyArgs) -> Result<bool, AgentError> {
    let res = resolve(&a.daemon, &a.target, a.repo.as_deref(), a.ps).await?;
    let id = res.id;
    let psq: Vec<(&'static str, String)> = res
        .ps
        .map(|n| vec![("ps", n.to_string())])
        .unwrap_or_default();
    let review = get_ok(&a.daemon, &format!("/api/reviews/{id}"), &[], "review show").await?;
    let (st, doc) = get(&a.daemon, &format!("/api/reviews/{id}/doc"), &psq).await?;
    let doc = match st {
        200..=299 => Some(doc),
        404 => None,
        s => return Err(AgentError::from_http(s, &doc, "review doc")),
    };
    let lint = if doc.is_some() {
        Some(
            get_ok(
                &a.daemon,
                &format!("/api/reviews/{id}/doc/lint"),
                &psq,
                "review lint",
            )
            .await?,
        )
    } else {
        None
    };
    let findings = get_ok(
        &a.daemon,
        &format!("/api/reviews/{id}/findings"),
        &psq,
        "review findings",
    )
    .await?;
    let inputs = VerifyInputs {
        review,
        doc,
        lint,
        findings,
    };
    let (data, passed) = evaluate_verify(&inputs, a.min_findings);
    if a.json {
        let next: Vec<NextArgv> = data["next"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| serde_json::from_value(n.clone()).ok())
            .collect();
        envelope::print_value(&envelope::ok_value(
            VERIFY_SCHEMA,
            &data,
            vec![],
            false,
            None,
            next,
        ));
    } else {
        println!(
            "review {id} verify: {}",
            data["result"].as_str().unwrap_or("?")
        );
        for c in data["checks"].as_array().into_iter().flatten() {
            println!(
                "  [{}] {}: {}",
                if c["ok"] == true { "ok" } else { "FAIL" },
                c["name"].as_str().unwrap_or("?"),
                c["detail"].as_str().unwrap_or("")
            );
        }
    }
    Ok(passed)
}

// --- compose --slugify --------------------------------------------------------------

/// `compose --slugify`: give every finding WITHOUT a valid slug the one kb
/// itself derives from its title (`kb_code_server::review_findings::
/// slug_from_title` — ASCII `f-…`, non-ASCII-safe), uniquified WITHIN the
/// batch (`-2`, `-3`, … — deterministic, so re-composing the same file
/// yields the same slugs and reconciles instead of duplicating). A valid
/// slug the author wrote is never touched. Accepts every shape compose
/// takes: a bare findings array, `{"findings": [...]}` (the sidecar), and
/// the V0 body's `{"findings": {"findings": [...]}}`. Returns
/// `(index, old, new)` for each rewrite.
pub fn slugify_findings(payload: &mut Value) -> Vec<(usize, Option<String>, String)> {
    use kb_code_server::review_findings::{is_valid_finding_slug, slug_from_title};
    let list = if payload.is_array() {
        payload.as_array_mut()
    } else if payload["findings"].is_array() {
        payload["findings"].as_array_mut()
    } else if payload["findings"]["findings"].is_array() {
        payload["findings"]["findings"].as_array_mut()
    } else {
        None
    };
    let Some(list) = list else {
        return Vec::new();
    };
    let mut taken: std::collections::HashSet<String> = list
        .iter()
        .filter_map(|f| f["slug"].as_str())
        .filter(|s| is_valid_finding_slug(s))
        .map(str::to_string)
        .collect();
    let mut changes = Vec::new();
    for (i, f) in list.iter_mut().enumerate() {
        let old = f["slug"].as_str().map(str::to_string);
        if old.as_deref().is_some_and(is_valid_finding_slug) {
            continue;
        }
        let title = f["title"].as_str().unwrap_or("");
        let base = slug_from_title(title);
        let mut slug = base.clone();
        let mut n = 2;
        while taken.contains(&slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        taken.insert(slug.clone());
        if let Some(obj) = f.as_object_mut() {
            obj.insert("slug".into(), Value::String(slug.clone()));
            changes.push((i, old, slug));
        }
    }
    changes
}

// --- start-pr / snapshot envelopes -------------------------------------------------

/// The `base{…}` object every start/snapshot/sync envelope carries (README
/// §12). RS-U6: when the daemon sends its own `base{mode, branch, set_by,
/// source, state, merge_base, fetched_at, …}` block ([`daemon_base`]),
/// that is what this carries (plus `ref`/`pinned`); an older daemon's
/// body falls back to what `base_ref`/`base_source`/`base_sha` say, with
/// `mode`/`state` `null`.
pub fn base_block(base_ref: Option<&str>, source: Option<&str>, merge_base: Option<&str>) -> Value {
    let pinned = base_ref.is_some_and(kb_code_server::reviews::is_full_sha);
    json!({
        "mode": Value::Null,
        "branch": if pinned { None } else { base_ref },
        "ref": base_ref,
        "source": source,
        "state": Value::Null,
        "merge_base": merge_base,
        "pinned": pinned,
    })
}

/// [`base_block`], preferring the daemon's own `base{…}` (RS-U6).
pub fn daemon_base(
    daemon: &Value,
    base_ref: Option<&str>,
    source: Option<&str>,
    merge_base: Option<&str>,
) -> Value {
    let mut out = base_block(base_ref, source, merge_base);
    if let Some(obj) = daemon.as_object() {
        for (k, v) in obj {
            out[k.as_str()] = v.clone();
        }
        out["pinned"] = json!(daemon["mode"].as_str() == Some("pin"));
        if out["merge_base"].is_null() {
            out["merge_base"] = json!(merge_base);
        }
    }
    out
}

/// The daemon's `warnings[]` (RS-U6: `{code, message}` objects) as the
/// envelope's `"code: message"` strings; `None` for an older daemon.
fn daemon_warnings(body: &Value) -> Option<Vec<String>> {
    body["warnings"].as_array().map(|ws| {
        ws.iter()
            .filter_map(|w| match (w["code"].as_str(), w["message"].as_str()) {
                (Some(c), Some(m)) => Some(format!("{c}: {m}")),
                (Some(c), None) => Some(c.to_string()),
                _ => w.as_str().map(str::to_string),
            })
            .collect()
    })
}

/// README §12's one stderr line for start / snapshot / fetch, e.g.
/// `base: tracking main (forge api) · merge-base 7c1ed0c · fetched via
/// gh-cli (someone)`. `None` when the daemon sent no `base{…}` block.
pub fn base_line(base: &Value) -> Option<String> {
    let obj = base.as_object()?;
    obj.get("set_by")?;
    let short = |s: &str| s.chars().take(7).collect::<String>();
    let source = base["source"].as_str().map(|s| match s {
        "forge-api" => "forge api".to_string(),
        "default-assumed" => "default branch, assumed".to_string(),
        "stack-parent" => "stack parent".to_string(),
        "merge-ref" => "merge ref".to_string(),
        "legacy" => "legacy row".to_string(),
        other => other.to_string(),
    });
    let branch = base["branch"].as_str().unwrap_or("?");
    let head = match base["mode"].as_str() {
        Some("track") => format!("tracking {branch}"),
        Some("local") => format!("local {branch}"),
        Some("pin") => "pinned".to_string(),
        _ => "legacy base".to_string(),
    };
    let mut parts = vec![match source {
        Some(s) => format!("base: {head} ({s})"),
        None => format!("base: {head}"),
    }];
    if let Some(mb) = base["merge_base"].as_str() {
        parts.push(format!("merge-base {}", short(mb)));
    }
    match (base["last_fetch"].as_str(), base["fetched_via"].as_str()) {
        (Some("fetched"), Some(via)) => parts.push(format!("fetched via {via}")),
        (Some("fetched"), None) => parts.push("fetched".into()),
        (Some("offline"), _) => parts.push("offline — cached base".into()),
        (Some("failed"), _) => parts.push("fetch failed — cached base".into()),
        (Some("cached"), _) => parts.push("cached".into()),
        _ => {}
    }
    if base["mode"].as_str() == Some("pin") {
        parts.push("will not follow rebases; use --base <branch>".into());
    }
    Some(parts.join(" · "))
}

/// Print [`base_line`] to stderr when the daemon sent a `base{…}` block.
pub fn eprint_base_line(body: &Value) {
    if let Some(line) = base_line(&body["base"]) {
        eprintln!("{line}");
    }
}

fn base_warnings(base_ref: Option<&str>) -> Vec<String> {
    match base_ref {
        Some(b) if kb_code_server::reviews::is_full_sha(b) => vec![format!(
            "base-pinned: this review compares against the fixed commit {} — it will not follow its target branch",
            &b[..12]
        )],
        _ => vec![],
    }
}

/// `review start-pr --json`'s envelope over the daemon's creation/reuse
/// body. `minted` is the daemon's own field when present (RS-U10a adds it),
/// else derived: a fresh review always minted ps1.
pub fn start_envelope(body: &Value) -> Value {
    let id = body["id"].as_i64();
    let reused = body["reused"].as_bool().unwrap_or(false);
    let minted = body["minted"].as_bool().unwrap_or(!reused);
    let base_ref = body["base_ref"].as_str();
    let mut warnings = daemon_warnings(body).unwrap_or_else(|| base_warnings(base_ref));
    if !body["pr_meta_unavailable_reason"].is_null() {
        let r = &body["pr_meta_unavailable_reason"];
        let code = r["code"].as_str().or(r.as_str()).unwrap_or("unavailable");
        warnings.push(format!("pr-meta-unavailable: {code}"));
    }
    let id_s = id.map(|i| i.to_string()).unwrap_or_default();
    envelope::ok_value(
        START_SCHEMA,
        json!({
            "id": id,
            "minted": minted,
            "reused": reused,
            "ps": body["latest_ps"],
            "tip_sha": body["tip_sha"],
            "base": daemon_base(
                &body["base"],
                base_ref,
                body["base_source"].as_str(),
                body["base_sha"].as_str(),
            ),
            "pr_number": body["pr_number"],
            "pr_head_sha": body["pr_head_sha"],
            "review": body,
        }),
        warnings,
        false,
        None,
        vec![
            argv(&["kb-code", "review", "diff", &id_s, "--stat"]),
            argv(&["kb-code", "review", "verify", &id_s]),
        ],
    )
}

/// `review snapshot --json`'s envelope. `review` is `GET /api/reviews/{id}`
/// when it could be read (for `base_ref`), else `Null`.
pub fn snapshot_envelope(body: &Value, review: &Value) -> Value {
    let id = body["review_id"].as_i64();
    let minted = body["minted"].as_bool().unwrap_or(true);
    let base_ref = review["base_ref"].as_str();
    let id_s = id.map(|i| i.to_string()).unwrap_or_default();
    let ps_s = body["ps_number"]
        .as_i64()
        .map(|n| format!("{id_s}/ps{n}"))
        .unwrap_or_else(|| id_s.clone());
    envelope::ok_value(
        SNAPSHOT_SCHEMA,
        json!({
            "id": id,
            "minted": minted,
            "ps": body["ps_number"],
            "tip_sha": body["tip_sha"],
            "base": daemon_base(&body["base"], base_ref, None, body["base_sha"].as_str()),
            "captured_at": body["captured_at"],
            "kind": body["kind"],
        }),
        daemon_warnings(body).unwrap_or_else(|| base_warnings(base_ref)),
        false,
        None,
        vec![argv(&["kb-code", "review", "diff", &ps_s, "--stat"])],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- a small JSON-schema-like shape check -----------------------------

    #[derive(Clone, Copy)]
    enum K {
        Str,
        Int,
        Bool,
        Arr,
        Obj,
        StrOrNull,
        IntOrNull,
        Any,
    }

    fn kind_ok(v: &Value, k: K) -> bool {
        match k {
            K::Str => v.is_string(),
            K::Int => v.is_i64() || v.is_u64(),
            K::Bool => v.is_boolean(),
            K::Arr => v.is_array(),
            K::Obj => v.is_object(),
            K::StrOrNull => v.is_string() || v.is_null(),
            K::IntOrNull => v.is_i64() || v.is_u64() || v.is_null(),
            K::Any => true,
        }
    }

    /// Assert `v` carries every `(pointer, kind)`.
    fn assert_shape(v: &Value, spec: &[(&str, K)]) {
        for (ptr, k) in spec {
            let got = v
                .pointer(ptr)
                .unwrap_or_else(|| panic!("missing {ptr} in {v:#}"));
            assert!(kind_ok(got, *k), "{ptr} has the wrong type in {v:#}");
        }
    }

    const ENVELOPE: &[(&str, K)] = &[
        ("/schema", K::Str),
        ("/ok", K::Bool),
        ("/data", K::Obj),
        ("/warnings", K::Arr),
        ("/degraded", K::Bool),
        ("/empty_reason", K::StrOrNull),
        ("/next", K::Arr),
    ];

    fn assert_next_is_argv(v: &Value) {
        for n in v["next"].as_array().unwrap() {
            let a = n.as_array().expect("each next is an argv array");
            assert!(!a.is_empty());
            assert!(a.iter().all(Value::is_string));
            assert_eq!(a[0], "kb-code");
        }
    }

    fn resolved(pr: Option<u32>) -> Resolved {
        Resolved {
            id: 12,
            ps: None,
            pr,
        }
    }

    // --- addressing -------------------------------------------------------

    #[test]
    fn parses_every_address_form() {
        assert_eq!(
            parse_review_ref("12"),
            Ok(ReviewRef {
                target: Target::Id(12),
                ps: None
            })
        );
        assert_eq!(
            parse_review_ref("12/ps3"),
            Ok(ReviewRef {
                target: Target::Id(12),
                ps: Some(3)
            })
        );
        assert_eq!(
            parse_review_ref("pr:4711"),
            Ok(ReviewRef {
                target: Target::Pr(4711),
                ps: None
            })
        );
        assert_eq!(
            parse_review_ref(" pr:7/ps2 "),
            Ok(ReviewRef {
                target: Target::Pr(7),
                ps: Some(2)
            })
        );
    }

    #[test]
    fn rejects_malformed_addresses() {
        for bad in [
            "",
            "0",
            "-1",
            "+3",
            "12/",
            "12/ps",
            "12/ps0",
            "12/p3",
            "12/ps-1",
            "pr:",
            "pr:0",
            "pr:-3",
            "pr:x",
            "PR:3",
            "12 3",
            "12/ps3/x",
            "../12",
            "12?x=1",
            "99999999999999999999",
            "pr:4294967296",
            "à",
        ] {
            assert!(parse_review_ref(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn display_round_trips() {
        for s in ["12", "12/ps3", "pr:4711", "pr:7/ps2"] {
            assert_eq!(parse_review_ref(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn ps_from_address_and_flag_must_agree() {
        assert_eq!(merge_ps(None, None), Ok(None));
        assert_eq!(merge_ps(Some(2), None), Ok(Some(2)));
        assert_eq!(merge_ps(None, Some(4)), Ok(Some(4)));
        assert_eq!(merge_ps(Some(2), Some(2)), Ok(Some(2)));
        assert!(merge_ps(Some(2), Some(3)).is_err());
    }

    // --- pr:<N> inference ------------------------------------------------

    fn find_body(rows: &[(i64, &str, &str)]) -> Value {
        let mut repos: Vec<&str> = Vec::new();
        let mut preferred = serde_json::Map::new();
        for (id, repo, _) in rows {
            if !repos.contains(repo) {
                repos.push(*repo);
                preferred.insert(repo.to_string(), json!(id));
            }
        }
        json!({
            "schema": "kbc-review-find/1",
            "pr_number": 7,
            "repos": repos,
            "preferred": preferred,
            "reviews": rows.iter().map(|(id, repo, st)| json!({"id": id, "repo": repo, "state": st})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn pr_resolves_when_one_repo_matches_preferring_the_daemons_pick() {
        let body = find_body(&[(5, "widgets", "open"), (3, "widgets", "closed")]);
        assert_eq!(
            pick_from_find(&body, 7, None),
            Ok((5, "widgets".to_string()))
        );
    }

    #[test]
    fn pr_in_two_repos_is_a_typed_ambiguity_listing_candidates() {
        let body = find_body(&[(5, "widgets-a", "open"), (9, "widgets-b", "open")]);
        let e = pick_from_find(&body, 7, None).unwrap_err();
        assert_eq!(e.code, "ambiguous-ref");
        assert_eq!(e.exit, envelope::EXIT_USAGE);
        let v = e.to_value();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "urn:kb:errors:ambiguous-ref");
        assert_eq!(v["error"]["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(v["error"]["next"].as_array().unwrap().len(), 2);
        assert_eq!(v["error"]["next"][0][6], "widgets-a");
    }

    #[test]
    fn pr_with_no_review_is_not_found_and_suggests_start_pr() {
        let body = find_body(&[]);
        let e = pick_from_find(&body, 7, Some("widgets")).unwrap_err();
        assert_eq!(e.exit, envelope::EXIT_NOT_FOUND);
        assert_eq!(
            e.next[0],
            argv(&["kb-code", "review", "start-pr", "--repo", "widgets", "--pr", "7"])
        );
    }

    // --- errors + exit codes ------------------------------------------------

    #[test]
    fn http_failures_map_onto_the_shipped_exit_table() {
        let cases = [
            (400, envelope::EXIT_USAGE, "urn:kb:errors:bad-request"),
            (401, envelope::EXIT_REFUSED, "urn:kb:errors:refused"),
            (403, envelope::EXIT_REFUSED, "urn:kb:errors:refused"),
            (404, envelope::EXIT_NOT_FOUND, "urn:kb:errors:not-found"),
            (409, envelope::EXIT_CONFLICT, "urn:kb:errors:conflict"),
            (503, envelope::EXIT_CONFLICT, "urn:kb:errors:unavailable"),
            (500, envelope::EXIT_GENERIC, "urn:kb:errors:daemon-error"),
        ];
        for (status, exit, urn) in cases {
            let e = AgentError::from_http(status, &json!({"error": "boom"}), "x");
            assert_eq!(e.exit, exit, "{status}");
            assert_eq!(e.to_value()["error"]["code"], urn, "{status}");
            assert_eq!(e.message, "boom");
        }
        // A typed daemon body keeps its own URN.
        let e = AgentError::from_http(
            403,
            &json!({"error": "denied", "type": "urn:kb:errors:redacted-by-policy"}),
            "cat",
        );
        assert_eq!(
            e.to_value()["error"]["code"],
            "urn:kb:errors:redacted-by-policy"
        );
        assert_eq!(e.exit, envelope::EXIT_REFUSED);
    }

    #[test]
    fn exit_codes_are_the_shipped_numbers() {
        // README §13's table was NOT adopted; these are envelope.rs's.
        assert_eq!(envelope::EXIT_GENERIC, 1);
        assert_eq!(envelope::EXIT_USAGE, 2);
        assert_eq!(envelope::EXIT_CONFLICT, 3);
        assert_eq!(envelope::EXIT_REFUSED, 4);
        assert_eq!(envelope::EXIT_UNREACHABLE, 5);
        assert_eq!(envelope::EXIT_UPSTREAM, 6);
        assert_eq!(envelope::EXIT_PARTIAL, 7);
        assert_eq!(envelope::EXIT_NOT_FOUND, 8);
    }

    #[test]
    fn error_envelope_shape() {
        let v = AgentError::usage("bad address")
            .with_hint("h")
            .with_next(vec![argv(&["kb-code", "review", "find", "--pr", "7"])])
            .to_value();
        assert_shape(
            &v,
            &[
                ("/ok", K::Bool),
                ("/error/code", K::Str),
                ("/error/message", K::Str),
                ("/error/hint", K::StrOrNull),
                ("/error/next", K::Arr),
            ],
        );
        assert!(v["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("urn:kb:errors:"));
    }

    // --- per-verb --json schemas ---------------------------------------------

    #[test]
    fn find_envelope_schema() {
        let v = find_envelope(&find_body(&[(5, "widgets", "open")]), None);
        assert_shape(&v, ENVELOPE);
        assert_eq!(v["schema"], "kbc-review-find/1");
        assert_shape(
            &v,
            &[
                ("/data/reviews", K::Arr),
                ("/data/repos", K::Arr),
                ("/data/preferred", K::Obj),
            ],
        );
        assert_eq!(v["empty_reason"], Value::Null);
        assert_next_is_argv(&v);
        let empty = find_envelope(&find_body(&[]), Some("widgets"));
        assert_eq!(empty["empty_reason"], "no-review-for-pr");
        assert_next_is_argv(&empty);
    }

    fn diff_body(mode: &str) -> Value {
        let mut b = json!({
            "schema": "kbc-review-diff/1",
            "review_id": 12, "ps_number": 2,
            "base_sha": "a".repeat(40), "tip_sha": "b".repeat(40),
            "mode": mode, "path": null,
            "files_total": 1, "files_count": 1, "additions": 3, "deletions": 1,
            "files": [{"path": "src/a.rs", "old_path": null, "status": "M",
                       "additions": 3, "deletions": 1, "binary": false}],
            "source": "work-tree",
        });
        if mode == "patch" {
            b["patch"] = json!("diff --git a/src/a.rs b/src/a.rs\n[kb-code: patch truncated]\n");
            b["patch_bytes"] = json!(4096);
            b["truncated"] = json!(true);
            b["budget_tokens"] = json!(10);
            b["redacted"] = json!([{"path": ".env", "pattern": ".env"}]);
        }
        b
    }

    #[test]
    fn diff_envelope_schema_stat_and_patch() {
        let stat = diff_envelope(&diff_body("stat"), "12", &resolved(None));
        assert_shape(&stat, ENVELOPE);
        assert_eq!(stat["schema"], "kbc-review-diff/1");
        assert_shape(
            &stat,
            &[
                ("/data/review_id", K::Int),
                ("/data/ps_number", K::Int),
                ("/data/base_sha", K::Str),
                ("/data/tip_sha", K::Str),
                ("/data/mode", K::Str),
                ("/data/files", K::Arr),
                ("/data/files/0/path", K::Str),
                ("/data/files/0/additions", K::Int),
                ("/data/ref", K::Str),
                ("/data/pr", K::IntOrNull),
            ],
        );
        assert_next_is_argv(&stat);

        let patch = diff_envelope(&diff_body("patch"), "pr:7", &resolved(Some(7)));
        assert_shape(
            &patch,
            &[
                ("/data/patch", K::Str),
                ("/data/truncated", K::Bool),
                ("/data/redacted", K::Arr),
                ("/data/pr", K::Int),
            ],
        );
        let w = patch["warnings"].as_array().unwrap();
        assert!(w
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("truncated:")));
        assert!(w
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("redacted:")));
    }

    #[test]
    fn log_and_cat_envelope_schemas() {
        let log = log_envelope(
            &json!({"schema": "kbc-review-log/1", "review_id": 12, "ps_number": 1,
                    "base_sha": "a", "tip_sha": "b", "count": 1, "truncated": false,
                    "commits": [{"sha": "c", "parents": [], "author": "A",
                                 "author_date": "2026-01-01", "subject": "s"}]}),
            "12",
            &resolved(None),
        );
        assert_shape(&log, ENVELOPE);
        assert_eq!(log["schema"], "kbc-review-log/1");
        assert_shape(
            &log,
            &[("/data/commits/0/sha", K::Str), ("/data/count", K::Int)],
        );
        assert_next_is_argv(&log);

        let cat = cat_envelope(
            &json!({"schema": "kbc-review-cat/1", "review_id": 12, "ps_number": 1,
                    "side": "old", "sha": "a", "path": "src/a.rs", "size": 3,
                    "blob_hash": "h", "encoding": "utf8", "content": "abc"}),
            "12/ps1",
            &resolved(None),
        );
        assert_shape(&cat, ENVELOPE);
        assert_eq!(cat["schema"], "kbc-review-cat/1");
        assert_shape(
            &cat,
            &[
                ("/data/side", K::Str),
                ("/data/content", K::Str),
                ("/data/encoding", K::Str),
            ],
        );
    }

    fn verify_inputs(verdict_ps: Option<i64>, doc: bool, orphan: bool) -> VerifyInputs {
        VerifyInputs {
            review: json!({
                "id": 12,
                "patchsets": [{"ps_number": 1}, {"ps_number": 2}],
                "verdict": match verdict_ps {
                    Some(p) => json!({"state": "approve", "ps": p}),
                    None => Value::Null,
                },
            }),
            doc: doc.then(|| json!({"revision": 1, "tier": "standard"})),
            lint: doc.then(|| json!({"errors": 0})),
            findings: json!({"ps": 2, "findings": [
                {"slug": "f-a", "superseded": false, "resolution": {"orphaned": orphan}},
                {"slug": "f-old", "superseded": true, "resolution": {"orphaned": true}},
            ]}),
        }
    }

    #[test]
    fn verify_passes_only_when_every_check_does() {
        let (data, ok) = evaluate_verify(&verify_inputs(Some(2), true, false), None);
        assert!(ok, "{data:#}");
        assert_eq!(data["result"], "ok");
        assert_eq!(
            data["findings_count"], 1,
            "superseded findings do not count"
        );
        let env = envelope::ok_value(VERIFY_SCHEMA, &data, vec![], false, None, vec![]);
        assert_shape(&env, ENVELOPE);
        assert_shape(
            &env,
            &[
                ("/data/result", K::Str),
                ("/data/review_id", K::Int),
                ("/data/latest_ps", K::Int),
                ("/data/findings_count", K::Int),
                ("/data/checks", K::Arr),
                ("/data/checks/0/name", K::Str),
                ("/data/checks/0/ok", K::Bool),
            ],
        );

        for (inp, failing) in [
            (verify_inputs(Some(1), true, false), "verdict"),
            (verify_inputs(None, true, false), "verdict"),
            (verify_inputs(Some(2), false, false), "document"),
            (verify_inputs(Some(2), true, true), "anchors"),
        ] {
            let (data, ok) = evaluate_verify(&inp, None);
            assert!(!ok);
            assert_eq!(data["result"], "fail");
            let failed: Vec<&str> = data["checks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["ok"] == false)
                .map(|c| c["name"].as_str().unwrap())
                .collect();
            assert!(failed.contains(&failing), "{failing}: {data:#}");
            assert!(!data["next"].as_array().unwrap().is_empty());
        }
        let (_, ok) = evaluate_verify(&verify_inputs(Some(2), true, false), Some(2));
        assert!(!ok, "--min-findings 2 with one live finding fails");
    }

    // --- start-pr / snapshot envelopes -----------------------------------------

    #[test]
    fn start_envelope_always_carries_id_minted_base_warnings() {
        let created = json!({"id": 12, "latest_ps": 1, "tip_sha": "t", "base_ref": "main",
                             "base_sha": "m", "base_source": "merge-base", "pr_number": 7,
                             "pr_meta_unavailable_reason": null});
        let v = start_envelope(&created);
        assert_shape(&v, ENVELOPE);
        assert_eq!(v["schema"], START_SCHEMA);
        assert_shape(
            &v,
            &[
                ("/data/id", K::Int),
                ("/data/minted", K::Bool),
                ("/data/base", K::Obj),
                ("/data/base/merge_base", K::StrOrNull),
                ("/data/base/branch", K::StrOrNull),
                ("/data/base/mode", K::Any),
                ("/data/base/state", K::Any),
                ("/data/review", K::Obj),
            ],
        );
        assert_eq!(v["data"]["minted"], true);
        assert_eq!(v["data"]["base"]["branch"], "main");
        assert!(v["warnings"].as_array().unwrap().is_empty());
        assert_next_is_argv(&v);

        // A reuse that captured nothing, against a pinned base.
        let pinned = "c".repeat(40);
        let reused = json!({"id": 12, "reused": true, "minted": false, "latest_ps": 3,
                            "base_ref": pinned, "base_sha": "m",
                            "pr_meta_unavailable_reason": {"code": "no-credentials"}});
        let v = start_envelope(&reused);
        assert_eq!(v["data"]["minted"], false);
        assert_eq!(v["data"]["base"]["pinned"], true);
        assert_eq!(v["data"]["base"]["branch"], Value::Null);
        let w: Vec<&str> = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert!(w.iter().any(|w| w.starts_with("base-pinned:")), "{w:?}");
        assert!(
            w.iter().any(|w| w.starts_with("pr-meta-unavailable:")),
            "{w:?}"
        );
        // An older daemon without `minted`: derived from `reused`.
        let legacy = json!({"id": 12, "reused": true, "latest_ps": 3});
        assert_eq!(start_envelope(&legacy)["data"]["minted"], false);
    }

    #[test]
    fn the_daemon_base_block_and_warnings_win_and_render_one_stderr_line() {
        let body = json!({"id": 3, "latest_ps": 2, "base_ref": "refs/remotes/origin/main",
                          "base_sha": "7c1ed0cfdd00000000000000000000000000abcd",
                          "minted": false,
                          "base": {"mode": "track", "branch": "main", "set_by": "auto",
                                   "source": "forge-api", "state": "ok",
                                   "merge_base": "7c1ed0cfdd00000000000000000000000000abcd",
                                   "fetched_at": 1, "last_fetch": "fetched",
                                   "fetched_via": "gh-cli (someone)"},
                          "warnings": [{"code": "pr-target-assumed", "message": "m"}]});
        let v = start_envelope(&body);
        assert_eq!(v["data"]["base"]["mode"], "track");
        assert_eq!(v["data"]["base"]["state"], "ok");
        assert_eq!(v["data"]["base"]["pinned"], false);
        assert_eq!(v["data"]["minted"], false);
        assert_eq!(v["warnings"][0], "pr-target-assumed: m");
        assert_eq!(
            base_line(&body["base"]).unwrap(),
            "base: tracking main (forge api) · merge-base 7c1ed0c · fetched via gh-cli (someone)"
        );
        let pin = json!({"mode": "pin", "set_by": "legacy", "source": "legacy",
                         "merge_base": "cc65611690000000000000000000000000000000"});
        let line = base_line(&pin).unwrap();
        assert!(
            line.starts_with("base: pinned (legacy row) · merge-base cc65611"),
            "{line}"
        );
        assert!(line.contains("will not follow rebases"), "{line}");
        assert!(base_line(&Value::Null).is_none());
    }

    #[test]
    fn snapshot_envelope_carries_id_minted_base_warnings() {
        let body = json!({"schema": "reviews/1", "review_id": 12, "ps_number": 4,
                          "tip_sha": "t", "base_sha": "m", "captured_at": 1, "minted": true});
        let v = snapshot_envelope(&body, &json!({"base_ref": "main"}));
        assert_shape(&v, ENVELOPE);
        assert_eq!(v["schema"], SNAPSHOT_SCHEMA);
        assert_shape(
            &v,
            &[
                ("/data/id", K::Int),
                ("/data/minted", K::Bool),
                ("/data/ps", K::Int),
                ("/data/base", K::Obj),
            ],
        );
        assert_eq!(v["next"][0][3], "12/ps4");
        // Base unknown (the review read failed): still a well-formed block.
        let v = snapshot_envelope(&body, &Value::Null);
        assert_eq!(v["data"]["base"]["merge_base"], "m");
    }

    // --- compose --slugify --------------------------------------------------------

    #[test]
    fn slugify_derives_ascii_slugs_for_non_ascii_titles_and_never_panics() {
        let mut sidecar = json!({"findings": [
            {"title": "Perché à rotto"},
            {"title": "Café déjà vu", "slug": "f-café"},
            {"title": "Größe über alles", "slug": ""},
            {"title": "🔥🔥"},
            {"title": "数据库 查询"},
            {"title": "Perché à rotto"},
            {"title": "kept", "slug": "f-kept"},
        ]});
        let changes = slugify_findings(&mut sidecar);
        let slugs: Vec<&str> = sidecar["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["slug"].as_str().unwrap())
            .collect();
        assert_eq!(
            slugs,
            vec![
                "f-perch-rotto",
                "f-caf-d-j-vu",
                "f-gr-e-ber-alles",
                "f-finding",
                "f-finding-2",
                "f-perch-rotto-2",
                "f-kept",
            ]
        );
        assert_eq!(changes.len(), 6, "the valid author slug is untouched");
        for s in slugs {
            assert!(s.is_ascii());
            assert!(
                kb_code_server::review_findings::is_valid_finding_slug(s),
                "{s}"
            );
        }
        // Deterministic: slugifying the original again yields the same.
        let mut again = json!([{"title": "Perché à rotto"}, {"title": "Perché à rotto"}]);
        slugify_findings(&mut again);
        assert_eq!(again[1]["slug"], "f-perch-rotto-2");
    }

    #[test]
    fn slugify_reaches_the_v0_body_and_ignores_other_shapes() {
        let mut v0 = json!({"summary": "s", "findings": {"schema": "kbc-findings/1",
                             "findings": [{"title": "Ünïcödé"}]}});
        assert_eq!(slugify_findings(&mut v0).len(), 1);
        assert_eq!(v0["findings"]["findings"][0]["slug"], "f-n-c-d");
        let mut other = json!({"summary": "no findings here"});
        assert!(slugify_findings(&mut other).is_empty());
    }
}
