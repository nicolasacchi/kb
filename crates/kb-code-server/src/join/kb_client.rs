//! Typed reqwest client for kb's session↔commit join surface —
//! `GET /api/sessions/by-commit?sha=` and `GET /api/sessions/commit-map?
//! since=&limit=&offset=` (`crates/kb-server/src/routes/sessions.rs`, W0.6).
//! Reuses [`crate::config::KbDaemonSection`] (`[kb_daemon]`, already the
//! W2.4 federation target for the Search-Everywhere box's sessions lane —
//! see `crate::search::sessions`'s module doc for the same "kb-code and kb
//! are sibling PROCESSES, not one library" framing).
//!
//! Unlike `search::sessions::search` (a stateless, build-a-client-per-call
//! helper fn), [`KbClient`] is a persistent per-boot object
//! (`AppState::kb_client`) because it ALSO owns a cached, TTL'd COMMIT-MAP
//! snapshot ([`KbClient::commit_map_snapshot`]): the join ladder's fuzzy/
//! squash-subject/time-window arms need to scan the whole recent commit
//! history kb knows about for every candidate commit, and re-fetching that
//! per commit would turn a bulk `kb-code join`-driven resolve into an
//! O(commits) storm of HTTP round trips against kb. The snapshot is fetched
//! ONCE, cached for [`SNAPSHOT_TTL`], and refreshed on demand (`force =
//! true` — the TTL-upgrade path: a `none`/`fuzzy` cache row that's gone
//! stale gets ONE chance to see fresher kb-side data before falling back to
//! its previous answer).
//!
//! **kb-sibling/1**: before the first real call of this process, the client
//! handshakes against kb's `/api/identity` Hello
//! ([`KbClient::ensure_sibling`]) and fails CLOSED on a mismatch — see the
//! `Handshake` enum below for the three outcomes (match, legacy peer,
//! mismatch) and why an unreachable peer is none of them.

use crate::config::KbDaemonSection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Mirrors `search::sessions::TIMEOUT` — this lane must never be the reason
/// a `kb-code join` resolution (or a bulk resolve) hangs.
pub const TIMEOUT: Duration = Duration::from_millis(1_500);

/// How long a fetched commit-map snapshot is trusted before the next
/// `commit_map_snapshot(false)` call re-fetches — see the module doc.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(300);

/// Page size for paging the full snapshot via `commit-map`'s own
/// `limit`/`next_offset` cursor.
const SNAPSHOT_PAGE_LIMIT: u32 = 500;

/// Safety ceiling on the TOTAL rows a snapshot fetch will accumulate across
/// pages — a runaway/misbehaving kb daemon (or a corpus with an enormous
/// commit history) can't make a single `commit_map_snapshot` call unbounded.
/// Comfortably above what one operator's own local session history would
/// ever produce; a fleet this large would need the real kb search surface,
/// not this bulk feed.
const SNAPSHOT_MAX_ROWS: usize = 20_000;

/// Floor on kb's `since` filter — the fuzzy/squash arms only ever care
/// about REASONABLY recent sessions (a commit being resolved today has no
/// business matching a session from a year ago); bounding the fetch window
/// also keeps `SNAPSHOT_MAX_ROWS` meaningful on a long-lived kb corpus.
const SNAPSHOT_LOOKBACK: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// One `GET /api/sessions/by-commit` match — mirrors kb-server's
/// `CommitMatchOut` (`crates/kb-server/src/routes/sessions.rs`) field for
/// field; `#[serde(default)]` on `trailers` because the server elides it
/// entirely (`skip_serializing_if = "Vec::is_empty"`) when there are none.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CommitMatch {
    pub kb: String,
    pub session_id: String,
    pub artifact_id: String,
    pub kind: String,
    pub sha: Option<String>,
    pub sha_full: Option<String>,
    pub resolved: bool,
    pub subject: Option<String>,
    #[serde(default)]
    pub trailers: Vec<String>,
    pub display_name: String,
    pub started_at: i64,
}

#[derive(Debug, Deserialize)]
struct ByCommitResponse {
    matches: Vec<CommitMatch>,
}

/// One `GET /api/sessions/{session_id}/commits` row — mirrors kb-server's
/// `CommitOut` (`crates/kb-server/src/routes/sessions.rs`) field for field.
/// Unlike [`CommitMatch`]/[`CommitMapRow`] (both scans ACROSS every commit
/// kb knows about, hence carrying `kb`/`session_id`/`started_at`), this is
/// already scoped to the ONE session the caller asked for — kb's own route
/// fans out across every corpus and returns a flat, `kb`-less list (a
/// session id is unique across the whole daemon in practice — kb-core
/// invariant #11 — so no corpus tag is needed to disambiguate a row here).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SessionCommitEntry {
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    pub sha: Option<String>,
    pub subject: Option<String>,
    pub resolved: bool,
    pub sha_full: Option<String>,
    pub repo_root: Option<String>,
    pub author: Option<String>,
    pub parents: Option<i64>,
    #[serde(default)]
    pub trailers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SessionCommitsResponse {
    commits: Vec<SessionCommitEntry>,
}

/// Pain-relevant subset of kb-server's `SessionOut` (V3.2-B2).
/// Full detail is larger; we only deserialize what fusion needs.
/// Note: `GET /api/sessions/{id}` flattens SessionOut onto the root
/// (`#[serde(flatten)]`) plus `memory_ids` — there is no nested `session`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SessionDetailSignals {
    pub session_id: String,
    #[serde(default)]
    pub error_count: u32,
    /// Honest active duration (seconds). Stored but never used as a pain proxy.
    #[serde(default)]
    pub active_secs: i64,
    /// Wall-clock span fallback when active_secs is 0.
    #[serde(default)]
    pub duration_ms: i64,
    /// Lance artifact ids whose `kb_session` matches this session — mirrors
    /// kb-server's `SessionDetailResponse.memory_ids` field for field.
    /// DISPLAY-ONLY: `provenance::why` surfaces these ids so a caller can
    /// render "session also wrote N memories" and link out — no
    /// `KbClient` method exists (and none should be added here) for kb's
    /// separate `GET /api/sessions/{id}/memories` route, which serves the
    /// full memory ROWS; fetching memory bodies/titles is out of scope for
    /// this client.
    #[serde(default)]
    pub memory_ids: Vec<String>,
}

/// One `GET /api/sessions/commit-map` row — mirrors kb-server's
/// `CommitMapRowOut`, which `#[serde(flatten)]`s its `CommitOut` half; since
/// this is a fresh deserialize target (not reusing kb-server's type), the
/// flattened shape is just... a flat struct. Deliberately does NOT carry
/// `display_name` — kb's bulk feed omits it on purpose ("no title/lance
/// resolution — the bulk feed exists to be cheap," `CommitMapRowOut`'s own
/// doc) — so fuzzy/squash/time-window attributions built from this feed
/// leave `Attribution::display_name` as `None` (honest absence, never
/// fabricated).
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CommitMapRow {
    pub kb: String,
    pub session_id: String,
    pub artifact_id: String,
    pub started_at: i64,
    pub kind: String,
    pub sha: Option<String>,
    pub subject: Option<String>,
    pub resolved: bool,
    pub sha_full: Option<String>,
    pub repo_root: Option<String>,
    pub author: Option<String>,
    pub parents: Option<i64>,
    #[serde(default)]
    pub trailers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CommitMapResponse {
    commits: Vec<CommitMapRow>,
    #[serde(default)]
    next_offset: Option<u32>,
}

/// One `GET /api/why?path=`'s `sessions[].decisions[]` entry — mirrors
/// kb-server's `DecisionOut` (`crates/kb-server/src/routes/sessions.rs`)
/// field for field. `Serialize` too (not just `Deserialize`): W3.4's
/// `provenance::why` re-serves this straight through in its own
/// `kb_context.decisions` — see that module's doc.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WhyDecision {
    pub kind: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
}

/// One `GET /api/why?path=`'s `sessions[]` entry — trimmed to the fields
/// `provenance::why` actually consumes (kb's own `WhySessionOut` also
/// carries `commits`, deliberately not mirrored here — see that route's
/// doc; an unrecognized extra field is simply ignored by serde).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WhySession {
    pub session_id: String,
    pub kb: String,
    pub display_name: String,
    pub started_at: i64,
    pub action: String,
    pub confidence: String,
    #[serde(default)]
    pub first_user_prompt: Option<String>,
    #[serde(default)]
    pub decisions: Vec<WhyDecision>,
}

#[derive(Debug, Deserialize)]
struct WhyResponse {
    #[serde(default)]
    sessions: Vec<WhySession>,
}

#[derive(Debug, thiserror::Error)]
pub enum KbClientError {
    #[error("kb daemon federation is disabled ([kb_daemon] enabled = false)")]
    Disabled,
    #[error("build http client: {0}")]
    ClientBuild(String),
    #[error("kb daemon unreachable at {0}: {1}")]
    Unreachable(String, String),
    #[error("kb daemon returned {0}")]
    BadStatus(reqwest::StatusCode),
    #[error("parse kb daemon response: {0}")]
    Parse(String),
    /// DCB-W2.B.R fix 1 (security) — belt-and-braces inside
    /// [`KbClient::resolve_doc_by_path`] itself: a `path` containing a `.`/
    /// `..` segment is refused here even though the route
    /// (`doclens::wire::resolve_path_route`) already checks it first, so a
    /// future direct caller of this client can't reopen the traversal.
    #[error("path {0:?} contains a \".\" or \"..\" segment")]
    InvalidPath(String),
    /// kb-sibling/1 — the peer answered the handshake but does not speak
    /// this contract. FAIL CLOSED: every call returns this, and it is
    /// deliberately DISTINCT from [`KbClientError::Unreachable`] (the peer
    /// is up and answering — talking to it is what's unsafe). Sticky for
    /// the process' lifetime; a redeployed peer needs a kb-code restart.
    #[error("kb daemon speaks a different sibling contract: {0}")]
    SiblingMismatch(String),
}

pub type Result<T> = std::result::Result<T, KbClientError>;

// --- DCB W1.C — `coderef/1` (kb's doc-level code-reference extraction) ----
//
// These are DESERIALIZE-only mirrors of kb-server's `routes/coderefs.rs`
// wire types (the frozen `coderef/1`, R1). Unknown fields are ignored, so
// kb may carry more than doclens reads. HINT names are load-bearing: kb has
// no tree and no symbols and is structurally unable to mint a trust class
// (the DCB invariant) — every classification happens in `crate::doclens`,
// live, and is never persisted.

/// One `coderef/1` group header. `key == anchor == kb-h-<slug>` BY
/// CONSTRUCTION on the producer side (the group key IS the derived heading
/// id; an ancestor element's `id` is never used) — doclens re-derives
/// neither and never assumes they differ.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CodeRefGroup {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub ordinal: u32,
}

/// `<meta name="kb-code-rev" content="<label>@<sha>[+dirty]">`, already
/// parsed by kb's extractor into `{label, sha, dirty}`. Consumed by W2.A's
/// remap. INBOUND field is `label` (the frozen wire); `repo_label` is kept
/// as an alias and is the OUTBOUND spelling on `codelens/1`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CodeRev {
    #[serde(alias = "repo_label")]
    pub label: String,
    pub sha: String,
    #[serde(default)]
    pub dirty: bool,
}

/// One extracted reference.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CodeRefRow {
    #[serde(default)]
    pub ordinal: u32,
    /// FK into `groups[].key`, or `null` for a ref before the first heading.
    /// A null is NEVER a sentinel group — see [`CodeRefsDoc::ungrouped_count`].
    #[serde(default)]
    pub group: Option<String>,
    /// `path | path_line | path_range | path_list | symbol_method |
    /// symbol_const | issue | external` — the frozen set (R10). No `dir`.
    #[serde(default)]
    pub kind: String,
    #[serde(alias = "raw_text", default)]
    pub raw: String,
    #[serde(default)]
    pub path_hint: Option<String>,
    /// For `kind == "issue"` this is the issue NUMBER, not a line (the
    /// producer's documented column overload — R11). `doclens::resolve`'s
    /// issue arm is the ONLY place that overload is decoded.
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    /// `"425,440"` / `"30,51-65,113-119"` on a `path_list` ref; `line_start`/
    /// `line_end` mirror the FIRST span.
    #[serde(default)]
    pub line_spans: Option<String>,
    #[serde(default)]
    pub symbol_container: Option<String>,
    #[serde(default)]
    pub symbol_member: Option<String>,
    /// HUMAN text (≤200 bytes). Never an input to any predicate.
    #[serde(default)]
    pub context: Option<String>,
    /// MACHINE tokens (≤8), the ONLY confirm-token source. Absent/empty ⇒
    /// `doclens::resolve::confirm_tokens`' prose-scan fallback.
    #[serde(default)]
    pub context_tokens: Vec<String>,
    #[serde(default)]
    pub declared: bool,
}

/// ONE struct deserializes BOTH the per-doc response and a feed `docs[]`
/// element (which omits `schema`/`kb` as redundant with its envelope) — two
/// Rust structs would be two places to forget a field.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CodeRefsDoc {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub kb: String,
    /// AUTHORITATIVE after a 301: reqwest follows kb's moves chain and the
    /// BODY names the FINAL id. Callers compare against what they asked for
    /// and re-key their pin — never parse the redirect `Location` (D13/R13).
    pub doc_id: String,
    /// The doc's SOURCE-RELATIVE path. Feeds `codelens/1`'s
    /// `doc_path`/`doc_href` — it is NOT a key: the artifact id is (D14).
    #[serde(default)]
    pub doc_path: Option<String>,
    /// Hash of the source at the LAST EXTRACTION, not necessarily of the
    /// file as it stands now (R14). A consumer treats `null` as "unknown —
    /// no banner", never as "changed".
    #[serde(default)]
    pub doc_hash: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `null` together with `never_scanned: true`.
    #[serde(default)]
    pub extracted_at: Option<i64>,
    /// True when kb has NO `code_refs_docs` row for this doc (indexed before
    /// DCB, or a memory-session transcript the hook's gate skips). doclens
    /// must say so explicitly and NEVER render "no code refs" — the SPA
    /// wording is "code refs never scanned" + a `kb reindex` remedy hint
    /// (CT-E3); the contract is the three-state distinction, not the words.
    #[serde(default)]
    pub never_scanned: bool,
    #[serde(default)]
    pub code_rev: Option<CodeRev>,
    #[serde(default)]
    pub ref_count: u32,
    /// Refs with `group == null`. Carried straight through to `codelens/1`
    /// (R9) so no consumer scans `refs` to learn it.
    #[serde(default)]
    pub ungrouped_count: u32,
    /// kb-side ref truncation (distinct from doclens' own
    /// `MAX_REFS_PER_LENS`).
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub groups: Vec<CodeRefGroup>,
    #[serde(default)]
    pub refs: Vec<CodeRefRow>,
}

/// One page of `GET /api/kb/{kb}/code-refs`.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CodeRefsFeedPage {
    #[serde(default)]
    pub docs: Vec<CodeRefsDoc>,
    /// ONE opaque string (R3). doclens NEVER parses it — it is round-tripped
    /// verbatim into the next request and (in W3) persisted verbatim. kb
    /// builds it as `"<extracted_at>:<artifact_id>"` and parses it
    /// server-side; that grammar is kb's business and may change without
    /// touching this client. OMITTED (not null) by kb on the last page.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

struct Snapshot {
    fetched_at: Instant,
    rows: Arc<Vec<CommitMapRow>>,
}

// --- kb-sibling/1 handshake ----------------------------------------------
//
// Before its FIRST real call this client GETs kb's `/api/identity` and
// checks the Hello fields (`kb_core::sibling`). rust-analyzer's
// advisory-only server/client handshake is the counter-example the design
// ruled against: advisory IS the documented failure mode, so a mismatch
// here is fail-closed.

/// The DESERIALIZE mirror of kb-server's identity Hello. Every field is
/// `Option` on purpose — that is how "a legacy kb binary, predating
/// kb-sibling/1" is DETECTED (absent, not wrong), and it is why a garbage
/// or proxied body degrades to the legacy path instead of erroring.
#[derive(Debug, Deserialize, Default)]
struct IdentityHello {
    #[serde(default)]
    sibling_protocol: Option<String>,
    #[serde(default)]
    sibling_major: Option<u32>,
}

/// The cached outcome of the one handshake this process performs. Only
/// REACHED conclusions are cached — a probe that never got an answer isn't
/// one, so it is retried on the next call (see [`KbClient::ensure_sibling`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Handshake {
    /// Hello present and matching — proceed, forever.
    Ok,
    /// **Legacy-peer grandfather rule.** The Hello fields are ABSENT: this
    /// is a kb binary older than kb-sibling/1. Warn once and proceed exactly
    /// as this client did before the handshake existed. Rolling deploys are
    /// the whole reason — during one, kb-code may reach the not-yet-upgraded
    /// kb, and refusing there would turn an ordering detail into an outage.
    /// A MISMATCH is a different thing entirely (the peer told us its
    /// contract and it isn't ours) and never lands here.
    LegacyPeer,
    /// Hello present and mismatched — every call fails closed with this
    /// reason string.
    Mismatch(String),
}

/// Per-boot federation handle — see the module doc. `Clone`-free by design
/// (held as `Arc<KbClient>` on `AppState`, same convention as `state.store`/
/// `state.file_index`): the cached snapshot lives behind an internal
/// `RwLock`, so cloning the whole client would just fork the cache.
pub struct KbClient {
    cfg: KbDaemonSection,
    snapshot: RwLock<Option<Snapshot>>,
    /// Serializes the actual page-walk in [`KbClient::commit_map_snapshot`]
    /// — see that fn's doc for why a plain read-then-write on `snapshot`
    /// isn't enough: N concurrent stale readers would each pay the full
    /// multi-page fetch. Held only around the fetch-and-refill path, never
    /// across the cheap cache-hit read.
    fetch_guard: tokio::sync::Mutex<()>,
    /// kb's bearer token, read ONCE at construction from
    /// `[kb_daemon] token_file` (`KbDaemonSection::bearer_token`) — needed
    /// when kb's `auth_bearer` doesn't see this process as loopback (the
    /// docker-published prod shape). `None` = token-less requests, the
    /// native side-by-side default.
    token: Option<String>,
    /// Built ONCE at construction and shared by every call — a
    /// `reqwest::Client` owns its own connection pool, so a fresh client
    /// per request (the pre-review-round shape) silently defeated pooling:
    /// every federation call paid a new TCP handshake, and a bulk resolve
    /// (backfill) opened one socket per commit. Builder failure is
    /// captured here (not panicked at boot) and surfaced per-call as
    /// `ClientBuild`, preserving the old lazy-build error behaviour.
    client: std::result::Result<reqwest::Client, String>,
    /// kb-sibling/1 — the once-per-process handshake outcome, resolved
    /// LAZILY on the first real call (never at construction: `KbClient::new`
    /// runs inside `bind_and_spawn`, and a kb daemon that happens to be down
    /// at that moment must not delay or fail kb-code's own boot). Cached for
    /// the process' lifetime, refreshed only by a restart — a `OnceCell`,
    /// not a TTL: the answer is a property of the two BINARIES, and either
    /// one changing means a redeploy.
    sibling: tokio::sync::OnceCell<Handshake>,
}

impl KbClient {
    pub fn new(cfg: KbDaemonSection) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string());
        let token = cfg.bearer_token();
        Self {
            cfg,
            snapshot: RwLock::new(None),
            fetch_guard: tokio::sync::Mutex::new(()),
            client,
            token,
            sibling: tokio::sync::OnceCell::new(),
        }
    }

    /// The shared pooled client — see the field doc.
    fn client(&self) -> Result<&reqwest::Client> {
        self.client
            .as_ref()
            .map_err(|e| KbClientError::ClientBuild(e.clone()))
    }

    /// `GET <url>` with the bearer token applied when configured — every
    /// request this client sends goes through here so no call site can
    /// forget the header.
    fn get(&self, client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
        let rb = client.get(url);
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// kb-sibling/1 — one `GET {url}/api/identity`, classified into a
    /// [`Handshake`]. `Err(())` means the probe never REACHED a conclusion
    /// (transport error, non-2xx, unparseable body): the caller must not
    /// cache that, and must proceed — a network failure is the peer being
    /// unreachable, which this client already degrades honestly per call,
    /// and turning it into a fail-closed latch would let one blip wedge
    /// federation until a restart.
    async fn probe_sibling(&self) -> std::result::Result<Handshake, ()> {
        let client = self.client().map_err(|_| ())?;
        let url = format!("{}/api/identity", self.cfg.url.trim_end_matches('/'));
        let resp = self.get(client, &url).send().await.map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        let hello: IdentityHello = resp.json().await.map_err(|_| ())?;
        let (Some(protocol), Some(major)) = (hello.sibling_protocol, hello.sibling_major) else {
            // Legacy peer — see `Handshake::LegacyPeer`. Deliberately also
            // the landing spot for a HALF-populated Hello: a peer that
            // names one field and not the other has told us nothing we can
            // check, which is the same information state as silence.
            return Ok(Handshake::LegacyPeer);
        };
        if protocol == kb_core::sibling::SIBLING_PROTOCOL
            && major == kb_core::sibling::SIBLING_MAJOR
        {
            return Ok(Handshake::Ok);
        }
        Ok(Handshake::Mismatch(format!(
            "peer at {} reports sibling_protocol={protocol:?} sibling_major={major}; \
             this daemon speaks {:?} major {}",
            self.cfg.url,
            kb_core::sibling::SIBLING_PROTOCOL,
            kb_core::sibling::SIBLING_MAJOR,
        )))
    }

    /// The gate every request method runs after its `enabled` check and
    /// before touching the wire. Cheap after the first call (one
    /// `OnceCell` read); the probe itself happens at most once per process
    /// per REACHED conclusion.
    async fn ensure_sibling(&self) -> Result<()> {
        let state = self
            .sibling
            .get_or_try_init(|| async {
                let outcome = self.probe_sibling().await?;
                // Logged HERE, inside the init closure, so each conclusion
                // is announced exactly once per process no matter how many
                // calls ride behind it.
                match &outcome {
                    Handshake::Ok => tracing::info!(
                        kb_url = %self.cfg.url,
                        protocol = kb_core::sibling::SIBLING_PROTOCOL,
                        "kb-code: kb-sibling handshake ok"
                    ),
                    Handshake::LegacyPeer => tracing::warn!(
                        kb_url = %self.cfg.url,
                        "kb-code: kb daemon carries no kb-sibling Hello (a binary older than \
                         kb-sibling/1) — proceeding under the legacy-peer grandfather rule"
                    ),
                    Handshake::Mismatch(reason) => tracing::error!(
                        kb_url = %self.cfg.url,
                        %reason,
                        "kb-code: kb-sibling handshake MISMATCH — federation with kb is now \
                         failing closed until this daemon restarts against a matching peer"
                    ),
                }
                Ok(outcome)
            })
            .await;
        match state {
            Ok(Handshake::Mismatch(reason)) => Err(KbClientError::SiblingMismatch(reason.clone())),
            // Ok / LegacyPeer both proceed; `Err(())` is an unreached
            // conclusion (nothing cached) — proceed too and let the real
            // call report the peer's unreachability itself.
            Ok(_) | Err(()) => Ok(()),
        }
    }

    /// `GET {url}/api/sessions/by-commit?sha=<sha>` — `sha` is passed through
    /// verbatim (the caller, `join::ladder`, has already locally
    /// disambiguated it to a full hex sha before this is ever called; this
    /// client applies no validation of its own beyond what reqwest's own
    /// URL-encoding does).
    /// V73-K3 — is the kb sibling configured at all? A read that DEGRADES
    /// without it (the hunk↔turn join's commit witness) needs to tell
    /// "kb said nothing" apart from "kb was never asked", and every method
    /// below already returns `Disabled` for the second case without saying
    /// so on the wire.
    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub async fn by_commit(&self, sha: &str) -> Result<Vec<CommitMatch>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/sessions/by-commit",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .query(&[("sha", sha)])
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: ByCommitResponse = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(body.matches)
    }

    /// `GET {url}/api/sessions/{session_id}/commits` (W3.5, the session
    /// diff) — every git action the session produced, kb's own capture
    /// order (`kb sessions capture`'s `seq` column). ONE round trip per
    /// session, already server-side scoped — the preferred path over
    /// paging `commit_map_snapshot` and filtering client-side by
    /// `session_id`, which would ALSO cost this client its `since`/
    /// `SNAPSHOT_MAX_ROWS` lookback bounds on a session outside that
    /// window. Returns an EMPTY `Vec` (not an error) for a session id kb
    /// has never heard of — kb's own route degrades the same way (see
    /// `crates/kb-server/src/routes/sessions.rs::commits`'s doc), so
    /// "unknown to kb" and "known but has no commits" are indistinguishable
    /// here by design; `sessiondiff::session_diff` decides "unknown
    /// session" from its OWN local transcript index instead, never from
    /// this call's emptiness.
    pub async fn session_commits(&self, session_id: &str) -> Result<Vec<SessionCommitEntry>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/sessions/{session_id}/commits",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: SessionCommitsResponse = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(body.commits)
    }

    /// `GET {url}/api/sessions/{session_id}` — V3.2-B2 pain inputs.
    ///
    /// Pulls the fields reachable WITHOUT new kb-daemon work:
    /// `error_count` (tool-result errors), `active_secs` (honest active
    /// duration). `SessionOut` has no distinct test-failure / fail_count
    /// field — callers store `fail_count = 0` and never invent one.
    /// Returns `None` on 404 (unknown session), not an error.
    pub async fn session_detail(&self, session_id: &str) -> Result<Option<SessionDetailSignals>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/sessions/{session_id}",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        // Flattened SessionOut + memory_ids; unknown fields ignored.
        let body: SessionDetailSignals = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(Some(body))
    }

    /// One `GET /api/sessions/commit-map` page.
    async fn commit_map_page(
        &self,
        client: &reqwest::Client,
        since: i64,
        limit: u32,
        offset: u32,
    ) -> Result<CommitMapResponse> {
        let url = format!(
            "{}/api/sessions/commit-map",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .query(&[
                ("since", since.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ])
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))
    }

    /// The cached snapshot if one exists and is still within
    /// [`SNAPSHOT_TTL`] — the "is the cache still good" check
    /// [`Self::commit_map_snapshot`] runs both BEFORE queuing for
    /// `fetch_guard` (the fast path, no lock contention on a warm cache) and
    /// AFTER acquiring it (the coalescing re-check — see that fn's doc).
    async fn fresh_snapshot(&self) -> Option<Arc<Vec<CommitMapRow>>> {
        let guard = self.snapshot.read().await;
        guard
            .as_ref()
            .filter(|snap| snap.fetched_at.elapsed() < SNAPSHOT_TTL)
            .map(|snap| snap.rows.clone())
    }

    /// The cached commit-map snapshot, fetched fresh (paginating until a
    /// short/empty page, `SNAPSHOT_MAX_ROWS`, or `SNAPSHOT_LOOKBACK`'s
    /// `since` floor bounds it) when there is no cached copy, the cached
    /// copy is older than [`SNAPSHOT_TTL`], or `force` is `true`. Returns a
    /// cheap `Arc` clone of the (possibly just-refreshed) row list either
    /// way — callers never see a torn/in-progress fetch (the lock is held
    /// only long enough to read or replace the cached `Snapshot`, never
    /// across the HTTP calls themselves — see the read/write split below).
    ///
    /// Double-checked locking against `fetch_guard` coalesces concurrent
    /// stale readers: on a plain read-then-fetch, N callers racing a TTL
    /// expiry would each independently run the full multi-page walk. A
    /// stale (or cache-less) `force = false` caller instead queues on
    /// `fetch_guard`, then RE-CHECKS freshness once it holds it — a waiter
    /// that queued behind whichever caller won the fetch sees that fetch's
    /// now-fresh result and returns it without paying for a second walk.
    /// `force = true` callers skip both freshness checks (by definition they
    /// want current data, not whatever's cached) but still queue on
    /// `fetch_guard` so a burst of forced refreshes serializes into fetches
    /// rather than firing concurrently — `force` only changes WHETHER a
    /// fetch happens, never whether it is exclusive.
    pub async fn commit_map_snapshot(&self, force: bool) -> Result<Arc<Vec<CommitMapRow>>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        if !force {
            if let Some(rows) = self.fresh_snapshot().await {
                return Ok(rows);
            }
        }

        let _fetch_permit = self.fetch_guard.lock().await;
        if !force {
            if let Some(rows) = self.fresh_snapshot().await {
                return Ok(rows);
            }
        }

        let client = self.client()?;
        let since = chrono::Utc::now().timestamp() - SNAPSHOT_LOOKBACK.as_secs() as i64;
        let mut rows = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = self
                .commit_map_page(client, since, SNAPSHOT_PAGE_LIMIT, offset)
                .await?;
            let page_len = page.commits.len();
            rows.extend(page.commits);
            if rows.len() >= SNAPSHOT_MAX_ROWS {
                rows.truncate(SNAPSHOT_MAX_ROWS);
                break;
            }
            match page.next_offset {
                // Strictly-advancing cursor required to continue: the
                // paging loop is ALREADY bounded (a short/empty page or
                // SNAPSHOT_MAX_ROWS ends it either way — see the fn doc), so
                // a misbehaving `next_offset` can only cause bounded wasted
                // work, never an infinite loop. Still cheap to harden: a
                // server that echoes back a non-advancing (or regressing)
                // cursor would otherwise re-fetch the same page repeatedly
                // until SNAPSHOT_MAX_ROWS silently absorbed it.
                Some(next) if page_len > 0 && next > offset => offset = next,
                Some(next) if page_len > 0 => {
                    tracing::warn!(
                        offset,
                        next_offset = next,
                        rows_accumulated = rows.len(),
                        "kb-code: kb daemon returned a non-advancing commit-map next_offset — stopping with what was accumulated",
                    );
                    break;
                }
                _ => break,
            }
        }

        let rows = Arc::new(rows);
        let mut guard = self.snapshot.write().await;
        *guard = Some(Snapshot {
            fetched_at: Instant::now(),
            rows: rows.clone(),
        });
        Ok(rows)
    }

    /// `GET {url}/api/why?path=<path>` (W3.4) — kb's own WHY assembler
    /// (`crates/kb-server/src/routes/sessions.rs::why`, R2), federated the
    /// SAME way `by_commit`/`commit_map_snapshot` are. `provenance::why`
    /// calls this ONLY after the join ladder has already resolved a
    /// `session_id` for the line/file in question, then filters the
    /// returned sessions down to that one id — see that module's doc. A
    /// short-lived per-call client (mirrors `by_commit`, not the cached
    /// snapshot machinery above: kb's `why` route is already cheap — pure
    /// joins over its own session_* tables, no fan-out cost this crate
    /// needs to amortize the way `commit_map_snapshot` does).
    pub async fn why(&self, path: &str) -> Result<Vec<WhySession>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!("{}/api/why", self.cfg.url.trim_end_matches('/'));
        let resp = self
            .get(client, &url)
            .query(&[("path", path)])
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: WhyResponse = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(body.sessions)
    }

    /// `GET {url}/api/kb/{kb}/docs/{doc}/code-refs` (DCB W1.C) — one doc's
    /// `coderef/1` payload. `Ok(None)` on 404 (kb has no such doc) — mirrors
    /// [`Self::session_detail`]'s 404-is-None contract, so a caller
    /// distinguishes "gone" from "kb down" without matching on a status code.
    ///
    /// `kb`/`doc` MUST already have passed
    /// `doclens::validate_{kb,doc}_segment` — this client interpolates them
    /// into a URL PATH and applies no validation of its own (the same
    /// delegation [`Self::session_commits`] documents). `doc` is the ARTIFACT
    /// ID (D14); a path-holding caller resolves it via kb's
    /// `/api/kb/{kb}/docs/by-path/{*path}` first.
    ///
    /// The client's default redirect policy (`Policy::limited(10)`) already
    /// follows kb's moves 301 chain; the returned body's `doc_id` is
    /// AUTHORITATIVE (D13/R13) and the caller re-keys from it — never from
    /// the redirect URL.
    pub async fn code_refs(&self, kb: &str, doc: &str) -> Result<Option<CodeRefsDoc>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/kb/{kb}/docs/{doc}/code-refs",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: CodeRefsDoc = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(Some(body))
    }

    /// `GET {url}/api/kb/{kb}/code-refs?cursor=&limit=[&refs=0]` — the CURSOR
    /// feed (the param is NOT `since`, which means a recency window in kb).
    /// The path has **no `/docs/` segment** (R3 — `/api/kb/{kb}/docs/code-refs`
    /// would bind `code-refs` to matchit's `{id}` on the sibling doc route and
    /// answer a different handler).
    ///
    /// `with_refs = false` sends `?refs=0` ⇒ kb returns headers only
    /// (`refs: []`, and `groups: []`, per doc): the cheap what-changed walk
    /// W3's sync does before fetching bodies for PINNED docs only. Shipped in
    /// W1.C though its only consumer is W3.A, so the client surface lands
    /// once. One page per call; the caller owns paging and its stop condition.
    ///
    /// **`cursor: None` OMITS the `?cursor=` param entirely, never sends it
    /// empty** — kb's `parse_feed_cursor` 400s an explicitly-present-but-empty
    /// `?cursor=` on purpose (it treats "the param is there" as "resume from
    /// this", so an empty value is a malformed resume token, not "start from
    /// page 1"; only a genuinely ABSENT param means that). The cursor is one
    /// opaque STRING — round-tripped verbatim, never constructed or parsed
    /// here.
    pub async fn code_refs_feed(
        &self,
        kb: &str,
        cursor: Option<&str>,
        limit: u32,
        with_refs: bool,
    ) -> Result<CodeRefsFeedPage> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/kb/{kb}/code-refs",
            self.cfg.url.trim_end_matches('/')
        );
        let limit_s = limit.to_string();
        let mut query: Vec<(&str, &str)> = vec![("limit", limit_s.as_str())];
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        if !with_refs {
            query.push(("refs", "0"));
        }
        let resp = self
            .get(client, &url)
            .query(&query)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: CodeRefsFeedPage = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(body)
    }

    /// `GET {url}/api/kb/{kb}/docs/by-path/{*path}` (DCB W2.B, R2/R20's
    /// path-addressed lens entry ramp) — resolves a source-relative path to
    /// kb's artifact id. `Ok(None)` on 404 (kb has no doc at that path),
    /// mirroring [`Self::code_refs`]/[`Self::session_detail`]'s
    /// 404-is-None contract.
    ///
    /// `path` is percent-encoded PER SEGMENT
    /// (`crate::doclens::encode_uri_component` — the SAME encoder
    /// `doclens::doc_href` uses, so this client and that builder never drift
    /// on the encoding rule) and interpolated into the URL PATH: kb's route
    /// is a splat (`{*path}`), never a query param, so a multi-segment
    /// source-relative path round-trips and the `/` separators stay literal.
    ///
    /// Server-side only — the caller is `doclens::wire::resolve_path_route`,
    /// never a browser directly, which is the whole point (R20): kb's own
    /// CORS layer is loopback-origin-only, so a browser on kb-code's origin
    /// could not make this call itself in prod.
    pub async fn resolve_doc_by_path(&self, kb: &str, path: &str) -> Result<Option<String>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        // DCB-W2.B.R fix 1 (security) — belt-and-braces: `resolve_path_route`
        // already refuses a dot-segment `path` before ever calling this fn,
        // but checking again HERE means a future caller of this client can
        // never reopen the traversal `path_has_dot_segment`'s doc describes
        // (encoding alone does not close it — `.` is in `encode_uri_
        // component`'s unreserved set, so `..` survives encoding unescaped
        // and reqwest's URL parser normalizes it out of the request path
        // before this ever leaves the process).
        if crate::doclens::path_has_dot_segment(path) {
            return Err(KbClientError::InvalidPath(path.to_string()));
        }
        // After the local validation above (which must keep refusing a
        // traversal without any network call at all), before the wire.
        self.ensure_sibling().await?;
        let client = self.client()?;
        let encoded_path = path
            .split('/')
            .map(crate::doclens::encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/api/kb/{kb}/docs/by-path/{encoded_path}",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: DocByPathResponse = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(Some(body.id))
    }

    /// `GET {url}/api/kb/{kb}/docs/{id}` — kb's existing, README-canon
    /// single-artifact-metadata route (`crates/kb-server/src/routes/
    /// docs.rs`). PRR-R2 (design doc §4.2) — WAS the ONLY new kb-code->kb
    /// call T2 added; **S2-A (kb-code v6.0 "One Inbox," 2026-08-30) adds
    /// two more — [`Self::desk`] and [`Self::open_comments`] — so that
    /// claim no longer holds** (left here, dated, rather than silently
    /// dropped, so the history of the claim stays legible). Used
    /// EXCLUSIVELY by `GET /api/reviews/{id}/artifact`'s live, unpersisted
    /// verification of a review's `artifact_hint_*` (never resolved/
    /// verified at `PATCH`-write time — kb-code has no business validating
    /// a kb doc id against a schema it doesn't own).
    ///
    /// `Ok(None)` on 404 (kb has no such doc at that id — an honest "hint
    /// points nowhere (anymore)"), mirroring [`Self::code_refs`]/
    /// [`Self::resolve_doc_by_path`]'s 404-is-None contract.
    ///
    /// `kb`/`id` MUST already have passed `doclens::validate_{kb,doc}_
    /// segment` — this client interpolates them into a URL PATH and applies
    /// no validation of its own (the same delegation [`Self::code_refs`]
    /// documents; the review-artifact route validates BOTH before ever
    /// calling this method).
    ///
    /// Never cached (kb-sibling/1's "never cache an unreached probe" —
    /// invariant #2/#4): a fresh call every time `GET /api/reviews/{id}/
    /// artifact` is hit.
    pub async fn doc_meta(&self, kb: &str, id: &str) -> Result<Option<DocMetaOut>> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!(
            "{}/api/kb/{kb}/docs/{id}",
            self.cfg.url.trim_end_matches('/')
        );
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        let body: DocMetaOut = resp
            .json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))?;
        Ok(Some(body))
    }

    /// `GET {url}/api/desk` (S2-A, kb-code v6.0 "One Inbox," design doc
    /// `/tmp/design-s2.md` § S2-A) — kb's federated handoff aggregate
    /// (`crates/kb-server/src/routes/desk.rs`, `DeskListResponse`). PURE
    /// RELAY, not a re-model: `items` deserializes as raw `serde_json::
    /// Value` rows in kb's own `DeskListItem` shape rather than a
    /// kb-code-side mirror struct, so a kb-side additive field flows
    /// through to `crate::unified_inbox`'s `kb.desk` lane the moment kb
    /// ships it — no kb-code release required (the module doc's "kb-code
    /// MAY read kb" law read as far as it goes here: reading, never
    /// re-interpreting). No `?kb=` filter — the unified inbox always wants
    /// the whole fleet, same scope as kb's own SPA desk pill.
    pub async fn desk(&self) -> Result<DeskRelay> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!("{}/api/desk", self.cfg.url.trim_end_matches('/'));
        let resp = self
            .get(client, &url)
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))
    }

    /// `GET {url}/api/inbox?limit=<limit>` (S2-A) — kb's fleet-wide
    /// open-comments inbox (`crates/kb-server/src/routes/inbox.rs`,
    /// `InboxResponse`; Z4). Same PURE-RELAY posture as [`Self::desk`]:
    /// `items` stays raw JSON in kb's own `InboxItem` shape, never
    /// re-modeled here. `limit` is passed straight through to kb's own
    /// `?limit=` (kb clamps it server-side to its own `MAX_LIMIT`); this
    /// client applies no ceiling of its own — `crate::unified_inbox`'s
    /// caller is the one enforcing the unified inbox's OWN 50-item cap on
    /// the relayed result, a client-side truncation distinct from kb's own
    /// page size.
    pub async fn open_comments(&self, limit: u32) -> Result<OpenCommentsRelay> {
        if !self.cfg.enabled {
            return Err(KbClientError::Disabled);
        }
        self.ensure_sibling().await?;
        let client = self.client()?;
        let url = format!("{}/api/inbox", self.cfg.url.trim_end_matches('/'));
        let resp = self
            .get(client, &url)
            .query(&[("limit", limit.to_string())])
            .send()
            .await
            .map_err(|e| KbClientError::Unreachable(url.clone(), e.to_string()))?;
        if !resp.status().is_success() {
            return Err(KbClientError::BadStatus(resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| KbClientError::Parse(e.to_string()))
    }
}

/// The RELAY shape of `GET /api/desk` — see [`KbClient::desk`]'s doc.
/// `items` is left as raw JSON on purpose (kb-code does not re-model kb's
/// `DeskListItem`); `#[serde(default)]` on both fields so a future kb
/// response missing either (or a test fixture that only sets one) still
/// parses rather than erroring.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeskRelay {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    #[serde(default)]
    pub attention: u64,
}

/// The RELAY shape of `GET /api/inbox` — see [`KbClient::open_comments`]'s
/// doc. Same relay posture and `#[serde(default)]` rationale as
/// [`DeskRelay`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenCommentsRelay {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    #[serde(default)]
    pub total_open: u64,
}

/// One `GET {url}/api/kb/{kb}/docs/{id}` response (kb-server's
/// `DocResponse`, `crates/kb-server/src/routes/docs.rs`) — this client only
/// needs these four fields; every other field on kb's full doc body is
/// unread and ignored. `#[serde(rename)]`s translate kb's own field names
/// (`path`, `tags`) to the honest names `GET /api/reviews/{id}/artifact`
/// echoes on its own wire (design doc §2 row 7: `{title, source_relative,
/// kb_tags, kb_category}`).
#[derive(Debug, Clone, Deserialize)]
pub struct DocMetaOut {
    pub title: String,
    #[serde(rename = "path")]
    pub source_relative: String,
    #[serde(default)]
    pub kb_category: Option<String>,
    #[serde(default, rename = "tags")]
    pub kb_tags: Vec<String>,
}

/// One `GET {url}/api/kb/{kb}/docs/by-path/{*path}` response (kb-server's
/// `DocOut`) — this client only needs the resolved `id`; every other field
/// on kb's full doc body is unread and ignored.
#[derive(Debug, Deserialize)]
struct DocByPathResponse {
    id: String,
}

/// Test-only support shared with `crate::join::ladder`'s own test module —
/// `pub(crate)` (not private) specifically so the ladder's much larger
/// arm/precedence test matrix reuses the SAME "spin up a mock kb daemon"
/// helper rather than a second copy. Gated `#[cfg(test)]` like the rest of
/// this file's test code; never compiled into a non-test build.
#[cfg(test)]
pub(crate) mod test_support {
    use axum::Router;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Spin up a minimal axum server on an ephemeral loopback port and
    /// serve `router` on it — the "MOCK kb daemon" every join-ladder test
    /// is required to use instead of depending on a real `:4000` daemon.
    /// The returned `JoinHandle` must be kept alive for the server's
    /// lifetime (dropping it aborts the task); tests bind it to `_server`.
    pub async fn mock_kb_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    pub fn daemon_cfg(url: String) -> super::KbDaemonSection {
        super::KbDaemonSection {
            enabled: true,
            url,
            token_file: None,
            public_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{daemon_cfg as cfg, mock_kb_server};
    use super::*;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- kb-sibling/1 handshake ------------------------------------------
    //
    // Every OTHER test in this module runs against a mock router that
    // serves no `/api/identity` at all — a 404, i.e. a probe that reached
    // no conclusion — which is exactly the "proceed, don't latch" path.
    // That those tests still pass unchanged IS the regression guard for it.

    /// An identity route serving `body`, plus a `by-commit` route so the
    /// same mock can answer a real call after the handshake.
    fn hello_router(body: serde_json::Value) -> Router {
        Router::new()
            .route(
                "/api/identity",
                get(move || {
                    let body = body.clone();
                    async move { Json(body) }
                }),
            )
            .route(
                "/api/sessions/by-commit",
                get(|| async { Json(serde_json::json!({ "matches": [] })) }),
            )
    }

    #[tokio::test]
    async fn sibling_handshake_ok_lets_calls_through() {
        let (addr, _server) = mock_kb_server(hello_router(serde_json::json!({
            "name": "kb",
            "sibling_protocol": "kb-sibling/1",
            "sibling_major": 1,
            "schema_epoch": 40
        })))
        .await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client.by_commit("abc").await.unwrap().is_empty());
    }

    /// FAIL CLOSED — and the reason string NAMES the mismatch (both what
    /// the peer said and what this daemon speaks), on EVERY method, not
    /// just the first one that probed.
    // invariant:2 kb-sibling/1 handshake fails closed on a mismatch
    #[tokio::test]
    async fn sibling_handshake_mismatch_fails_closed_on_every_call() {
        let (addr, _server) = mock_kb_server(hello_router(serde_json::json!({
            "name": "kb",
            "sibling_protocol": "kb-sibling/2",
            "sibling_major": 2,
            "schema_epoch": 99
        })))
        .await;
        let client = KbClient::new(cfg(format!("http://{addr}")));

        let err = client.by_commit("abc").await.unwrap_err();
        let KbClientError::SiblingMismatch(reason) = &err else {
            panic!("expected a SiblingMismatch, got {err:?}");
        };
        assert!(reason.contains("kb-sibling/2"), "{reason}");
        assert!(reason.contains("kb-sibling/1"), "{reason}");

        // Sticky, and it reaches the other verbs too.
        assert!(matches!(
            client.why("src/lib.rs").await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
        assert!(matches!(
            client.commit_map_snapshot(false).await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
        assert!(matches!(
            client.code_refs("platform", "d1").await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
        assert!(matches!(
            client
                .resolve_doc_by_path("platform", "a.html")
                .await
                .unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
        // S2-A additions — same fail-closed gate, no carve-out.
        assert!(matches!(
            client.desk().await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
        assert!(matches!(
            client.open_comments(50).await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
    }

    /// A matching protocol string but a different MAJOR is still a
    /// mismatch — the two fields are checked together, never either alone.
    #[tokio::test]
    async fn sibling_handshake_same_protocol_wrong_major_is_a_mismatch() {
        let (addr, _server) = mock_kb_server(hello_router(serde_json::json!({
            "sibling_protocol": "kb-sibling/1",
            "sibling_major": 2
        })))
        .await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(matches!(
            client.by_commit("abc").await.unwrap_err(),
            KbClientError::SiblingMismatch(_)
        ));
    }

    /// Legacy-peer grandfather rule — a kb binary predating kb-sibling/1
    /// answers identity WITHOUT the Hello fields; the call proceeds.
    // invariant:2 kb-sibling/1 legacy-peer grandfather rule
    #[tokio::test]
    async fn sibling_handshake_absent_fields_grandfathers_a_legacy_peer() {
        let (addr, _server) = mock_kb_server(hello_router(serde_json::json!({
            "name": "kb",
            "version": "v0.35-legacy",
            "kbs": ["memory"]
        })))
        .await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client.by_commit("abc").await.unwrap().is_empty());
    }

    /// A HALF-populated Hello carries no checkable claim — same
    /// information state as silence, so it grandfathers rather than
    /// failing closed.
    #[tokio::test]
    async fn sibling_handshake_half_a_hello_grandfathers_rather_than_guessing() {
        let (addr, _server) = mock_kb_server(hello_router(serde_json::json!({
            "sibling_protocol": "kb-sibling/1"
        })))
        .await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client.by_commit("abc").await.unwrap().is_empty());
    }

    /// An UNREACHABLE peer is not a mismatch: the pre-handshake degrade is
    /// unchanged (`Unreachable`, not `SiblingMismatch`), and — because no
    /// conclusion was reached — nothing is cached, so a peer that comes up
    /// later is handshaken normally without a kb-code restart.
    #[tokio::test]
    async fn sibling_handshake_unreachable_peer_keeps_the_old_degrade() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        assert!(matches!(
            client.by_commit("abc").await.unwrap_err(),
            KbClientError::Unreachable(_, _)
        ));
        assert!(
            client.sibling.get().is_none(),
            "an unreached conclusion must never be cached"
        );
    }

    /// A probe the peer answers with a NON-2xx (a 401 from a token
    /// misconfiguration, a proxy's 502) is likewise unreached: proceed,
    /// and let the real call report the status itself.
    #[tokio::test]
    async fn sibling_handshake_non_2xx_identity_is_unreached_not_a_mismatch() {
        let router = Router::new()
            .route(
                "/api/identity",
                get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "nope") }),
            )
            .route(
                "/api/sessions/by-commit",
                get(|| async { Json(serde_json::json!({ "matches": [] })) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client.by_commit("abc").await.unwrap().is_empty());
        assert!(client.sibling.get().is_none());
    }

    /// The probe happens ONCE per process, not once per call.
    #[tokio::test]
    async fn sibling_handshake_probes_at_most_once() {
        let probes = Arc::new(AtomicUsize::new(0));
        let probes_clone = probes.clone();
        let router = Router::new()
            .route(
                "/api/identity",
                get(move || {
                    let probes = probes_clone.clone();
                    async move {
                        probes.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "sibling_protocol": "kb-sibling/1",
                            "sibling_major": 1
                        }))
                    }
                }),
            )
            .route(
                "/api/sessions/by-commit",
                get(|| async { Json(serde_json::json!({ "matches": [] })) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        for _ in 0..5 {
            client.by_commit("abc").await.unwrap();
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    /// `[kb_daemon] enabled = false` short-circuits BEFORE the handshake —
    /// a disabled federation must still make no network call at all.
    #[tokio::test]
    async fn by_commit_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.by_commit("deadbeef").await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
        assert!(client.sibling.get().is_none());
    }

    #[tokio::test]
    async fn by_commit_unreachable_daemon_is_reported_not_panicked() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        let err = client.by_commit("deadbeef").await.unwrap_err();
        assert!(matches!(err, KbClientError::Unreachable(_, _)));
    }

    #[tokio::test]
    async fn by_commit_parses_a_real_response_from_a_mock_daemon() {
        let router = Router::new().route(
            "/api/sessions/by-commit",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("sha").map(String::as_str), Some("abc123"));
                Json(serde_json::json!({
                    "matches": [{
                        "kb": "memory",
                        "session_id": "sess-1",
                        "artifact_id": "art-1",
                        "kind": "commit",
                        "sha": "abc123",
                        "sha_full": "abc123fullsha",
                        "resolved": true,
                        "subject": "fixed the thing",
                        "trailers": ["Kb-Session: sess-1"],
                        "display_name": "fixed the thing",
                        "started_at": 1_700_000_000
                    }]
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let matches = client.by_commit("abc123").await.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "sess-1");
        assert_eq!(matches[0].sha_full.as_deref(), Some("abc123fullsha"));
        assert_eq!(matches[0].trailers, vec!["Kb-Session: sess-1".to_string()]);
    }

    #[tokio::test]
    async fn by_commit_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/sessions/by-commit",
            get(|| async { (axum::http::StatusCode::BAD_REQUEST, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let err = client.by_commit("xx").await.unwrap_err();
        assert!(matches!(err, KbClientError::BadStatus(_)));
    }

    #[tokio::test]
    async fn session_commits_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.session_commits("sess-1").await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    #[tokio::test]
    async fn session_commits_parses_a_real_response_from_a_mock_daemon() {
        let router = Router::new().route(
            "/api/sessions/{session_id}/commits",
            get(
                |axum::extract::Path(session_id): axum::extract::Path<String>| async move {
                    assert_eq!(session_id, "sess-1");
                    Json(serde_json::json!({
                        "commits": [{
                            "kind": "commit",
                            "sha": "abc123",
                            "subject": "fixed the thing",
                            "resolved": true,
                            "sha_full": "abc123fullsha",
                            "repo_root": "/repo",
                            "author": "Ada <ada@example.com>",
                            "parents": 1,
                            "trailers": ["Kb-Session: sess-1"]
                        }]
                    }))
                },
            ),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let commits = client.session_commits("sess-1").await.unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha_full.as_deref(), Some("abc123fullsha"));
        assert_eq!(commits[0].repo_root.as_deref(), Some("/repo"));
        assert_eq!(commits[0].trailers, vec!["Kb-Session: sess-1".to_string()]);
    }

    #[tokio::test]
    async fn session_commits_unknown_session_is_an_empty_list_not_an_error() {
        let router = Router::new().route(
            "/api/sessions/{session_id}/commits",
            get(|| async { Json(serde_json::json!({ "commits": [] })) }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let commits = client.session_commits("no-such-session").await.unwrap();
        assert!(commits.is_empty());
    }

    #[tokio::test]
    async fn commit_map_snapshot_pages_until_a_short_page() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let router = Router::new().route(
            "/api/sessions/commit-map",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let offset: u32 = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let limit: u32 = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(500);
                    // Two full pages then a short (empty) third page.
                    let total = 2 * limit;
                    if offset >= total {
                        return Json(
                            serde_json::json!({ "commits": [], "limit": limit, "offset": offset }),
                        );
                    }
                    let commits: Vec<_> = (0..limit)
                        .map(|i| {
                            serde_json::json!({
                                "kb": "memory",
                                "session_id": format!("sess-{}", offset + i),
                                "artifact_id": "art",
                                "started_at": 1_700_000_000,
                                "kind": "commit",
                                "resolved": false,
                                "trailers": []
                            })
                        })
                        .collect();
                    let next_offset = offset + limit;
                    Json(serde_json::json!({
                        "commits": commits,
                        "limit": limit,
                        "offset": offset,
                        "next_offset": next_offset
                    }))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let snapshot = client.commit_map_snapshot(false).await.unwrap();
        assert_eq!(snapshot.len(), 2 * SNAPSHOT_PAGE_LIMIT as usize);
        assert!(calls.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn commit_map_snapshot_is_cached_until_forced() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let router = Router::new().route(
            "/api/sessions/commit-map",
            get(move || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 }))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));

        client.commit_map_snapshot(false).await.unwrap();
        client.commit_map_snapshot(false).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second call within the TTL must be served from cache"
        );

        client.commit_map_snapshot(true).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "force=true must always re-fetch"
        );
    }

    /// A1 — N concurrent `commit_map_snapshot(false)` callers racing a
    /// cache-less/stale client must coalesce into exactly ONE page-walk: the
    /// double-checked `fetch_guard` re-check means every waiter behind the
    /// winner sees the winner's freshly-written snapshot instead of each
    /// independently re-running the fetch. The mock handler sleeps briefly
    /// so every spawned task reaches the `fetch_guard` queue before the
    /// first response lands — without that, a fast-enough round trip could
    /// let this pass even with the old plain read-then-fetch code, since
    /// there'd be no real race window.
    #[tokio::test]
    async fn commit_map_snapshot_coalesces_concurrent_stale_readers() {
        let page_requests = Arc::new(AtomicUsize::new(0));
        let page_requests_clone = page_requests.clone();
        let router = Router::new().route(
            "/api/sessions/commit-map",
            get(move || {
                let page_requests = page_requests_clone.clone();
                async move {
                    page_requests.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 }))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = Arc::new(KbClient::new(cfg(format!("http://{addr}"))));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let client = client.clone();
            tasks.push(tokio::spawn(async move {
                client.commit_map_snapshot(false).await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }

        assert_eq!(
            page_requests.load(Ordering::SeqCst),
            1,
            "8 concurrent stale readers must coalesce into exactly one page-walk"
        );
    }

    /// A1 — `force = true` callers still serialize through `fetch_guard`
    /// (never a concurrent stampede of forced fetches), but — unlike the
    /// `force = false` coalescing case above — EACH one still re-fetches
    /// once it holds the guard (matches `commit_map_snapshot_is_cached_
    /// until_forced`'s "force=true must always re-fetch" contract).
    #[tokio::test]
    async fn commit_map_snapshot_force_true_serializes_but_still_refetches_each_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let router = Router::new().route(
            "/api/sessions/commit-map",
            get(move || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 }))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = Arc::new(KbClient::new(cfg(format!("http://{addr}"))));

        let mut tasks = Vec::new();
        for _ in 0..4 {
            let client = client.clone();
            tasks.push(tokio::spawn(async move {
                client.commit_map_snapshot(true).await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "every force=true call must still perform its own fetch"
        );
    }

    /// A2 — a server that echoes a non-advancing `next_offset` must not
    /// spin: the loop breaks with whatever it accumulated instead of
    /// re-fetching the same page forever (bounded waste, never an infinite
    /// loop — see the fn doc).
    #[tokio::test]
    async fn commit_map_snapshot_stops_on_a_non_advancing_next_offset() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let router = Router::new().route(
            "/api/sessions/commit-map",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let offset: u32 = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
                    // ALWAYS echoes back the SAME offset it was called
                    // with as `next_offset` — a misbehaving/buggy server,
                    // never advancing the cursor.
                    Json(serde_json::json!({
                        "commits": [{
                            "kb": "memory",
                            "session_id": format!("sess-{offset}"),
                            "artifact_id": "art",
                            "started_at": 1_700_000_000,
                            "kind": "commit",
                            "resolved": false,
                            "trailers": []
                        }],
                        "limit": 500,
                        "offset": offset,
                        "next_offset": offset
                    }))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let snapshot = client.commit_map_snapshot(false).await.unwrap();

        // Stopped after the FIRST page rather than looping forever or
        // running all the way to SNAPSHOT_MAX_ROWS.
        assert_eq!(snapshot.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn commit_map_snapshot_disabled_short_circuits() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.commit_map_snapshot(false).await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    // --- session_detail (V3.2-B2) --------------------------------------------

    #[tokio::test]
    async fn session_detail_parses_error_count_and_memory_ids_from_a_flattened_response() {
        let router = Router::new().route(
            "/api/sessions/{session_id}",
            get(
                |axum::extract::Path(session_id): axum::extract::Path<String>| async move {
                    assert_eq!(session_id, "sess-1");
                    Json(serde_json::json!({
                        "id": "art-1",
                        "kb": "memory",
                        "artifact_id": "art-1",
                        "session_id": "sess-1",
                        "started_at": 1_700_000_000,
                        "ended_at": 1_700_000_100,
                        "duration_ms": 100_000,
                        "message_count": 3,
                        "memory_count": 2,
                        "source_relative": "sessions/sess-1.html",
                        "display_name": "fixed the gizmo",
                        "files_read_count": 1,
                        "files_edited_count": 1,
                        "error_count": 2,
                        "active_secs": 42,
                        "memory_ids": ["mem-a", "mem-b"]
                    }))
                },
            ),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let detail = client.session_detail("sess-1").await.unwrap().unwrap();
        assert_eq!(detail.error_count, 2);
        assert_eq!(detail.active_secs, 42);
        assert_eq!(
            detail.memory_ids,
            vec!["mem-a".to_string(), "mem-b".to_string()]
        );
    }

    /// A response with no `memory_ids` at all (an older kb daemon predating
    /// the field) must not fail to parse — `#[serde(default)]` degrades to
    /// an empty `Vec`, never an error.
    #[tokio::test]
    async fn session_detail_missing_memory_ids_defaults_to_empty() {
        let router = Router::new().route(
            "/api/sessions/{session_id}",
            get(|| async {
                Json(serde_json::json!({
                    "session_id": "sess-1",
                    "error_count": 0,
                    "active_secs": 0,
                    "duration_ms": 0
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let detail = client.session_detail("sess-1").await.unwrap().unwrap();
        assert!(detail.memory_ids.is_empty());
    }

    #[tokio::test]
    async fn session_detail_404_is_none_not_an_error() {
        let router = Router::new().route(
            "/api/sessions/{session_id}",
            get(|| async { axum::http::StatusCode::NOT_FOUND }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client
            .session_detail("no-such-session")
            .await
            .unwrap()
            .is_none());
    }

    // --- why (W3.4) ---------------------------------------------------------

    #[tokio::test]
    async fn why_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.why("src/lib.rs").await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    #[tokio::test]
    async fn why_unreachable_daemon_is_reported_not_panicked() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        let err = client.why("src/lib.rs").await.unwrap_err();
        assert!(matches!(err, KbClientError::Unreachable(_, _)));
    }

    #[tokio::test]
    async fn why_parses_a_real_response_from_a_mock_daemon() {
        let router = Router::new().route(
            "/api/why",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("path").map(String::as_str), Some("src/lib.rs"));
                Json(serde_json::json!({
                    "path": "src/lib.rs",
                    "basename": "lib.rs",
                    "sessions": [{
                        "session_id": "sess-1",
                        "kb": "memory",
                        "display_name": "fixed the gizmo",
                        "started_at": 1_700_000_000,
                        "action": "edit",
                        "confidence": "exact",
                        "first_user_prompt": "why does the gizmo race?",
                        "decisions": [{"kind": "answer", "prompt": "p", "answer": "a"}],
                        "commits": []
                    }]
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let sessions = client.why("src/lib.rs").await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-1");
        assert_eq!(sessions[0].display_name, "fixed the gizmo");
        assert_eq!(
            sessions[0].first_user_prompt.as_deref(),
            Some("why does the gizmo race?")
        );
        assert_eq!(sessions[0].decisions.len(), 1);
        assert_eq!(sessions[0].decisions[0].kind, "answer");
    }

    #[tokio::test]
    async fn why_missing_sessions_array_is_empty_not_a_panic() {
        let router = Router::new().route(
            "/api/why",
            get(|| async { Json(serde_json::json!({ "path": "x", "basename": "x" })) }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let sessions = client.why("x").await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn why_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/why",
            get(|| async { (axum::http::StatusCode::BAD_REQUEST, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let err = client.why("x").await.unwrap_err();
        assert!(matches!(err, KbClientError::BadStatus(_)));
    }

    // --- S2-A ("One Inbox") — `desk`/`open_comments` -----------------------

    #[tokio::test]
    async fn desk_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.desk().await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    #[tokio::test]
    async fn desk_unreachable_daemon_is_reported_not_panicked() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        let err = client.desk().await.unwrap_err();
        assert!(matches!(err, KbClientError::Unreachable(_, _)));
    }

    #[tokio::test]
    async fn desk_relays_items_verbatim_without_re_modeling() {
        let router = Router::new().route(
            "/api/desk",
            get(|| async {
                Json(serde_json::json!({
                    "items": [{
                        "kb": "memory",
                        "id": "abc123",
                        "source_relative": "handoff/x.html",
                        "title": "handoff draft",
                        "updated_unix": 1_700_000_000_i64,
                        "comments_open": 2,
                        "comments_total": 3,
                        "read_state": "unread",
                        "changed_since_read": true,
                        // An additive field kb might ship later — proves
                        // the relay posture: unknown fields survive.
                        "future_field": "surprise"
                    }],
                    "attention": 3
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let relay = client.desk().await.unwrap();
        assert_eq!(relay.attention, 3);
        assert_eq!(relay.items.len(), 1);
        assert_eq!(relay.items[0]["id"], "abc123");
        assert_eq!(relay.items[0]["future_field"], "surprise");
    }

    #[tokio::test]
    async fn desk_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/desk",
            get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let err = client.desk().await.unwrap_err();
        assert!(matches!(err, KbClientError::BadStatus(_)));
    }

    #[tokio::test]
    async fn open_comments_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.open_comments(50).await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    #[tokio::test]
    async fn open_comments_unreachable_daemon_is_reported_not_panicked() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        let err = client.open_comments(50).await.unwrap_err();
        assert!(matches!(err, KbClientError::Unreachable(_, _)));
    }

    #[tokio::test]
    async fn open_comments_relays_items_and_passes_the_limit_through() {
        let router = Router::new().route(
            "/api/inbox",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("limit").map(String::as_str), Some("7"));
                Json(serde_json::json!({
                    "items": [{
                        "kb": "memory",
                        "artifact_id": "d1",
                        "title": "a comment",
                        "comment_id": "c1",
                        "excerpt": "why here?",
                        "author": "you",
                        "reply_count": 0,
                        "anchor": "selection",
                        "stale": false,
                        "created_at": 1_700_000_000_i64,
                        "updated_at": 1_700_000_000_i64
                    }],
                    "total_open": 12
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let relay = client.open_comments(7).await.unwrap();
        assert_eq!(relay.total_open, 12);
        assert_eq!(relay.items.len(), 1);
        assert_eq!(relay.items[0]["comment_id"], "c1");
    }

    #[tokio::test]
    async fn open_comments_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/inbox",
            get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let err = client.open_comments(50).await.unwrap_err();
        assert!(matches!(err, KbClientError::BadStatus(_)));
    }

    // --- DCB W1.C — `coderef/1` ------------------------------------------

    fn coderef_body(doc_id: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "coderef/1",
            "kb": "platform",
            "doc_id": doc_id,
            "doc_path": "features/algolia/piano.html",
            "doc_hash": "3f1a",
            "title": "Piano",
            "extracted_at": 1_754_500_000_i64,
            "never_scanned": false,
            "code_rev": { "label": "acme-shop", "sha": "bcd13a1d3", "dirty": false },
            "ref_count": 1,
            "ungrouped_count": 1,
            "truncated": false,
            "groups": [],
            "refs": [{
                "ordinal": 41,
                "group": null,
                "kind": "path_line",
                "raw": "carts_controller.rb:284",
                "path_hint": "carts_controller.rb",
                "line_start": 284,
                "line_end": 284,
                "line_spans": null,
                "symbol_container": null,
                "symbol_member": null,
                "context": "il checkout",
                "context_tokens": ["algolia_user_token"],
                "declared": false
            }]
        })
    }

    #[tokio::test]
    async fn code_refs_parses_a_real_coderef_1_response() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|| async { Json(coderef_body("9f8b7182d433")) }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let doc = client
            .code_refs("platform", "9f8b7182d433")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.doc_id, "9f8b7182d433");
        assert_eq!(doc.doc_path.as_deref(), Some("features/algolia/piano.html"));
        assert_eq!(doc.ungrouped_count, 1);
        assert!(!doc.never_scanned);
        let rev = doc.code_rev.unwrap();
        assert_eq!(rev.label, "acme-shop");
        assert!(!rev.dirty);
        let r = &doc.refs[0];
        assert_eq!(r.ordinal, 41);
        assert_eq!(r.raw, "carts_controller.rb:284");
        assert_eq!(r.context_tokens, vec!["algolia_user_token".to_string()]);
        assert!(r.group.is_none(), "a null group is never a sentinel");
    }

    #[tokio::test]
    async fn code_refs_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        assert!(matches!(
            client.code_refs("platform", "d1").await.unwrap_err(),
            KbClientError::Disabled
        ));
        assert!(matches!(
            client
                .code_refs_feed("platform", None, 25, true)
                .await
                .unwrap_err(),
            KbClientError::Disabled
        ));
    }

    #[tokio::test]
    async fn code_refs_unknown_doc_is_none_not_an_error() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({})),
                )
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client
            .code_refs("platform", "gone")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn code_refs_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(matches!(
            client.code_refs("platform", "d1").await.unwrap_err(),
            KbClientError::BadStatus(_)
        ));
    }

    /// D13/R13 — the BODY is authoritative after a 301. The mock's `Location`
    /// carries a deliberately unusable extra header, and nothing in the
    /// client may parse a URL to recover the new id.
    #[tokio::test]
    async fn code_refs_follows_a_301_and_reports_the_new_doc_id() {
        let router = Router::new()
            .route(
                "/api/kb/{kb}/docs/oldid/code-refs",
                get(|| async {
                    axum::response::Response::builder()
                        .status(axum::http::StatusCode::MOVED_PERMANENTLY)
                        .header("location", "/api/kb/platform/docs/newid/code-refs")
                        .header("x-kb-nonsense", "///not-a-doc-id///")
                        .body(axum::body::Body::empty())
                        .unwrap()
                }),
            )
            .route(
                "/api/kb/{kb}/docs/newid/code-refs",
                get(|| async { Json(coderef_body("newid")) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let doc = client
            .code_refs("platform", "oldid")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.doc_id, "newid");
    }

    #[tokio::test]
    async fn code_refs_surfaces_never_scanned_and_a_null_extracted_at() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}/code-refs",
            get(|| async {
                Json(serde_json::json!({
                    "schema": "coderef/1",
                    "kb": "platform",
                    "doc_id": "d1",
                    "doc_path": "a.html",
                    "title": "A",
                    "doc_hash": null,
                    "extracted_at": null,
                    "never_scanned": true,
                    "code_rev": null,
                    "ref_count": 0,
                    "ungrouped_count": 0,
                    "truncated": false,
                    "groups": [],
                    "refs": []
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let doc = client.code_refs("platform", "d1").await.unwrap().unwrap();
        assert!(doc.never_scanned, "the THIRD state, not an empty refs list");
        assert!(doc.extracted_at.is_none() && doc.doc_hash.is_none());
        assert!(doc.refs.is_empty());
    }

    #[test]
    fn code_refs_accepts_both_raw_and_raw_text_spellings() {
        let row: CodeRefRow =
            serde_json::from_value(serde_json::json!({"kind": "path", "raw_text": "a.rb"}))
                .unwrap();
        assert_eq!(row.raw, "a.rb");
        let row: CodeRefRow =
            serde_json::from_value(serde_json::json!({"kind": "path", "raw": "b.rb"})).unwrap();
        assert_eq!(row.raw, "b.rb");
    }

    #[test]
    fn code_rev_accepts_the_repo_label_alias() {
        let rev: CodeRev =
            serde_json::from_value(serde_json::json!({"repo_label": "x", "sha": "abc"})).unwrap();
        assert_eq!(rev.label, "x");
        assert!(!rev.dirty);
    }

    #[test]
    fn code_refs_deserializes_a_feed_docs_element_without_schema_or_kb() {
        // ONE struct for both shapes — two would be two places to forget a
        // field (a feed `docs[]` element omits `schema`/`kb` as redundant).
        let doc: CodeRefsDoc = serde_json::from_value(serde_json::json!({
            "doc_id": "d1",
            "doc_path": "a.html",
            "ref_count": 3,
            "refs": []
        }))
        .unwrap();
        assert_eq!(doc.doc_id, "d1");
        assert!(doc.schema.is_empty() && doc.kb.is_empty());
        assert_eq!(doc.ref_count, 3);
    }

    /// R3 — the cursor is ONE opaque string, round-tripped verbatim and never
    /// parsed client-side. `cursor: None` must OMIT the param entirely: kb
    /// 400s an explicitly-present-but-empty `?cursor=` on purpose.
    #[tokio::test]
    async fn code_refs_feed_pages_via_an_opaque_next_cursor() {
        let router = Router::new().route(
            "/api/kb/{kb}/code-refs",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                match q.get("cursor").map(String::as_str) {
                    None => {
                        assert_eq!(q.get("limit").map(String::as_str), Some("2"));
                        Json(serde_json::json!({
                            "schema": "coderef-feed/1",
                            "kb": "platform",
                            "docs": [{"doc_id": "d1", "refs": []}, {"doc_id": "d2", "refs": []}],
                            "next_cursor": "1754500000:d2"
                        }))
                    }
                    Some(c) => {
                        assert_eq!(c, "1754500000:d2", "round-tripped VERBATIM");
                        Json(serde_json::json!({
                            "schema": "coderef-feed/1",
                            "kb": "platform",
                            "docs": [{"doc_id": "d3", "refs": []}]
                        }))
                    }
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));

        let page1 = client
            .code_refs_feed("platform", None, 2, true)
            .await
            .unwrap();
        assert_eq!(page1.docs.len(), 2);
        assert_eq!(page1.next_cursor.as_deref(), Some("1754500000:d2"));

        let page2 = client
            .code_refs_feed("platform", page1.next_cursor.as_deref(), 2, true)
            .await
            .unwrap();
        assert_eq!(page2.docs.len(), 1);
        // Omitted (not null) by kb on the last page.
        assert!(page2.next_cursor.is_none());
    }

    /// m28/R13 — the cheap what-changed walk W3's sync does before fetching
    /// bodies for PINNED docs only.
    #[tokio::test]
    async fn code_refs_feed_with_refs_false_sends_refs_0_and_accepts_empty_ref_arrays() {
        let router = Router::new().route(
            "/api/kb/{kb}/code-refs",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("refs").map(String::as_str), Some("0"));
                assert!(!q.contains_key("cursor"), "None must OMIT the param");
                Json(serde_json::json!({
                    "schema": "coderef-feed/1",
                    "kb": "platform",
                    "docs": [{"doc_id": "d1", "ref_count": 12, "refs": [], "groups": []}]
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let page = client
            .code_refs_feed("platform", None, 25, false)
            .await
            .unwrap();
        assert_eq!(
            page.docs[0].ref_count, 12,
            "counts survive headers-only mode"
        );
        assert!(page.docs[0].refs.is_empty());
    }

    // --- resolve_doc_by_path (DCB W2.B) -------------------------------------

    #[tokio::test]
    async fn resolve_doc_by_path_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client
            .resolve_doc_by_path("platform", "a.html")
            .await
            .unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    /// DCB-W2.B.R fix 1 (security) — the belt-and-braces guard: a
    /// dot-segment `path` is refused BEFORE any client/URL is built, so no
    /// outbound request is ever attempted — this test uses an unreachable
    /// `0.0.0.0:0`-shaped daemon URL (mirroring `by_commit_disabled_short_
    /// circuits_without_a_network_call`'s convention) so the assertion would
    /// fail with `Unreachable`, not `InvalidPath`, if the guard were ever
    /// skipped.
    #[tokio::test]
    async fn resolve_doc_by_path_refuses_a_dot_segment_without_a_network_call() {
        let client = KbClient::new(cfg("http://127.0.0.1:0".to_string()));
        for traversal in [
            "../../../research/docs/by-path/a.html",
            "features/../../../research/a.html",
            "..",
            "a/./b",
        ] {
            let err = client
                .resolve_doc_by_path("platform", traversal)
                .await
                .unwrap_err();
            assert!(
                matches!(err, KbClientError::InvalidPath(_)),
                "{traversal:?} must be refused as InvalidPath, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn resolve_doc_by_path_parses_the_id_from_a_real_response() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/by-path/{*path}",
            get(
                |axum::extract::Path((kb, path)): axum::extract::Path<(String, String)>| async move {
                    assert_eq!(kb, "platform");
                    assert_eq!(path, "features/algolia/piano.html");
                    Json(serde_json::json!({
                        "id": "9f8b7182d433",
                        "path": "/repo/features/algolia/piano.html",
                        "title": "Piano"
                    }))
                },
            ),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let id = client
            .resolve_doc_by_path("platform", "features/algolia/piano.html")
            .await
            .unwrap();
        assert_eq!(id.as_deref(), Some("9f8b7182d433"));
    }

    #[tokio::test]
    async fn resolve_doc_by_path_encodes_each_segment() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/by-path/{*path}",
            get(
                |axum::extract::Path((_kb, path)): axum::extract::Path<(String, String)>| async move {
                    // axum decodes the splat back to the raw path — proves
                    // the client percent-encoded (not left raw, which would
                    // have split "a b/c#d.html" across more path segments
                    // than intended or 400'd on the literal space/`#`).
                    assert_eq!(path, "a b/c#d.html");
                    Json(serde_json::json!({"id": "encoded-ok"}))
                },
            ),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let id = client
            .resolve_doc_by_path("platform", "a b/c#d.html")
            .await
            .unwrap();
        assert_eq!(id.as_deref(), Some("encoded-ok"));
    }

    #[tokio::test]
    async fn resolve_doc_by_path_unknown_path_is_none_not_an_error() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/by-path/{*path}",
            get(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({})),
                )
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client
            .resolve_doc_by_path("platform", "gone.html")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn resolve_doc_by_path_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/by-path/{*path}",
            get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(matches!(
            client
                .resolve_doc_by_path("platform", "a.html")
                .await
                .unwrap_err(),
            KbClientError::BadStatus(_)
        ));
    }

    // --- doc_meta (PRR-R2) --------------------------------------------------

    #[tokio::test]
    async fn doc_meta_disabled_short_circuits_without_a_network_call() {
        let client = KbClient::new(KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        });
        let err = client.doc_meta("platform", "abc123").await.unwrap_err();
        assert!(matches!(err, KbClientError::Disabled));
    }

    #[tokio::test]
    async fn doc_meta_parses_title_path_tags_and_category_from_a_real_response() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}",
            get(
                |axum::extract::Path((kb, id)): axum::extract::Path<(String, String)>| async move {
                    assert_eq!(kb, "platform");
                    assert_eq!(id, "abc123");
                    Json(serde_json::json!({
                        "id": "abc123",
                        "title": "PR #42 review",
                        "path": "changelog/tasks/pr-42-review.html",
                        "kb_category": "review",
                        "tags": ["pr-review", "rails"]
                    }))
                },
            ),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let doc = client
            .doc_meta("platform", "abc123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.title, "PR #42 review");
        assert_eq!(doc.source_relative, "changelog/tasks/pr-42-review.html");
        assert_eq!(doc.kb_category.as_deref(), Some("review"));
        assert_eq!(doc.kb_tags, vec!["pr-review", "rails"]);
    }

    #[tokio::test]
    async fn doc_meta_tolerates_missing_optional_fields() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}",
            get(|| async {
                Json(serde_json::json!({
                    "id": "abc123",
                    "title": "Untagged doc",
                    "path": "notes/x.html"
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        let doc = client
            .doc_meta("platform", "abc123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.kb_category, None);
        assert!(doc.kb_tags.is_empty());
    }

    #[tokio::test]
    async fn doc_meta_unknown_id_is_none_not_an_error() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}",
            get(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({})),
                )
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(client.doc_meta("platform", "gone").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn doc_meta_bad_status_is_reported() {
        let router = Router::new().route(
            "/api/kb/{kb}/docs/{id}",
            get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "nope") }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = KbClient::new(cfg(format!("http://{addr}")));
        assert!(matches!(
            client.doc_meta("platform", "abc123").await.unwrap_err(),
            KbClientError::BadStatus(_)
        ));
    }
}
