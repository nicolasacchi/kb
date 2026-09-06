//! `kb slate` — the per-project blackboard (`kb-slate/1`, SL3).
//!
//! Design of record: `docs/research/kb-slate-design-2026-09.html` — §5 the
//! digest, §9 the CLI fence + wire shapes, §13 lifecycle, and the §4 rules
//! matrix wherever the prose is looser.
//!
//! Four things about this module, because they are what a later change is
//! most likely to break:
//!
//! 1. **The CLI never renders a digest.** `kb_core::slate::render` is the
//!    ONE renderer and the daemon already ran it: `open`, `delta` and the
//!    hook lanes print the server's `text` field VERBATIM (`print!`, not
//!    `println!` — `text` already ends in `\n`), so
//!    `CLI stdout == DigestResponse.text` byte for byte. Everything else
//!    this module prints is feedback ABOUT a write, never a re-rendering
//!    of the board.
//! 2. **Exit codes are the loop contract** (§9): 0 ok · 1 error · 2 not
//!    found · 3 refused. 3 covers BOTH `slate-taken` and
//!    `slate-live-author`, so a `/loop` caller branches on the exit status
//!    and never parses stderr. The code is read from the problem+json
//!    `code` extension member when present and from `detail`'s `<code>: `
//!    prefix otherwise — both forms are pinned by SL2.
//! 3. **The cursor and the topic marker are CLIENT state** (rules matrix
//!    "Cursor": no read ever writes). `open` writes
//!    `~/.cache/kb/slate-cursor-<sid>`; `delta` reads it when `--since` is
//!    absent and advances it; `delta --since N` never touches the file.
//!    `open --topic T` writes `~/.cache/kb/slate-topic-<sid>`, which every
//!    POSTING verb then reads as its default topic — a read is never
//!    silently narrowed by a marker the user forgot about.
//! 4. **Human rendering adds no fact the JSON lacks.** Every line below is
//!    assembled from fields on the response; `--json` passes the whole
//!    response through untouched, refusals included.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};
use crate::sse::{open_events_stream, FrameReader};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

/// §9 exit codes. `1` is anyhow's own default (main returns `Result`), so
/// only 2 and 3 are ever raised explicitly.
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_NOT_FOUND: i32 = 2;
pub const EXIT_REFUSED: i32 = 3;

/// The two refusals a loop caller must be able to branch on without
/// parsing stderr (§9): a contested take and a live author's post.
const REFUSAL_CODES: [&str; 2] = ["slate-taken", "slate-live-author"];

/// Cap on a quoted line inside the `pushed off the board:` feedback line.
/// The FULL text is one `--json` (or one `kb slate show #n`) away; five
/// 200-character lines would bury the post confirmation they annotate.
const DISPLACED_LINE_CHARS: usize = 60;

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested below; no I/O, no network)
// ---------------------------------------------------------------------------

/// `#12` / `12` → `12`. The digest prints `#n`, so both forms must work
/// or every copy-paste from the board is an error.
pub(crate) fn parse_seq(raw: &str) -> Result<u64> {
    let t = raw.trim().trim_start_matches('#');
    t.parse::<u64>()
        .with_context(|| format!("{raw:?} is not a post number — use #12 or 12"))
}

/// The Hand-packet rule (rules matrix): the CLI moves everything after the
/// first newline into `body`; an explicit `--body` wins over the split.
/// Returns `(line, body)`; the line is trimmed of trailing whitespace only
/// (a leading space in a quoted line is the author's).
pub(crate) fn split_line_body(raw: &str, explicit: Option<&str>) -> (String, Option<String>) {
    let (first, rest) = match raw.split_once('\n') {
        Some((a, b)) => (a, Some(b)),
        None => (raw, None),
    };
    let line = first.trim_end().to_string();
    let body = match explicit {
        Some(b) => Some(b.to_string()),
        None => rest
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    (line, body)
}

/// The `code` a refusal carries: the RFC 7807 extension member when SL2
/// set one, else `detail`'s `<code>: <text>` prefix (§9 pins both forms).
/// A prefix only counts when it looks like a code — lowercase, dashes, no
/// spaces — so an ordinary sentence with a colon is never mistaken for one.
pub(crate) fn refusal_code(body: &Value) -> Option<String> {
    if let Some(c) = body.get("code").and_then(Value::as_str) {
        if !c.trim().is_empty() {
            return Some(c.trim().to_string());
        }
    }
    let detail = body.get("detail").and_then(Value::as_str)?;
    let (head, _) = detail.split_once(": ")?;
    let ok = !head.is_empty()
        && head.len() <= 40
        && head
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit());
    ok.then(|| head.to_string())
}

/// §9's three non-zero codes. `slate-taken`/`slate-live-author` → 3 so a
/// loop caller can retry with `--anyway` or stop; a 404 → 2; anything
/// else → 1.
pub(crate) fn exit_code_for(status: u16, code: &str) -> i32 {
    if REFUSAL_CODES.contains(&code) {
        return EXIT_REFUSED;
    }
    if status == 404 {
        return EXIT_NOT_FOUND;
    }
    EXIT_ERROR
}

/// Truncate to `max` CHARACTERS (never bytes — a slate line is UTF-8 and
/// a mid-codepoint cut would panic), appending `…` when it bit.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{}…", head.trim_end())
}

/// §5's feedback line: what this append pushed off the default digest.
/// `total` is `displaced_total` — when it exceeds what the response
/// carried (`DISPLACED_CAP` = 5), the overflow is named rather than
/// silently dropped. `None` when nothing was displaced.
pub(crate) fn render_displaced(displaced: &[Value], total: u64) -> Option<String> {
    if displaced.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = displaced
        .iter()
        .map(|d| {
            let seq = d.get("seq").and_then(Value::as_u64).unwrap_or(0);
            let kind = d.get("kind").and_then(Value::as_str).unwrap_or("post");
            let line = d.get("line").and_then(Value::as_str).unwrap_or_default();
            format!(
                "#{seq} {kind} \"{}\"",
                ellipsize(line, DISPLACED_LINE_CHARS)
            )
        })
        .collect();
    let more = total.saturating_sub(displaced.len() as u64);
    if more > 0 {
        parts.push(format!("… and {more} more"));
    }
    Some(format!(
        "pushed off the board: {} — kb slate drop/edit to tidy, or open --all",
        parts.join(" · ")
    ))
}

/// The one-line confirmation every mutating verb prints before its
/// displaced/nudge lines. Every field comes off the returned `post`.
pub(crate) fn render_posted(post: &Value, slug: &str) -> String {
    let seq = post.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let kind = post.get("kind").and_then(Value::as_str).unwrap_or("post");
    let target = post
        .get("supersedes")
        .and_then(Value::as_u64)
        .or_else(|| post.get("re").and_then(Value::as_u64))
        .or_else(|| post.get("over").and_then(Value::as_u64));
    let topic = post
        .get("topic")
        .and_then(Value::as_str)
        .map(|t| format!(" [{t}]"))
        .unwrap_or_default();
    match target {
        Some(n) => format!("#{seq} {kind} → #{n} · {slug}{topic}"),
        None => format!("#{seq} {kind} · {slug}{topic}"),
    }
}

/// One raw ledger `Post` as a watch/history line. Kind, author tag and
/// line only — the unfolded view is `kb slate show #n`.
pub(crate) fn render_post_row(post: &Value) -> String {
    let seq = post.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let kind = post.get("kind").and_then(Value::as_str).unwrap_or("post");
    let prov = post.get("prov").cloned().unwrap_or(Value::Null);
    let harness = prov
        .get("harness")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let who = prov
        .get("session_id")
        .and_then(Value::as_str)
        .map(|s| format!("{harness}/{}", short_session(s)))
        .unwrap_or_else(|| harness.to_string());
    let target = post
        .get("supersedes")
        .and_then(Value::as_u64)
        .or_else(|| post.get("re").and_then(Value::as_u64))
        .map(|n| format!(" → #{n}"))
        .unwrap_or_default();
    let line = post.get("line").and_then(Value::as_str).unwrap_or_default();
    format!("#{seq} {kind}{target} [{who}] {line}")
}

/// `kb_core::slate::session_short`'s four-character tag, mirrored here so
/// a watch row and the digest name the same author the same way. Kept in
/// lock-step by `render_post_row_matches_core_session_short`.
fn short_session(sid: &str) -> String {
    kb_core::slate::session_short(Some(sid))
}

// ---------------------------------------------------------------------------
// Client-side markers: the cursor and the session topic (rules matrix
// "Cursor" — no READ ever writes; `open` and `delta` own these two files)
// ---------------------------------------------------------------------------

/// `${XDG_CACHE_HOME:-$HOME/.cache}/kb` — the directory the SessionStart
/// hooks already keep `current-session` in.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("kb"))
}

pub(crate) fn cursor_file(dir: &Path, sid: &str) -> PathBuf {
    dir.join(format!("slate-cursor-{sid}"))
}

pub(crate) fn topic_file(dir: &Path, sid: &str) -> PathBuf {
    dir.join(format!("slate-topic-{sid}"))
}

/// Read a marker file, trimmed; `None` on any failure (these are hints,
/// never a reason to fail a verb).
fn read_marker(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let t = raw.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Write a marker atomically (tmp sibling + rename), exactly the way the
/// shell hooks do, so a hook and the CLI can race without either reading a
/// half-written cursor. Best-effort: a failure never fails the verb.
fn write_marker(path: &Path, value: &str) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, format!("{value}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The session-id ladder (§12's harness-reach table, mirrored by
/// kb-wake.sh and kb-beat.sh): explicit flag → `KB_SESSION_ID` → the
/// `current-session` marker the SessionStart hook wrote.
pub(crate) fn resolve_session_id(flag: Option<&str>, env: Option<&str>) -> Option<String> {
    let pick = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(s) = flag.and_then(pick) {
        return Some(s);
    }
    if let Some(s) = env.and_then(pick) {
        return Some(s);
    }
    crate::session_marker::read_session_marker()
}

/// Rules matrix "`origin`": client-declared, never verified. Explicit
/// `--origin` wins; `--as you` means the operator; a `--job` post is the
/// dispatcher's import. `unattributed` is the ONE value a client cannot
/// send — the daemon stamps it when no session id resolves.
pub(crate) fn resolve_origin(
    origin: Option<&str>,
    as_who: Option<&str>,
    job: Option<&str>,
) -> Result<&'static str> {
    if let Some(o) = origin {
        return match o.trim() {
            "agent" => Ok("agent"),
            "human" => Ok("human"),
            "import" => Ok("import"),
            other => bail!(
                "--origin must be agent|human|import (got {other:?}) — `unattributed` is stamped by the daemon, never sent"
            ),
        };
    }
    if let Some(w) = as_who {
        return match w.trim() {
            "you" | "human" | "operator" => Ok("human"),
            other => bail!("--as takes `you` (the operator); got {other:?}"),
        };
    }
    if job.is_some() {
        return Ok("import");
    }
    Ok("agent")
}

// ---------------------------------------------------------------------------
// The resolved invocation: everything every verb shares
// ---------------------------------------------------------------------------

/// The common flags, resolved once. Built by [`Ctx::resolve`] so a verb
/// body never re-derives a slug, a session id or an origin.
pub struct Ctx {
    pub base: String,
    pub bearer: Option<String>,
    pub slug: String,
    /// Absolute working directory this post claims (`prov.cwd`), and the
    /// directory the slug was derived from.
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub origin: &'static str,
    pub job_id: Option<String>,
    /// Explicit `--topic`, else the marker `open --topic` wrote. Applied
    /// to POSTS only — a read is never silently narrowed (see the module
    /// doc's point 3).
    pub post_topic: Option<String>,
    /// Explicit `--topic` only: the `?topic=` filter on a read.
    pub read_topic: Option<String>,
    pub refs: Vec<String>,
    pub re: Option<u64>,
    pub supersedes: Option<u64>,
    pub body: Option<String>,
    pub json: bool,
    pub cache: Option<PathBuf>,
}

/// The flag bundle `main.rs` parses; kept as one struct so the dispatch
/// arm stays a single line per verb.
pub struct CommonArgs<'a> {
    pub slate: Option<&'a str>,
    pub cwd: Option<&'a Path>,
    pub topic: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub model: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub as_who: Option<&'a str>,
    pub job: Option<&'a str>,
    pub refs: &'a [String],
    pub re: Option<u64>,
    pub supersedes: Option<u64>,
    pub body: Option<&'a str>,
    pub daemon: Option<&'a str>,
    pub bearer: Option<&'a str>,
    pub json: bool,
}

impl Ctx {
    pub fn resolve(a: &CommonArgs<'_>) -> Result<Self> {
        let cwd_path: PathBuf = match a.cwd {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir().unwrap_or_default(),
        };
        // §12 "Shell slug drift": the slug ALWAYS comes from the git MAIN
        // checkout root, never from `--show-toplevel`, or every linked
        // worktree fragments into its own slate. `current_repo_slug_in` is
        // the same `--git-common-dir` ladder `kb recall --scope auto` uses.
        let slug = match a.slate {
            Some(s) => s.trim().to_string(),
            None => super::memory::current_repo_slug_in(&cwd_path),
        };
        if slug.is_empty() {
            bail!(
                "no slate slug — pass --slate <slug>, or run inside a git repo (the slug is the main checkout's basename)"
            );
        }
        let cache = cache_dir();
        let session_id =
            resolve_session_id(a.session_id, std::env::var("KB_SESSION_ID").ok().as_deref());
        let post_topic = match a.topic {
            Some(t) => Some(t.trim().to_string()),
            None => cache
                .as_deref()
                .zip(session_id.as_deref())
                .and_then(|(d, sid)| read_marker(&topic_file(d, sid))),
        };
        let harness = a
            .harness
            .map(str::to_string)
            .or_else(|| std::env::var("KB_HARNESS").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "claude".to_string());
        let mut refs: Vec<String> = a.refs.to_vec();
        if let Some(j) = a.job {
            // Rules matrix "Job provenance": the job id rides `prov.job_id`
            // AND a `job:` ref, NEVER `session_id` (invariant #11's join key).
            let r = format!("job:{j}");
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
        Ok(Ctx {
            base: a
                .daemon
                .unwrap_or(DEFAULT_DAEMON)
                .trim_end_matches('/')
                .to_string(),
            bearer: a.bearer.map(str::to_string),
            slug,
            cwd: Some(cwd_path.to_string_lossy().to_string()).filter(|s| !s.is_empty()),
            session_id,
            harness,
            model: a.model.map(str::to_string),
            origin: resolve_origin(a.origin, a.as_who, a.job)?,
            job_id: a.job.map(str::to_string),
            post_topic: post_topic.filter(|t| !t.is_empty()),
            read_topic: a
                .topic
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            refs,
            re: a.re,
            supersedes: a.supersedes,
            body: read_body_arg(a.body)?,
            json: a.json,
            cache,
        })
    }

    /// `prov` — the beat tuple every harness hook already carries, plus
    /// the origin. `prov.user` is stamped by the daemon, never sent.
    fn prov(&self) -> Value {
        let mut p = json!({ "harness": self.harness, "origin": self.origin });
        if let Some(s) = &self.session_id {
            p["session_id"] = json!(s);
        }
        if let Some(m) = &self.model {
            p["model"] = json!(m);
        }
        if let Some(c) = &self.cwd {
            p["cwd"] = json!(c);
        }
        if let Some(j) = &self.job_id {
            p["job_id"] = json!(j);
        }
        p
    }

    fn url(&self, tail: &str) -> String {
        format!(
            "{}/api/slates/{}{tail}",
            self.base,
            encode_path_segment(&self.slug)
        )
    }

    fn client(&self, secs: u64) -> Result<reqwest::Client> {
        client_with_timeout_and_bearer(secs, self.bearer.as_deref())
    }
}

// ---------------------------------------------------------------------------
// HTTP: one call helper, one refusal path (§9's code/holder contract)
// ---------------------------------------------------------------------------

/// Send a request; return the parsed body on 2xx, and on anything else
/// print the refusal and EXIT with §9's code. A refusal is never an
/// `anyhow` error: the exit status IS the contract, and letting it bubble
/// through `main` would collapse 2 and 3 into 1.
async fn call(ctx: &Ctx, req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
    let resp = req.send().await.with_context(|| what.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(body);
    }
    refuse(ctx.json, status, &body, &text, what)
}

/// The one refusal renderer. `--json` prints the problem+json body
/// VERBATIM (the machine contract: `code`, `holder`, `detail`, `status`);
/// human mode prints `detail` — which SL2 already formats as
/// `<code>: <text>` — plus the holder facts when a take was contested.
fn refuse(json_mode: bool, status: u16, body: &Value, raw: &str, what: &str) -> ! {
    let code = refusal_code(body).unwrap_or_default();
    if json_mode {
        match serde_json::to_string_pretty(body) {
            Ok(s) if !body.is_null() => println!("{s}"),
            _ => println!("{}", json!({ "status": status, "detail": raw })),
        }
    } else {
        let detail = body
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_else(|| raw.trim());
        if detail.is_empty() {
            eprintln!("{what}: HTTP {status}");
        } else {
            eprintln!("{detail}");
        }
        if let Some(h) = body.get("holder").filter(|h| !h.is_null()) {
            let seq = h.get("seq").and_then(Value::as_u64).unwrap_or(0);
            let harness = h.get("harness").and_then(Value::as_str).unwrap_or("?");
            let short = h
                .get("session_short")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let live = h.get("liveness").and_then(Value::as_str).unwrap_or("?");
            let age = h.get("age_secs").and_then(Value::as_i64).unwrap_or(0);
            eprintln!(
                "  holder: #{seq} {harness}/{short} ({live}, {})",
                kb_core::slate::fmt_age(age)
            );
        }
    }
    std::process::exit(exit_code_for(status, &code));
}

/// `GET`, with query pairs already encoded by reqwest.
async fn get(ctx: &Ctx, tail: &str, query: &[(&str, String)]) -> Result<Value> {
    let url = ctx.url(tail);
    let req = ctx.client(15)?.get(&url).query(query);
    call(ctx, req, &format!("GET {url}")).await
}

/// `POST …/posts` — the ONE mutation the twelve kinds share. Prints the
/// append response (post confirmation, displaced, nudge) unless `--json`,
/// which passes the whole envelope through.
async fn append(ctx: &Ctx, mut body: Value) -> Result<Value> {
    body["prov"] = ctx.prov();
    let url = ctx.url("/posts");
    let req = ctx.client(20)?.post(&url).json(&body);
    let resp = call(ctx, req, &format!("POST {url}")).await?;
    print_append(ctx, &resp)?;
    Ok(resp)
}

/// §5: "the append response is the poster's only feedback loop with the
/// finite surface" — so every mutating verb prints all three parts.
fn print_append(ctx: &Ctx, resp: &Value) -> Result<()> {
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(resp)?);
        return Ok(());
    }
    let post = resp.get("post").cloned().unwrap_or(Value::Null);
    println!("{}", render_posted(&post, &ctx.slug));
    let displaced: Vec<Value> = resp
        .get("displaced")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = resp
        .get("displaced_total")
        .and_then(Value::as_u64)
        .unwrap_or(displaced.len() as u64);
    if let Some(line) = render_displaced(&displaced, total) {
        println!("{line}");
    }
    if let Some(n) = resp.get("nudge").and_then(Value::as_str) {
        println!("{n}");
    }
    Ok(())
}

/// A post body with the kind, the split line/body, the shared topic and
/// the shared refs already applied.
fn post_body(ctx: &Ctx, kind: &str, raw_line: &str) -> Value {
    let (line, body) = split_line_body(raw_line, ctx.body.as_deref());
    let mut v = json!({ "kind": kind, "line": line });
    if let Some(b) = body {
        v["body"] = json!(b);
    }
    if let Some(t) = &ctx.post_topic {
        v["topic"] = json!(t);
    }
    if !ctx.refs.is_empty() {
        v["refs"] = json!(ctx.refs);
    }
    if let Some(n) = ctx.re {
        v["re"] = json!(n);
    }
    if let Some(n) = ctx.supersedes {
        v["supersedes"] = json!(n);
    }
    v
}

/// `--body -` reads stdin (the heredoc form every other kb verb accepts);
/// any other value is the Markdown body verbatim.
fn read_body_arg(raw: Option<&str>) -> Result<Option<String>> {
    match raw {
        None => Ok(None),
        Some("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read --body from stdin")?;
            Ok(Some(s))
        }
        Some(b) => Ok(Some(b.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Reads: open · delta · history · show · ls
// ---------------------------------------------------------------------------

/// `kb slate open [--topic T] [--budget N] [--all|--hybrid] [--json]`.
///
/// Prints the server's `text` VERBATIM (module doc point 1) and then
/// advances the cursor to `head_seq` — `open` is the seed for the whole
/// cursor lane, which is why it writes in EVERY mode, not just `--hybrid`.
pub async fn open(
    ctx: &Ctx,
    budget: Option<usize>,
    all: bool,
    hybrid: bool,
    declare_topic: bool,
) -> Result<()> {
    let mut q: Vec<(&str, String)> = Vec::new();
    if hybrid {
        q.push(("mode", "hybrid".to_string()));
    }
    if let Some(b) = budget {
        q.push(("budget", b.to_string()));
    }
    if all {
        q.push(("all", "1".to_string()));
    }
    if let Some(t) = &ctx.read_topic {
        q.push(("topic", t.clone()));
    }
    if let Some(s) = &ctx.session_id {
        q.push(("session", s.clone()));
    }
    // `since` drives the header's `seen #a → #b` clause only (rules matrix
    // "Cursor"): the digest itself is never filtered by it.
    if let Some(c) = ctx.read_cursor() {
        q.push(("since", c.to_string()));
    }
    let resp = get(ctx, "", &q).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else if let Some(t) = resp.get("text").and_then(Value::as_str) {
        print!("{t}");
    }
    ctx.advance_cursor(&resp);
    if declare_topic {
        ctx.write_topic_marker();
    }
    report_cursor(ctx, &resp).await;
    Ok(())
}

/// The `GET …/delta` query, PURE so `--kinds` (D26, v0.42) and every other
/// param's presence/absence is unit-tested without a client. `kinds` is
/// forwarded VERBATIM as a single `?kinds=` csv — the server owns parsing
/// and the 400 `bad-kind` on an unknown word.
pub(crate) fn delta_query_pairs(
    since: Option<u64>,
    limit: Option<usize>,
    budget: Option<usize>,
    session: Option<&str>,
    kinds: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut q: Vec<(&'static str, String)> = Vec::new();
    if let Some(s) = since {
        q.push(("since", s.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    if let Some(b) = budget {
        q.push(("budget", b.to_string()));
    }
    if let Some(s) = session {
        q.push(("session", s.to_string()));
    }
    if let Some(k) = kinds {
        q.push(("kinds", k.to_string()));
    }
    q
}

/// `kb slate delta [--since SEQ] [--limit N] [--kinds a,b] [--json]` — the
/// per-prompt lane. With no `--since` the cursor file IS the since, and the
/// cursor advances on success; with an explicit `--since` the file is never
/// touched (the caller owns its own bookkeeping). `--kinds` (D26, v0.42) is
/// forwarded verbatim as `?kinds=` — the server narrows both the posts and
/// the rendered `text`, and an unknown word comes back as its own 400
/// `bad-kind` (§9's ordinary EXIT_ERROR path, nothing kind-specific here).
pub async fn delta(
    ctx: &Ctx,
    since: Option<u64>,
    limit: Option<usize>,
    budget: Option<usize>,
    kinds: Option<&str>,
) -> Result<()> {
    let owns_cursor = since.is_none();
    let effective = since.or_else(|| ctx.read_cursor());
    let q = delta_query_pairs(effective, limit, budget, ctx.session_id.as_deref(), kinds);
    let resp = get(ctx, "/delta", &q).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else if let Some(t) = resp.get("text").and_then(Value::as_str) {
        print!("{t}");
    }
    if owns_cursor {
        ctx.advance_cursor(&resp);
    }
    report_cursor(ctx, &resp).await;
    Ok(())
}

impl Ctx {
    /// The client-side cursor for THIS session, or `None` (no session id,
    /// no cache dir, or a first read — the header then says `(first read)`).
    fn read_cursor(&self) -> Option<u64> {
        let dir = self.cache.as_deref()?;
        let sid = self.session_id.as_deref()?;
        read_marker(&cursor_file(dir, sid))?.trim().parse().ok()
    }

    /// Advance the cursor to the response's `head_seq`. "Seen" means the
    /// daemon served this session everything up to head — so an EMPTY read
    /// still advances, exactly as the shell hooks do, or the next prompt
    /// re-asks for a range that produced no text.
    fn advance_cursor(&self, resp: &Value) {
        let (Some(dir), Some(sid)) = (self.cache.as_deref(), self.session_id.as_deref()) else {
            return;
        };
        let Some(head) = resp.get("head_seq").and_then(Value::as_u64) else {
            return;
        };
        write_marker(&cursor_file(dir, sid), &head.to_string());
    }

    /// `open --topic T` declares the session's topic; every posting verb
    /// then defaults to it.
    fn write_topic_marker(&self) {
        let (Some(dir), Some(sid), Some(topic)) = (
            self.cache.as_deref(),
            self.session_id.as_deref(),
            self.read_topic.as_deref(),
        ) else {
            return;
        };
        write_marker(&topic_file(dir, sid), topic);
    }
}

// ---------------------------------------------------------------------------
// D27 (v0.42) — "seen is a reported cursor, never a read that writes". The
// CLI reports fire-and-forget after `open`/`delta`; `kb slate cursor` is the
// same report as an explicit, non-silent verb for adapters that bypass both.
// ---------------------------------------------------------------------------

/// The wire body a cursor report sends — PURE, so the "what gets posted"
/// contract is unit-tested without a daemon or a client.
pub(crate) fn cursor_report_body(session_id: &str, seq: u64, harness: &str) -> Value {
    json!({ "session_id": session_id, "seq": seq, "harness": harness })
}

/// Whether a fire-and-forget report should even be attempted — PURE, so
/// the skip rules are unit-tested without spinning up a client. A report
/// needs a session id to key the cursor on and a `head_seq` on the
/// response to report (every 2xx `open`/`delta` response carries one; a
/// non-2xx response never reaches this point at all, because [`call`]
/// exits the process on refusal before either caller's `Ok(resp)` return).
pub(crate) fn should_report_cursor(session_id: Option<&str>, resp: &Value) -> bool {
    session_id.is_some() && resp.get("head_seq").and_then(Value::as_u64).is_some()
}

/// After every SUCCESSFUL `open`/`delta`, POST the served `head_seq` as
/// this session's cursor (D27). Three properties:
///
/// 1. **Fire-and-forget.** A 1s client timeout; ANY failure (network,
///    non-2xx, timeout) is swallowed here — the caller already printed
///    its output and returns `Ok(())` regardless, so this can never move
///    the exit code or add a line to stdout/stderr.
/// 2. **Skipped when there is nothing to report.** No session id resolved
///    (nothing to key the cursor on), or no `head_seq` on the response.
/// 3. **Skipped on an unknown slate for free.** `open`/`delta` route every
///    non-2xx response through [`call`]'s [`refuse`], which prints and
///    calls `std::process::exit` before either caller's `Ok(resp)`
///    return — so a 404 (open on a slate that has never been posted to)
///    never reaches this function at all.
async fn report_cursor(ctx: &Ctx, resp: &Value) {
    if !should_report_cursor(ctx.session_id.as_deref(), resp) {
        return;
    }
    let sid = ctx.session_id.as_deref().unwrap_or_default();
    let head = resp
        .get("head_seq")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let Ok(client) = ctx.client(1) else { return };
    let body = cursor_report_body(sid, head, &ctx.harness);
    let _ = client.post(ctx.url("/cursor")).json(&body).send().await;
}

/// `kb slate cursor [--seq N]` — an explicit cursor report for adapters
/// that don't route through `open`/`delta` (D27). Unlike [`report_cursor`]
/// this is an ordinary mutating verb: it prints and exits per §9's ladder
/// on failure, because a caller invoking it BY NAME wants to know whether
/// it landed. Defaults `--seq` to the local cursor marker's value (the
/// same file `open`/`delta` write) so `kb slate cursor` with no arguments
/// reports exactly what the last read already saw.
pub async fn cursor(ctx: &Ctx, seq: Option<u64>) -> Result<()> {
    let sid = ctx.session_id.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no session id resolved — pass --session-id, set KB_SESSION_ID, or run inside a harness session"
        )
    })?;
    let seq = match seq {
        Some(s) => s,
        None => ctx.read_cursor().ok_or_else(|| {
            anyhow::anyhow!(
                "no local cursor yet for this session — pass --seq, or run `kb slate open`/`kb slate delta` first"
            )
        })?,
    };
    let body = cursor_report_body(&sid, seq, &ctx.harness);
    let url = ctx.url("/cursor");
    let req = ctx.client(20)?.post(&url).json(&body);
    let resp = call(ctx, req, &format!("POST {url}")).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let stored = resp.get("seq").and_then(Value::as_u64).unwrap_or(seq);
    let head = resp.get("head_seq").and_then(Value::as_u64).unwrap_or(0);
    println!("{}: cursor #{stored} reported (head #{head})", ctx.slug);
    Ok(())
}

/// `kb slate history [--since SEQ] [--limit N]` — what was dropped or
/// edited away, by whom, why (§9). Newest first, current generation only.
pub async fn history(ctx: &Ctx, since: Option<u64>, limit: Option<usize>) -> Result<()> {
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(s) = since {
        q.push(("since", s.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    let resp = get(ctx, "/history", &q).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let rows = resp.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("nothing removed from {} yet", ctx.slug);
        return Ok(());
    }
    for r in &rows {
        let post = r.get("post").cloned().unwrap_or(Value::Null);
        let reason = r.get("reason").and_then(Value::as_str).unwrap_or("hidden");
        let who = r.get("who").and_then(Value::as_str).unwrap_or("?");
        let by = r.get("hidden_by").and_then(Value::as_u64).unwrap_or(0);
        let why = r
            .get("why")
            .and_then(Value::as_str)
            .map(|w| format!(": \"{w}\""))
            .unwrap_or_default();
        println!("{}", render_post_row(&post));
        println!("    {reason} by {who} (#{by}){why}");
    }
    Ok(())
}

/// `kb slate show #n` — one post unfolded: its body, its resolved refs and
/// the thread beneath it (§9).
pub async fn show(ctx: &Ctx, key: &str) -> Result<()> {
    let tail = format!(
        "/posts/{}",
        encode_path_segment(key.trim_start_matches('#'))
    );
    let resp = get(ctx, &tail, &[]).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let post = resp.get("post").cloned().unwrap_or(Value::Null);
    println!("{}", render_post_row(&post));
    if let Some(s) = post.get("subject").and_then(Value::as_str) {
        println!("    subject: {s}");
    }
    if let Some(b) = post.get("body").and_then(Value::as_str) {
        for l in b.lines() {
            println!("    {l}");
        }
    }
    for r in resp
        .get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let raw = r.get("raw").and_then(Value::as_str).unwrap_or("?");
        let display = r.get("display").and_then(Value::as_str).unwrap_or(raw);
        let mark = if r.get("resolved").and_then(Value::as_bool) == Some(true) {
            "→"
        } else {
            "·"
        };
        println!("    ref {mark} {raw}  {display}");
    }
    let thread = resp
        .get("thread")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !thread.is_empty() {
        println!("  thread ({}):", thread.len());
        for t in &thread {
            println!("    {}", render_post_row(t));
        }
    }
    Ok(())
}

/// `kb slate ls` — every slate on the daemon (`GET /api/slates`). The
/// attention number is `hand_unack + ask_open + take_contested +
/// take_stale`, summed CLIENT-side (§9: the daemon never sums an
/// attention number for you).
pub(crate) fn attention_of(counts: &Value) -> u64 {
    ["hand_unack", "ask_open", "take_contested", "take_stale"]
        .iter()
        .map(|k| counts.get(*k).and_then(Value::as_u64).unwrap_or(0))
        .sum()
}

pub async fn ls(ctx: &Ctx) -> Result<()> {
    let url = format!("{}/api/slates", ctx.base);
    let req = ctx.client(15)?.get(&url);
    let resp = call(ctx, req, &format!("GET {url}")).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let rows = resp.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no slates yet — `kb slate now \"<what you're on>\"` opens one");
        return Ok(());
    }
    println!(
        "{:<24} {:>6} {:>4} {:>5}  topics",
        "slate", "head", "gen", "att"
    );
    for r in &rows {
        let slug = r.get("slug").and_then(Value::as_str).unwrap_or("?");
        let head = r.get("head_seq").and_then(Value::as_u64).unwrap_or(0);
        let gen = r.get("generation").and_then(Value::as_u64).unwrap_or(1);
        let att = attention_of(r.get("counts").unwrap_or(&Value::Null));
        let closed = if r.get("closed").and_then(Value::as_bool) == Some(true) {
            " (closed)"
        } else {
            ""
        };
        let topics: Vec<&str> = r
            .get("topics")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        println!(
            "{slug:<24} {:>6} {gen:>4} {att:>5}  {}{closed}",
            format!("#{head}"),
            topics.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Posting verbs (§9's fence). Each is one `append` with its kind's fields.
// ---------------------------------------------------------------------------

/// `now` / `warn` / `idea` — line-only kinds.
pub async fn plain(ctx: &Ctx, kind: &str, line: &str) -> Result<()> {
    append(ctx, post_body(ctx, kind, line)).await.map(|_| ())
}

/// `ask "…?"` — the `?` is enforced server-side (`ask-needs-question`).
pub async fn ask(ctx: &Ctx, line: &str) -> Result<()> {
    append(ctx, post_body(ctx, "ask", line)).await.map(|_| ())
}

/// `found "…" --ref R` — a found with no ref is a 400 by design
/// ("post it as idea if it is a guess").
pub async fn found(ctx: &Ctx, line: &str) -> Result<()> {
    append(ctx, post_body(ctx, "found", line)).await.map(|_| ())
}

/// `answer #n "…"` — requires an `ask` (rules matrix "Re-target matrix").
pub async fn answer(ctx: &Ctx, target: u64, line: &str) -> Result<()> {
    let mut b = post_body(ctx, "answer", line);
    b["re"] = json!(target);
    append(ctx, b).await.map(|_| ())
}

/// `tried "…" --failed "…" [--was #n]`. `--was` APPENDS `(was #n)` to the
/// line and drops nothing (rules matrix / D22: crossing out is a post).
pub async fn tried(ctx: &Ctx, line: &str, failed: Option<&str>, was: Option<u64>) -> Result<()> {
    let line = match was {
        Some(n) => format!("{} (was #{n})", line.trim_end()),
        None => line.to_string(),
    };
    let mut b = post_body(ctx, "tried", &line);
    if let Some(f) = failed {
        b["failed"] = json!(f);
    }
    append(ctx, b).await.map(|_| ())
}

/// `take <subject> "…" [--anyway|--over #n]`. When the subject parses as
/// `#n` the take INHERITS an open hand's subject and carries `re: n`
/// (rules matrix "`take #n` on a hand") — the server owns that rule; the
/// CLI just routes the argument to the right field.
pub async fn take(
    ctx: &Ctx,
    subject: &str,
    line: &str,
    anyway: bool,
    over: Option<u64>,
) -> Result<()> {
    let mut b = post_body(ctx, "take", line);
    match subject
        .trim()
        .strip_prefix('#')
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(n) => b["re"] = json!(n),
        None => b["subject"] = json!(subject.trim()),
    }
    if anyway {
        b["anyway"] = json!(true);
    }
    if let Some(n) = over {
        b["over"] = json!(n);
    }
    append(ctx, b).await.map(|_| ())
}

/// `hand <subject> "…" [--to H]` — the packet convention; the CLI moves
/// everything after the first newline into `body` (rules matrix "Hand
/// packet").
pub async fn hand(ctx: &Ctx, subject: &str, line: &str, to: Option<&str>) -> Result<()> {
    let mut b = post_body(ctx, "hand", line);
    b["subject"] = json!(subject.trim());
    if let Some(t) = to {
        b["to"] = json!(t);
    }
    append(ctx, b).await.map(|_| ())
}

/// `done #n "…" [--abandoned "<state>"]` — `--abandoned` keeps the target
/// OPEN with your state attached (rules matrix "Re-target matrix").
pub async fn done(ctx: &Ctx, target: u64, line: &str, abandoned: Option<&str>) -> Result<()> {
    let mut b = post_body(ctx, "done", line);
    b["re"] = json!(target);
    if let Some(a) = abandoned {
        b["abandoned"] = json!(a);
    }
    append(ctx, b).await.map(|_| ())
}

/// `drop #n "…" [--anyway]` — the attributed tombstone (D17).
pub async fn drop_post(ctx: &Ctx, target: u64, line: &str, anyway: bool) -> Result<()> {
    let mut b = post_body(ctx, "drop", line);
    b["re"] = json!(target);
    if anyway {
        b["anyway"] = json!(true);
    }
    append(ctx, b).await.map(|_| ())
}

/// `mark #n [--pin|--unpin]` — circle someone else's post. `pin`/`unpin`
/// are the wire form of the same kind and need `origin: human`
/// (`--as you`); an agent that sends one gets SL2's 400 `pin-is-human`,
/// printed as-is.
pub async fn mark(ctx: &Ctx, target: u64, line: Option<&str>, pin: Option<bool>) -> Result<()> {
    let default = match pin {
        Some(true) => "pin",
        Some(false) => "unpin",
        None => "+1",
    };
    let mut b = post_body(ctx, "mark", line.unwrap_or(default));
    b["re"] = json!(target);
    if let Some(p) = pin {
        b["pin"] = json!(p);
    }
    append(ctx, b).await.map(|_| ())
}

/// Build an `edit`'s superseding post from the target it replaces. PURE,
/// so the copy rule ("kind, topic, subject and refs unless overridden") is
/// unit-tested without a daemon.
pub(crate) fn edit_body(
    target: &Value,
    seq: u64,
    line: String,
    body: Option<String>,
    topic_override: Option<&str>,
    refs_override: &[String],
    anyway: bool,
) -> Value {
    let mut v = json!({
        "kind": target.get("kind").cloned().unwrap_or(json!("now")),
        "line": line,
        "supersedes": seq,
    });
    if let Some(b) = body {
        v["body"] = json!(b);
    }
    let topic = topic_override.map(str::to_string).or_else(|| {
        target
            .get("topic")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    if let Some(t) = topic {
        v["topic"] = json!(t);
    }
    if let Some(s) = target.get("subject").and_then(Value::as_str) {
        v["subject"] = json!(s);
    }
    let refs: Vec<String> = if refs_override.is_empty() {
        target
            .get("refs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        refs_override.to_vec()
    };
    if !refs.is_empty() {
        v["refs"] = json!(refs);
    }
    if anyway {
        v["anyway"] = json!(true);
    }
    v
}

/// `kb slate edit #n "…" [--body -|<md>] [--anyway]` — client-side sugar
/// (§9): read #n, copy its kind/topic/subject/refs unless overridden, and
/// post a superseding post of the SAME kind (a kind mismatch is SL2's 400).
pub async fn edit(ctx: &Ctx, seq: u64, line: &str, anyway: bool) -> Result<()> {
    let detail = get(ctx, &format!("/posts/{seq}"), &[]).await?;
    let target = detail.get("post").cloned().unwrap_or(Value::Null);
    let (line, body) = split_line_body(line, ctx.body.as_deref());
    let body = body.or_else(|| {
        target
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let v = edit_body(
        &target,
        seq,
        line,
        body,
        ctx.read_topic.as_deref(),
        &ctx.refs,
        anyway,
    );
    append(ctx, v).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// Lifecycle: close · reopen · rotate (rules matrix "Rotate and close")
// ---------------------------------------------------------------------------

pub async fn lifecycle(ctx: &Ctx, verb: &str, line: Option<&str>) -> Result<()> {
    let url = ctx.url(&format!("/{verb}"));
    let mut req = ctx.client(20)?.post(&url);
    if verb == "close" {
        // `close "<final now>"` appends a terminal `now` with topic null,
        // so it carries provenance like any other post.
        req = req.json(&json!({ "line": line.unwrap_or("closed"), "prov": ctx.prov() }));
    }
    let resp = call(ctx, req, &format!("POST {url}")).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let head = resp.get("head_seq").and_then(Value::as_u64).unwrap_or(0);
    let gen = resp.get("generation").and_then(Value::as_u64).unwrap_or(1);
    let closed = resp.get("closed").and_then(Value::as_bool) == Some(true);
    let from = resp
        .get("rotated_from")
        .and_then(Value::as_u64)
        .map(|g| format!(" (rotated from generation {g})"))
        .unwrap_or_default();
    let past = match verb {
        "close" => "closed",
        "reopen" => "reopened",
        _ => "rotated",
    };
    println!(
        "{} {past} — head #{head}, generation {gen}{from}{}",
        ctx.slug,
        if closed { " · closed" } else { "" }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// promote (§13 "Promotion, lessons live once") — a CLI COMPOSITION over
// verbs that already exist. The daemon never authors the memory text.
// ---------------------------------------------------------------------------

/// The dated plan-file line `--to plan` appends. Pure so the format is
/// pinned without touching the filesystem.
pub(crate) fn plan_line(date: &str, slug: &str, seq: u64, line: &str) -> String {
    format!("- {date}: {line} (slate {slug} #{seq})")
}

/// `kb slate promote #n --to memory|note|plan` — runs the existing write
/// (`kb remember` / `kb notes append` / a plan-file append) and then posts
/// `done #n "promoted → …"` so the slate keeps a pointer (§13).
#[allow(clippy::too_many_arguments)]
pub async fn promote(
    ctx: &Ctx,
    seq: u64,
    to: &str,
    plan: Option<&Path>,
    kb: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    let detail = get(ctx, &format!("/posts/{seq}"), &[]).await?;
    let post = detail.get("post").cloned().unwrap_or(Value::Null);
    let line = post
        .get("line")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let body = post.get("body").and_then(Value::as_str).unwrap_or_default();
    let text = if body.is_empty() {
        line.clone()
    } else {
        format!("{line}\n\n{body}")
    };

    let pointer = match to.trim() {
        "memory" => {
            // The SAME write `kb remember` performs, called in-process so
            // the minted id can close the loop below, carrying `--session-id`
            // (invariant #11's join key). `link` is left to MI-W0.2's own
            // ladder in `resolve_memory_kb`, which ALREADY links a memory to
            // a kb named exactly this repo slug when one exists — passing
            // `--link <slug>` outright would 400 on every project that has no
            // kb of that name, which is most of them.
            let written = super::memory::remember_inner(
                &text,
                None,
                None,
                kb,
                None,
                "memory-user",
                None,
                None,
                None,
                None,
                None,
                None,
                false,
                ctx.session_id.as_deref(),
                false,
                false,
                None,
                Some(&ctx.base),
                ctx.bearer.as_deref(),
            )
            .await?;
            let id = written
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            if !ctx.json {
                println!(
                    "remembered {id}  ({})",
                    written.get("path").and_then(Value::as_str).unwrap_or("?")
                );
            }
            format!("mem {id}")
        }
        "note" => {
            let target = note.ok_or_else(|| {
                anyhow::anyhow!("--to note needs --note <id|path|title> — which note to append to")
            })?;
            super::notes::append(kb, target, &line, Some(&ctx.base), ctx.bearer.as_deref()).await?;
            format!("note {target}")
        }
        "plan" => {
            let path = plan
                .map(Path::to_path_buf)
                .or_else(|| std::env::var_os("KB_PLAN_FILE").map(PathBuf::from))
                .ok_or_else(|| anyhow::anyhow!("--to plan needs --plan <path> or $KB_PLAN_FILE"))?;
            let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let entry = plan_line(&date, &ctx.slug, seq, &line);
            let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
            if !existing.is_empty() && !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(&entry);
            existing.push('\n');
            std::fs::write(&path, existing)
                .with_context(|| format!("append to plan file {}", path.display()))?;
            if !ctx.json {
                println!("{}  ← {entry}", path.display());
            }
            format!("plan {}", path.display())
        }
        other => bail!("--to must be memory|note|plan (got {other:?})"),
    };

    done(ctx, seq, &format!("promoted → {pointer}"), None).await
}

// ---------------------------------------------------------------------------
// watch — seed-then-diff over `slate.updated` (the comments_watch shape)
// ---------------------------------------------------------------------------

/// The SSE query §9 pins: `slug:` is the fourth token SL2 added to the
/// events filter grammar, and the payload's own `slug` is re-checked
/// client-side (a filter is an optimisation, never the authority).
pub(crate) fn watch_query(slug: &str) -> String {
    format!(
        "types=slate.updated&filter=slug:{}",
        encode_path_segment(slug)
    )
}

/// `kb slate watch [--once] [--timeout S] [--json]`.
///
/// Seed-then-diff: the seen set is seeded from the CURRENT head, and every
/// wake-up refetches `…/posts?since=<last>` rather than trusting the event
/// payload — so the connect-time replay is idempotent by construction and a
/// reconnect gap can never swallow a post. Your OWN session's posts are
/// skipped, which is what stops the loop reacting to itself.
pub async fn watch(ctx: &Ctx, once: bool, timeout_secs: Option<u64>) -> Result<()> {
    let mut last: u64 = {
        let head = get(ctx, "", &[("all", "1".to_string())]).await?;
        head.get("head_seq").and_then(Value::as_u64).unwrap_or(0)
    };
    let mut seen: HashSet<u64> = HashSet::new();
    eprintln!("[kb slate watch] {} from #{last}", ctx.slug);

    let deadline = timeout_secs.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    let query = watch_query(&ctx.slug);
    let mut backoff = 1u64;
    loop {
        let stream = open_events_stream(&ctx.base, ctx.bearer.as_deref(), None, &query).await;
        let resp = match stream {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[kb slate watch] {e}; retrying in {backoff}s …");
                if sleep_or_done(deadline, backoff).await {
                    return Ok(());
                }
                backoff = (backoff * 2).min(30);
                continue;
            }
        };
        backoff = 1;
        let mut reader = FrameReader::from_response(resp);
        // Drain whatever landed while we were away, then follow.
        if drain(ctx, &mut last, &mut seen, once).await? && once {
            return Ok(());
        }
        loop {
            let frame = tokio::select! {
                biased;
                _ = sleep_until_opt(deadline) => return Ok(()),
                f = reader.next_frame() => f?,
            };
            let Some(frame) = frame else { break };
            if frame.event.as_deref() != Some("slate.updated") {
                continue;
            }
            let envelope: Value =
                serde_json::from_str(frame.data.as_deref().unwrap_or("{}")).unwrap_or(Value::Null);
            let payload = envelope.get("payload").unwrap_or(&envelope);
            if payload.get("slug").and_then(Value::as_str) != Some(ctx.slug.as_str()) {
                continue;
            }
            if drain(ctx, &mut last, &mut seen, once).await? && once {
                return Ok(());
            }
        }
        eprintln!("[kb slate watch] stream closed; reconnecting in {backoff}s …");
        if sleep_or_done(deadline, backoff).await {
            return Ok(());
        }
        backoff = (backoff * 2).min(30);
    }
}

/// Fetch everything past `last` and emit what this session hasn't seen.
/// Returns true when anything was emitted.
async fn drain(ctx: &Ctx, last: &mut u64, seen: &mut HashSet<u64>, once: bool) -> Result<bool> {
    let rows = get(ctx, "/posts", &[("since", last.to_string())]).await?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    let mut emitted = false;
    for p in rows {
        let seq = p.get("seq").and_then(Value::as_u64).unwrap_or(0);
        *last = (*last).max(seq);
        if !seen.insert(seq) {
            continue;
        }
        let mine = ctx.session_id.is_some()
            && p.get("prov")
                .and_then(|v| v.get("session_id"))
                .and_then(Value::as_str)
                == ctx.session_id.as_deref();
        if mine {
            continue;
        }
        if ctx.json {
            println!("{}", serde_json::to_string(&p)?);
        } else {
            println!("{}", render_post_row(&p));
        }
        emitted = true;
        if once {
            return Ok(true);
        }
    }
    Ok(emitted)
}

/// Sleep `secs`, or return true when the idle deadline fires first.
async fn sleep_or_done(deadline: Option<tokio::time::Instant>, secs: u64) -> bool {
    tokio::select! {
        biased;
        _ = sleep_until_opt(deadline) => true,
        _ = tokio::time::sleep(Duration::from_secs(secs)) => false,
    }
}

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

// ---------------------------------------------------------------------------
// stats (§13 "Metrics without a benchmark") — every number is COUNTED off
// the ledger and the projection; nothing here is a quality verdict.
// ---------------------------------------------------------------------------

fn kind_is(p: &Value, k: &str) -> bool {
    p.get("kind").and_then(Value::as_str) == Some(k)
}

fn seq_of(p: &Value) -> u64 {
    p.get("seq").and_then(Value::as_u64).unwrap_or(0)
}

fn re_of(p: &Value) -> Option<u64> {
    p.get("re").and_then(Value::as_u64)
}

fn section<'a>(digest: &'a Value, name: &str) -> &'a [Value] {
    digest
        .get("sections")
        .and_then(|s| s.get(name))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn total(digest: &Value, name: &str) -> u64 {
    digest.get(name).and_then(Value::as_u64).unwrap_or(0)
}

/// Sorted `name count` pairs, descending by count then by name, so the
/// output is byte-stable for a golden.
fn tally(pairs: Vec<String>) -> Vec<(String, usize)> {
    let mut map: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for p in pairs {
        *map.entry(p).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Every number `kb slate stats` reports (§13 "Metrics without a
/// benchmark"). One struct so `--json` and the human render are the SAME
/// arithmetic — the human render adds no fact the JSON lacks. Nothing here
/// is a quality verdict: these are counts, and "fewer abandonments, not
/// speed, is what to watch".
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SlateStats {
    pub slug: String,
    pub generation: u64,
    pub head_seq: u64,
    pub ledger_posts: usize,
    pub hands_offered: usize,
    pub hands_acknowledged: usize,
    pub hands_unacknowledged: u64,
    pub takes_opened: usize,
    pub takes_done: usize,
    pub takes_live: usize,
    pub takes_stale: usize,
    pub takes_expired: usize,
    pub takes_contested: usize,
    pub asks_asked: usize,
    pub asks_answered: usize,
    pub asks_open: u64,
    pub tried_posted: usize,
    pub tried_naming_failure: usize,
    /// `tried --was #n` — D22's crossing-out, counted off the `(was #n)`
    /// the CLI appends to the line.
    pub tried_crossing_out: usize,
    /// The observable "duplicate work avoided" signal: a dead end another
    /// SESSION marked. A mark is never your own post, so this can only be
    /// someone else saying "this saved me".
    pub tried_echoed_by_others: usize,
    pub found: usize,
    pub idea: usize,
    pub found_idea_shown: u64,
    pub found_idea_dropped: u64,
    pub drops: usize,
    pub marks: usize,
    pub done_abandoned: usize,
    pub by_harness: Vec<(String, usize)>,
    pub by_session: Vec<(String, usize)>,
}

/// PURE over the two reads `stats` needs: the `--all` digest (open state,
/// liveness, marks) and the raw ledger (lifetime counts, provenance).
pub(crate) fn compute_stats(slug: &str, digest: &Value, posts: &[Value]) -> SlateStats {
    let count = |k: &str| posts.iter().filter(|p| kind_is(p, k)).count();
    let dropped: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "drop"))
        .filter_map(re_of)
        .collect();
    // `--abandoned` never closes its target (rules matrix "Re-target
    // matrix"), and a dropped `done` closes nothing either.
    let closed: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "done") && p.get("abandoned").map(Value::is_null).unwrap_or(true))
        .filter(|p| !dropped.contains(&seq_of(p)))
        .filter_map(re_of)
        .collect();
    let take_seqs: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "take"))
        .map(seq_of)
        .collect();
    let hand_seqs: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "hand"))
        .map(seq_of)
        .collect();
    let ask_seqs: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "ask"))
        .map(seq_of)
        .collect();
    let tried_seqs: HashSet<u64> = posts
        .iter()
        .filter(|p| kind_is(p, "tried"))
        .map(seq_of)
        .collect();

    // A hand is acknowledged when a take names it (rules matrix "`take #n`
    // on a hand") or a done closes it.
    let hands_acknowledged = hand_seqs
        .iter()
        .filter(|h| {
            closed.contains(h)
                || posts
                    .iter()
                    .any(|p| kind_is(p, "take") && re_of(p) == Some(**h))
        })
        .count();
    let asks_answered = ask_seqs
        .iter()
        .filter(|a| {
            posts
                .iter()
                .any(|p| kind_is(p, "answer") && re_of(p) == Some(**a))
        })
        .count();
    let tried_echoed_by_others = tried_seqs
        .iter()
        .filter(|t| {
            posts
                .iter()
                .any(|p| kind_is(p, "mark") && re_of(p) == Some(**t))
        })
        .count();
    let liveness = |want: &str| {
        section(digest, "take")
            .iter()
            .filter(|t| t.get("liveness").and_then(Value::as_str) == Some(want))
            .count()
    };
    let author = |p: &Value| -> (String, String) {
        let prov = p.get("prov");
        let h = prov
            .and_then(|v| v.get("harness"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let s = match prov
            .and_then(|v| v.get("session_id"))
            .and_then(Value::as_str)
        {
            Some(sid) => format!("{h}/{}", short_session(sid)),
            None => format!("{h}/(no session)"),
        };
        (h, s)
    };

    SlateStats {
        slug: slug.to_string(),
        generation: digest
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(1),
        head_seq: digest.get("head_seq").and_then(Value::as_u64).unwrap_or(0),
        ledger_posts: posts.len(),
        hands_offered: hand_seqs.len(),
        hands_acknowledged,
        hands_unacknowledged: total(digest, "hand_total"),
        takes_opened: take_seqs.len(),
        takes_done: take_seqs.iter().filter(|s| closed.contains(s)).count(),
        takes_live: liveness("live"),
        takes_stale: liveness("stale"),
        takes_expired: liveness("expired"),
        takes_contested: section(digest, "take")
            .iter()
            .filter(|t| t.get("contested").and_then(Value::as_bool) == Some(true))
            .count(),
        asks_asked: ask_seqs.len(),
        asks_answered,
        asks_open: total(digest, "ask_total"),
        tried_posted: tried_seqs.len(),
        tried_naming_failure: posts
            .iter()
            .filter(|p| kind_is(p, "tried"))
            .filter(|p| p.get("failed").is_some_and(|f| !f.is_null()))
            .count(),
        tried_crossing_out: posts
            .iter()
            .filter(|p| kind_is(p, "tried"))
            .filter(|p| {
                p.get("line")
                    .and_then(Value::as_str)
                    .is_some_and(|l| l.contains("(was #"))
            })
            .count(),
        tried_echoed_by_others,
        found: count("found"),
        idea: count("idea"),
        found_idea_shown: total(digest, "found_idea_total"),
        found_idea_dropped: digest
            .get("dropped")
            .and_then(|d| d.get("found_idea"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        drops: count("drop"),
        marks: count("mark"),
        done_abandoned: posts
            .iter()
            .filter(|p| kind_is(p, "done"))
            .filter(|p| p.get("abandoned").is_some_and(|a| !a.is_null()))
            .count(),
        by_harness: tally(posts.iter().map(|p| author(p).0).collect()),
        by_session: tally(posts.iter().map(|p| author(p).1).collect()),
    }
}

/// The human render — a presenter over [`SlateStats`], adding no fact.
pub(crate) fn render_stats(s: &SlateStats) -> String {
    let pairs = |rows: &[(String, usize)]| {
        rows.iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let mut out = String::new();
    out.push_str(&format!(
        "{} · generation {} · head #{} · {} posts on the ledger\n\n",
        s.slug, s.generation, s.head_seq, s.ledger_posts
    ));
    out.push_str(&format!(
        "hands   {} offered · {} acknowledged · {} still unacknowledged\n",
        s.hands_offered, s.hands_acknowledged, s.hands_unacknowledged
    ));
    out.push_str(&format!(
        "takes   {} opened · {} done · {} live · {} stale · {} expired · {} contested now\n",
        s.takes_opened,
        s.takes_done,
        s.takes_live,
        s.takes_stale,
        s.takes_expired,
        s.takes_contested
    ));
    out.push_str(&format!(
        "asks    {} asked · {} answered · {} still open\n",
        s.asks_asked, s.asks_answered, s.asks_open
    ));
    out.push_str(&format!(
        "tried   {} posted · {} naming what failed · {} crossing out a prior post · {} echoed by another session\n",
        s.tried_posted, s.tried_naming_failure, s.tried_crossing_out, s.tried_echoed_by_others
    ));
    out.push_str(&format!(
        "found   {} found · {} idea · {} shown · {} dropped\n",
        s.found, s.idea, s.found_idea_shown, s.found_idea_dropped
    ));
    out.push_str(&format!(
        "tidy    {} dropped · {} marked · {} done --abandoned\n",
        s.drops, s.marks, s.done_abandoned
    ));
    out.push_str(&format!("by harness  {}\n", pairs(&s.by_harness)));
    out.push_str(&format!("by session  {}\n", pairs(&s.by_session)));
    out
}

pub async fn stats(ctx: &Ctx) -> Result<()> {
    let digest = get(ctx, "", &[("all", "1".to_string())]).await?;
    let posts = get(ctx, "/posts", &[]).await?;
    let posts = posts.as_array().cloned().unwrap_or_default();
    let stats = compute_stats(&ctx.slug, &digest, &posts);
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }
    print!("{}", render_stats(&stats));
    Ok(())
}

// ---------------------------------------------------------------------------
// doctor (§13: "`kb slate doctor` lists what the daemon can flag
// structurally") — a LINT, never a tidy. It proposes; `/kb-slate-tidy`
// (SL5) and the operator dispose. Exit status is always 0.
// ---------------------------------------------------------------------------

/// Prefixes that are a credential by construction, whatever surrounds them.
const TOKEN_PREFIXES: [&str; 11] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "authelia_at_",
];

/// Words that turn a following opaque run into a credential.
const SECRET_WORDS: [&str; 6] = ["token", "secret", "password", "passwd", "apikey", "api_key"];

/// Shortest opaque run that counts as token-shaped after a secret word.
const OPAQUE_MIN: usize = 24;

/// Does this text LOOK like it carries a credential? A lint, deliberately
/// biased toward false positives: §12's dispatcher bridge secret-scans the
/// brief the digest is prepended to, and a token-shaped slate line would
/// abort an offload with exit 2 before any job exists. Naming it here is
/// cheaper than debugging that. Returns the reason, so the report says WHY.
pub(crate) fn looks_token_shaped(s: &str) -> Option<&'static str> {
    if s.contains("-----BEGIN ") && s.contains("PRIVATE KEY") {
        return Some("a PEM private key block");
    }
    for p in TOKEN_PREFIXES {
        if s.contains(p) {
            return Some("a known credential prefix");
        }
    }
    let lower = s.to_ascii_lowercase();
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'=' && *b != b':' {
            continue;
        }
        // A secret word within the 24 characters before the separator …
        let from = i.saturating_sub(OPAQUE_MIN);
        let before = &lower[from..i];
        if !SECRET_WORDS.iter().any(|w| before.contains(w)) {
            continue;
        }
        // … followed by a long opaque run (quotes and spaces skipped).
        let run: String = s[i + 1..]
            .chars()
            .skip_while(|c| matches!(c, ' ' | '"' | '\'' | '\t'))
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
            .collect();
        if run.chars().count() >= OPAQUE_MIN {
            return Some("a secret-shaped value after a token/secret/password key");
        }
    }
    None
}

/// Every structural complaint, in a fixed order so the report is stable.
/// PURE over the same two reads `stats` uses.
pub(crate) fn render_doctor(digest: &Value, posts: &[Value]) -> Vec<String> {
    use kb_core::slate as core;
    let mut out = Vec::new();

    for t in section(digest, "take") {
        if t.get("contested").and_then(Value::as_bool) == Some(true) {
            out.push(format!(
                "contested take #{} — two live sessions hold overlapping subjects: {}",
                t.get("seq").and_then(Value::as_u64).unwrap_or(0),
                t.get("line").and_then(Value::as_str).unwrap_or_default()
            ));
        }
    }
    for h in section(digest, "hand") {
        let age = h.get("age_secs").and_then(Value::as_i64).unwrap_or(0);
        if h.get("acknowledged").and_then(Value::as_bool) == Some(false) && age > 3_600 {
            out.push(format!(
                "hand #{} unacknowledged for {} — nobody is picking it up: take it or drop it",
                h.get("seq").and_then(Value::as_u64).unwrap_or(0),
                core::fmt_age(age)
            ));
        }
    }
    for a in section(digest, "ask") {
        let answers = a.get("answers").and_then(Value::as_u64).unwrap_or(0);
        if answers > 0 {
            out.push(format!(
                "ask #{} has {answers} answer(s) and is still open — `kb slate done #{} \"<what settled it>\"`",
                a.get("seq").and_then(Value::as_u64).unwrap_or(0),
                a.get("seq").and_then(Value::as_u64).unwrap_or(0)
            ));
        }
    }
    let near = |have: u64, cap: usize| have * 5 >= (cap as u64) * 4;
    for (name, have, cap, remedy) in [
        (
            "open warns",
            total(digest, "warn_total"),
            core::MAX_OPEN_WARNS,
            "drop one that no longer applies",
        ),
        (
            "open asks",
            total(digest, "ask_total"),
            core::MAX_OPEN_ASKS,
            "answer or drop one",
        ),
        (
            "unacknowledged hands",
            total(digest, "hand_total"),
            core::MAX_OPEN_HANDS,
            "take or drop one",
        ),
    ] {
        if near(have, cap) {
            out.push(format!("{have} {name} (cap {cap}) — {remedy}"));
        }
    }
    if near(posts.len() as u64, core::LEDGER_MAX_POSTS) {
        out.push(format!(
            "{} posts on the ledger (cap {}) — `kb slate rotate` archives it and starts a fresh generation",
            posts.len(),
            core::LEDGER_MAX_POSTS
        ));
    }
    for p in posts {
        let seq = seq_of(p);
        let line = p.get("line").and_then(Value::as_str).unwrap_or_default();
        let body = p.get("body").and_then(Value::as_str).unwrap_or_default();
        if let Some(why) = looks_token_shaped(line).or_else(|| looks_token_shaped(body)) {
            out.push(format!(
                "#{seq} looks like it carries {why} — a slate post is prepended to dispatcher briefs and secret-scanned there; `kb slate edit #{seq}` or drop it"
            ));
        }
    }
    out
}

pub async fn doctor(ctx: &Ctx) -> Result<()> {
    let digest = get(ctx, "", &[("all", "1".to_string())]).await?;
    let posts = get(ctx, "/posts", &[]).await?;
    let posts = posts.as_array().cloned().unwrap_or_default();
    let findings = render_doctor(&digest, &posts);
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "slug": ctx.slug, "findings": findings }))?
        );
        return Ok(());
    }
    if findings.is_empty() {
        println!("{}: nothing structural to flag", ctx.slug);
        return Ok(());
    }
    println!("{}: {} thing(s) to look at", ctx.slug, findings.len());
    for f in &findings {
        println!("  · {f}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_seq_accepts_both_forms_the_digest_prints() {
        assert_eq!(parse_seq("#12").unwrap(), 12);
        assert_eq!(parse_seq("12").unwrap(), 12);
        assert_eq!(parse_seq("  #7 ").unwrap(), 7);
        assert!(parse_seq("e_deadbeef").is_err());
    }

    #[test]
    fn split_line_body_moves_everything_after_the_first_newline_into_body() {
        let (line, body) =
            split_line_body("review_gate.rs — bearer done\n3 red in x\nsuspect y", None);
        assert_eq!(line, "review_gate.rs — bearer done");
        assert_eq!(body.as_deref(), Some("3 red in x\nsuspect y"));
    }

    #[test]
    fn split_line_body_keeps_a_one_line_post_bodyless() {
        let (line, body) = split_line_body("one line only", None);
        assert_eq!(line, "one line only");
        assert!(body.is_none());
    }

    #[test]
    fn split_line_body_lets_an_explicit_body_win() {
        let (line, body) = split_line_body("head\nignored tail", Some("from --body"));
        assert_eq!(line, "head");
        assert_eq!(body.as_deref(), Some("from --body"));
    }

    #[test]
    fn refusal_code_prefers_the_extension_member() {
        let v = json!({"code": "slate-taken", "detail": "slate-taken: #55 is held by …"});
        assert_eq!(refusal_code(&v).as_deref(), Some("slate-taken"));
    }

    #[test]
    fn refusal_code_falls_back_to_the_detail_prefix() {
        let v = json!({"detail": "slate-live-author: #12 belongs to a live session"});
        assert_eq!(refusal_code(&v).as_deref(), Some("slate-live-author"));
    }

    #[test]
    fn refusal_code_never_reads_an_ordinary_sentence_as_a_code() {
        let v = json!({"detail": "Something went wrong: try again"});
        assert_eq!(refusal_code(&v), None);
        let v = json!({"detail": "no colon here at all"});
        assert_eq!(refusal_code(&v), None);
    }

    #[test]
    fn exit_codes_are_the_loop_contract() {
        assert_eq!(exit_code_for(409, "slate-taken"), EXIT_REFUSED);
        assert_eq!(exit_code_for(409, "slate-live-author"), EXIT_REFUSED);
        // A closed slate is an error, not a "someone else has it" refusal:
        // retrying with --anyway would never help.
        assert_eq!(exit_code_for(409, "slate-closed"), EXIT_ERROR);
        assert_eq!(exit_code_for(404, "slate-unknown"), EXIT_NOT_FOUND);
        assert_eq!(exit_code_for(404, "bad-target"), EXIT_NOT_FOUND);
        assert_eq!(exit_code_for(413, "slate-full"), EXIT_ERROR);
        assert_eq!(exit_code_for(400, ""), EXIT_ERROR);
    }

    #[test]
    fn displaced_renders_the_spec_line() {
        let d = vec![
            json!({"seq": 41, "kind": "idea", "line": "cache tokens client-side", "age_secs": 86100}),
            json!({"seq": 38, "kind": "found", "line": "auth ends at the gate", "age_secs": 900}),
        ];
        assert_eq!(
            render_displaced(&d, 2).unwrap(),
            "pushed off the board: #41 idea \"cache tokens client-side\" · #38 found \"auth ends at the gate\" — kb slate drop/edit to tidy, or open --all"
        );
    }

    #[test]
    fn displaced_names_the_overflow_past_the_five_it_carries() {
        let d: Vec<Value> = (1..=5)
            .map(|n| json!({"seq": n, "kind": "idea", "line": "x", "age_secs": 1}))
            .collect();
        let line = render_displaced(&d, 9).unwrap();
        assert!(line.contains("… and 4 more"), "{line}");
    }

    #[test]
    fn displaced_is_absent_when_nothing_was_pushed_off() {
        assert!(render_displaced(&[], 0).is_none());
    }

    #[test]
    fn displaced_ellipsizes_on_a_char_boundary() {
        let long = "é".repeat(200);
        let d = vec![json!({"seq": 1, "kind": "idea", "line": long, "age_secs": 1})];
        let line = render_displaced(&d, 1).unwrap();
        assert!(line.contains('…'));
        assert!(line.chars().count() < 200);
    }

    #[test]
    fn posted_line_names_the_target_when_there_is_one() {
        let p = json!({"seq": 67, "kind": "done", "re": 59, "topic": "v7"});
        assert_eq!(render_posted(&p, "kb"), "#67 done → #59 · kb [v7]");
        let p = json!({"seq": 66, "kind": "found"});
        assert_eq!(render_posted(&p, "kb"), "#66 found · kb");
    }

    #[test]
    fn post_row_uses_the_same_four_char_session_tag_kb_core_does() {
        let p = json!({
            "seq": 12, "kind": "found", "line": "auth ends at the gate",
            "prov": {"harness": "codex", "session_id": "8f2a1111-2222"}
        });
        let row = render_post_row(&p);
        let want = kb_core::slate::session_short(Some("8f2a1111-2222"));
        assert!(row.contains(&format!("[codex/{want}]")), "{row}");
        assert!(row.starts_with("#12 found "), "{row}");
    }

    #[test]
    fn post_row_falls_back_to_the_harness_when_no_session_is_declared() {
        let p = json!({"seq": 3, "kind": "idea", "line": "x", "prov": {"harness": "grok"}});
        assert_eq!(render_post_row(&p), "#3 idea [grok] x");
    }

    #[test]
    fn session_id_ladder_is_flag_then_env_then_marker() {
        assert_eq!(
            resolve_session_id(Some("from-flag"), Some("from-env")).as_deref(),
            Some("from-flag")
        );
        assert_eq!(
            resolve_session_id(None, Some("from-env")).as_deref(),
            Some("from-env")
        );
        assert_eq!(
            resolve_session_id(Some("  "), Some("from-env")).as_deref(),
            Some("from-env")
        );
    }

    #[test]
    fn origin_is_client_declared_and_unattributed_is_refused() {
        assert_eq!(resolve_origin(None, None, None).unwrap(), "agent");
        assert_eq!(resolve_origin(None, Some("you"), None).unwrap(), "human");
        assert_eq!(resolve_origin(None, None, Some("01M11")).unwrap(), "import");
        // An explicit --origin wins over both sugars.
        assert_eq!(
            resolve_origin(Some("agent"), Some("you"), Some("01M11")).unwrap(),
            "agent"
        );
        assert!(resolve_origin(Some("unattributed"), None, None).is_err());
        assert!(resolve_origin(None, Some("claude"), None).is_err());
    }

    #[test]
    fn cursor_and_topic_markers_key_on_the_session_id() {
        let dir = Path::new("/c/kb");
        assert_eq!(cursor_file(dir, "s1"), Path::new("/c/kb/slate-cursor-s1"));
        assert_eq!(topic_file(dir, "s1"), Path::new("/c/kb/slate-topic-s1"));
    }

    #[test]
    fn marker_round_trips_through_the_atomic_write() {
        let tmp = tempfile::tempdir().unwrap();
        let f = cursor_file(tmp.path(), "sess-1");
        assert!(read_marker(&f).is_none());
        write_marker(&f, "66");
        assert_eq!(read_marker(&f).as_deref(), Some("66"));
        write_marker(&f, "70");
        assert_eq!(read_marker(&f).as_deref(), Some("70"));
    }

    // ---- D27 cursor report (arg construction + skip conditions) --------

    #[test]
    fn cursor_report_body_carries_session_seq_and_harness() {
        let v = cursor_report_body("sess-a", 42, "codex");
        assert_eq!(
            v,
            json!({"session_id": "sess-a", "seq": 42, "harness": "codex"})
        );
    }

    #[test]
    fn delta_query_pairs_forwards_kinds_verbatim_when_present() {
        let q = delta_query_pairs(Some(5), None, None, Some("sess-a"), Some("now,warn"));
        assert_eq!(
            q,
            vec![
                ("since", "5".to_string()),
                ("session", "sess-a".to_string()),
                ("kinds", "now,warn".to_string()),
            ]
        );
    }

    #[test]
    fn delta_query_pairs_omits_kinds_when_absent() {
        let q = delta_query_pairs(None, Some(10), Some(2000), None, None);
        assert_eq!(
            q,
            vec![("limit", "10".to_string()), ("budget", "2000".to_string())]
        );
        assert!(q.iter().all(|(k, _)| *k != "kinds"));
    }

    #[test]
    fn should_report_cursor_needs_both_a_session_id_and_a_head_seq() {
        let with_head = json!({"head_seq": 5});
        let without_head = json!({"text": "…"});
        assert!(should_report_cursor(Some("sess-a"), &with_head));
        assert!(
            !should_report_cursor(None, &with_head),
            "no session id — nothing to key the cursor on"
        );
        assert!(
            !should_report_cursor(Some("sess-a"), &without_head),
            "no head_seq on the response — nothing to report"
        );
        assert!(!should_report_cursor(None, &without_head));
    }

    #[test]
    fn edit_copies_kind_topic_subject_and_refs_unless_overridden() {
        let target = json!({
            "kind": "take", "topic": "v7", "subject": "crates/kb-server",
            "refs": ["path:crates/kb-server/src/lib.rs"], "line": "old"
        });
        let v = edit_body(&target, 55, "new line".into(), None, None, &[], false);
        assert_eq!(v["kind"], "take");
        assert_eq!(v["topic"], "v7");
        assert_eq!(v["subject"], "crates/kb-server");
        assert_eq!(v["refs"], json!(["path:crates/kb-server/src/lib.rs"]));
        assert_eq!(v["supersedes"], 55);
        assert!(v.get("anyway").is_none());
    }

    #[test]
    fn edit_overrides_win_over_the_copy() {
        let target = json!({"kind": "found", "topic": "v7", "refs": ["path:a"]});
        let over = vec!["path:b".to_string()];
        let v = edit_body(
            &target,
            9,
            "l".into(),
            Some("b".into()),
            Some("perf"),
            &over,
            true,
        );
        assert_eq!(v["topic"], "perf");
        assert_eq!(v["refs"], json!(["path:b"]));
        assert_eq!(v["body"], "b");
        assert_eq!(v["anyway"], true);
    }

    #[test]
    fn watch_subscribes_with_the_slug_filter_token_sl2_added() {
        assert_eq!(watch_query("kb"), "types=slate.updated&filter=slug:kb");
        assert_eq!(watch_query("a/b"), "types=slate.updated&filter=slug:a%2Fb");
    }

    #[test]
    fn attention_is_summed_client_side_from_the_four_named_counts() {
        let c = json!({
            "now": 9, "warn": 9, "hand_unack": 2, "ask_open": 3,
            "take_live": 9, "take_stale": 1, "take_contested": 1,
            "found": 9, "idea": 9, "tried": 9
        });
        assert_eq!(attention_of(&c), 7);
    }

    #[test]
    fn plan_line_is_dated_and_cites_the_post() {
        assert_eq!(
            plan_line("2026-09-05", "kb", 61, "A4 the Desk landed"),
            "- 2026-09-05: A4 the Desk landed (slate kb #61)"
        );
    }

    // ---- stats + doctor -------------------------------------------------

    /// One fixture that exercises every branch of both reports: an
    /// acknowledged hand, an answered-but-open ask, a contested take, a
    /// crossed-out tried echoed by another session, and a leaked token.
    fn fixture() -> (Value, Vec<Value>) {
        let prov = |h: &str, s: Option<&str>| match s {
            Some(s) => json!({"harness": h, "session_id": s, "origin": "agent"}),
            None => json!({"harness": h, "origin": "unattributed"}),
        };
        let posts = vec![
            json!({"seq": 1, "kind": "hand", "line": "h", "subject": "x", "prov": prov("claude", Some("aaaa1111"))}),
            json!({"seq": 2, "kind": "take", "line": "t", "re": 1, "prov": prov("codex", Some("bbbb2222"))}),
            json!({"seq": 3, "kind": "ask", "line": "why?", "prov": prov("claude", Some("aaaa1111"))}),
            json!({"seq": 4, "kind": "answer", "line": "because", "re": 3, "prov": prov("kimi", Some("cccc3333"))}),
            json!({"seq": 5, "kind": "tried", "line": "e2e + cargo concurrently (was #9)", "failed": "OOM", "prov": prov("claude", Some("aaaa1111"))}),
            json!({"seq": 6, "kind": "mark", "line": "+1", "re": 5, "prov": prov("codex", Some("bbbb2222"))}),
            json!({"seq": 7, "kind": "found", "line": "token=ghp_0123456789abcdefghij", "refs": ["path:a"], "prov": prov("claude", Some("aaaa1111"))}),
            json!({"seq": 8, "kind": "idea", "line": "i", "prov": prov("grok", None)}),
            json!({"seq": 9, "kind": "take", "line": "t2", "subject": "y", "anyway": true, "prov": prov("claude", Some("aaaa1111"))}),
            json!({"seq": 10, "kind": "done", "line": "shipped", "re": 2, "prov": prov("codex", Some("bbbb2222"))}),
        ];
        let digest = json!({
            "generation": 1, "head_seq": 10,
            "hand_total": 1, "ask_total": 1, "warn_total": 4,
            "found_idea_total": 2, "dropped": {"found_idea": 1},
            "sections": {
                "hand": [{"seq": 1, "acknowledged": false, "age_secs": 7200, "line": "h"}],
                "ask": [{"seq": 3, "answers": 1}],
                "take": [
                    {"seq": 9, "contested": true, "liveness": "live", "line": "t2"},
                    {"seq": 2, "contested": false, "liveness": "stale", "line": "t"}
                ]
            }
        });
        (digest, posts)
    }

    #[test]
    fn stats_counts_the_lifecycle_off_the_ledger_and_the_projection() {
        let (digest, posts) = fixture();
        let s = compute_stats("kb", &digest, &posts);
        assert_eq!(s.ledger_posts, 10);
        assert_eq!(s.hands_offered, 1);
        assert_eq!(
            s.hands_acknowledged, 1,
            "a take naming the hand acknowledges it"
        );
        assert_eq!(s.hands_unacknowledged, 1);
        assert_eq!(s.takes_opened, 2);
        assert_eq!(s.takes_done, 1);
        assert_eq!(s.takes_live, 1);
        assert_eq!(s.takes_stale, 1);
        assert_eq!(s.takes_contested, 1);
        assert_eq!(s.asks_asked, 1);
        assert_eq!(s.asks_answered, 1);
        assert_eq!(s.tried_posted, 1);
        assert_eq!(s.tried_naming_failure, 1);
        assert_eq!(s.tried_crossing_out, 1);
        assert_eq!(s.tried_echoed_by_others, 1);
        assert_eq!(s.found, 1);
        assert_eq!(s.idea, 1);
        assert_eq!(s.marks, 1);
        assert_eq!(s.done_abandoned, 0);
        assert_eq!(s.by_harness[0], ("claude".to_string(), 5));
    }

    #[test]
    fn abandoned_done_never_counts_a_take_as_done() {
        let (digest, mut posts) = fixture();
        posts[9]["abandoned"] = json!("three specs still red");
        let s = compute_stats("kb", &digest, &posts);
        assert_eq!(s.takes_done, 0);
        assert_eq!(s.done_abandoned, 1);
    }

    #[test]
    fn stats_render_is_the_pinned_layout() {
        let (digest, posts) = fixture();
        let text = render_stats(&compute_stats("kb", &digest, &posts));
        assert_eq!(
            text,
            "kb · generation 1 · head #10 · 10 posts on the ledger\n\n\
             hands   1 offered · 1 acknowledged · 1 still unacknowledged\n\
             takes   2 opened · 1 done · 1 live · 1 stale · 0 expired · 1 contested now\n\
             asks    1 asked · 1 answered · 1 still open\n\
             tried   1 posted · 1 naming what failed · 1 crossing out a prior post · 1 echoed by another session\n\
             found   1 found · 1 idea · 2 shown · 1 dropped\n\
             tidy    0 dropped · 1 marked · 0 done --abandoned\n\
             by harness  claude 5 · codex 3 · grok 1 · kimi 1\n\
             by session  claude/aaaa 5 · codex/bbbb 3 · grok/(no session) 1 · kimi/cccc 1\n"
        );
    }

    #[test]
    fn doctor_flags_the_five_structural_things_and_nothing_else() {
        let (digest, posts) = fixture();
        let f = render_doctor(&digest, &posts);
        assert!(
            f.iter().any(|l| l.starts_with("contested take #9")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|l| l.contains("hand #1 unacknowledged")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|l| l.contains("ask #3 has 1 answer(s)")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|l| l.contains("4 open warns (cap 5)")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|l| l.contains("#7 looks like it carries")),
            "{f:?}"
        );
        assert_eq!(f.len(), 5, "no other complaint on this fixture: {f:?}");
    }

    #[test]
    fn doctor_is_silent_on_a_healthy_slate() {
        let digest = json!({
            "generation": 1, "head_seq": 2, "hand_total": 0, "ask_total": 0, "warn_total": 0,
            "found_idea_total": 1, "dropped": {"found_idea": 0},
            "sections": {"hand": [], "ask": [], "take": []}
        });
        let posts = vec![
            json!({"seq": 1, "kind": "idea", "line": "a plain idea", "prov": {"harness": "claude"}}),
        ];
        assert!(render_doctor(&digest, &posts).is_empty());
    }

    #[test]
    fn token_lint_accepts_ordinary_lines_and_refuses_credential_shapes() {
        assert!(looks_token_shaped("review_gate reads ConnectInfo from extensions").is_none());
        assert!(looks_token_shaped("path:crates/kb-server/src/review_gate.rs:88").is_none());
        // A colon-bearing sentence with a short value stays clean.
        assert!(looks_token_shaped("token: 3 left").is_none());
        assert!(looks_token_shaped("ghp_0123456789abcdefghijklmnopqrstuvwx").is_some());
        assert!(looks_token_shaped("KB_TOKEN=Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MA==").is_some());
        assert!(looks_token_shaped("-----BEGIN OPENSSH PRIVATE KEY-----").is_some());
    }
}
