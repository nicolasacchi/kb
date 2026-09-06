//! I1 — embedder subprocess protocol + IPC client.
//!
//! `Request`/`Response` types are shared between the daemon (which
//! drives the IPC) and the `kb-embedder` binary (which loads ONNX and
//! responds). One stdin/stdout NDJSON line per envelope.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// IPC request envelope (parent -> child). One per stdin line, NDJSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Embed {
        req_id: u64,
        texts: Vec<String>,
    },
    /// Cross-encoder rerank (reranker-mode subprocess). `documents` are the
    /// candidate texts in input order; the child returns one score per
    /// document in the SAME order (no sort, no top-n).
    Rerank {
        req_id: u64,
        query: String,
        documents: Vec<String>,
    },
    Shutdown,
}

/// IPC response envelope (child -> parent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ready {
        model: String,
        dim: u32,
    },
    EmbedOk {
        req_id: u64,
        vectors: Vec<Vec<f32>>,
    },
    /// `scores[i]` is the cross-encoder score for the input `documents[i]`
    /// (input order). The daemon sorts + applies top-n itself.
    RerankOk {
        req_id: u64,
        scores: Vec<f32>,
    },
    Error {
        req_id: u64,
        msg: String,
    },
}

/// Which kind of model a `kb-embedder` subprocess loads. Selects the CLI
/// flag (`--model` vs `--reranker`); the rest of the protocol is shared.
#[derive(Debug, Clone, Copy)]
enum SpawnMode {
    Embed,
    Rerank,
}

impl SpawnMode {
    fn flag(self) -> &'static str {
        match self {
            SpawnMode::Embed => "--model",
            SpawnMode::Rerank => "--reranker",
        }
    }
}

/// Daemon-side IPC client.
pub struct IpcBackend {
    child: Child,
    stdin: ChildStdin,
    // `Option` so a timed read can move the reader onto a throwaway thread and
    // leave `None` behind when the child wedges (the reader is detached, not
    // recovered); `respawn` repopulates it. `Some` in every steady state.
    stdout: Option<BufReader<ChildStdout>>,
    next_req_id: u64,
    dead: bool,
    // Retained so a dead subprocess can be respawned in place. The embedder
    // is shared per-model across every kb on that model (one
    // `Arc<Mutex<Embedder>>`), so a single subprocess crash would otherwise
    // wedge semantic search + index-time embedding for ALL of them until a
    // full daemon restart — `embed_batch` self-heals instead.
    model_name: String,
    cache_dir: PathBuf,
    nice: i32,
    mode: SpawnMode,
    respawns: u64,
}

impl IpcBackend {
    /// Spawn an embedding subprocess (`kb-embedder --model NAME`).
    pub fn spawn(model_name: &str, cache_dir: &Path, nice: i32) -> Result<Self> {
        Self::spawn_mode(SpawnMode::Embed, model_name, cache_dir, nice)
    }

    /// Spawn a reranking subprocess (`kb-embedder --reranker NAME`).
    pub fn spawn_reranker(model_name: &str, cache_dir: &Path, nice: i32) -> Result<Self> {
        Self::spawn_mode(SpawnMode::Rerank, model_name, cache_dir, nice)
    }

    fn spawn_mode(mode: SpawnMode, model_name: &str, cache_dir: &Path, nice: i32) -> Result<Self> {
        let (child, stdin, stdout) = Self::connect(mode, model_name, cache_dir, nice)?;
        Ok(Self {
            child,
            stdin,
            stdout: Some(stdout),
            next_req_id: 1,
            dead: false,
            model_name: model_name.to_string(),
            cache_dir: cache_dir.to_path_buf(),
            nice,
            mode,
            respawns: 0,
        })
    }

    /// Spawn the kb-embedder subprocess and complete the `Ready` handshake.
    /// Shared by [`spawn`](Self::spawn) and [`respawn`](Self::respawn).
    fn connect(
        mode: SpawnMode,
        model_name: &str,
        cache_dir: &Path,
        nice: i32,
    ) -> Result<(Child, ChildStdin, BufReader<ChildStdout>)> {
        let bin = locate_embedder_bin()?;
        let timeout = handshake_timeout();
        tracing::info!(
            model = %model_name,
            bin = %bin.display(),
            nice,
            handshake_timeout_secs = timeout.as_secs(),
            mode = ?mode,
            "spawning kb-embedder subprocess",
        );

        let mut cmd = Command::new(&bin);
        cmd.arg(mode.flag())
            .arg(model_name)
            .arg("--cache")
            .arg(cache_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        #[cfg(unix)]
        {
            apply_nice_before_run(&mut cmd, nice);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Storage(format!("spawn {}: {e}", bin.display())))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Storage("child stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Storage("child stdout missing".into()))?;
        let stdout = BufReader::new(stdout);

        // Bounded handshake wait. The child's ONNX load can wedge without
        // exiting (a broken onnxruntime install futex-waits forever right
        // after its "loading embedder" log), and a bare `read_line` here
        // would hang the daemon-side caller with it — the field workaround
        // used to be deleting `embedding_model` from kb.toml. Read on a
        // throwaway thread and bound the wait; on timeout, kill the child
        // (which unblocks the reader via EOF) and surface a plain error so
        // the caller degrades to keyword-only search.
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut line = String::new();
            let res = stdout.read_line(&mut line);
            let _ = tx.send((res, line, stdout));
        });
        let (read_res, line, stdout) = match rx.recv_timeout(timeout) {
            Ok(handshake) => {
                let _ = reader.join();
                handshake
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                // Deliberately NOT joined: read_line unblocks only when the
                // pipe's last write end closes, and a grandchild of a killed
                // child can keep it open past the kill — joining here would
                // block exactly as long as the hang we're escaping. The
                // detached thread owns only the BufReader + a Sender to a
                // receiver we're about to drop; it exits when the pipe dies.
                drop(reader);
                let what = match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => format!(
                        "embedder handshake timed out after {}s — the model load looks \
                         wedged; is onnxruntime loadable? (check ORT_DYLIB_PATH / the \
                         system onnxruntime; raise KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS \
                         if a first-run model download is genuinely this slow)",
                        timeout.as_secs()
                    ),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => {
                        "embedder handshake reader thread died".to_string()
                    }
                };
                return Err(Error::Storage(what));
            }
        };
        read_res.map_err(|e| Error::Storage(format!("read handshake: {e}")))?;
        if line.is_empty() {
            return Err(Error::Storage(
                "embedder exited before handshake (check stderr)".into(),
            ));
        }
        let resp: Response = serde_json::from_str(line.trim())
            .map_err(|e| Error::Storage(format!("parse handshake {line:?}: {e}")))?;
        match resp {
            Response::Ready { model, dim } => {
                // Defence-in-depth: a stale/mismatched kb-embedder binary that
                // loaded a DIFFERENT model would otherwise only surface far
                // downstream (e.g. a lance vector-width panic). The child
                // echoes the model it actually loaded; fail fast if it isn't
                // the one we asked for. (Embedding dim stays validated by the
                // lance schema width check — the test fakes use a small dim.)
                if model != model_name {
                    return Err(Error::Storage(format!(
                        "embedder handshake model {model:?} != requested {model_name:?} \
                         (wrong/stale kb-embedder binary?)"
                    )));
                }
                tracing::info!(model, dim, "kb-embedder ready");
                Ok((child, stdin, stdout))
            }
            other => Err(Error::Storage(format!(
                "expected Ready handshake, got {other:?}"
            ))),
        }
    }

    /// Bring up a fresh subprocess after the current one died, swapping it in
    /// place so the shared `Arc<Mutex<Embedder>>` recovers without a daemon
    /// restart. Reloads the model, so it blocks for the handshake; that cost
    /// is paid only on the rare recovery path. Propagates the spawn error if
    /// the binary itself is now unavailable (caller then degrades to keyword
    /// search rather than looping).
    fn respawn(&mut self) -> Result<()> {
        let (child, stdin, stdout) =
            Self::connect(self.mode, &self.model_name, &self.cache_dir, self.nice)?;
        // Replacing `self.child` drops the dead Child, whose `Drop` reaps it.
        self.child = child;
        self.stdin = stdin;
        self.stdout = Some(stdout);
        self.next_req_id = 1;
        self.dead = false;
        self.respawns += 1;
        tracing::warn!(
            model = %self.model_name,
            respawns = self.respawns,
            "respawned a dead kb-embedder subprocess (semantic search degraded to keyword-only until now)",
        );
        Ok(())
    }

    /// Number of times this backend has respawned its subprocess. Non-zero
    /// means the embedder has crashed at least once this daemon lifetime.
    pub fn respawn_count(&self) -> u64 {
        self.respawns
    }

    /// Proactive liveness probe + recovery (v0.16 Q-track). Cheap when
    /// healthy: a non-blocking [`Child::try_wait`] that detects an *idle*
    /// death — e.g. an OOM-kill between requests — which `embed_batch`
    /// wouldn't notice until the next embed silently degraded to a
    /// respawn on the user's request. If the child has exited, respawn it
    /// here so the next search/index embed rides a live process; returns
    /// whether the embedder is live afterwards (`false` = respawn failed,
    /// e.g. the binary went missing → semantic search will fall back to
    /// keyword-only). Drives [`respawn`](Self::respawn), so callers MUST
    /// hold the same lock the embed path uses — the ticker probes under
    /// the shared `Arc<Mutex<Embedder>>`, which can't race a concurrent
    /// mid-request respawn.
    pub fn ensure_alive(&mut self) -> bool {
        if !self.dead {
            match self.child.try_wait() {
                Ok(Some(_status)) => self.dead = true, // exited while idle
                Ok(None) => return true,               // still running
                Err(_) => self.dead = true,            // can't tell → assume dead
            }
        }
        // Dead (already, or just detected): bring a fresh subprocess up now
        // rather than on the next request. A failed respawn leaves `dead`
        // set, so the next tick retries.
        self.respawn().is_ok()
    }

    pub fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_batch(&[text.to_string()])?;
        out.pop()
            .ok_or_else(|| Error::Storage("embedder returned empty vectors".into()))
    }

    pub fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Self-heal: a prior call may have killed the subprocess. Bring a fresh
        // one up before trying, and if THIS call kills it (transport death
        // mid-request), respawn once and retry the same batch. A non-transport
        // error (parse / req_id mismatch / an `Error` envelope) is returned as-is
        // — only a dead transport triggers a respawn, so we never loop on a
        // legitimate embedder error.
        if self.dead {
            self.respawn()?;
        }
        match self.send_recv(texts) {
            Ok(vectors) => Ok(vectors),
            Err(e) if self.dead => {
                tracing::warn!(error = %e, "kb-embedder died mid-request; respawning and retrying once");
                self.respawn()?;
                self.send_recv(texts)
            }
            Err(e) => Err(e),
        }
    }

    /// One request/response round-trip. Sets `self.dead` on any transport
    /// failure (write / flush / EOF / read), which [`embed_batch`](Self::embed_batch)
    /// turns into a respawn.
    fn send_recv(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let req_id = self.next_req_id;
        self.next_req_id = self.next_req_id.saturating_add(1);
        let req = Request::Embed {
            req_id,
            texts: texts.to_vec(),
        };
        let line = serde_json::to_string(&req)
            .map_err(|e| Error::Storage(format!("serialise request: {e}")))?;
        if let Err(e) = writeln!(self.stdin, "{line}") {
            self.dead = true;
            return Err(Error::Storage(format!("write to embedder: {e}")));
        }
        if let Err(e) = self.stdin.flush() {
            self.dead = true;
            return Err(Error::Storage(format!("flush embedder stdin: {e}")));
        }
        let resp_line = self.read_line_timeout(request_timeout())?;
        let resp: Response = serde_json::from_str(resp_line.trim())
            .map_err(|e| Error::Storage(format!("parse response: {e}")))?;
        match resp {
            Response::EmbedOk {
                req_id: got,
                vectors,
            } if got == req_id => Ok(vectors),
            // A req_id mismatch / unexpected envelope means the stdin↔stdout
            // stream has desynced; since next_req_id has already advanced,
            // every later request would read the off-by-one line and loop
            // forever. Mark the transport dead so the retry-once path
            // respawns a fresh process and resyncs. (A `Response::Error` is
            // a legitimate application error, NOT a desync — leave it alone
            // so we never respawn-loop on a deterministic embedder error.)
            Response::EmbedOk { req_id: got, .. } => {
                self.dead = true;
                Err(Error::Storage(format!(
                    "embedder req_id mismatch: sent {req_id}, got {got}"
                )))
            }
            Response::Error { msg, .. } => Err(Error::Storage(format!("embedder error: {msg}"))),
            Response::Ready { .. } => {
                self.dead = true;
                Err(Error::Storage(
                    "unexpected duplicate Ready envelope mid-stream".into(),
                ))
            }
            Response::RerankOk { .. } => {
                self.dead = true;
                Err(Error::Storage(
                    "unexpected rerank response on the embed path".into(),
                ))
            }
        }
    }

    /// Read one NDJSON response line, bounded by `timeout`. A wedged-but-alive
    /// subprocess (swap thrash, an ONNX inference that futex-waits forever)
    /// used to block this `read_line` — and with it the shared
    /// `Arc<Mutex<Embedder>>` — indefinitely, which is exactly what stalled
    /// kb.example.com recalls past 10 s on 2026-07-03. Mirrors the handshake's
    /// throwaway-reader pattern (see [`connect`](Self::connect)): move the
    /// reader onto a detached thread, `recv_timeout` the result, and on timeout
    /// kill the child (closing the pipe so the detached reader hits EOF and
    /// exits) + mark the transport dead so [`embed_batch`](Self::embed_batch)'s
    /// retry-once path respawns. On success the reader is restored to `self`.
    fn read_line_timeout(&mut self, timeout: std::time::Duration) -> Result<String> {
        let mut stdout = self.stdout.take().ok_or_else(|| {
            Error::Storage("embedder stdout gone (prior timeout, not respawned)".into())
        })?;
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            let res = stdout.read_line(&mut line);
            let _ = tx.send((res, line, stdout));
        });
        match rx.recv_timeout(timeout) {
            Ok((read_res, line, stdout)) => {
                let _ = reader.join();
                self.stdout = Some(stdout); // restore for the next request
                match read_res {
                    Ok(0) => {
                        self.dead = true;
                        Err(Error::Storage("embedder closed stdout (EOF)".into()))
                    }
                    Ok(_) => Ok(line),
                    Err(e) => {
                        self.dead = true;
                        Err(Error::Storage(format!("read embedder stdout: {e}")))
                    }
                }
            }
            Err(_) => {
                // Timed out: the child is wedged mid-request. Kill it and mark
                // the transport dead. Do NOT join the reader — read_line unblocks
                // only when the pipe's last write end closes, and a grandchild of
                // the killed child can hold it open past the kill (same reasoning
                // as the handshake). `self.stdout` stays `None`; `respawn` refills it.
                let _ = self.child.kill();
                let _ = self.child.wait();
                drop(reader);
                self.dead = true;
                Err(Error::Storage(format!(
                    "embedder request timed out after {}s (wedged mid-request); \
                     killed + will respawn (raise KB_EMBED_TIMEOUT_SECS if a large \
                     batch on a niced embedder under load is legitimately this slow)",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// Rerank `documents` against `query`, returning one score per document
    /// in input order. Self-heals the subprocess exactly like
    /// [`embed_batch`](Self::embed_batch). Only valid on a reranker-mode
    /// backend (spawned via [`spawn_reranker`](Self::spawn_reranker)).
    pub fn rerank(&mut self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        if self.dead {
            self.respawn()?;
        }
        match self.send_recv_rerank(query, documents) {
            Ok(scores) => Ok(scores),
            Err(e) if self.dead => {
                tracing::warn!(error = %e, "kb-embedder (reranker) died mid-request; respawning and retrying once");
                self.respawn()?;
                self.send_recv_rerank(query, documents)
            }
            Err(e) => Err(e),
        }
    }

    /// One rerank round-trip. Mirrors [`send_recv`](Self::send_recv): sets
    /// `self.dead` on any transport failure, which `rerank` turns into a
    /// respawn.
    fn send_recv_rerank(&mut self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let req_id = self.next_req_id;
        self.next_req_id = self.next_req_id.saturating_add(1);
        let req = Request::Rerank {
            req_id,
            query: query.to_string(),
            documents: documents.to_vec(),
        };
        let line = serde_json::to_string(&req)
            .map_err(|e| Error::Storage(format!("serialise rerank request: {e}")))?;
        if let Err(e) = writeln!(self.stdin, "{line}") {
            self.dead = true;
            return Err(Error::Storage(format!("write to reranker: {e}")));
        }
        if let Err(e) = self.stdin.flush() {
            self.dead = true;
            return Err(Error::Storage(format!("flush reranker stdin: {e}")));
        }
        let resp_line = self.read_line_timeout(request_timeout())?;
        let resp: Response = serde_json::from_str(resp_line.trim())
            .map_err(|e| Error::Storage(format!("parse rerank response: {e}")))?;
        match resp {
            Response::RerankOk {
                req_id: got,
                scores,
            } if got == req_id => Ok(scores),
            // Desync → mark dead so the retry-once path respawns + resyncs
            // (mirrors `send_recv`); a `Response::Error` is a real app
            // error, so it does NOT trip a respawn.
            Response::RerankOk { req_id: got, .. } => {
                self.dead = true;
                Err(Error::Storage(format!(
                    "reranker req_id mismatch: sent {req_id}, got {got}"
                )))
            }
            Response::Error { msg, .. } => Err(Error::Storage(format!("reranker error: {msg}"))),
            other => {
                self.dead = true;
                Err(Error::Storage(format!(
                    "unexpected rerank response: {other:?}"
                )))
            }
        }
    }

    pub fn shutdown(&mut self) {
        if self.dead {
            return;
        }
        let payload = serde_json::to_string(&Request::Shutdown).unwrap_or_default();
        let _ = writeln!(self.stdin, "{payload}");
        let _ = self.stdin.flush();
        self.dead = true;
    }
}

/// Daemon-side reranker handle — drives a `kb-embedder --reranker` subprocess
/// over IPC. Always compiled (no fastembed); replaces the old in-process
/// `embed::Reranker` in the daemon so kb-server links zero ONNX. Held behind
/// `Arc<Mutex<RerankerClient>>` like the embedder.
pub struct RerankerClient {
    inner: IpcBackend,
    model_name: &'static str,
}

impl RerankerClient {
    /// Spawn a reranker subprocess for `model_name` at the given niceness.
    /// Validates the name against the registry (so a typo fails fast rather
    /// than spawning a doomed child).
    pub fn spawn_ipc(model_name: &str, cache_dir: PathBuf, nice: i32) -> Result<Self> {
        let info = crate::embed::reranker_info(model_name)
            .ok_or_else(|| Error::BadRequest(format!("unknown reranker model: {model_name:?}")))?;
        let inner = IpcBackend::spawn_reranker(info.name, &cache_dir, nice)?;
        Ok(Self {
            inner,
            model_name: info.name,
        })
    }

    /// User-facing reranker model name.
    pub fn model_name(&self) -> &'static str {
        self.model_name
    }

    /// Liveness probe + recovery, mirroring the embedder path.
    pub fn ensure_alive(&mut self) -> bool {
        self.inner.ensure_alive()
    }

    /// Rerank `documents` against `query`, returning `(original_index, score)`
    /// for the top `top_n`, score-descending — the same shape the old
    /// in-process `embed::Reranker::rerank` returned, so the search route is
    /// unchanged. Empty input short-circuits (no IPC round-trip).
    pub fn rerank(
        &mut self,
        query: &str,
        documents: &[String],
        top_n: usize,
    ) -> Result<Vec<(usize, f32)>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let scores = self.inner.rerank(query, documents)?;
        let mut ranked: Vec<(usize, f32)> = scores.into_iter().enumerate().collect();
        // Stable sort, score-descending — matches fastembed's internal sort
        // (and keeps tie-ordering deterministic by original index).
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.truncate(top_n);
        Ok(ranked)
    }
}

impl Drop for IpcBackend {
    fn drop(&mut self) {
        // Ask the child to exit (it returns from its serve loop on Shutdown)
        // BEFORE waiting on it. stdin is still open at this point, so a healthy
        // child blocked in `read_line` would otherwise never see EOF until this
        // Drop returns — making `wait_timeout_or_kill` burn the full 2 s grace
        // window and SIGKILL on every teardown (embedder + reranker = ~4 s per
        // daemon stop / in-process config restart). `shutdown()` is a no-op if
        // the process is already dead.
        self.shutdown();
        let _ = self.child.wait_timeout_or_kill();
    }
}

trait WaitTimeoutOrKill {
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()>;
}

impl WaitTimeoutOrKill for Child {
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.try_wait()? {
                Some(_) => return Ok(()),
                None => {
                    if Instant::now() >= deadline {
                        let _ = self.kill();
                        let _ = self.wait();
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

#[cfg(unix)]
fn apply_nice_before_run(cmd: &mut Command, nice: i32) {
    // Defined in a separate module to keep the Unix-only syscall
    // wiring out of the public API surface of this file.
    nice_unix::install(cmd, nice);
}

#[cfg(not(unix))]
fn apply_nice_before_run(_cmd: &mut Command, _nice: i32) {}

#[cfg(unix)]
mod nice_unix {
    use std::process::Command;

    pub fn install(cmd: &mut Command, nice: i32) {
        let nice = nice.clamp(0, 19);
        use std::os::unix::process::CommandExt;
        unsafe {
            let raw = cmd as *mut Command;
            (*raw).pre_exec(move || {
                let rc = libc::setpriority(libc::PRIO_PROCESS, 0, nice);
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// Handshake wait bound for [`IpcBackend::spawn`] / respawn. Generous
/// default: a FIRST run legitimately downloads the model before it can
/// answer `Ready` (bge-small is ~100 MB; slow links take minutes).
/// `KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS` overrides it (tests use 1).
fn handshake_timeout() -> std::time::Duration {
    let secs = std::env::var("KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300);
    std::time::Duration::from_secs(secs)
}

/// Per-request response wait bound for embed/rerank round-trips
/// ([`read_line_timeout`](IpcBackend::read_line_timeout)). Distinct from the
/// handshake bound: the model is already loaded, so a response that never
/// arrives means the subprocess wedged mid-inference (swap thrash, a futex-
/// waiting ONNX op) — the failure mode that stalled kb.example.com recalls past
/// 10 s on 2026-07-03, since the hung read held the shared embedder mutex.
/// Env-tuned like [`handshake_timeout`] (no config-struct plumbing);
/// `KB_EMBED_TIMEOUT_SECS` overrides. Default 60 s: generous enough that a
/// large legitimate batch on a niced embedder under load never trips it,
/// tight enough that a genuine wedge recovers in bounded time. Tests set 1.
fn request_timeout() -> std::time::Duration {
    let secs = std::env::var("KB_EMBED_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(60);
    std::time::Duration::from_secs(secs)
}

pub fn locate_embedder_bin() -> Result<PathBuf> {
    if let Ok(env) = std::env::var("KB_EMBEDDER_BIN") {
        let p = PathBuf::from(env);
        if p.is_file() {
            return Ok(p);
        }
        return Err(Error::Storage(format!(
            "$KB_EMBEDDER_BIN points at non-file: {}",
            p.display()
        )));
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("kb-embedder");
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join("kb-embedder");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(Error::Storage(
        "kb-embedder binary not found (set $KB_EMBEDDER_BIN, \
         install next to kb, or put it on PATH)"
            .into(),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_kind_tag() {
        let req = Request::Embed {
            req_id: 7,
            texts: vec!["a".into(), "b".into()],
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains(r#""kind":"embed""#));
        assert!(s.contains(r#""req_id":7"#));
    }

    #[test]
    fn shutdown_request_round_trips() {
        let req = Request::Shutdown;
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Request::Shutdown));
    }

    #[test]
    fn ready_response_round_trips() {
        let resp = Response::Ready {
            model: "bge-small-en-v1.5".into(),
            dim: 384,
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        match back {
            Response::Ready { model, dim } => {
                assert_eq!(model, "bge-small-en-v1.5");
                assert_eq!(dim, 384);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn rerank_request_serializes_with_kind_tag() {
        let req = Request::Rerank {
            req_id: 9,
            query: "q".into(),
            documents: vec!["a".into(), "b".into()],
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains(r#""kind":"rerank""#));
        let back: Request = serde_json::from_str(&s).unwrap();
        match back {
            Request::Rerank {
                req_id,
                query,
                documents,
            } => {
                assert_eq!(req_id, 9);
                assert_eq!(query, "q");
                assert_eq!(documents, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected Rerank, got {other:?}"),
        }
    }

    #[test]
    fn rerank_ok_response_round_trips() {
        let resp = Response::RerankOk {
            req_id: 9,
            scores: vec![0.9, 0.1],
        };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""kind":"rerank_ok""#));
        let back: Response = serde_json::from_str(&s).unwrap();
        match back {
            Response::RerankOk { req_id, scores } => {
                assert_eq!(req_id, 9);
                assert_eq!(scores, vec![0.9, 0.1]);
            }
            other => panic!("expected RerankOk, got {other:?}"),
        }
    }

    // The tests below mutate process-global env vars (`KB_EMBEDDER_BIN`,
    // `KB_FAKE_CNT`, `KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS`); this lock
    // serialises them so parallel execution can't race (one removing a
    // var while another reads it). `pub(crate)` (2026-08-21 ci-host hotfix) so
    // `indexer.rs`'s quarantine-gate test — which also spawns a fake
    // `KB_EMBEDDER_BIN` subprocess via `crate::embed::Embedder::spawn_ipc`
    // — serialises against the SAME lock instead of racing it with one of
    // its own (both mutate the identical env var name).
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn locate_embedder_bin_with_env_var_pointing_at_self() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let current = std::env::current_exe().unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &current);
        let got = locate_embedder_bin().unwrap();
        assert_eq!(got, current);
        std::env::remove_var("KB_EMBEDDER_BIN");
    }

    #[test]
    fn locate_embedder_bin_rejects_missing_env_var_target() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("KB_EMBEDDER_BIN", "/definitely/does/not/exist/kb-embedder");
        let err = locate_embedder_bin().unwrap_err();
        assert!(err.to_string().contains("KB_EMBEDDER_BIN"));
        std::env::remove_var("KB_EMBEDDER_BIN");
    }

    // A portable fake kb-embedder. It bumps a counter file on each launch:
    // the FIRST process speaks the Ready handshake then exits(1) on its first
    // Embed request (simulating an OOM/segfault crash); every later process
    // behaves normally (Ready, then echoes an EmbedOk per request). This lets
    // the test exercise the real respawn path with no ONNX dependency.
    #[cfg(unix)]
    const FAKE_EMBEDDER: &str = r#"#!/bin/sh
cnt="$KB_FAKE_CNT"
n=$(cat "$cnt" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$cnt"
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":3}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*)
      if [ "$n" -eq 1 ]; then exit 1; fi
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      printf '{"kind":"embed_ok","req_id":%s,"vectors":[[0.1,0.2,0.3]]}\n' "$rid"
      ;;
  esac
done
"#;

    #[cfg(unix)]
    #[test]
    fn embed_batch_respawns_after_subprocess_death() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let script = dir.path().join("fake-embedder.sh");
        std::fs::write(&script, FAKE_EMBEDDER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_FAKE_CNT", &counter);

        let mut be = IpcBackend::spawn("bge-small-en-v1.5", dir.path(), 19).unwrap();
        assert_eq!(be.respawn_count(), 0);

        // Process #1 crashes on this request; embed_batch must respawn process
        // #2 and retry, so the call still succeeds instead of wedging forever.
        let v = be.embed_one("hello").unwrap();
        assert_eq!(v, vec![0.1, 0.2, 0.3]);
        assert_eq!(be.respawn_count(), 1, "exactly one respawn after the crash");

        // The next call rides the live process #2 — no further respawn.
        let v2 = be.embed_one("again").unwrap();
        assert_eq!(v2, vec![0.1, 0.2, 0.3]);
        assert_eq!(be.respawn_count(), 1);

        std::env::remove_var("KB_EMBEDDER_BIN");
        std::env::remove_var("KB_FAKE_CNT");
    }

    // A fake embedder that records the size of every Embed request it
    // receives (one line per request, appended to $KB_REQ_LOG) and replies
    // with one vector per text, each carrying a strictly increasing counter
    // ($KB_FAKE_CTR persists it across requests) as its first component —
    // so a test can both assert every individual IPC request was
    // <= MAX_BATCH_SIZE texts AND that results concatenate back in the
    // original input order. Counts texts in the `"texts":[...]` array by
    // counting commas (+1) rather than parsing JSON — safe because the
    // test's fixture texts never contain a literal comma.
    #[cfg(unix)]
    const CHUNK_RECORDING_EMBEDDER: &str = r#"#!/bin/sh
reqlog="$KB_REQ_LOG"
ctr_file="$KB_FAKE_CTR"
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":3}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*)
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      texts_part=$(printf '%s' "$line" | sed -n 's/.*"texts":\[\(.*\)\]}.*/\1/p')
      if [ -z "$texts_part" ]; then
        count=0
      else
        commas=$(printf '%s' "$texts_part" | tr -cd ',' | wc -c)
        count=$((commas + 1))
      fi
      echo "$count" >> "$reqlog"
      ctr=$(cat "$ctr_file" 2>/dev/null || echo 0)
      vecs=""
      i=0
      while [ "$i" -lt "$count" ]; do
        ctr=$((ctr + 1))
        if [ -n "$vecs" ]; then vecs="$vecs,"; fi
        vecs="$vecs[$ctr.0,0.0,0.0]"
        i=$((i + 1))
      done
      echo "$ctr" > "$ctr_file"
      printf '{"kind":"embed_ok","req_id":%s,"vectors":[%s]}\n' "$rid" "$vecs"
      ;;
  esac
done
"#;

    /// Restructure regression test (2026-08-21 ci-host incident, defect 2): the
    /// Ipc arm of `Embedder::embed_batch` used to pass the whole slice
    /// through to the subprocess in ONE request, contradicting the
    /// `MAX_BATCH_SIZE` doc comment and letting an oversized batch reach
    /// the subprocess unchunked. Drives the fake subprocess through
    /// `crate::embed::Embedder` (not raw `IpcBackend`) so this exercises
    /// the actual chunking loop in `embed.rs`.
    #[cfg(unix)]
    #[test]
    fn embedder_embed_batch_chunks_ipc_requests_at_max_batch_size() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("chunk-recorder.sh");
        std::fs::write(&script, CHUNK_RECORDING_EMBEDDER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let reqlog = dir.path().join("reqlog");
        let ctr_file = dir.path().join("ctr");
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_REQ_LOG", &reqlog);
        std::env::set_var("KB_FAKE_CTR", &ctr_file);

        let mut emb =
            crate::embed::Embedder::spawn_ipc("bge-small-en-v1.5", dir.path().to_path_buf(), 19)
                .unwrap();

        let n = crate::embed::MAX_BATCH_SIZE * 2 + 6; // e.g. 70 when MAX_BATCH_SIZE=32
        let texts: Vec<String> = (0..n).map(|i| format!("text{i}")).collect();
        let out = emb.embed_batch(&texts).unwrap();

        assert_eq!(out.len(), n, "all texts embedded");
        // Order preserved: the fake subprocess's counter is strictly
        // increasing across requests, so output[i][0] must equal i+1.
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v[0], (i + 1) as f32, "vector {i} out of order");
        }

        let log = std::fs::read_to_string(&reqlog).unwrap();
        let sizes: Vec<usize> = log.lines().map(|l| l.parse().unwrap()).collect();
        assert!(
            sizes.iter().all(|&s| s <= crate::embed::MAX_BATCH_SIZE),
            "every IPC request must be <= MAX_BATCH_SIZE: {sizes:?}"
        );
        assert_eq!(
            sizes.iter().sum::<usize>(),
            n,
            "chunk sizes must sum to the total input"
        );
        assert!(
            sizes.len() > 1,
            "input larger than MAX_BATCH_SIZE must reach the subprocess as multiple requests"
        );

        std::env::remove_var("KB_EMBEDDER_BIN");
        std::env::remove_var("KB_REQ_LOG");
        std::env::remove_var("KB_FAKE_CTR");
    }

    // A fake embedder that *idle-dies*: process #1 speaks the Ready
    // handshake then exits(0) immediately (no read loop) — simulating an
    // OOM-kill between requests. Process #2+ behave normally. The request
    // path never runs, so only the proactive liveness probe can notice.
    #[cfg(unix)]
    const IDLE_DYING_EMBEDDER: &str = r#"#!/bin/sh
cnt="$KB_FAKE_CNT"
n=$(cat "$cnt" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$cnt"
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":3}\n'
if [ "$n" -eq 1 ]; then exit 0; fi
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*)
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      printf '{"kind":"embed_ok","req_id":%s,"vectors":[[0.1,0.2,0.3]]}\n' "$rid"
      ;;
  esac
done
"#;

    #[cfg(unix)]
    #[test]
    fn ensure_alive_detects_idle_death_and_respawns() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let script = dir.path().join("idle-embedder.sh");
        std::fs::write(&script, IDLE_DYING_EMBEDDER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_FAKE_CNT", &counter);

        let mut be = IpcBackend::spawn("bge-small-en-v1.5", dir.path(), 19).unwrap();
        assert_eq!(be.respawn_count(), 0);

        // Process #1 exits(0) right after the handshake. Give it a beat so
        // try_wait() observes the exit.
        std::thread::sleep(std::time::Duration::from_millis(250));

        // The proactive probe detects the idle death and brings up #2.
        assert!(be.ensure_alive(), "probe should recover a fresh subprocess");
        assert_eq!(
            be.respawn_count(),
            1,
            "exactly one respawn after idle death"
        );

        // #2 is live: a probe is a no-op, and embeds ride it.
        assert!(be.ensure_alive());
        assert_eq!(be.respawn_count(), 1, "no extra respawn while live");
        assert_eq!(be.embed_one("hi").unwrap(), vec![0.1, 0.2, 0.3]);
        assert_eq!(be.respawn_count(), 1);

        std::env::remove_var("KB_EMBEDDER_BIN");
        std::env::remove_var("KB_FAKE_CNT");
    }

    // A fake embedder that HANGS before the handshake — the
    // broken-onnxruntime futex_wait wedge. Pre-timeout, spawn() blocked
    // forever on read_line and the daemon hung with it. `exec` so the
    // sleep IS the child (kill closes the stdout pipe immediately —
    // no orphaned grandchild holding it open past the test).
    #[cfg(unix)]
    const HANGING_EMBEDDER: &str = "#!/bin/sh\nexec sleep 600\n";

    #[cfg(unix)]
    #[test]
    fn spawn_times_out_when_handshake_never_arrives() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hanging-embedder.sh");
        std::fs::write(&script, HANGING_EMBEDDER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS", "1");

        let started = std::time::Instant::now();
        let err = match IpcBackend::spawn("bge-small-en-v1.5", dir.path(), 19) {
            Ok(_) => panic!("spawn must time out against a hanging embedder"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("handshake timed out"),
            "got: {err}"
        );
        // Returned at the 1s bound (+ margin), not after the child's 600s sleep.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));

        std::env::remove_var("KB_EMBEDDER_HANDSHAKE_TIMEOUT_SECS");
        std::env::remove_var("KB_EMBEDDER_BIN");
    }

    // A fake embedder that HANGS mid-request: speaks the Ready handshake, then
    // reads each embed request and never replies (consumes stdin, writes
    // nothing) — the wedged-inference case (swap thrash / futex-waiting ONNX
    // op) that pre-timeout blocked `send_recv`'s read_line forever while
    // holding the shared embedder mutex. Every process behaves this way, so the
    // respawn-and-retry-once path also hits the wedge and gives up bounded.
    #[cfg(unix)]
    const HANGING_REQUEST_EMBEDDER: &str = r#"#!/bin/sh
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":3}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*) : ;;  # consume the request, deliberately never reply
  esac
done
"#;

    #[cfg(unix)]
    #[test]
    fn embed_batch_times_out_when_response_never_arrives() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hanging-request-embedder.sh");
        std::fs::write(&script, HANGING_REQUEST_EMBEDDER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_EMBED_TIMEOUT_SECS", "1");

        let mut be = IpcBackend::spawn("bge-small-en-v1.5", dir.path(), 19).unwrap();

        // The handshake succeeds; the first embed request wedges. Pre-fix this
        // call blocked forever; now read_line_timeout kills + respawns (once)
        // and the retry wedges too, so embed_batch returns an Err in bounded
        // time instead of hanging the caller (and the shared mutex).
        let started = std::time::Instant::now();
        let err = be.embed_one("hello").unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
        // Bounded: ~2 * 1s (initial + one respawn+retry) + margin, not forever.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "took {:?}",
            started.elapsed()
        );
        // Exactly one respawn: the retry-once path fired, then gave up (it did
        // not respawn-loop on the persistent wedge).
        assert_eq!(be.respawn_count(), 1);

        std::env::remove_var("KB_EMBED_TIMEOUT_SECS");
        std::env::remove_var("KB_EMBEDDER_BIN");
    }

    // A fake RERANKER subprocess: speaks the Ready handshake (dim 0, like a
    // real reranker), then process #1 exits(1) on its first Rerank request
    // (crash); #2+ echo a `rerank_ok` with one score per document, in input
    // order. Exercises the rerank transport + the respawn-and-retry path with
    // no ONNX dependency — the rerank twin of `FAKE_EMBEDDER`.
    #[cfg(unix)]
    const FAKE_RERANKER: &str = r#"#!/bin/sh
cnt="$KB_FAKE_CNT"
n=$(cat "$cnt" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$cnt"
printf '{"kind":"ready","model":"bge-reranker-base","dim":0}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"rerank"'*)
      if [ "$n" -eq 1 ]; then exit 1; fi
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      printf '{"kind":"rerank_ok","req_id":%s,"scores":[0.5,0.25]}\n' "$rid"
      ;;
  esac
done
"#;

    #[cfg(unix)]
    #[test]
    fn rerank_respawns_after_subprocess_death() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let script = dir.path().join("fake-reranker.sh");
        std::fs::write(&script, FAKE_RERANKER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        std::env::set_var("KB_FAKE_CNT", &counter);

        let mut be = IpcBackend::spawn_reranker("bge-reranker-base", dir.path(), 19).unwrap();
        assert_eq!(be.respawn_count(), 0);

        // Process #1 crashes on this request; `rerank` must respawn #2 and
        // retry, so the call still succeeds (scores returned in input order).
        let docs = vec!["a".to_string(), "b".to_string()];
        let scores = be.rerank("q", &docs).unwrap();
        assert_eq!(scores, vec![0.5, 0.25]);
        assert_eq!(be.respawn_count(), 1, "exactly one respawn after the crash");

        // The next call rides the live process #2 — no further respawn.
        let scores2 = be.rerank("q", &docs).unwrap();
        assert_eq!(scores2, vec![0.5, 0.25]);
        assert_eq!(be.respawn_count(), 1);

        std::env::remove_var("KB_EMBEDDER_BIN");
        std::env::remove_var("KB_FAKE_CNT");
    }
}
