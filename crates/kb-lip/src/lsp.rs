//! The LSP child process: spawn, JSON-RPC request/response correlation,
//! the initialize/initialized handshake, lazy `textDocument/didOpen` with
//! an LRU-bounded open-docs set (didClose on eviction), didChange on a
//! stale doc, and graceful shutdown. One [`LspClient`] == one live child
//! process; [`crate::supervisor::Supervisor`] owns restart-on-crash.

use crate::blob::Blob;
use crate::config::Config;
use crate::rpc;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// Open-docs cap ("keep an open-docs set; send didClose on LRU eviction
/// beyond ~64 docs", design-lip.md Phase L1).
const MAX_OPEN_DOCS: usize = 64;

type PendingMap = Arc<std::sync::Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>>;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("spawn {command:?}: {source}")]
    Spawn {
        command: Vec<String>,
        #[source]
        source: std::io::Error,
    },
    #[error("child stdio not piped (internal bug)")]
    NoStdio,
    #[error("initialize handshake failed: {0}")]
    InitializeFailed(String),
    #[error("request channel closed (server crashed mid-request)")]
    ChannelClosed,
    #[error("write to child stdin: {0}")]
    Write(#[source] std::io::Error),
    #[error("server returned a JSON-RPC error {code}: {message}")]
    RpcError { code: i64, message: String },
    /// `textDocument/diagnostic` (LSP 3.17 pull) replied with a `kind`
    /// other than `"full"`/`"unchanged"`, or no `kind` at all — a
    /// malformed `DocumentDiagnosticReport`. Treated by the caller exactly
    /// like any other failed LSP request (`refused: "server_down"`,
    /// PRR-L5) — never guessed at.
    #[error("malformed textDocument/diagnostic response (no valid \"kind\"): {0}")]
    MalformedDiagnosticReport(Value),
}

/// What the `initialize` handshake told us about the child server —
/// surfaced verbatim on `GET /lip/identity` and used to pre-empt an
/// unsupported request before ever opening a document.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub name: Option<String>,
    pub version: Option<String>,
    capabilities: Value,
}

impl ServerInfo {
    fn from_initialize_result(result: &Value) -> Self {
        Self {
            name: result["serverInfo"]["name"].as_str().map(str::to_string),
            version: result["serverInfo"]["version"].as_str().map(str::to_string),
            capabilities: result.get("capabilities").cloned().unwrap_or(Value::Null),
        }
    }

    /// Does the server's advertised `initialize` capabilities include
    /// `capability_key` (e.g. `"hoverProvider"`) as anything other than
    /// absent/null/`false`? LSP capabilities can be `true`, an options
    /// object, or absent — all three "options object" and "true" count as
    /// supported.
    pub fn supports(&self, capability_key: &str) -> bool {
        match self.capabilities.get(capability_key) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(_) => true,
        }
    }

    /// LSP 3.17 pull-vs-push diagnostics mode this server advertised at
    /// `initialize` (PRR-L5 — closes the gap PRR-L4 found: ruby-lsp
    /// 0.26.11 implements ONLY pull diagnostics and never pushes
    /// `publishDiagnostics`, so a push-cache-only `/lip/diagnostics`
    /// returns `[]` forever against it). `"pull"` iff
    /// `capabilities.diagnosticProvider` is present as anything other than
    /// absent/null/`false` — same leniency as `supports()` (the spec only
    /// ever sends an OBJECT, `DiagnosticOptions`, here, never a bare bool,
    /// but treating a hypothetical `true` the same way costs nothing and
    /// matches every other capability check in this file). `"push"`
    /// otherwise: the server never advertised pull support, so kb-lip
    /// falls back to the original `publishDiagnostics` cache path
    /// (design-addendum-2.md §D) — a server may push at any time
    /// regardless of what it advertises, per the LSP spec.
    pub fn diagnostics_mode(&self) -> &'static str {
        if self.supports("diagnosticProvider") {
            "pull"
        } else {
            "push"
        }
    }

    /// Does the server's `codeActionProvider` capability ADDITIONALLY
    /// advertise `resolveProvider: true` (LSP `CodeActionOptions`)? Gates
    /// whether `/lip/code-actions` (`http::guarded_code_actions`) is worth
    /// attempting a `codeAction/resolve` round trip for an action returned
    /// without `.edit` — a bare `codeActionProvider: true`/an options
    /// object missing the flag/absent all read `false` here, same
    /// leniency-in-the-strict-direction posture as every other capability
    /// check in this file (never ASSUME resolve support). Distinct from
    /// `supports("codeActionProvider")` (the design-lip.md-style gate used
    /// by `capability_key_for`/`request_if_supported` to decide whether
    /// `textDocument/codeAction` itself is worth sending at all).
    pub fn code_action_resolve_supported(&self) -> bool {
        self.capabilities["codeActionProvider"]["resolveProvider"] == Value::Bool(true)
    }
}

/// Maps a lip/1 endpoint name to the LSP `initialize` capability key that
/// gates it. `"diagnostics"` is deliberately NOT here: it doesn't route
/// through `request_if_supported` at all — `/lip/diagnostics` has its own
/// pipeline (`http::guarded_diagnostics`) that branches on
/// `ServerInfo::diagnostics_mode()` instead, since a server's diagnostics
/// behavior is never a simple "supported/unsupported" gate (push servers
/// may publish at any time with no capability flag at all; pull servers
/// advertise `diagnosticProvider` and are queried via a distinct LSP
/// method, `textDocument/diagnostic`, never `capability_key_for`'s
/// `lsp_method` parameter). `"code-actions"` (S2-C, design-s2.md — the
/// design-lip.md-era "no code actions" refusal is REVERSED) IS gated the
/// ordinary way here: `textDocument/codeAction` itself is a plain
/// capability-gated request like hover/definition/references/symbols, even
/// though its OWN pipeline (`http::guarded_code_actions`) additionally
/// consults `ServerInfo::code_action_resolve_supported` for the
/// per-action `codeAction/resolve` follow-up round trip, which is not a
/// `capability_key_for`-style gate at all.
pub fn capability_key_for(lip_method: &str) -> &'static str {
    match lip_method {
        "hover" => "hoverProvider",
        "definition" => "definitionProvider",
        "references" => "referencesProvider",
        "symbols" => "documentSymbolProvider",
        "code-actions" => "codeActionProvider",
        _ => "",
    }
}

/// The `POST /lip/diagnostics` cache: LSP diagnostics are PUSH-based
/// (`textDocument/publishDiagnostics`, a server->client notification with
/// no request/response of its own), so the adapter must cache the latest
/// publish per open doc rather than ever sending a request for it.
///
/// Per-uri state is one of:
/// - **dirty** (no trustworthy data yet — just opened/changed, no publish
///   since): a diagnostics query must WAIT (bounded) for the next publish.
/// - **clean** (a publish has landed since the last open/change): a query
///   returns the cached list immediately — the "cached path".
///
/// Each `publishDiagnostics` WHOLESALE REPLACES the cached list for that
/// uri (LSP semantics: the full current list, never a delta) and clears
/// dirty. A wait that times out without a publish returns whatever's
/// cached (empty if none ever arrived) — NEVER a refusal
/// (design-addendum-2.md §D: "empty-after-wait = valid {diagnostics: []},
/// never a refusal").
///
/// PRR-L5 amendment: this SAME `cached` map also backs PULL mode (LSP 3.17
/// `textDocument/diagnostic`, see `LspClient::pull_diagnostics`) — a
/// server is either push-only or pull-only in practice (never both live at
/// once for one uri), so there's no collision risk sharing the map. Pull
/// mode additionally threads `result_ids`: the last FULL report's
/// `resultId` per uri, sent back as `previousResultId` on the next pull so
/// an unchanged server can answer cheaply with `{"kind": "unchanged"}"`
/// instead of resending the same diagnostics. Pull mode never consults
/// `dirty`/`wait_for` — it always sends a live request (that IS the pull),
/// it just may get a cheap reply back.
struct DiagnosticsCache {
    inner: std::sync::Mutex<DiagInner>,
}

#[derive(Default)]
struct DiagInner {
    dirty: HashMap<String, bool>,
    cached: HashMap<String, Vec<Value>>,
    /// PULL MODE ONLY (LSP 3.17): the last FULL report's `resultId` per
    /// uri. A uri absent from this map has never had a full pull report
    /// (either never pulled, or the server never sent a `resultId`).
    result_ids: HashMap<String, String>,
}

impl DiagnosticsCache {
    fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(DiagInner::default()),
        }
    }

    /// Mark `uri` dirty — called by `ensure_doc_open` BEFORE it sends the
    /// `didOpen`/`didChange` that triggers a fresh publish, so a publish
    /// racing in immediately after can never be mistaken for stale data
    /// left over from before this open/change.
    fn mark_dirty(&self, uri: &str) {
        self.lock().dirty.insert(uri.to_string(), true);
    }

    /// Record an incoming `publishDiagnostics` notification.
    fn record_publish(&self, uri: &str, diagnostics: Vec<Value>) {
        let mut inner = self.lock();
        inner.cached.insert(uri.to_string(), diagnostics);
        inner.dirty.insert(uri.to_string(), false);
    }

    fn is_dirty(&self, uri: &str) -> bool {
        self.lock().dirty.get(uri).copied().unwrap_or(false)
    }

    fn cached(&self, uri: &str) -> Vec<Value> {
        self.lock().cached.get(uri).cloned().unwrap_or_default()
    }

    /// PULL MODE: the `resultId` from the last FULL report served for
    /// `uri`, if any — threaded into the next pull's `previousResultId`.
    fn result_id(&self, uri: &str) -> Option<String> {
        self.lock().result_ids.get(uri).cloned()
    }

    /// PULL MODE: record a FULL `DocumentDiagnosticReport` — wholesale
    /// replaces the cached items (same semantics as `record_publish`) and
    /// updates (or clears, if the server omitted one) the `resultId` used
    /// for the next `previousResultId`.
    fn record_full_pull(&self, uri: &str, items: Vec<Value>, result_id: Option<String>) {
        let mut inner = self.lock();
        inner.cached.insert(uri.to_string(), items);
        match result_id {
            Some(id) => {
                inner.result_ids.insert(uri.to_string(), id);
            }
            None => {
                inner.result_ids.remove(uri);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DiagInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// If `uri` is clean, returns the cache immediately (the cached path).
    /// If dirty, polls (10ms interval — well under any wait budget worth
    /// configuring) until a publish clears it or `timeout` elapses, then
    /// returns whatever's cached (possibly still empty — a valid answer,
    /// never a refusal).
    async fn wait_for(&self, uri: &str, timeout: std::time::Duration) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if !self.is_dirty(uri) {
                return self.cached(uri);
            }
            if tokio::time::Instant::now() >= deadline {
                return self.cached(uri);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

/// Tracks LSP `$/progress` work-done lifecycle (`WorkDoneProgressBegin` /
/// `...Report` / `...End`, spec-defined and server-agnostic — never a
/// guess at WHAT the work is, e.g. never string-matching a `title` like
/// "indexing") so the adapter can tell whether the server currently has
/// ANY outstanding background work in flight. This is a NECESSARY (not
/// sufficient — see the doc comment on `LspClient::is_indexing`) signal
/// for the real-world finding that querying ruby-lsp immediately after
/// `didOpen`, before its cold-boot workspace index finishes, silently
/// returns null/empty for hover/definition/references/diagnostics even on
/// a plain intra-file, Prism-only position (PRR-L3 smoke → PRR-L4
/// confirmed live reproduction: hover was null when queried before any
/// progress `begin` arrived, then real/non-null once observed mid-index).
/// Requires the client to declare `window.workDoneProgress: true` in its
/// `initialize` capabilities (see `LspClient::start`) — confirmed live
/// that ruby-lsp 0.26.11 sends ZERO `$/progress` traffic over 25s when
/// that capability is undeclared (kb-lip's PRE-fix behavior), vs. a full
/// begin→report×N→end cycle within seconds of declaring it.
struct ProgressTracker {
    active: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl ProgressTracker {
    fn new() -> Self {
        Self {
            active: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record one `$/progress` notification's `token` + `value.kind`.
    /// `"report"` (and anything else unrecognized) is a no-op — only
    /// `"begin"`/`"end"` change the active set.
    fn record(&self, token: &str, kind: &str) {
        match kind {
            "begin" => {
                self.lock().insert(token.to_string());
            }
            "end" => {
                self.lock().remove(token);
            }
            _ => {}
        }
    }

    /// True iff at least one progress token has sent `begin` with no
    /// matching `end` yet.
    fn is_active(&self) -> bool {
        !self.lock().is_empty()
    }
}

/// A JSON-RPC `ProgressToken` is `integer | string` (LSP spec) — canonicalize
/// either representation to one string key for the active-token set.
fn progress_token_key(token: &Value) -> Option<String> {
    match token {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

struct OpenDoc {
    version: i64,
    blob_sha: String,
}

struct OpenDocs {
    docs: HashMap<String, OpenDoc>,
    /// Front = least recently used, back = most recently used.
    lru: VecDeque<String>,
}

impl OpenDocs {
    fn new() -> Self {
        Self {
            docs: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn touch(&mut self, uri: &str) {
        if let Some(pos) = self.lru.iter().position(|u| u == uri) {
            self.lru.remove(pos);
        }
        self.lru.push_back(uri.to_string());
    }

    fn is_full(&self) -> bool {
        self.docs.len() >= MAX_OPEN_DOCS
    }
}

/// Minimal (no percent-encoding beyond spaces) `file://` URI for a
/// workspace-root-relative absolute path — a documented v1 limitation
/// (design-lip.md's "minimal capabilities" framing); paths with unusual
/// characters beyond spaces are a follow-up.
pub fn path_to_file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
}

pub struct LspClient {
    child: AsyncMutex<Child>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
    reader_task: tokio::task::JoinHandle<()>,
    pub server_info: ServerInfo,
    open_docs: AsyncMutex<OpenDocs>,
    diagnostics: Arc<DiagnosticsCache>,
    progress: Arc<ProgressTracker>,
    pub pid: Option<u32>,
}

impl LspClient {
    /// Spawn `config.command` (never through a shell — argv passed
    /// directly to `Command`) and run the initialize/initialized
    /// handshake.
    pub async fn start(config: &Config) -> Result<Self, LspError> {
        let mut cmd = Command::new(&config.command[0]);
        cmd.args(&config.command[1..])
            // The child's Bundler/Gemfile detection (e.g. ruby-lsp's
            // `Bundler.default_gemfile`) runs from the process's actual
            // working directory, NOT from the LSP-protocol `rootUri`/
            // `rootPath` sent in `initialize` params below — without this,
            // a kb-lip started from anywhere other than the target repo
            // (systemd unit, container with an unrelated WORKDIR, an
            // operator's shell) makes the child bootstrap in degraded mode
            // even with `workspace_root` configured correctly (confirmed
            // live against ruby-lsp 0.26.11, PRR-L3 smoke).
            .current_dir(&config.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|source| LspError::Spawn {
            command: config.command.clone(),
            source,
        })?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or(LspError::NoStdio)?;
        let stdout = child.stdout.take().ok_or(LspError::NoStdio)?;

        let stdin = Arc::new(AsyncMutex::new(stdin));
        let pending: PendingMap = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let diagnostics = Arc::new(DiagnosticsCache::new());
        let progress = Arc::new(ProgressTracker::new());

        let reader_task = tokio::spawn(reader_loop(
            stdout,
            pending.clone(),
            alive.clone(),
            stdin.clone(),
            diagnostics.clone(),
            progress.clone(),
        ));

        let next_id = AtomicI64::new(1);
        let root_uri = path_to_file_uri(&config.workspace_root);
        let ws_name = config
            .workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": config.workspace_root.to_string_lossy(),
            "workspaceFolders": [{"uri": root_uri, "name": ws_name}],
            "capabilities": {
                // Required for the server to send us window/workDoneProgress/
                // create + $/progress at all (LSP spec: a server must not
                // initiate server-side progress unless the client declared
                // this — confirmed live: ruby-lsp 0.26.11 sends ZERO
                // progress traffic without it, see ProgressTracker's doc
                // comment). This is what `LspClient::is_indexing` reads.
                "window": {"workDoneProgress": true},
                "textDocument": {
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {},
                    "references": {},
                    "documentSymbol": {},
                    // S2-C (design-s2.md, operator ratified 2026-08-28):
                    // declares the standard predefined `CodeActionKind`s
                    // (LSP 3.17 §3.17.11.1) so a server that gates its own
                    // reply on the client's declared kind set doesn't
                    // silently omit anything; `resolveSupport` +
                    // `dataSupport` let a server return a code action
                    // WITHOUT `.edit` (carrying only `data`) and have
                    // kb-lip round-trip `codeAction/resolve` for it — see
                    // `http::guarded_code_actions`.
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": [
                                    "",
                                    "quickfix",
                                    "refactor",
                                    "refactor.extract",
                                    "refactor.inline",
                                    "refactor.rewrite",
                                    "source",
                                    "source.organizeImports",
                                    "source.fixAll"
                                ]
                            }
                        },
                        "resolveSupport": {"properties": ["edit"]},
                        "dataSupport": true
                    }
                }
            },
            "initializationOptions": config.initialization_options.clone().unwrap_or(Value::Null),
        });

        let init_result = send_request(&stdin, &pending, &next_id, "initialize", init_params)
            .await
            .map_err(|e| LspError::InitializeFailed(e.to_string()))?;
        send_notification(&stdin, "initialized", json!({}))
            .await
            .map_err(|e| LspError::InitializeFailed(e.to_string()))?;
        let server_info = ServerInfo::from_initialize_result(&init_result);

        Ok(Self {
            child: AsyncMutex::new(child),
            stdin,
            next_id,
            pending,
            alive,
            reader_task,
            server_info,
            open_docs: AsyncMutex::new(OpenDocs::new()),
            diagnostics,
            progress,
            pid,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Does the server currently have an outstanding, unfinished
    /// `$/progress` work item (e.g. ruby-lsp's cold-boot workspace
    /// index)? Surfaced as `indexing` on `GET /lip/identity` and used to
    /// REFUSE the four query endpoints with `refused: "indexing"` rather
    /// than let them silently return a null/empty result while the
    /// server's answer would be incomplete — see `ProgressTracker`'s doc
    /// comment for the confirmed live reproduction this closes.
    ///
    /// KNOWN GAP (documented, not closed by this check): the window
    /// between `initialized` and the server's FIRST `$/progress` `begin`
    /// (confirmed live: ~16s on a cold ruby-lsp boot against a real,
    /// large Rails app) is indistinguishable from "this server will never
    /// send progress at all" — `is_indexing` reads `false` in both cases,
    /// since there is no active token yet either way. Closing that
    /// remaining gap needs an operator-configurable settle delay or an
    /// equivalent heuristic; deferred (see providers/README.md's
    /// known-issues section).
    pub fn is_indexing(&self) -> bool {
        self.progress.is_active()
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        send_request(&self.stdin, &self.pending, &self.next_id, method, params).await
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        send_notification(&self.stdin, method, params).await
    }

    /// Send a request only if the server advertised support for it;
    /// otherwise short-circuit with `None` (caller maps that to
    /// `refused: "unsupported"`) — avoids opening a document for a
    /// method the server never claimed to implement.
    pub async fn request_if_supported(
        &self,
        lip_method: &str,
        lsp_method: &str,
        params: Value,
    ) -> Option<Result<Value, LspError>> {
        let key = capability_key_for(lip_method);
        if !key.is_empty() && !self.server_info.supports(key) {
            return None;
        }
        Some(self.request(lsp_method, params).await)
    }

    /// Open `uri` lazily with `blob`'s current content before the first
    /// query on it, or `didChange` it if it was already open with
    /// different content (so the LSP always sees the exact bytes the
    /// caller's blob guard just verified).
    pub async fn ensure_doc_open(
        &self,
        uri: &str,
        blob: &Blob,
        language_id: &str,
    ) -> Result<(), LspError> {
        let mut docs = self.open_docs.lock().await;
        if let Some(existing) = docs.docs.get(uri) {
            if existing.blob_sha == blob.sha {
                docs.touch(uri);
                return Ok(());
            }
            let new_version = existing.version + 1;
            let text = String::from_utf8_lossy(&blob.bytes).into_owned();
            // Mark dirty BEFORE sending — a publish racing in the instant
            // after didChange must never be mistaken for a stale publish
            // left over from the PREVIOUS content (see DiagnosticsCache's
            // doc comment).
            self.diagnostics.mark_dirty(uri);
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": new_version},
                    "contentChanges": [{"text": text}]
                }),
            )
            .await?;
            docs.docs.insert(
                uri.to_string(),
                OpenDoc {
                    version: new_version,
                    blob_sha: blob.sha.clone(),
                },
            );
            docs.touch(uri);
            return Ok(());
        }

        if docs.is_full() {
            if let Some(evict_uri) = docs.lru.pop_front() {
                docs.docs.remove(&evict_uri);
                self.notify(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": evict_uri}}),
                )
                .await?;
            }
        }
        let text = String::from_utf8_lossy(&blob.bytes).into_owned();
        // Same ordering rationale as the didChange branch above: mark
        // dirty BEFORE didOpen so the wait path can never miss a publish
        // that races in immediately.
        self.diagnostics.mark_dirty(uri);
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await?;
        docs.docs.insert(
            uri.to_string(),
            OpenDoc {
                version: 1,
                blob_sha: blob.sha.clone(),
            },
        );
        docs.touch(uri);
        Ok(())
    }

    /// The current diagnostics for `uri` — returns the cache immediately
    /// if a publish has already landed since the doc was last
    /// opened/changed, otherwise waits (bounded by `wait`) for the first
    /// one. ALWAYS a `Vec` (possibly empty), never a refusal — see
    /// [`DiagnosticsCache`]'s doc comment. Callers must have already
    /// called [`LspClient::ensure_doc_open`] for `uri` in THIS request
    /// (so a fresh open's dirty flag is set before this is called).
    pub async fn diagnostics_for(&self, uri: &str, wait: std::time::Duration) -> Vec<Value> {
        self.diagnostics.wait_for(uri, wait).await
    }

    /// A NON-WAITING snapshot of the PUSH diagnostics cache for `uri` —
    /// used by `/lip/code-actions`'s `context.diagnostics` build
    /// (`http::guarded_code_actions`) in push mode, which must never delay
    /// a code-actions round trip waiting on a publish that may never land
    /// (unlike `/lip/diagnostics`'s own bounded [`LspClient::diagnostics_for`]
    /// wait). Returns whatever is currently cached — possibly empty, e.g.
    /// on a freshly-opened doc with no publish yet — never a refusal.
    pub fn diagnostics_snapshot(&self, uri: &str) -> Vec<Value> {
        self.diagnostics.cached(uri)
    }

    /// PULL-mode diagnostics (LSP 3.17 `textDocument/diagnostic`), used
    /// instead of `diagnostics_for` when the server advertised
    /// `diagnosticProvider` at `initialize` (`ServerInfo::diagnostics_mode`
    /// == `"pull"`, PRR-L5 — closes the gap PRR-L4 found against ruby-lsp
    /// 0.26.11, which implements pull only and never pushes
    /// `publishDiagnostics`). Sends the request with `previousResultId`
    /// set to the last FULL report's `resultId` for `uri` (if any), so an
    /// unchanged server can reply cheaply with `{"kind": "unchanged"}`
    /// instead of resending the same diagnostics — per LSP 3.17's
    /// `DocumentDiagnosticReport` contract. A `"full"` reply REPLACES the
    /// cache (items + resultId) and is returned; an `"unchanged"` reply
    /// serves the cache untouched (the server is required to only reply
    /// `"unchanged"` when it recognizes the `previousResultId` we sent, so
    /// the cache is guaranteed populated in that branch). Any other reply
    /// shape (missing/unrecognized `kind`, e.g. a malformed or `null`
    /// result) is a hard `LspError` — propagated by the caller as
    /// `refused: "server_down"`, same as any other failed LSP request,
    /// never a silent empty guess. Callers must have already called
    /// [`LspClient::ensure_doc_open`] for `uri` in THIS request.
    pub async fn pull_diagnostics(&self, uri: &str) -> Result<Vec<Value>, LspError> {
        let mut params = json!({"textDocument": {"uri": uri}});
        if let Some(previous_result_id) = self.diagnostics.result_id(uri) {
            params["previousResultId"] = json!(previous_result_id);
        }
        let result = self.request("textDocument/diagnostic", params).await?;
        match result.get("kind").and_then(Value::as_str) {
            Some("full") => {
                let items = result
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let result_id = result
                    .get("resultId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.diagnostics
                    .record_full_pull(uri, items.clone(), result_id);
                Ok(items)
            }
            Some("unchanged") => Ok(self.diagnostics.cached(uri)),
            _ => Err(LspError::MalformedDiagnosticReport(result)),
        }
    }

    /// `codeAction/resolve` round trip (S2-C, `http::guarded_code_actions`)
    /// for one action `textDocument/codeAction` returned WITHOUT `.edit` —
    /// only ever attempted when [`ServerInfo::code_action_resolve_supported`]
    /// is true. `action` is the exact `CodeAction` object the server
    /// returned (title/kind/data/… verbatim, whatever `data` it attached),
    /// relayed back unmodified as the resolve request's params, per the
    /// LSP spec's "send the previously-returned code action back" resolve
    /// contract. Never called for a bare `Command` item (no `data` to
    /// resolve against) — the caller gates on `data`'s presence first.
    pub async fn resolve_code_action(&self, action: Value) -> Result<Value, LspError> {
        self.request("codeAction/resolve", action).await
    }

    /// Graceful shutdown: LSP `shutdown` request + `exit` notification,
    /// then wait briefly for the child to exit on its own, escalating to
    /// SIGTERM (unix) if it doesn't. Best-effort throughout — called from
    /// the adapter's own SIGTERM handler, so it must not hang.
    pub async fn shutdown(&self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            {
                let mut child = self.child.lock().await;
                if let Ok(Some(_status)) = child.try_wait() {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: libc::kill with a valid pid and a standard signal
            // number is not memory-unsafe; failure (already-exited pid) is
            // silently ignored, matching kb-cli's own `libc::kill` usage
            // (crates/kb-cli/src/commands/daemon.rs).
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        let mut child = self.child.lock().await;
        let _ = child.wait().await;
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

async fn send_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    pending: &PendingMap,
    next_id: &AtomicI64,
    method: &str,
    params: Value,
) -> Result<Value, LspError> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, tx);
    let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    {
        let mut s = stdin.lock().await;
        if let Err(e) = rpc::framed::write_message(&mut *s, &msg).await {
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(LspError::Write(e));
        }
    }
    match rx.await {
        Ok(result) => result,
        Err(_) => Err(LspError::ChannelClosed),
    }
}

async fn send_notification(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    method: &str,
    params: Value,
) -> Result<(), LspError> {
    let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
    let mut s = stdin.lock().await;
    rpc::framed::write_message(&mut *s, &msg)
        .await
        .map_err(LspError::Write)
}

/// Background task: reads framed messages off the child's stdout for the
/// whole lifetime of the process. Dispatches responses to their pending
/// oneshot, auto-replies to any server-initiated request with a harmless
/// empty result (a minimal client must never let a real-world server hang
/// waiting on e.g. `workspace/configuration`), routes
/// `textDocument/publishDiagnostics` into the [`DiagnosticsCache`], and
/// drops every other notification. On EOF or a read error, flips `alive`
/// to false and fails every outstanding pending request — this is the
/// crash-detection signal [`crate::supervisor::Supervisor`] polls via
/// [`LspClient::is_alive`].
async fn reader_loop(
    stdout: ChildStdout,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    diagnostics: Arc<DiagnosticsCache>,
    progress: Arc<ProgressTracker>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match rpc::framed::read_message(&mut reader).await {
            Ok(Some(msg)) => handle_incoming(&msg, &pending, &stdin, &diagnostics, &progress).await,
            Ok(None) => {
                alive.store(false, Ordering::SeqCst);
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "lsp reader error; treating child as crashed");
                alive.store(false, Ordering::SeqCst);
                break;
            }
        }
    }
    let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(LspError::ChannelClosed));
    }
}

async fn handle_incoming(
    msg: &Value,
    pending: &PendingMap,
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    diagnostics: &Arc<DiagnosticsCache>,
    progress: &Arc<ProgressTracker>,
) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();

    if method.is_none() {
        // A response to one of our own requests.
        if let Some(id) = id.as_ref().and_then(Value::as_i64) {
            let sender = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(sender) = sender {
                let result = if let Some(err) = msg.get("error") {
                    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Err(LspError::RpcError { code, message })
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
        }
        return;
    }

    if let Some(id) = id {
        // A server-initiated REQUEST — reply so the server never hangs.
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": Value::Null});
        let stdin = stdin.clone();
        tokio::spawn(async move {
            let mut s = stdin.lock().await;
            let _ = rpc::framed::write_message(&mut *s, &reply).await;
        });
        return;
    }

    // A notification. `publishDiagnostics` and `$/progress` are the ones
    // we act on; every other notification is dropped (as before).
    if method == Some("textDocument/publishDiagnostics") {
        let uri = msg["params"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let diags = msg["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !uri.is_empty() {
            diagnostics.record_publish(&uri, diags);
        }
    } else if method == Some("$/progress") {
        if let Some(token) = progress_token_key(&msg["params"]["token"]) {
            let kind = msg["params"]["value"]["kind"].as_str().unwrap_or("");
            progress.record(&token, kind);
        }
    } else {
        tracing::trace!(method = ?method, "ignoring server-initiated notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_info_parses_name_version_and_capabilities() {
        let result = json!({
            "capabilities": {"hoverProvider": true, "definitionProvider": {"foo": 1}, "renameProvider": false},
            "serverInfo": {"name": "ruby-lsp", "version": "0.1.2"}
        });
        let info = ServerInfo::from_initialize_result(&result);
        assert_eq!(info.name.as_deref(), Some("ruby-lsp"));
        assert_eq!(info.version.as_deref(), Some("0.1.2"));
        assert!(info.supports("hoverProvider"));
        assert!(
            info.supports("definitionProvider"),
            "an options object counts as supported"
        );
        assert!(
            !info.supports("renameProvider"),
            "explicit false is unsupported"
        );
        assert!(
            !info.supports("referencesProvider"),
            "absent key is unsupported"
        );
    }

    #[test]
    fn server_info_missing_fields_degrade_gracefully() {
        let info = ServerInfo::from_initialize_result(&json!({}));
        assert_eq!(info.name, None);
        assert_eq!(info.version, None);
        assert!(!info.supports("hoverProvider"));
    }

    #[test]
    fn diagnostics_mode_is_pull_when_diagnostic_provider_is_an_object() {
        // The only shape LSP 3.17 actually sends (`DiagnosticOptions`).
        let info = ServerInfo::from_initialize_result(&json!({
            "capabilities": {"diagnosticProvider": {"interFileDependencies": false, "workspaceDiagnostics": false}}
        }));
        assert_eq!(info.diagnostics_mode(), "pull");
    }

    #[test]
    fn diagnostics_mode_is_pull_when_diagnostic_provider_is_bare_true() {
        // Not a legal LSP 3.17 shape, but `supports()`'s own leniency
        // treats it as supported the same way it does for the other four
        // capability keys — pinned so that leniency covers this key too.
        let info = ServerInfo::from_initialize_result(&json!({
            "capabilities": {"diagnosticProvider": true}
        }));
        assert_eq!(info.diagnostics_mode(), "pull");
    }

    #[test]
    fn diagnostics_mode_is_push_when_diagnostic_provider_is_absent_null_or_false() {
        assert_eq!(
            ServerInfo::from_initialize_result(&json!({"capabilities": {}})).diagnostics_mode(),
            "push",
            "absent"
        );
        assert_eq!(
            ServerInfo::from_initialize_result(
                &json!({"capabilities": {"diagnosticProvider": null}})
            )
            .diagnostics_mode(),
            "push",
            "null"
        );
        assert_eq!(
            ServerInfo::from_initialize_result(
                &json!({"capabilities": {"diagnosticProvider": false}})
            )
            .diagnostics_mode(),
            "push",
            "explicit false"
        );
    }

    #[test]
    fn capability_key_mapping_covers_every_endpoint() {
        assert_eq!(capability_key_for("hover"), "hoverProvider");
        assert_eq!(capability_key_for("definition"), "definitionProvider");
        assert_eq!(capability_key_for("references"), "referencesProvider");
        assert_eq!(capability_key_for("symbols"), "documentSymbolProvider");
        assert_eq!(capability_key_for("code-actions"), "codeActionProvider");
    }

    #[test]
    fn code_action_resolve_supported_requires_the_nested_flag() {
        assert!(
            ServerInfo::from_initialize_result(&json!({
                "capabilities": {"codeActionProvider": {"resolveProvider": true}}
            }))
            .code_action_resolve_supported(),
            "an options object with resolveProvider: true must count as supported"
        );
        assert!(
            !ServerInfo::from_initialize_result(&json!({
                "capabilities": {"codeActionProvider": true}
            }))
            .code_action_resolve_supported(),
            "a bare `true` codeActionProvider carries no resolveProvider flag"
        );
        assert!(
            !ServerInfo::from_initialize_result(&json!({
                "capabilities": {"codeActionProvider": {}}
            }))
            .code_action_resolve_supported(),
            "an options object missing resolveProvider must not be assumed supported"
        );
        assert!(
            !ServerInfo::from_initialize_result(&json!({"capabilities": {}}))
                .code_action_resolve_supported(),
            "absent codeActionProvider entirely"
        );
    }

    #[test]
    fn path_to_file_uri_encodes_spaces() {
        assert_eq!(
            path_to_file_uri(Path::new("/home/me/my repo")),
            "file:///home/me/my%20repo"
        );
        assert_eq!(path_to_file_uri(Path::new("/repo")), "file:///repo");
    }

    #[test]
    fn open_docs_lru_touch_moves_to_back() {
        let mut docs = OpenDocs::new();
        for uri in ["a", "b", "c"] {
            docs.docs.insert(
                uri.to_string(),
                OpenDoc {
                    version: 1,
                    blob_sha: "x".to_string(),
                },
            );
            docs.lru.push_back(uri.to_string());
        }
        docs.touch("a");
        assert_eq!(
            docs.lru,
            VecDeque::from(vec!["b".to_string(), "c".to_string(), "a".to_string()])
        );
    }

    #[test]
    fn open_docs_is_full_at_cap() {
        let mut docs = OpenDocs::new();
        assert!(!docs.is_full());
        for i in 0..MAX_OPEN_DOCS {
            docs.docs.insert(
                format!("uri-{i}"),
                OpenDoc {
                    version: 1,
                    blob_sha: "x".to_string(),
                },
            );
        }
        assert!(docs.is_full());
    }

    #[test]
    fn diagnostics_cache_starts_clean_with_an_empty_cache() {
        let cache = DiagnosticsCache::new();
        assert!(!cache.is_dirty("file:///a.rb"));
        assert_eq!(cache.cached("file:///a.rb"), Vec::<Value>::new());
    }

    #[test]
    fn diagnostics_cache_record_publish_wholesale_replaces_and_clears_dirty() {
        let cache = DiagnosticsCache::new();
        let uri = "file:///a.rb";
        cache.mark_dirty(uri);
        assert!(cache.is_dirty(uri));

        cache.record_publish(uri, vec![json!({"message": "first"})]);
        assert!(!cache.is_dirty(uri));
        assert_eq!(cache.cached(uri), vec![json!({"message": "first"})]);

        // A second publish REPLACES, not merges (LSP semantics).
        cache.record_publish(uri, vec![json!({"message": "second"})]);
        assert_eq!(cache.cached(uri), vec![json!({"message": "second"})]);

        // An EMPTY publish (server says "clean now") is a valid replace,
        // not treated as "no data".
        cache.record_publish(uri, vec![]);
        assert_eq!(cache.cached(uri), Vec::<Value>::new());
    }

    #[test]
    fn diagnostics_cache_record_full_pull_stores_items_and_result_id() {
        let cache = DiagnosticsCache::new();
        let uri = "file:///a.rb";
        assert_eq!(cache.result_id(uri), None);

        cache.record_full_pull(
            uri,
            vec![json!({"message": "first"})],
            Some("result-1".to_string()),
        );
        assert_eq!(cache.cached(uri), vec![json!({"message": "first"})]);
        assert_eq!(cache.result_id(uri), Some("result-1".to_string()));

        // A second full pull WHOLESALE REPLACES both, same as record_publish.
        cache.record_full_pull(
            uri,
            vec![json!({"message": "second"})],
            Some("result-2".to_string()),
        );
        assert_eq!(cache.cached(uri), vec![json!({"message": "second"})]);
        assert_eq!(cache.result_id(uri), Some("result-2".to_string()));
    }

    #[test]
    fn diagnostics_cache_record_full_pull_with_no_result_id_clears_it() {
        let cache = DiagnosticsCache::new();
        let uri = "file:///a.rb";
        cache.record_full_pull(uri, vec![], Some("result-1".to_string()));
        assert_eq!(cache.result_id(uri), Some("result-1".to_string()));

        // A server that stops sending a resultId must not leave a stale
        // one behind — the next pull must omit previousResultId entirely.
        cache.record_full_pull(uri, vec![], None);
        assert_eq!(cache.result_id(uri), None);
    }

    #[tokio::test]
    async fn diagnostics_cache_wait_for_returns_immediately_when_clean() {
        let cache = DiagnosticsCache::new();
        let uri = "file:///a.rb";
        cache.record_publish(uri, vec![json!({"message": "cached"})]);

        let start = tokio::time::Instant::now();
        let got = cache
            .wait_for(uri, std::time::Duration::from_millis(500))
            .await;
        assert_eq!(got, vec![json!({"message": "cached"})]);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(100),
            "the cached path must not wait: took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn diagnostics_cache_wait_for_catches_a_publish_that_races_in() {
        let cache = Arc::new(DiagnosticsCache::new());
        let uri = "file:///a.rb";
        cache.mark_dirty(uri);

        let publisher = {
            let cache = cache.clone();
            let uri = uri.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                cache.record_publish(&uri, vec![json!({"message": "late"})]);
            })
        };

        let got = cache
            .wait_for(uri, std::time::Duration::from_millis(500))
            .await;
        publisher.await.unwrap();
        assert_eq!(got, vec![json!({"message": "late"})]);
    }

    #[test]
    fn progress_tracker_starts_inactive() {
        let tracker = ProgressTracker::new();
        assert!(!tracker.is_active());
    }

    #[test]
    fn progress_tracker_begin_makes_it_active_end_clears_it() {
        let tracker = ProgressTracker::new();
        tracker.record("tok-1", "begin");
        assert!(tracker.is_active());
        tracker.record("tok-1", "end");
        assert!(!tracker.is_active());
    }

    #[test]
    fn progress_tracker_report_is_a_no_op() {
        let tracker = ProgressTracker::new();
        tracker.record("tok-1", "report");
        assert!(
            !tracker.is_active(),
            "a report with no prior begin must not flip active"
        );
        tracker.record("tok-1", "begin");
        tracker.record("tok-1", "report");
        assert!(
            tracker.is_active(),
            "a report between begin and end must not clear active"
        );
    }

    #[test]
    fn progress_tracker_stays_active_while_any_of_multiple_tokens_is_open() {
        let tracker = ProgressTracker::new();
        tracker.record("a", "begin");
        tracker.record("b", "begin");
        tracker.record("a", "end");
        assert!(
            tracker.is_active(),
            "token b is still open even though a ended"
        );
        tracker.record("b", "end");
        assert!(!tracker.is_active());
    }

    #[test]
    fn progress_token_key_accepts_string_and_integer_tokens() {
        assert_eq!(
            progress_token_key(&json!("indexing-progress")),
            Some("indexing-progress".to_string())
        );
        assert_eq!(progress_token_key(&json!(42)), Some("42".to_string()));
        assert_eq!(progress_token_key(&json!(null)), None);
    }

    #[tokio::test]
    async fn diagnostics_cache_wait_for_times_out_to_an_empty_vec_never_panicking() {
        let cache = DiagnosticsCache::new();
        let uri = "file:///never-published.rb";
        cache.mark_dirty(uri);

        let start = tokio::time::Instant::now();
        let got = cache
            .wait_for(uri, std::time::Duration::from_millis(50))
            .await;
        assert_eq!(
            got,
            Vec::<Value>::new(),
            "timeout must be a valid empty result, not a panic/error"
        );
        assert!(start.elapsed() >= std::time::Duration::from_millis(50));
    }
}
