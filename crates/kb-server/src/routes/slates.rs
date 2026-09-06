//! SL2 — `kb-slate/1`: the per-project blackboard's HTTP surface.
//!
//! Design of record: `docs/research/kb-slate-design-2026-09.html` — §7
//! storage, §8 security posture, §9 the route table + wire shapes, and the
//! §4 "Rules matrix" wherever the prose is looser. The PURE engine is
//! `kb_core::slate` (SL1): every rule, every projection and every refusal
//! is decided there; this module is the I/O shell around it — read the
//! ledger, take the lock, mint the seq, append the line, emit the event.
//!
//! Four things about the shape, because they are the ones a later change
//! is most likely to break:
//!
//! 1. **The store is daemon-wide, not per-kb** (D2). A slate keys on a
//!    PROJECT, and a project maps to several kbs or none — so there is no
//!    `{kb}` segment on any route here, the lock is
//!    `SlateRegistry::lock_for(slug)` rather than `review_lock_for(kb)`,
//!    and the ledger lives at `<state>/slates/<slug>/ledger.jsonl`
//!    (`KbPaths::slate_*`). Nothing here touches the storage actor, so the
//!    index generation is never bumped (#15) and the three artifact-id
//!    registries of #2 are untouched.
//! 2. **Reads never lock and never write** (§7, and the rules matrix's
//!    "Cursor" row: the cursor is client-side, `?since=` only drives the
//!    `seen` header). Only the mutations below take the per-slug lock.
//! 3. **Routes ride `auth_bearer`**, deliberately NOT loopback-only, so the
//!    SPA behind Authelia works (§8) — mounted beside `/sessions/beat` in
//!    `router.rs`. A non-loopback caller gets the same posts through the
//!    forced-scrub floor `routes::sessions::redact_for_non_loopback` uses,
//!    narrowed to its FIRST clause only: `prov.cwd` collapses to a
//!    basename and nothing else changes (re-capping lines/bodies at
//!    `OUTCOME_WIRE_MAX_CHARS` would truncate every body and every sketch
//!    at kb.example.com). `DELETE …?purge=true` is the one loopback-only verb,
//!    checked INSIDE the handler like `sessions::presence`.
//! 4. **`slate.updated` fires once per append, never per read** (#24), and
//!    is registered in `routes::schema::V0_0_1_TYPES` in this same commit
//!    — the drift test scans for the literal.

use crate::middleware::{error_to_problem_json, is_loopback_origin, Identity};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::paths::KbPaths;
use kb_core::sessions::live::{LivePolicy, StateSource};
use kb_core::slate::{
    self, codes, CursorRow, Displaced, HideEntry, HistoryRow, Kind, Mode, Origin, Post, PostBody,
    Presence, ProjectOpts, Projected, SlateDigest, SlateError, SlateMeta, SlateSlug,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;

/// The COUNT-cap codes. §9's code table names the size caps (which live in
/// `kb_core::slate::codes`, raised by the pure validator) and `slate-full`
/// for the ledger; the four per-section/per-session count caps are raised
/// HERE, because they need the projection, so their codes live here too.
/// Same 413-with-a-reason posture: never a silent truncation.
pub mod cap_codes {
    pub const TOO_MANY_OPEN_WARNS: &str = "too-many-open-warns";
    pub const TOO_MANY_OPEN_ASKS: &str = "too-many-open-asks";
    pub const TOO_MANY_OPEN_HANDS: &str = "too-many-open-hands";
    pub const TOO_MANY_LIVE_TAKES: &str = "too-many-live-takes";
    /// 400 — `prov.harness` outside `kb_core::sessions::HARNESSES`
    /// (§9: "400 on an unknown kind or harness"; an unknown KIND is
    /// refused by serde before the handler runs).
    pub const UNKNOWN_HARNESS: &str = "unknown-harness";
}

// ---------------------------------------------------------------------------
// Wire types (§9 "Wire shapes")
// ---------------------------------------------------------------------------

/// The board chip's raw material: `hand_unack + ask_open + take_contested +
/// take_stale`, summed CLIENT-side (§9) — the daemon never sums an
/// attention number for you.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize)]
pub struct SlateCounts {
    pub now: usize,
    pub warn: usize,
    pub hand_unack: usize,
    pub ask_open: usize,
    pub take_live: usize,
    pub take_stale: usize,
    pub take_contested: usize,
    pub found: usize,
    pub idea: usize,
    pub tried: usize,
}

/// One row of `GET /api/slates`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct SlateSummary {
    pub slug: String,
    pub head_seq: u64,
    pub generation: u32,
    /// The newest post's `at`, or `created_unix` on a slate with no posts.
    pub updated_unix: i64,
    pub closed: bool,
    /// Distinct non-null topics across the CURRENT generation, sorted.
    pub topics: Vec<String>,
    pub counts: SlateCounts,
    /// D27 (v0.42) — how many distinct sessions have REPORTED a cursor
    /// here (`meta.cursors.len()`). Served, never "read": it is
    /// attribution, not acknowledgement, and nothing expires it (D5).
    pub sessions_served: usize,
}

/// `GET /api/slates/{slug}` — the projection plus the ONE fact
/// `SlateDigest` deliberately does not carry: `generation` is `meta.json`
/// state, not a projection output (SL1's doc comment on `SlateDigest`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct DigestResponse {
    #[serde(flatten)]
    pub digest: SlateDigest,
    pub generation: u32,
    pub closed: bool,
}

/// `?view=board` — a [`Projected`] plus the three things the board needs
/// and the digest deliberately omits (§9). Never budget-truncated: the
/// board scrolls.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct BoardCard {
    #[serde(flatten)]
    pub post: Projected,
    pub body: Option<String>,
    /// Author tags of the distinct marker SESSIONS, dropped marks excluded
    /// (`kb_core::slate::marks_by` — the same rule `Projected.marks`
    /// counts by, so the ring and the roster can never disagree).
    pub marks_by: Vec<String>,
    /// The body carries a ```mermaid fence (D21: no `sketch` kind, a fence
    /// in a body sandboxed on the board).
    pub has_sketch: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize)]
pub struct BoardSections {
    pub now: Vec<BoardCard>,
    pub warn: Vec<BoardCard>,
    pub hand: Vec<BoardCard>,
    pub ask: Vec<BoardCard>,
    pub take: Vec<BoardCard>,
    pub found_idea: Vec<BoardCard>,
    pub tried: Vec<BoardCard>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct BoardResponse {
    pub slug: String,
    pub head_seq: u64,
    pub topics: Vec<String>,
    pub sections: BoardSections,
}

/// `GET …/delta` — the per-prompt lane. No echo line (rules matrix "Echo
/// line placement"); `hides` is what makes a delta consumer's fold agree
/// with a full projection.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct DeltaResponse {
    pub slug: String,
    pub head_seq: u64,
    pub posts: Vec<Projected>,
    pub hides: Vec<HideEntry>,
    pub text: String,
    pub truncated: bool,
    /// D26 (v0.42) — `?kinds=` narrowed this response. The push adapters
    /// ask for the hybrid subset (`now,warn,hand,ask,answer`); found,
    /// idea and tried stay pull-only. `false` when no filter was asked
    /// for, so an unfiltered response is byte-identical to v0.41 apart
    /// from this one member.
    pub filtered: bool,
}

/// `GET …/posts/{id-or-seq}` — one post unfolded: its body rides on
/// `post`, its refs resolved to display strings, and the thread beneath it
/// (answers, done, drops, edits), oldest first.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct PostDetail {
    pub slug: String,
    pub post: Post,
    pub refs: Vec<slate::RefDisplay>,
    pub thread: Vec<Post>,
}

/// The 201 body (§5 "The append response is the poster's only feedback
/// loop with the finite surface").
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct AppendResponse {
    pub post: Post,
    /// At most [`slate::DISPLACED_CAP`]; the full count is
    /// `displaced_total`.
    pub displaced: Vec<Displaced>,
    pub displaced_total: usize,
    pub nudge: Option<String>,
    pub head_seq: u64,
}

/// `POST /api/slates/{slug}/cursor` body (D27, v0.42) — a session
/// REPORTING the newest seq it has been served. Fire-and-forget from the
/// CLI after a successful `open`/`delta`; never sent by a GET handler,
/// because a read that writes is a read that lies.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Deserialize)]
pub struct CursorBody {
    pub session_id: String,
    pub seq: u64,
    /// Free-form, defaulted to [`kb_core::sessions::HARNESS_DEFAULT`] when
    /// absent — display metadata on the board's chip, never a gate.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub harness: Option<String>,
}

/// The 201 body: what is STORED (never necessarily what was sent — a
/// lower report is ignored, so `seq` can come back higher than the one
/// posted) plus the head the caller is being measured against.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CursorResponse {
    pub session_id: String,
    pub seq: u64,
    pub head_seq: u64,
}

/// `POST …/close|reopen|rotate` — the lifecycle verbs' shared body.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleResponse {
    pub slug: String,
    pub head_seq: u64,
    pub generation: u32,
    pub closed: bool,
    pub closed_unix: Option<i64>,
    pub rotated_from: Option<u32>,
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct DigestParams {
    /// `full` (default) | `hybrid` — the session-start injection block.
    pub mode: Option<String>,
    /// Characters, never tokens (D18). Defaults to `BUDGET_OPEN` for
    /// `full` and `BUDGET_HYBRID` for `hybrid`.
    pub budget: Option<usize>,
    pub topic: Option<String>,
    /// `1`/`true` — no budget truncation at all.
    pub all: Option<String>,
    /// Marks "your own asks with new answers"; NEVER written anywhere.
    pub session: Option<String>,
    /// The client-side cursor; drives the `seen #a → #b` header only.
    pub since: Option<u64>,
    /// `board` — the SPA projection (§9).
    pub view: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PostsParams {
    pub since: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DeltaParams {
    pub since: Option<u64>,
    pub session: Option<String>,
    pub budget: Option<usize>,
    pub limit: Option<usize>,
    /// D26 — a csv of kind words (`now,warn,hand,ask,answer`). Narrows
    /// the posts AND the hides; an unknown word is a 400 `bad-kind`,
    /// never a silently empty delta.
    pub kinds: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PurgeParams {
    pub purge: Option<String>,
}

fn flag(v: Option<&String>) -> bool {
    matches!(v.map(|s| s.trim()), Some("1" | "true" | "yes"))
}

/// Hard cap on any `?limit=`-bearing read, so a client can never ask one
/// slate to serialise more than a rotated ledger's worth of posts.
const READ_LIMIT_CAP: usize = 2_000;

// ---------------------------------------------------------------------------
// Problem+json with the `code` and `holder` extension members (§9)
// ---------------------------------------------------------------------------

/// The slate's OWN response builder. `middleware::error_to_problem_json`
/// sets `title` to the status's canonical reason and carries no extension
/// members, so a slate refusal cannot ride it: §9 pins `code` as an RFC
/// 7807 extension member AND as `detail`'s prefix (the CLI reads `code`
/// when present and the prefix otherwise), plus `holder` on a
/// `slate-taken`. Same `application/problem+json` content type, same
/// `title` rule, same URN `type` vocabulary — only the two extra members
/// and the 413 status differ.
fn slate_problem(err: &SlateError) -> Response<Body> {
    let status = StatusCode::from_u16(err.status).unwrap_or(StatusCode::BAD_REQUEST);
    let urn = match err.status {
        409 => "conflict",
        429 => "rate-limited",
        413 => "payload-too-large",
        403 => "forbidden",
        404 => "not-found",
        _ => "bad-request",
    };
    let mut body = json!({
        "type": format!("urn:kb:errors:{urn}"),
        "title": status.canonical_reason().unwrap_or("Error"),
        "status": err.status,
        // §9: `detail: "<code>: <text>"` — SlateError's Display IS that form.
        "detail": err.to_string(),
        "code": err.code,
    });
    if let Some(h) = &err.holder {
        body["holder"] = serde_json::to_value(h.as_ref()).unwrap_or(serde_json::Value::Null);
    }
    let mut resp = (status, Json(body)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Every 2xx on this family is `Cache-Control: no-store` (§8) — a slate is
/// working state, and a cached board is a lie about who holds what.
fn no_store_json<T: Serialize>(status: StatusCode, body: T) -> Response<Body> {
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn bad(code: &'static str, detail: impl Into<String>) -> Response<Body> {
    slate_problem(&SlateError::new(code, 400, detail))
}

fn not_found(slug: &str) -> Response<Body> {
    slate_problem(&SlateError::new(
        "slate-unknown",
        404,
        format!("no slate {slug:?} — post to it to create it"),
    ))
}

/// Parse + validate the `{slug}` path segment. The grammar is
/// `SlateSlug`'s (`[a-z0-9_-]{1,64}`), which is also what keeps the
/// segment from ever becoming a traversal.
///
/// `Err` is a whole `Response` — the `routes::proposals::validate` shape,
/// and the reason for the same `result_large_err` allow it carries: the
/// refusal must keep the slate's own `code`/`holder` problem+json body,
/// which a `kb_core::Error` cannot express.
#[allow(clippy::result_large_err)]
fn parse_slug(raw: &str) -> Result<SlateSlug, Response<Body>> {
    SlateSlug::new(raw).map_err(|e| bad("bad-slug", e.to_string()))
}

// ---------------------------------------------------------------------------
// Ledger + meta I/O (the only impure part of the slate; §7 "Storage")
// ---------------------------------------------------------------------------

/// Read `meta.json`. `Ok(None)` = no such slate. A schema mismatch REFUSES
/// (`SlateMeta::validate_schema`, the `review::load` rule) rather than
/// being read as `kb-slate/1`.
fn load_meta(paths: &KbPaths, slug: &SlateSlug) -> kb_core::Result<Option<SlateMeta>> {
    let path = paths.slate_meta_file(slug);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta: SlateMeta = serde_json::from_slice(&bytes)?;
    meta.validate_schema()?;
    Ok(Some(meta))
}

/// `fsx::write_atomic` (tmp sibling + rename) — the caller holds the
/// per-slug lock.
fn save_meta(paths: &KbPaths, slug: &SlateSlug, meta: &SlateMeta) -> kb_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(meta)?;
    kb_core::fsx::write_atomic(&paths.slate_meta_file(slug), &bytes)
}

/// Read the CURRENT generation's ledger. A trailing line with no newline
/// is an append caught mid-flight and is DROPPED, not parsed: reads never
/// lock (§7), so the one thing a lock-free reader must tolerate is a
/// partially-written last record. Everything else is a real corruption and
/// surfaces as an error.
///
/// `pub(crate)` (SL3c): `routes::context`'s scent-line lane reuses this
/// SAME loader for the caller's slate rather than growing a second parser.
pub(crate) fn load_posts(paths: &KbPaths, slug: &SlateSlug) -> kb_core::Result<Vec<Post>> {
    let path = paths.slate_ledger_file(slug);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let committed = match raw.rfind('\n') {
        Some(i) => &raw[..=i],
        None => "",
    };
    slate::parse_ledger(committed)
}

/// One line, `O_APPEND` + fsync (§7 "Lock"). `O_APPEND` is what makes the
/// write atomic against any other writer; the fsync is what makes it
/// survive a crash between the ledger and `meta.json`. Caller holds the
/// per-slug lock.
fn append_line(paths: &KbPaths, slug: &SlateSlug, post: &Post) -> kb_core::Result<()> {
    let dir = paths.slate_dir(slug);
    std::fs::create_dir_all(&dir)?;
    let mut line = slate::to_ledger_line(post)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.slate_ledger_file(slug))?;
    f.write_all(line.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Bytes the current ledger occupies — half of the `slate-full` cap.
fn ledger_bytes(paths: &KbPaths, slug: &SlateSlug) -> u64 {
    std::fs::metadata(paths.slate_ledger_file(slug))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Every slug with a `meta.json`, sorted. Blocking; callers wrap it.
fn list_slugs(paths: &KbPaths) -> Vec<SlateSlug> {
    let root = paths.state.join("slates");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<SlateSlug> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| SlateSlug::new(e.file_name().to_string_lossy().into_owned()).ok())
        .filter(|s| paths.slate_meta_file(s).exists())
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Presence, scrub floor, projection options
// ---------------------------------------------------------------------------

/// The presence slice the PURE engine takes as an argument (§7: "no
/// registry access"). Extracted from `LiveRegistry::snapshot`, which
/// already derives against `now_unix`; the slate only needs the raw
/// last-activity instant, since `slate::Liveness` re-derives its own three
/// labels from `derive_state`'s six lanes.
///
/// `pub(crate)` (SL3c): `routes::context` projects the caller's slate with
/// this SAME presence slice, so a take's liveness label never disagrees
/// between the scent count and the board.
pub(crate) fn presence_slice(state: &KbHandles, now_unix: i64) -> Vec<Presence> {
    state
        .live_registry
        .snapshot(now_unix)
        .into_iter()
        .map(|(r, _, _)| Presence {
            session_id: r.session_id,
            last_activity_unix: r.last_activity_unix,
            source: StateSource::Hook,
        })
        .collect()
}

/// The scrub floor (§8, rules matrix "Scrub floor"): off loopback `cwd`
/// collapses to its basename and NOTHING else changes. Deliberately the
/// first clause of `sessions::redact_for_non_loopback` and not its second
/// — re-capping at `OUTCOME_WIRE_MAX_CHARS` (240) would truncate every
/// body and every sketch at kb.example.com, and lines/bodies already carry
/// their own 200/2,000 caps.
fn scrub_posts(posts: &mut [Post]) {
    for p in posts {
        if let Some(cwd) = p.prov.cwd.take() {
            let base = std::path::Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(cwd);
            p.prov.cwd = Some(base);
        }
    }
}

/// Distinct non-null topics across the ledger, sorted — what the summary
/// and the board list. Derived from the RECORDS (not from the header's
/// topic list, which only names topics that currently carry a NOW).
fn topics_of(posts: &[Post]) -> Vec<String> {
    let mut t: Vec<String> = posts.iter().filter_map(|p| p.topic.clone()).collect();
    t.sort();
    t.dedup();
    t
}

/// The `main <cwd>` clause of the header (§5's example). The daemon has no
/// cwd of its own and `meta.json` records no root, so the slate's recorded
/// root IS the newest post that declared one — which is also why it rides
/// the scrub floor above (off loopback it renders as a basename). Absent
/// on a slate whose posters never sent a `cwd`; the clause is then omitted
/// entirely rather than guessed.
fn header_context(posts: &[Post]) -> Option<String> {
    posts.iter().rev().find_map(|p| p.prov.cwd.clone())
}

/// The `you: <who>` tag: this session's own author tag when it has posted
/// here before, else the short session id alone. `None` when the caller
/// named no session — the whole `you:` clause is then omitted (rules
/// matrix "Cursor").
fn who_tag(posts: &[Post], session: Option<&str>) -> Option<String> {
    let sid = session?;
    let own = posts
        .iter()
        .rev()
        .find(|p| p.prov.session_id.as_deref() == Some(sid));
    Some(match own {
        Some(p) => slate::author_tag(p),
        None => slate::session_short(Some(sid)),
    })
}

/// Read options, assembled once so the digest, the board and the delta all
/// project through the SAME opts (the engine-plus-presenters rule).
#[allow(clippy::too_many_arguments)]
fn read_opts(
    slug: &SlateSlug,
    posts: &[Post],
    mode: Mode,
    budget: usize,
    topic: Option<String>,
    all: bool,
    session: Option<String>,
    since: Option<u64>,
    cursors: BTreeMap<String, CursorRow>,
) -> ProjectOpts {
    ProjectOpts {
        budget,
        topic,
        all,
        who: who_tag(posts, session.as_deref()),
        session_id: session,
        since,
        mode,
        slug: slug.to_string(),
        header_context: header_context(posts),
        cursors,
    }
}

/// The projection SL2 asks for when it needs facts rather than a rendering
/// — counts, board membership, `hide` entries. `all: true`, `Mode::Full`,
/// no topic filter: the budget and the hybrid cut are RENDERING concerns.
fn facts_opts(slug: &SlateSlug) -> ProjectOpts {
    ProjectOpts {
        all: true,
        slug: slug.to_string(),
        ..ProjectOpts::default()
    }
}

/// Everything a read handler needs, loaded once. `loopback` decides the
/// scrub floor; the posts are scrubbed BEFORE anything projects them, so
/// no surface can leak a full `cwd` by forgetting to call the scrubber.
struct Loaded {
    meta: SlateMeta,
    posts: Vec<Post>,
}

fn load_for_read(
    paths: &KbPaths,
    slug: &SlateSlug,
    loopback: bool,
) -> kb_core::Result<Option<Loaded>> {
    let Some(meta) = load_meta(paths, slug)? else {
        return Ok(None);
    };
    let mut posts = load_posts(paths, slug)?;
    if !loopback {
        scrub_posts(&mut posts);
    }
    Ok(Some(Loaded { meta, posts }))
}

/// `spawn_blocking` wrapper — the ledger read is file I/O and must never
/// run on a tokio worker (the same rule `routes::proposals::
/// collect_proposals` follows).
async fn load_for_read_async(
    paths: Arc<KbPaths>,
    slug: SlateSlug,
    loopback: bool,
) -> kb_core::Result<Option<Loaded>> {
    tokio::task::spawn_blocking(move || load_for_read(&paths, &slug, loopback))
        .await
        .map_err(|e| kb_core::Error::Storage(format!("slate read join error: {e}")))?
}

// ---------------------------------------------------------------------------
// Reads — never lock, never write (§7)
// ---------------------------------------------------------------------------

/// `GET /api/slates` — every slate with its open counts (§9). One
/// `spawn_blocking` for the whole walk; a slate whose meta or ledger is
/// unreadable is SKIPPED rather than 500ing the list (the
/// `collect_proposals` posture — one bad directory can't blind the fleet).
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response<Body> {
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let now_unix = chrono::Utc::now().timestamp();
    let presence = presence_slice(&state, now_unix);
    let paths = state.paths.clone();

    let rows = tokio::task::spawn_blocking(move || {
        let policy = LivePolicy::default();
        list_slugs(&paths)
            .into_iter()
            .filter_map(|slug| {
                let loaded = load_for_read(&paths, &slug, loopback).ok().flatten()?;
                Some(summarize(&slug, &loaded, now_unix, &policy, &presence))
            })
            .collect::<Vec<SlateSummary>>()
    })
    .await
    .unwrap_or_default();

    no_store_json(StatusCode::OK, rows)
}

fn summarize(
    slug: &SlateSlug,
    loaded: &Loaded,
    now_unix: i64,
    policy: &LivePolicy,
    presence: &[Presence],
) -> SlateSummary {
    let d = slate::project(&loaded.posts, now_unix, policy, presence, &facts_opts(slug));
    let mut counts = SlateCounts {
        now: d.now_total,
        warn: d.warn_total,
        hand_unack: d.hand_total,
        ask_open: d.ask_total,
        tried: d.tried_total,
        ..SlateCounts::default()
    };
    for t in &d.sections.take {
        match t.liveness {
            Some(slate::Liveness::Live) => counts.take_live += 1,
            Some(slate::Liveness::Stale) => counts.take_stale += 1,
            _ => {}
        }
        if t.contested {
            counts.take_contested += 1;
        }
    }
    for p in &d.sections.found_idea {
        match p.kind {
            Kind::Found => counts.found += 1,
            Kind::Idea => counts.idea += 1,
            _ => {}
        }
    }
    SlateSummary {
        slug: slug.to_string(),
        head_seq: loaded.meta.head_seq,
        generation: loaded.meta.generation,
        updated_unix: loaded
            .posts
            .last()
            .map(|p| p.at)
            .unwrap_or(loaded.meta.created_unix),
        closed: loaded.meta.closed(),
        topics: topics_of(&loaded.posts),
        counts,
        sessions_served: loaded.meta.cursors.len(),
    }
}

/// `GET /api/slates/{slug}?mode=&budget=&topic=&all=1&session=&since=` —
/// the digest, and (`?view=board`) the board projection. ONE handler
/// because §9 gives them one route; the branch is the last thing it does,
/// so both read exactly the same ledger through exactly the same opts.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<DigestParams>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let loaded = match load_for_read_async(state.paths.clone(), slug.clone(), loopback).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found(slug.as_str()),
        Err(e) => return error_to_problem_json(&e),
    };

    let now_unix = chrono::Utc::now().timestamp();
    let policy = LivePolicy::default();
    let presence = presence_slice(&state, now_unix);
    let board = q.view.as_deref().map(str::trim) == Some("board");

    let mode = match q.mode.as_deref().map(str::trim) {
        None | Some("") | Some("full") => Mode::Full,
        Some("hybrid") => Mode::Hybrid,
        Some(other) => {
            return bad(
                "bad-mode",
                format!("mode must be full|hybrid, got {other:?}"),
            )
        }
    };
    let budget = q.budget.unwrap_or(match mode {
        Mode::Full => slate::BUDGET_OPEN,
        Mode::Hybrid => slate::BUDGET_HYBRID,
    });
    // The board scrolls, so it is NEVER budget-truncated (§9).
    let all = board || flag(q.all.as_ref());
    let opts = read_opts(
        &slug,
        &loaded.posts,
        if board { Mode::Full } else { mode },
        budget,
        q.topic.clone(),
        all,
        q.session.clone(),
        q.since,
        loaded.meta.cursors.clone(),
    );
    let digest = slate::project(&loaded.posts, now_unix, &policy, &presence, &opts);

    if board {
        return no_store_json(
            StatusCode::OK,
            BoardResponse {
                slug: slug.to_string(),
                head_seq: digest.head_seq,
                topics: topics_of(&loaded.posts),
                sections: board_sections(&digest, &loaded.posts),
            },
        );
    }
    no_store_json(
        StatusCode::OK,
        DigestResponse {
            digest,
            generation: loaded.meta.generation,
            closed: loaded.meta.closed(),
        },
    )
}

fn board_sections(d: &SlateDigest, posts: &[Post]) -> BoardSections {
    let card = |p: &Projected| BoardCard {
        body: posts
            .iter()
            .find(|raw| raw.seq == p.seq)
            .and_then(|raw| raw.body.clone()),
        marks_by: slate::marks_by(posts, p.seq),
        has_sketch: slate::has_sketch(
            posts
                .iter()
                .find(|raw| raw.seq == p.seq)
                .and_then(|raw| raw.body.as_deref()),
        ),
        post: p.clone(),
    };
    BoardSections {
        now: d.sections.now.iter().map(&card).collect(),
        warn: d.sections.warn.iter().map(&card).collect(),
        hand: d.sections.hand.iter().map(&card).collect(),
        ask: d.sections.ask.iter().map(&card).collect(),
        take: d.sections.take.iter().map(&card).collect(),
        found_idea: d.sections.found_idea.iter().map(&card).collect(),
        tried: d.sections.tried.iter().map(&card).collect(),
    }
}

/// `GET /api/slates/{slug}/posts?since=&limit=` — RAW records, the watch
/// loop's refetch (§9). No projection: this is the one read that hands
/// back the ledger as written (minus the scrub floor).
pub async fn posts(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<PostsParams>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let loaded = match load_for_read_async(state.paths.clone(), slug.clone(), loopback).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found(slug.as_str()),
        Err(e) => return error_to_problem_json(&e),
    };
    let since = q.since.unwrap_or(0);
    let mut out: Vec<Post> = loaded.posts.into_iter().filter(|p| p.seq > since).collect();
    out.truncate(q.limit.unwrap_or(READ_LIMIT_CAP).min(READ_LIMIT_CAP));
    no_store_json(StatusCode::OK, out)
}

/// `GET /api/slates/{slug}/posts/{id-or-seq}` — one post unfolded (§9).
/// `{id-or-seq}` is a bare sequence number or an `e_<12 hex>` id; the `#`
/// the CLI prints is not a legal path character and is accepted stripped.
pub async fn post_detail(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((slug, key)): Path<(String, String)>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let loaded = match load_for_read_async(state.paths.clone(), slug.clone(), loopback).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found(slug.as_str()),
        Err(e) => return error_to_problem_json(&e),
    };
    let key = key.trim().trim_start_matches('#');
    let post = match key.parse::<u64>() {
        Ok(seq) => loaded.posts.iter().find(|p| p.seq == seq),
        Err(_) => loaded.posts.iter().find(|p| p.id == key),
    };
    let Some(post) = post.cloned() else {
        return slate_problem(&SlateError::new(
            "bad-target",
            404,
            format!("no post {key:?} on slate {slug}"),
        ));
    };
    // The thread: everything that acts on this post, oldest first —
    // answers, done, drops, marks and the edit that replaced it.
    let thread: Vec<Post> = loaded
        .posts
        .iter()
        .filter(|p| p.seq != post.seq && (p.re == Some(post.seq) || p.supersedes == Some(post.seq)))
        .cloned()
        .collect();
    let refs = resolve_refs(&state, &post, &loaded.posts).await;
    no_store_json(
        StatusCode::OK,
        PostDetail {
            slug: slug.to_string(),
            post,
            refs,
            thread,
        },
    )
}

/// Ref resolution for display (rules matrix "Refs"). kb-core resolves only
/// `post:#n` — every other prefix needs I/O the pure engine does not have,
/// which is why this lives here. `path:`, `job:`, `commit:` and `plan:`
/// render verbatim with `resolved: false` BY DESIGN: the daemon has no
/// tree, no dispatcher and no git checkout to check them against, and a
/// guessed display string is worse than an honest raw one.
async fn resolve_refs(state: &KbHandles, post: &Post, posts: &[Post]) -> Vec<slate::RefDisplay> {
    let mut out = Vec::new();
    for raw in &post.refs {
        let (prefix, rest) = match raw.split_once(':') {
            Some(v) => v,
            None => {
                out.push(unresolved(raw));
                continue;
            }
        };
        let display = match prefix {
            "post" => rest
                .strip_prefix('#')
                .and_then(|n| n.parse::<u64>().ok())
                .and_then(|n| posts.iter().find(|p| p.seq == n))
                .map(|p| p.line.clone()),
            "kb" => match rest.split_once('/') {
                Some((kb, id)) => doc_title_in(state, kb, id).await,
                None => None,
            },
            "mem" => doc_title_anywhere(state, rest).await,
            "session" => session_title(state, rest).await,
            _ => None,
        };
        out.push(match display {
            Some(display) => slate::RefDisplay {
                raw: raw.clone(),
                display,
                resolved: true,
            },
            None => unresolved(raw),
        });
    }
    out
}

fn unresolved(raw: &str) -> slate::RefDisplay {
    slate::RefDisplay {
        raw: raw.to_string(),
        display: raw.to_string(),
        resolved: false,
    }
}

async fn doc_title_in(state: &KbHandles, kb: &str, id: &str) -> Option<String> {
    let name = kb_core::types::KbName::new(kb).ok()?;
    let ctx = state.kbs.get(&name)?;
    ctx.storage
        .get_by_id(id.to_string())
        .await
        .ok()?
        .map(|d| d.title)
}

/// The `mem:<id>` rule: the FIRST kb whose row matches. Fanned out in
/// submission (BTreeMap) order via `buffered_join`, so "first" is
/// deterministic across restarts (invariant #28).
async fn doc_title_anywhere(state: &KbHandles, id: &str) -> Option<String> {
    let mut futs: Vec<super::CorpusFut<'_, Option<String>>> = Vec::new();
    for ctx in state.kbs.values() {
        let id = id.to_string();
        futs.push(Box::pin(async move {
            ctx.storage
                .get_by_id(id)
                .await
                .ok()
                .flatten()
                .map(|d| d.title)
        }));
    }
    super::buffered_join(futs, super::FANOUT_CAP)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// `session:<sid>` → the NEWEST capture's title (invariant #11's
/// newest-capture rule; `sessions_get` already returns the superset row).
async fn session_title(state: &KbHandles, sid: &str) -> Option<String> {
    let mut futs: Vec<super::CorpusFut<'_, Option<String>>> = Vec::new();
    for ctx in state.kbs.values() {
        let sid = sid.to_string();
        futs.push(Box::pin(async move {
            let row = ctx.storage.sessions_get(sid).await.ok().flatten()?;
            row.title.or(row.first_user_prompt)
        }));
    }
    super::buffered_join(futs, super::FANOUT_CAP)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// `GET /api/slates/{slug}/delta?since=&session=&budget=&limit=` — the
/// per-prompt lane (§9). Built by SEEDING `ProjectState` with everything
/// at or before the cursor and then folding ONE batch of everything after
/// it: the returned `posts` are what ENTERED the board and `hides` what
/// the same batch removed, so a consumer that adds the former and drops
/// the latter lands on exactly the full projection (the SL1
/// chunk-equivalence golden is that property).
pub async fn delta(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<DeltaParams>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let loaded = match load_for_read_async(state.paths.clone(), slug.clone(), loopback).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found(slug.as_str()),
        Err(e) => return error_to_problem_json(&e),
    };

    let now_unix = chrono::Utc::now().timestamp();
    let policy = LivePolicy::default();
    let presence = presence_slice(&state, now_unix);
    let since = q.since.unwrap_or(0);
    let budget = q.budget.unwrap_or(slate::BUDGET_DELTA);
    // D26's kind filter is parsed BEFORE any work: an unknown word is a
    // 400, never a silently empty delta a push adapter would read as "no
    // news".
    let kinds = match parse_kinds(q.kinds.as_deref()) {
        Ok(k) => k,
        Err(word) => {
            return bad(
                "bad-kind",
                format!("unknown slate kind {word:?} in ?kinds="),
            )
        }
    };
    let opts = read_opts(
        &slug,
        &loaded.posts,
        Mode::Full,
        budget,
        None,
        false,
        q.session.clone(),
        Some(since),
        loaded.meta.cursors.clone(),
    );

    let split = loaded.posts.partition_point(|p| p.seq <= since);
    let (seen, fresh) = loaded.posts.split_at(split);
    let mut st = slate::ProjectState::new(now_unix, &policy, &presence, &opts);
    if !seen.is_empty() {
        // Seeding is what makes `since` a cursor rather than a filter: the
        // shown set BEFORE the cursor is the baseline the diff is against.
        let _ = slate::project_append(&mut st, seen);
    }
    let mut batch = slate::project_append(&mut st, fresh);

    // D26: narrow to the asked-for kinds FIRST (a semantic selection),
    // then apply `?limit=` (a safety cap). A hide passes when its TARGET
    // carries one of the kinds — the adapter that asked for hands must
    // still hear that the hand it was told about is gone, or its fold
    // would keep a closed item forever.
    let filtered = kinds.is_some();
    if let Some(want) = &kinds {
        batch.posts.retain(|p| want.contains(&p.kind));
        batch.hides.retain(|h| {
            loaded
                .posts
                .iter()
                .find(|p| p.seq == h.hide)
                .is_some_and(|p| want.contains(&p.kind))
        });
    }

    // `?limit=` caps the JSON array, never the ledger. It is a safety cap
    // on a hook-lane read, so it keeps the FIRST N in section (priority)
    // order and says so via `truncated` — the raw refetch is `…/posts`.
    let limit = q.limit.unwrap_or(READ_LIMIT_CAP).min(READ_LIMIT_CAP);
    let limited = batch.posts.len() > limit;
    if limited {
        batch.posts.truncate(limit);
    }
    // Rendered from the FILTERED set, so `text` and `posts` can never
    // disagree about what the caller was told.
    let (text, cut) = slate::render_delta_batch(&batch.posts, &batch.hides, &loaded.posts, budget);

    no_store_json(
        StatusCode::OK,
        DeltaResponse {
            slug: slug.to_string(),
            head_seq: loaded.meta.head_seq,
            posts: batch.posts,
            hides: batch.hides,
            text,
            truncated: cut || limited,
            filtered,
        },
    )
}

/// D26's `?kinds=a,b` — `None` when the caller asked for no filter (the
/// param absent, or present and empty). `Err` carries the OFFENDING WORD,
/// not a response: the handler owns the 400, so this stays a small pure
/// parser (and clippy's `result_large_err` stays quiet).
fn parse_kinds(raw: Option<&str>) -> Result<Option<std::collections::HashSet<Kind>>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let mut out = std::collections::HashSet::new();
    for word in raw.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match word.parse::<Kind>() {
            Ok(k) => {
                out.insert(k);
            }
            Err(()) => return Err(word.to_string()),
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

/// `GET /api/slates/{slug}/history?since=&limit=` — dropped and superseded
/// posts only, newest first (§9). The permanent record behind D17's
/// attributed tombstone: a `done` closes an item, it does not tombstone
/// it, so it is NOT here. Current generation only; the archives are for
/// distill.
pub async fn history(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<PostsParams>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let loaded = match load_for_read_async(state.paths.clone(), slug.clone(), loopback).await {
        Ok(Some(l)) => l,
        Ok(None) => return not_found(slug.as_str()),
        Err(e) => return error_to_problem_json(&e),
    };
    let now_unix = chrono::Utc::now().timestamp();
    let presence = presence_slice(&state, now_unix);
    let rows: Vec<HistoryRow> = slate::history(
        &loaded.posts,
        now_unix,
        &LivePolicy::default(),
        &presence,
        q.since,
        Some(q.limit.unwrap_or(READ_LIMIT_CAP).min(READ_LIMIT_CAP)),
    );
    no_store_json(StatusCode::OK, rows)
}

// ---------------------------------------------------------------------------
// The cursor — D27's "seen is a REPORTED cursor, never a read that writes"
// ---------------------------------------------------------------------------

/// `POST /api/slates/{slug}/cursor` — record the newest seq a session has
/// been SERVED (D27, v0.42).
///
/// Four properties, in the order a later change is most likely to break
/// them:
///
/// 1. **Nothing else may call this.** No GET handler touches `cursors`;
///    the CLI fires it after a successful `open`/`delta`. A read that
///    writes would make "seen by 2" mean "two sessions polled", which is
///    a different and less honest claim.
/// 2. **Monotonic.** A report lower than (or equal to) the stored seq is
///    IGNORED — 201 with the STORED value, nothing written. An
///    out-of-order retry from a flaky adapter can never rewind a cursor.
/// 3. **Bounded by `head_seq`.** A seq the slate has never minted is a
///    400 `bad-cursor`: it would make a post look seen before it existed.
/// 4. **Emits nothing.** `slate.updated` fires once per APPEND (#24), and
///    this appends no post — an SSE storm of "someone read it" is exactly
///    what D27 refuses.
pub async fn cursor(
    State(state): State<Arc<KbHandles>>,
    Path(slug): Path<String>,
    Json(body): Json<CursorBody>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return bad("bad-cursor", "session_id must not be empty");
    }
    let harness = body
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or(kb_core::sessions::HARNESS_DEFAULT)
        .to_string();
    let seq = body.seq;
    let now_unix = chrono::Utc::now().timestamp();

    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;

    let paths = state.paths.clone();
    let slug_owned = slug.clone();
    let sid = session_id.clone();
    let joined = tokio::task::spawn_blocking(move || {
        cursor_locked(&paths, &slug_owned, sid, seq, harness, now_unix)
    })
    .await;
    drop(guard);

    match joined {
        Ok(Ok((stored, head_seq))) => no_store_json(
            StatusCode::CREATED,
            CursorResponse {
                session_id,
                seq: stored,
                head_seq,
            },
        ),
        Ok(Err(e)) => append_fail(e),
        Err(e) => error_to_problem_json(&kb_core::Error::Storage(format!(
            "slate cursor join error: {e}"
        ))),
    }
}

/// The critical section: read `meta.json`, apply the monotonic rule,
/// `write_atomic` only when the value actually moved. Returns the STORED
/// seq and the head it was measured against. Caller holds the per-slug
/// lock.
fn cursor_locked(
    paths: &KbPaths,
    slug: &SlateSlug,
    session_id: String,
    seq: u64,
    harness: String,
    now_unix: i64,
) -> Result<(u64, u64), AppendFail> {
    let Some(mut meta) = load_meta(paths, slug)? else {
        return Err(AppendFail::Core(kb_core::Error::NotFound(format!(
            "no slate {slug}"
        ))));
    };
    if seq > meta.head_seq {
        return Err(SlateError::new(
            "bad-cursor",
            400,
            format!(
                "seq #{seq} is past this slate's head #{} — a cursor reports what was SERVED",
                meta.head_seq
            ),
        )
        .into());
    }
    // Monotonic: a report lower than or equal to the stored one writes
    // NOTHING — not the seq, not the `at` — so a retry storm can never
    // rewrite `meta.json` and never rewind a cursor. The first report
    // from a session always writes, even at seq 0.
    let existing = meta.cursors.get(&session_id).map(|c| c.seq);
    match existing {
        Some(stored) if seq <= stored => Ok((stored, meta.head_seq)),
        _ => {
            meta.cursors.insert(
                session_id,
                CursorRow {
                    seq,
                    harness,
                    at: now_unix,
                },
            );
            save_meta(paths, slug, &meta)?;
            Ok((seq, meta.head_seq))
        }
    }
}

// ---------------------------------------------------------------------------
// The append — the ONE mutation the twelve verbs share (§9, §7 "Lock")
// ---------------------------------------------------------------------------

/// Either refusal kind the critical section can produce: a slate refusal
/// (which carries its own `code`, status and holder) or an ordinary I/O /
/// serde error (which rides `error_to_problem_json` unchanged).
enum AppendFail {
    Slate(Box<SlateError>),
    Core(kb_core::Error),
}

impl From<SlateError> for AppendFail {
    fn from(e: SlateError) -> Self {
        AppendFail::Slate(Box::new(e))
    }
}
impl From<kb_core::Error> for AppendFail {
    fn from(e: kb_core::Error) -> Self {
        AppendFail::Core(e)
    }
}

fn append_fail(e: AppendFail) -> Response<Body> {
    match e {
        AppendFail::Slate(s) => slate_problem(&s),
        AppendFail::Core(c) => error_to_problem_json(&c),
    }
}

struct Appended {
    status: StatusCode,
    response: AppendResponse,
    /// `None` on the idempotent duplicate-mark path: nothing was written,
    /// so nothing is announced (`slate.updated` fires once per APPEND).
    event: Option<serde_json::Value>,
    head: crate::slate_registry::CachedHead,
}

/// `POST /api/slates/{slug}/posts` — append any of the twelve kinds.
///
/// The whole critical section runs in ONE `spawn_blocking` under the
/// per-slug async lock: read meta → mint seq → validate → conflict and
/// live-author checks against the presence slice → `O_APPEND` + fsync →
/// atomic meta → project before/after at the DEFAULT budget → displaced.
/// Doing it in one closure is what makes "no gap in the seqs" true by
/// construction: nothing between the read of `head_seq` and the write of
/// `head_seq + 1` can yield to another appender.
pub async fn append(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    axum::Extension(identity): axum::Extension<Identity>,
    Json(body): Json<PostBody>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let loopback = is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let now_unix = chrono::Utc::now().timestamp();
    let presence = presence_slice(&state, now_unix);

    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;

    let paths = state.paths.clone();
    let registry = state.slates.clone();
    let slug_owned = slug.clone();
    let user = identity.user.clone();
    let joined = tokio::task::spawn_blocking(move || {
        append_locked(
            &paths,
            &registry,
            &slug_owned,
            body,
            user,
            now_unix,
            &presence,
        )
    })
    .await;
    drop(guard);

    let outcome = match joined {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return append_fail(e),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "slate append join error: {e}"
            )))
        }
    };
    state.slates.note_head(slug.as_str(), outcome.head);

    // ONE event per append, never per read (#24) — and never on the
    // idempotent duplicate-mark path, which wrote nothing.
    if let Some(payload) = outcome.event {
        state.bus.emit("slate.updated", payload);
    }

    let mut response = outcome.response;
    if !loopback {
        scrub_posts(std::slice::from_mut(&mut response.post));
    }
    no_store_json(outcome.status, response)
}

/// The critical section itself. Pure-ish: every argument is explicit and
/// the only side effects are the two writes (ledger line, then meta). Runs
/// on a blocking thread with the per-slug lock held by the caller.
#[allow(clippy::too_many_arguments)]
fn append_locked(
    paths: &KbPaths,
    registry: &crate::slate_registry::SlateRegistry,
    slug: &SlateSlug,
    mut body: PostBody,
    user: String,
    now_unix: i64,
    presence: &[Presence],
) -> Result<Appended, AppendFail> {
    let policy = LivePolicy::default();

    // A slate is created by its FIRST post — there is no create verb in
    // §9's table, and `kb slate now "…"` on a fresh project must work.
    let mut meta = load_meta(paths, slug)?.unwrap_or_else(|| SlateMeta::new(slug, now_unix));
    if meta.closed() {
        return Err(SlateError::new(
            codes::SLATE_CLOSED,
            409,
            format!("slate {slug} is closed — `kb slate reopen {slug}` first"),
        )
        .into());
    }
    let posts = load_posts(paths, slug)?;

    // Ledger caps (§7): a lifecycle event, not a board-full refusal (D18).
    if posts.len() >= slate::LEDGER_MAX_POSTS {
        return Err(SlateError::new(
            codes::SLATE_FULL,
            413,
            format!(
                "slate {slug} holds {} posts (cap {}) — `kb slate rotate {slug}` archives it and starts a fresh generation",
                posts.len(),
                slate::LEDGER_MAX_POSTS
            ),
        )
        .into());
    }
    let bytes = ledger_bytes(paths, slug);
    if bytes >= slate::LEDGER_MAX_BYTES as u64 {
        return Err(SlateError::new(
            codes::SLATE_FULL,
            413,
            format!(
                "slate {slug}'s ledger is {bytes} bytes (cap {}) — `kb slate rotate {slug}`",
                slate::LEDGER_MAX_BYTES
            ),
        )
        .into());
    }

    // --- provenance the DAEMON owns (rules matrix "`origin`") -------------
    body.prov.user = Some(user);
    body.prov.session_id = body.prov.session_id.filter(|s| !s.trim().is_empty());
    if body.prov.session_id.is_none() && body.prov.origin == Origin::Agent {
        // The ONE value a client cannot send: stamped in place of `agent`
        // when no session id resolves. `human` and `import` are left alone.
        body.prov.origin = Origin::Unattributed;
    }
    // A human is not a harness: `origin: human` posts carry the SURFACE that
    // wrote them (`spa`, `cli`) unvalidated; agent/import/unattributed posts
    // must name one of the six session harnesses (rules matrix "`origin`").
    if body.prov.origin != Origin::Human
        && !kb_core::sessions::HARNESSES.contains(&body.prov.harness.as_str())
    {
        return Err(SlateError::new(
            cap_codes::UNKNOWN_HARNESS,
            400,
            format!(
                "unknown harness {:?} — one of {}",
                body.prov.harness,
                kb_core::sessions::HARNESSES.join(", ")
            ),
        )
        .into());
    }
    if body.prov.origin == Origin::Human && body.prov.harness.trim().is_empty() {
        body.prov.harness = "human".to_string();
    }

    // --- the idempotent mark (rules matrix "Mark idempotency") ------------
    // Before validation and before the rate window: a repeat is a NO-OP,
    // and a no-op must neither refuse nor spend budget.
    if let Some(existing) = slate::existing_mark(&posts, &body) {
        return Ok(Appended {
            status: StatusCode::OK,
            response: AppendResponse {
                post: existing.clone(),
                displaced: Vec::new(),
                displaced_total: 0,
                nudge: slate::nudge(
                    slate::session_found_idea_count(&posts, body.prov.session_id.as_deref()),
                    slug,
                ),
                head_seq: meta.head_seq,
            },
            event: None,
            head: crate::slate_registry::CachedHead {
                head_seq: meta.head_seq,
                generation: meta.generation,
            },
        });
    }

    // --- the pure rules, then the two liveness-dependent ones -------------
    slate::validate_post(&body, meta.head_seq, &posts)?;
    check_count_caps(&body, &posts, now_unix, &policy, presence, slug)?;
    slate::check_take(&body, &posts, now_unix, &policy, presence)?;
    slate::check_drop_or_edit(&body, &posts, now_unix, &policy, presence)?;

    // --- the rate window, last, so a refused post spends nothing ----------
    let rate_key = body
        .prov
        .session_id
        .clone()
        .unwrap_or_else(|| format!("anon:{}", body.prov.harness));
    if !registry.allow_post(slug.as_str(), &rate_key, now_unix) {
        return Err(SlateError::new(
            codes::SLATE_RATE,
            429,
            format!(
                "this session has posted {} times to {slug} in the last minute — slow down; the board is working state, not a log",
                slate::MAX_POSTS_PER_SESSION_PER_MINUTE
            ),
        )
        .into());
    }
    Ok(commit_post(
        paths, slug, &mut meta, posts, body, now_unix, &policy, presence,
    )?)
}

/// Mint, write, and measure — the last third of the critical section,
/// shared by `POST …/posts` and by `close` (whose terminal `now` record is
/// an ordinary post, §4 "Rotate and close").
#[allow(clippy::too_many_arguments)]
fn commit_post(
    paths: &KbPaths,
    slug: &SlateSlug,
    meta: &mut SlateMeta,
    posts: Vec<Post>,
    body: PostBody,
    now_unix: i64,
    policy: &LivePolicy,
    presence: &[Presence],
) -> kb_core::Result<Appended> {
    // `displaced` is the digest at the DEFAULT budget before and after,
    // diffed inside the same lock (§5) — deterministic, golden-pinned.
    // SL3c: `displaced_by_append` (not the plain `displaced`) so a
    // `done`/`drop`/`supersede` target this very post just closed is never
    // reported as budget-pushed off the board.
    let read = ProjectOpts {
        budget: slate::BUDGET_OPEN,
        slug: slug.to_string(),
        header_context: header_context(&posts),
        ..ProjectOpts::default()
    };
    let before = slate::project(&posts, now_unix, policy, presence, &read);

    let post = Post::mint(body, meta.head_seq + 1, now_unix);
    append_line(paths, slug, &post)?;
    meta.head_seq = post.seq;
    save_meta(paths, slug, meta)?;

    let mut after_posts = posts;
    after_posts.push(post.clone());
    let after = slate::project(&after_posts, now_unix, policy, presence, &read);
    let (displaced, displaced_total) = slate::displaced_by_append(&before, &after, post.seq);

    // `hide` on the event: the seq THIS post removed from the shown set,
    // or null. Read off the membership projection (`all: true`) so a
    // budget truncation can never be mistaken for a tombstone.
    let facts = slate::project(&after_posts, now_unix, policy, presence, &facts_opts(slug));
    let hide = facts
        .hidden
        .values()
        .filter(|h| h.by == post.seq)
        .map(|h| h.hide)
        .min();

    let nudge = slate::nudge(
        slate::session_found_idea_count(&after_posts, post.prov.session_id.as_deref()),
        slug,
    );
    let event = json!({
        "slug": slug.as_str(),
        "seq": post.seq,
        "kind": post.kind.as_str(),
        "id": post.id,
        "topic": post.topic,
        "re": post.re,
        "hide": hide,
        "pin": post.pin,
    });
    Ok(Appended {
        status: StatusCode::CREATED,
        response: AppendResponse {
            post,
            displaced,
            displaced_total,
            nudge,
            head_seq: meta.head_seq,
        },
        event: Some(event),
        head: crate::slate_registry::CachedHead {
            head_seq: meta.head_seq,
            generation: meta.generation,
        },
    })
}

/// The four COUNT caps (§7): open warns ≤5, open asks ≤20, open hands ≤10,
/// live takes per SESSION ≤3. Measured off the membership projection, not
/// off raw records, so a dropped warn and a superseded ask free their slot
/// the moment they leave the board. An `edit` (a post carrying
/// `supersedes`) is exempt: it REPLACES one open item with another and is
/// net-zero against every one of these.
fn check_count_caps(
    body: &PostBody,
    posts: &[Post],
    now_unix: i64,
    policy: &LivePolicy,
    presence: &[Presence],
    slug: &SlateSlug,
) -> Result<(), SlateError> {
    if body.supersedes.is_some() {
        return Ok(());
    }
    if !matches!(body.kind, Kind::Warn | Kind::Ask | Kind::Hand | Kind::Take) {
        return Ok(());
    }
    let d = slate::project(posts, now_unix, policy, presence, &facts_opts(slug));
    let full = |code: &'static str, what: &str, have: usize, cap: usize, remedy: &str| {
        SlateError::new(
            code,
            413,
            format!("{slug} already has {have} {what} (cap {cap}) — {remedy}"),
        )
    };
    match body.kind {
        Kind::Warn if d.warn_total >= slate::MAX_OPEN_WARNS => Err(full(
            cap_codes::TOO_MANY_OPEN_WARNS,
            "open warns",
            d.warn_total,
            slate::MAX_OPEN_WARNS,
            "drop one that no longer applies",
        )),
        Kind::Ask if d.ask_total >= slate::MAX_OPEN_ASKS => Err(full(
            cap_codes::TOO_MANY_OPEN_ASKS,
            "open asks",
            d.ask_total,
            slate::MAX_OPEN_ASKS,
            "answer or drop one first",
        )),
        Kind::Hand if d.hand_total >= slate::MAX_OPEN_HANDS => Err(full(
            cap_codes::TOO_MANY_OPEN_HANDS,
            "unacknowledged hands",
            d.hand_total,
            slate::MAX_OPEN_HANDS,
            "nobody is picking these up; take or drop one first",
        )),
        Kind::Take => {
            let mine = d
                .sections
                .take
                .iter()
                .filter(|t| t.liveness.is_some_and(slate::Liveness::is_live))
                .filter(|t| {
                    posts
                        .iter()
                        .any(|p| p.seq == t.seq && p.prov.session_id == body.prov.session_id)
                })
                .count();
            if body.prov.session_id.is_some() && mine >= slate::MAX_LIVE_TAKES_PER_SESSION {
                return Err(full(
                    cap_codes::TOO_MANY_LIVE_TAKES,
                    "live takes by this session",
                    mine,
                    slate::MAX_LIVE_TAKES_PER_SESSION,
                    "finish or hand one off first",
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle: close · reopen · rotate · purge (rules matrix "Rotate and close")
// ---------------------------------------------------------------------------

/// `POST …/close` — the final line. Appended as an ordinary `now` post
/// with `topic: null`, which is why it carries provenance like any other.
#[derive(Debug, Deserialize)]
pub struct CloseBody {
    pub line: String,
    #[serde(default)]
    pub prov: slate::Prov,
}

// `POST …/reopen` and `POST …/rotate` take no body at all: neither appends
// a record, so there is no provenance to stamp. A caller that sends one is
// not refused — those handlers simply declare no body extractor.

/// The shared shape all three lifecycle verbs answer with.
fn lifecycle_response(slug: &SlateSlug, meta: &SlateMeta) -> LifecycleResponse {
    LifecycleResponse {
        slug: slug.to_string(),
        head_seq: meta.head_seq,
        generation: meta.generation,
        closed: meta.closed(),
        closed_unix: meta.closed_unix,
        rotated_from: meta.rotated_from,
    }
}

/// `POST /api/slates/{slug}/close` — append a terminal `now` (topic null)
/// and freeze the ledger. ONE `slate.updated` with `kind: "close"`,
/// carrying the appended post's seq and id (§9's lifecycle row); the
/// terminal post does not also announce itself as a `now`.
pub async fn close(
    State(state): State<Arc<KbHandles>>,
    Path(slug): Path<String>,
    axum::Extension(identity): axum::Extension<Identity>,
    Json(body): Json<CloseBody>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let now_unix = chrono::Utc::now().timestamp();
    let presence = presence_slice(&state, now_unix);
    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;

    let paths = state.paths.clone();
    let slug_owned = slug.clone();
    let user = identity.user.clone();
    let joined = tokio::task::spawn_blocking(move || {
        close_locked(&paths, &slug_owned, body, user, now_unix, &presence)
    })
    .await;
    drop(guard);

    let (meta, post) = match joined {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return append_fail(e),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "slate close join error: {e}"
            )))
        }
    };
    state.slates.note_head(
        slug.as_str(),
        crate::slate_registry::CachedHead {
            head_seq: meta.head_seq,
            generation: meta.generation,
        },
    );
    state.bus.emit(
        "slate.updated",
        json!({
            "slug": slug.as_str(),
            "seq": post.seq,
            "kind": "close",
            "id": post.id,
            "topic": serde_json::Value::Null,
            "re": serde_json::Value::Null,
            "hide": serde_json::Value::Null,
            "pin": serde_json::Value::Null,
        }),
    );
    no_store_json(StatusCode::OK, lifecycle_response(&slug, &meta))
}

fn close_locked(
    paths: &KbPaths,
    slug: &SlateSlug,
    body: CloseBody,
    user: String,
    now_unix: i64,
    presence: &[Presence],
) -> Result<(SlateMeta, Post), AppendFail> {
    let Some(mut meta) = load_meta(paths, slug)? else {
        return Err(AppendFail::Core(kb_core::Error::NotFound(format!(
            "no slate {slug}"
        ))));
    };
    if meta.closed() {
        return Err(SlateError::new(
            codes::SLATE_CLOSED,
            409,
            format!("slate {slug} is already closed"),
        )
        .into());
    }
    let mut prov = body.prov;
    prov.user = Some(user);
    prov.session_id = prov.session_id.filter(|s| !s.trim().is_empty());
    if prov.session_id.is_none() && prov.origin == Origin::Agent {
        prov.origin = Origin::Unattributed;
    }
    if prov.harness.is_empty() {
        prov.harness = kb_core::sessions::HARNESS_DEFAULT.to_string();
    }
    if prov.origin != Origin::Human
        && !kb_core::sessions::HARNESSES.contains(&prov.harness.as_str())
    {
        return Err(SlateError::new(
            cap_codes::UNKNOWN_HARNESS,
            400,
            format!("unknown harness {:?}", prov.harness),
        )
        .into());
    }
    let posts = load_posts(paths, slug)?;
    let terminal = PostBody {
        kind: Kind::Now,
        line: body.line,
        body: None,
        topic: None,
        subject: None,
        refs: Vec::new(),
        re: None,
        supersedes: None,
        pin: None,
        anyway: false,
        over: None,
        abandoned: None,
        failed: None,
        to: None,
        prov,
    };
    slate::validate_post(&terminal, meta.head_seq, &posts)?;
    let committed = commit_post(
        paths,
        slug,
        &mut meta,
        posts,
        terminal,
        now_unix,
        &LivePolicy::default(),
        presence,
    )?;
    meta.closed_unix = Some(now_unix);
    save_meta(paths, slug, &meta)?;
    Ok((meta, committed.response.post))
}

/// `POST /api/slates/{slug}/reopen` — clears `closed_unix` and appends
/// NOTHING (rules matrix "Rotate and close"). One `slate.updated` with
/// `kind: "reopen"`.
pub async fn reopen(
    State(state): State<Arc<KbHandles>>,
    Path(slug): Path<String>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;
    let paths = state.paths.clone();
    let slug_owned = slug.clone();
    let joined = tokio::task::spawn_blocking(move || -> kb_core::Result<Option<SlateMeta>> {
        let Some(mut meta) = load_meta(&paths, &slug_owned)? else {
            return Ok(None);
        };
        meta.closed_unix = None;
        save_meta(&paths, &slug_owned, &meta)?;
        Ok(Some(meta))
    })
    .await;
    drop(guard);

    let meta = match joined {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => return not_found(slug.as_str()),
        Ok(Err(e)) => return error_to_problem_json(&e),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "slate reopen join error: {e}"
            )))
        }
    };
    state.bus.emit(
        "slate.updated",
        json!({
            "slug": slug.as_str(),
            "seq": meta.head_seq,
            "kind": "reopen",
            "id": serde_json::Value::Null,
            "topic": serde_json::Value::Null,
            "re": serde_json::Value::Null,
            "hide": serde_json::Value::Null,
            "pin": serde_json::Value::Null,
        }),
    );
    no_store_json(StatusCode::OK, lifecycle_response(&slug, &meta))
}

/// `POST /api/slates/{slug}/rotate` — archive `ledger.jsonl` as
/// `ledger.<gen>.jsonl`, start a fresh one, `generation + 1`,
/// `rotated_from: <gen>` (rules matrix "Rotate and close": ONE directory
/// per slug, always — never a `<slug>@2`, which the slug grammar refuses).
///
/// Two resolutions the design leaves open, taken here:
/// **`head_seq` does NOT reset.** Seqs stay unique across generations, so
/// a client cursor parked at `#66` still advances after a rotate and a
/// `post:#n` ref can never silently point at a different post. The
/// `slate-full` cap counts POSTS IN THE CURRENT LEDGER, so a rotate still
/// buys the full 2,000 back.
/// **Rotating does not close.** "close + archive" seals the generation;
/// leaving `closed_unix` set would mean every `slate-full` recovery needed
/// a `reopen` too, which is exactly the friction rotate exists to remove.
pub async fn rotate(
    State(state): State<Arc<KbHandles>>,
    Path(slug): Path<String>,
) -> Response<Body> {
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;
    let paths = state.paths.clone();
    let slug_owned = slug.clone();
    let joined = tokio::task::spawn_blocking(move || -> kb_core::Result<Option<SlateMeta>> {
        let Some(mut meta) = load_meta(&paths, &slug_owned)? else {
            return Ok(None);
        };
        let live = paths.slate_ledger_file(&slug_owned);
        let archive = paths.slate_archive_file(&slug_owned, meta.generation);
        if live.exists() {
            std::fs::rename(&live, &archive)?;
        }
        // A fresh, empty ledger so a read between the rename and the next
        // append sees an empty board rather than a missing file.
        std::fs::write(&live, b"")?;
        meta.rotated_from = Some(meta.generation);
        meta.generation += 1;
        save_meta(&paths, &slug_owned, &meta)?;
        Ok(Some(meta))
    })
    .await;
    drop(guard);

    let meta = match joined {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => return not_found(slug.as_str()),
        Ok(Err(e)) => return error_to_problem_json(&e),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "slate rotate join error: {e}"
            )))
        }
    };
    state.slates.note_head(
        slug.as_str(),
        crate::slate_registry::CachedHead {
            head_seq: meta.head_seq,
            generation: meta.generation,
        },
    );
    state.bus.emit(
        "slate.updated",
        json!({
            "slug": slug.as_str(),
            "seq": meta.head_seq,
            "kind": "rotate",
            "id": serde_json::Value::Null,
            "topic": serde_json::Value::Null,
            "re": serde_json::Value::Null,
            "hide": serde_json::Value::Null,
            "pin": serde_json::Value::Null,
        }),
    );
    no_store_json(StatusCode::OK, lifecycle_response(&slug, &meta))
}

/// `DELETE /api/slates/{slug}?purge=true` — the ONE hard delete (§7:
/// "Only `DELETE …?purge=true` (loopback-only) removes a directory").
///
/// **Loopback-only, checked INSIDE the handler** like
/// `sessions::presence`, and for the same reason the design gives: this is
/// the only verb on the family that destroys an attributed record, and no
/// redaction floor makes that safe over a proxy. `ConnectInfo` is read as
/// an axum extractor (the house pattern for a route HANDLER — invariant
/// #3's "read from extensions" rule targets generic `Request`-taking
/// middleware), so a wiring without it fails CLOSED by rejecting.
///
/// `?purge=true` is REQUIRED: an unqualified DELETE is a 400, never a
/// delete. There is no soft delete — a slate's remedy for "too much" is
/// `drop`, `close` or `rotate`.
pub async fn purge(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<PurgeParams>,
) -> Response<Body> {
    if !is_loopback_origin(Some(peer.ip()), &headers, &state.origin.trusted_proxies) {
        return slate_problem(&SlateError::new(
            "loopback-only",
            403,
            "purging a slate is loopback-only",
        ));
    }
    let slug = match parse_slug(&slug) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !flag(q.purge.as_ref()) {
        return bad(
            "purge-required",
            format!("DELETE /api/slates/{slug} needs ?purge=true — there is no soft delete"),
        );
    }
    let lock = state.slate_lock_for(slug.as_str());
    let guard = lock.lock().await;
    let paths = state.paths.clone();
    let slug_owned = slug.clone();
    let joined = tokio::task::spawn_blocking(move || -> kb_core::Result<bool> {
        let dir = paths.slate_dir(&slug_owned);
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir)?;
        Ok(true)
    })
    .await;
    drop(guard);

    match joined {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return not_found(slug.as_str()),
        Ok(Err(e)) => return error_to_problem_json(&e),
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "slate purge join error: {e}"
            )))
        }
    }
    state.slates.forget(slug.as_str());
    state
        .bus
        .emit("slate.deleted", json!({ "slug": slug.as_str() }));
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::slate::Prov;

    fn prov(session: Option<&str>) -> Prov {
        Prov {
            harness: "claude".to_string(),
            session_id: session.map(str::to_string),
            model: None,
            cwd: Some("/home/someone/work/orchard".to_string()),
            origin: Origin::Agent,
            user: None,
            job_id: None,
        }
    }

    fn post(seq: u64, kind: Kind, line: &str, session: Option<&str>) -> Post {
        Post::mint(
            PostBody {
                kind,
                line: line.to_string(),
                body: None,
                topic: None,
                subject: None,
                refs: Vec::new(),
                re: None,
                supersedes: None,
                pin: None,
                anyway: false,
                over: None,
                abandoned: None,
                failed: None,
                to: None,
                prov: prov(session),
            },
            seq,
            1_767_225_600,
        )
    }

    #[test]
    fn the_scrub_floor_collapses_cwd_and_touches_nothing_else() {
        let mut posts = vec![post(
            1,
            Kind::Warn,
            "a line with /an/absolute/path in it",
            None,
        )];
        posts[0].body = Some("a body with /another/absolute/path".to_string());
        let before_line = posts[0].line.clone();
        let before_body = posts[0].body.clone();
        scrub_posts(&mut posts);
        assert_eq!(posts[0].prov.cwd.as_deref(), Some("orchard"));
        assert_eq!(posts[0].line, before_line, "lines keep their own 200 cap");
        assert_eq!(
            posts[0].body, before_body,
            "bodies keep their own 2,000 cap"
        );
    }

    #[test]
    fn a_ledger_line_without_its_newline_is_not_yet_committed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke".to_string());
        let slug = SlateSlug::new("orchard").unwrap();
        append_line(&paths, &slug, &post(1, Kind::Now, "first", None)).unwrap();
        // A torn append: bytes with no terminating newline. A lock-free
        // reader must drop it, not fail the whole read.
        let ledger = paths.slate_ledger_file(&slug);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        f.write_all(b"{\"seq\":2,\"id\":\"e_ff\"").unwrap();
        drop(f);
        let posts = load_posts(&paths, &slug).unwrap();
        assert_eq!(
            posts.len(),
            1,
            "the torn tail is dropped, the read succeeds"
        );
        assert_eq!(posts[0].seq, 1);
    }

    #[test]
    fn meta_round_trips_and_a_foreign_schema_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke".to_string());
        let slug = SlateSlug::new("orchard").unwrap();
        assert!(load_meta(&paths, &slug).unwrap().is_none());
        let mut meta = SlateMeta::new(&slug, 1_767_225_600);
        meta.head_seq = 7;
        save_meta(&paths, &slug, &meta).unwrap();
        assert_eq!(load_meta(&paths, &slug).unwrap().unwrap().head_seq, 7);

        meta.schema = "kb-slate/2".to_string();
        save_meta(&paths, &slug, &meta).unwrap();
        assert!(
            load_meta(&paths, &slug).is_err(),
            "a foreign schema REFUSES, it is never read as kb-slate/1"
        );
    }

    #[test]
    fn list_slugs_skips_dirs_without_a_meta_and_illegal_names() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke".to_string());
        let good = SlateSlug::new("orchard").unwrap();
        save_meta(&paths, &good, &SlateMeta::new(&good, 1)).unwrap();
        std::fs::create_dir_all(paths.state.join("slates/no-meta-here")).unwrap();
        std::fs::create_dir_all(paths.state.join("slates/NotALegalSlug")).unwrap();
        let slugs = list_slugs(&paths);
        assert_eq!(slugs.len(), 1);
        assert_eq!(slugs[0].as_str(), "orchard");
    }

    #[tokio::test]
    async fn the_problem_body_carries_code_in_both_places_and_the_holder() {
        let holder = slate::HolderInfo {
            seq: 55,
            line: "review_gate.rs".to_string(),
            harness: "codex".to_string(),
            session_short: "8f2a".to_string(),
            age_secs: 1260,
            liveness: slate::Liveness::Stale,
        };
        let err =
            SlateError::new(codes::SLATE_TAKEN, 409, "held by codex/8f2a").with_holder(holder);
        let resp = slate_problem(&err);
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "slate-taken");
        assert_eq!(body["type"], "urn:kb:errors:conflict");
        assert_eq!(body["title"], "Conflict");
        assert_eq!(
            body["detail"], "slate-taken: held by codex/8f2a",
            "§9 pins `<code>: <text>` so a CLI without the extension member still reads it"
        );
        assert_eq!(body["holder"]["seq"], 55);
        assert_eq!(body["holder"]["liveness"], "stale");
    }

    #[test]
    fn a_413_cap_keeps_its_status_even_though_kb_core_has_no_413_variant() {
        let resp = slate_problem(&SlateError::new(
            cap_codes::TOO_MANY_OPEN_WARNS,
            413,
            "five already",
        ));
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn topics_and_header_context_come_from_the_records() {
        let mut posts = vec![
            post(1, Kind::Now, "one", None),
            post(2, Kind::Found, "two", None),
        ];
        posts[0].topic = Some("v7".to_string());
        posts[1].topic = Some("perf".to_string());
        assert_eq!(
            topics_of(&posts),
            vec!["perf".to_string(), "v7".to_string()]
        );
        assert_eq!(
            header_context(&posts).as_deref(),
            Some("/home/someone/work/orchard"),
            "the slate's recorded root is the newest post that declared one"
        );
        assert_eq!(header_context(&[]), None, "absent, never guessed");
    }

    #[test]
    fn who_tag_prefers_the_sessions_own_author_tag() {
        let posts = vec![post(1, Kind::Now, "one", Some("4b7e91c2a0"))];
        assert_eq!(who_tag(&posts, None), None, "no session ⇒ no `you:` clause");
        assert_eq!(
            who_tag(&posts, Some("4b7e91c2a0")).as_deref(),
            Some("claude/4b7e")
        );
        assert_eq!(
            who_tag(&posts, Some("aaaabbbbcccc")).as_deref(),
            Some("aaaa"),
            "a session that has not posted here yet still gets a short tag"
        );
    }

    #[test]
    fn has_sketch_reads_the_fence_not_the_word() {
        assert!(slate::has_sketch(Some("text\n```mermaid\ngraph TD\n```\n")));
        assert!(!slate::has_sketch(Some("we should draw a mermaid diagram")));
        assert!(!slate::has_sketch(Some("```rust\nfn main() {}\n```")));
        assert!(!slate::has_sketch(None));
    }
}
