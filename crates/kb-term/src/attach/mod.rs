//! `kb-term attach|new|ls`: the host-local mirror client.
//!
//! The mirror is a normal tap viewer (`<run-dir>/s/<id>/tap.sock`):
//! it writes the leading `ESC[3J` itself, then the twin SEED, then the
//! live stream from the exact offset the seed was taken at — the whole
//! session in native scrollback. Picture rows become dim one-row
//! labels from [`label::LabelRewriter`]. Runner control (list, launch,
//! take-size, the hidden-client keys path) goes through
//! [`crate::runner_client`]; the tap socket carries only the stream
//! (viewer-open + the `kb-term/1` frames).

pub mod fallback;
pub mod label;
mod tables;
pub mod tty;
pub mod width;

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::runner_client::{run_dir_of, tap_socket_path, RunnerClient, RunnerClientError};
use crate::wire::{ControlMessage, KeyInput, Request, ResumeInfo, ServerFrame, TextFrame};

use label::LabelRewriter;
use tty::{InputFilter, RawGuard, StdinDecision, RESTORE_BYTES};

// ---------------------------------------------------------------- CLI args

/// Arguments for `kb-term attach`.
#[derive(Debug, Clone)]
pub struct AttachArgs {
    /// Session id to attach to.
    pub id: String,
    /// Watch only: no take-size, no stdin.
    pub watch: bool,
    /// Runner socket path (flag/env override).
    pub runner_sock: Option<String>,
    /// Do not dim the picture labels.
    pub no_labels_dim: bool,
}

/// Arguments for `kb-term new`.
#[derive(Debug, Clone)]
pub struct NewArgs {
    /// Closed-set harness name.
    pub harness: String,
    /// Harness session id to resume, if any (passed through as
    /// `resume_sid`; the runner builds the argv — never a bare
    /// continue).
    pub resume_sid: Option<String>,
    /// Working directory (defaults to the current dir).
    pub cwd: Option<String>,
    /// Grid columns (`None` = the runner default).
    pub cols: Option<u16>,
    /// Grid rows (`None` = the runner default).
    pub rows: Option<u16>,
    /// Attach after launch (unless `--no-attach`).
    pub attach: bool,
    /// Runner socket path (flag/env override).
    pub runner_sock: Option<String>,
}

/// Arguments for `kb-term ls`.
#[derive(Debug, Clone)]
pub struct LsArgs {
    /// Print the raw RPC rows.
    pub json: bool,
    /// Runner socket path (flag/env override).
    pub runner_sock: Option<String>,
}

// ---------------------------------------------------------------- backoff

/// Exponential reconnect backoff: 100 ms → 5 s, with injected jitter
/// (the jitter source is a parameter, so tests are deterministic).
#[derive(Debug, Clone)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// Create a fresh backoff.
    #[must_use]
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// The delay before the next attempt, given a jitter in
    /// milliseconds. The TOTAL (jitter included) is capped at 5 s.
    /// Resets after a successful connect ([`Backoff::reset`]).
    pub fn next_delay(&mut self, jitter_ms: u64) -> Duration {
        let base_ms = 100u64.saturating_mul(1 << self.attempt.min(7));
        self.attempt += 1;
        let total_ms = base_ms.min(5000).saturating_add(jitter_ms % 1000);
        Duration::from_millis(total_ms.min(5000))
    }

    /// Reset after a successful connect.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Production jitter source: the wall clock mixed with the pid.
fn jitter_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) * 0x9E37_79B9_7F4A_7C15)
}

// ---------------------------------------------------------- stream state

/// The client-side stream state: the applied-offset counter and marker
/// bookkeeping. The contract is brief §2.5: on a full join the mirror
/// writes `ESC[3J`, then the seed, then live data from the seed's
/// offset. T7's seed relation (mirroring `tap/src/viewers.ts`
/// `joinNow`): the seed is captured at `X = ring.head`, the seed
/// chunks carry header `X`, and `viewer.sentOffset` starts at `X` —
/// the seed bytes are NOT stream bytes from a later offset, so the
/// client counts exactly `Seed.len` bytes as the seed (ignoring chunk
/// headers; both the all-at-`X` and the stepped styles work) and then
/// treats `X` as the live continuation offset.
#[derive(Debug, Clone, Default)]
struct StreamState {
    epoch: Option<String>,
    /// The next expected stream offset, once the seed is consumed.
    /// `None`: waiting for a seed.
    applied: Option<u64>,
    /// The highest marker seq applied (None = none yet).
    mseq: Option<u64>,
    /// Seed bytes still to consume (None = no seed pending).
    seed_remaining: Option<u64>,
    /// The seed's offset `X` (the live continuation offset).
    seed_x: Option<u64>,
}

impl StreamState {
    fn resume_info(&self) -> Option<ResumeInfo> {
        let (offset, epoch) = match (self.applied, &self.epoch) {
            (Some(o), Some(e)) => (o, e.clone()),
            _ => return None,
        };
        Some(ResumeInfo {
            offset,
            mseq: self.mseq.unwrap_or(0),
            epoch,
        })
    }
}

/// What to do with one live binary frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryDecision {
    /// The payload advances `applied`.
    Accept,
    /// Already seen (offset replayed by the tap's rewind): drop.
    Duplicate,
    /// Bytes were skipped: close and resume from `applied`.
    Gap,
    /// Waiting for a seed: this binary is seed bytes.
    PreSeed,
}

fn decide_binary(state: &StreamState, offset: u64) -> BinaryDecision {
    if state.seed_remaining.is_some() {
        return BinaryDecision::PreSeed;
    }
    match state.applied {
        None => BinaryDecision::PreSeed,
        Some(a) => {
            if offset < a {
                BinaryDecision::Duplicate
            } else if offset == a {
                BinaryDecision::Accept
            } else {
                BinaryDecision::Gap
            }
        }
    }
}

// --------------------------------------------------------------- run_attach

/// Everything the attach loop needs beyond the runner client.
struct AttachContext {
    args: AttachArgs,
    tap_path: PathBuf,
    viewer: String,
    /// The local pane size at start (for the Profile banner and takes).
    local_size: (u16, u16),
    dim: bool,
    runner: RunnerClient,
}

impl AttachContext {
    fn is_watch(&self) -> bool {
        self.args.watch
    }
}

pub async fn run_attach(args: AttachArgs) -> u8 {
    // Only the CLI flag is passed; env fallback lives in
    // `resolve_runner_socket` (an empty env var must not become an
    // empty path).
    let runner_sock = crate::runner_client::resolve_runner_socket(args.runner_sock.as_deref());
    let mut runner = match RunnerClient::connect(&runner_sock).await {
        Ok(c) => c,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    let sessions = match list_sessions(&mut runner).await {
        Ok(s) => s,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    if !sessions.iter().any(|r| row_id(r) == Some(args.id.as_str())) {
        eprintln!("kb-term: session {} not found", args.id);
        return 4;
    }
    let view_id = fallback::local_owner(std::process::id());
    let run_dir = run_dir_of(&runner_sock);
    let tap_path = tap_socket_path(&run_dir, &args.id);
    let local_size = RawGuard::size(0).unwrap_or((120, 40));
    let ctx = AttachContext {
        dim: !args.no_labels_dim,
        args,
        tap_path,
        viewer: view_id,
        local_size,
        runner,
    };
    attach_session(ctx).await
}

async fn list_sessions(
    runner: &mut RunnerClient,
) -> Result<Vec<serde_json::Value>, RunnerClientError> {
    let data = runner.call(&Request::List {}).await?;
    Ok(data.as_array().cloned().unwrap_or_else(|| vec![data]))
}

fn row_id(row: &serde_json::Value) -> Option<&str> {
    row.get("id").and_then(serde_json::Value::as_str)
}

/// Why a join attempt ended.
enum SessionResult {
    /// The session exited (exit code shown, exit 0).
    Normal(Option<i32>),
    /// The user detached / a signal arrived / stdin closed.
    Detached,
    /// The grid is owned by someone else (fallback path).
    GridOwned(String),
    /// The tap connection died (retry with resume + backoff).
    Error(&'static str),
}

fn print_session_ended(code: Option<i32>) {
    match code {
        Some(n) => println!("[session ended: code {n}]"),
        None => println!("[session ended]"),
    }
}

async fn attach_session(mut ctx: AttachContext) -> u8 {
    let watch = ctx.is_watch();
    let mut raw = if watch {
        None
    } else {
        Some(RawGuard::enter(0))
    };
    // One stdin pump + filter per attach (never per join: a leaked
    // pump from a dead join would keep stealing keystrokes).
    let mut stdin_rx = if watch { None } else { Some(spawn_stdin()) };
    let mut filter = InputFilter::new();
    // Open the viewer's hidden client once per attach: the real runner
    // rejects `keys` for a viewer that never opened (`no-viewer`).
    let _ = ctx
        .runner
        .call(&Request::ViewerOpen {
            id: ctx.args.id.clone(),
            viewer: ctx.viewer.clone(),
        })
        .await;
    let mut rewriter = LabelRewriter::new(ctx.local_size.0, ctx.dim);
    let mut state = StreamState::default();
    let mut backoff = Backoff::new();
    let mut banner_printed = false;
    let result = 'outer: loop {
        let resume = state.resume_info();
        match run_one_join(
            &mut ctx,
            &mut rewriter,
            &mut state,
            resume,
            &mut banner_printed,
            &mut backoff,
            stdin_rx.as_mut(),
            &mut filter,
        )
        .await
        {
            SessionResult::Normal(code) => {
                print_session_ended(code);
                break 'outer SessionResult::Normal(code);
            }
            SessionResult::Detached => {
                break 'outer SessionResult::Detached;
            }
            SessionResult::GridOwned(owner) => {
                // The in-join fallback already watched (and took the
                // size back on the first key): restart the attach
                // (steps 3-5: a fresh join with a new leading ED3).
                let _ = owner;
                state = StreamState::default();
                rewriter.reset();
                continue 'outer;
            }
            SessionResult::Error(msg) => {
                // Tap unreachable / reset / gap: backoff, then re-join
                // with ResumeFrom{applied}.
                eprintln!("kb-term: tap: {msg}; reconnecting");
                let d = backoff.next_delay(jitter_ms());
                tokio::time::sleep(d).await;
                continue 'outer;
            }
        }
    };
    let _ = ctx
        .runner
        .call(&Request::ViewerClose {
            id: ctx.args.id.clone(),
            viewer: ctx.viewer.clone(),
        })
        .await;
    if let Some(g) = raw.as_mut() {
        g.restore();
    }
    let mut stdout = tokio::io::stdout();
    let _ = stdout.write_all(RESTORE_BYTES).await;
    let _ = stdout.flush().await;
    match result {
        SessionResult::Normal(_) | SessionResult::Detached | SessionResult::GridOwned(_) => 0,
        SessionResult::Error(_) => 1,
    }
}

/// One join: connect to the tap, viewer-open, take-size (unless
/// watch), then pump frames until the session ends.
#[allow(clippy::too_many_arguments)]
async fn run_one_join(
    ctx: &mut AttachContext,
    rewriter: &mut LabelRewriter,
    state: &mut StreamState,
    resume: Option<ResumeInfo>,
    banner_printed: &mut bool,
    backoff: &mut Backoff,
    mut stdin_rx: Option<&mut tokio::sync::mpsc::Receiver<Vec<u8>>>,
    filter: &mut InputFilter,
) -> SessionResult {
    let mut conn = match connect_tap(&ctx.tap_path, &ctx.viewer, resume).await {
        Ok(c) => c,
        Err(_) => return SessionResult::Error("tap unreachable"),
    };
    // A successful connection resets the reconnect backoff.
    backoff.reset();
    // take-size BEFORE expecting the seed (runner socket, unless watch).
    let mut last_taken = ctx.local_size;
    // The grid-owned fallback: the banner prints once, then the join
    // keeps pumping the tap as a watcher, and the first stdin key
    // takes the size back (force) and restarts the attach.
    let mut grid_owned: Option<String> = None;
    if !ctx.is_watch() {
        let args_id = ctx.args.id.clone();
        let viewer = ctx.viewer.clone();
        match runner_takesize(&mut ctx.runner, &args_id, ctx.local_size, &viewer).await {
            Ok(()) => {}
            Err(RunnerClientError::Code { code, message }) if code == "grid-owned" => {
                let owner = message
                    .strip_prefix("grid is owned by ")
                    .unwrap_or(&message)
                    .to_string();
                eprintln!("{}", fallback::grid_banner(&owner));
                grid_owned = Some(owner);
            }
            Err(e) => {
                eprintln!("kb-term: take_size: {e}");
                return SessionResult::Error("take_size failed");
            }
        }
    }
    // Signals.
    let mut sig_int =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).ok();
    let mut sig_term =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let mut sig_winch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();
    let mut winch_deadline: Option<tokio::time::Instant> = None;
    // A pending bare Esc becomes a key once idle for 20 ms.
    let mut esc_deadline: Option<tokio::time::Instant> = None;
    let mut sink: Vec<u8> = Vec::new();
    loop {
        tokio::select! {
            result = conn.read_next_envelope() => {
                match result {
                    Ok(Some(env)) => {
                        sink.clear();
                        match handle_envelope(&env, ctx, rewriter, state, banner_printed, &mut sink) {
                            EnvelopeOutcome::Continue => {}
                            EnvelopeOutcome::Exited(code) => return SessionResult::Normal(code),
                            EnvelopeOutcome::DropAndResume => return SessionResult::Error("tap rejoin"),
                            EnvelopeOutcome::GapResume => return SessionResult::Error("tap gap"),
                        }
                        if !sink.is_empty() && write_out(&sink).await.is_err() {
                            return SessionResult::Detached;
                        }
                    }
                    Ok(None) => return SessionResult::Error("tap closed"),
                    Err(_) => return SessionResult::Error("tap read"),
                }
            }
            data = async { match stdin_rx.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending::<Option<Vec<u8>>>().await } } => {
                let Some(data) = data else {
                    return SessionResult::Detached;
                };
                match filter.decide(&data) {
                    StdinDecision::Detach => return SessionResult::Detached,
                    StdinDecision::Forward(bytes) => {
                        if filter.has_pending() {
                            esc_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(20));
                        }
                        if bytes.is_empty() {
                            continue;
                        }
                        if grid_owned.is_some() {
                            // Fallback take key: NOT forwarded; force the
                            // size back and restart the attach.
                            let size = RawGuard::size(0).unwrap_or(ctx.local_size);
                            let args_id = ctx.args.id.clone();
                            let viewer = ctx.viewer.clone();
                            match ctx.runner.call(&Request::TakeSize {
                                id: args_id,
                                cols: size.0,
                                rows: size.1,
                                owner: viewer,
                                force: true,
                            }).await {
                                Ok(_) => return SessionResult::GridOwned(grid_owned.unwrap_or_default()),
                                Err(_) => return SessionResult::Detached,
                            }
                        } else {
                            send_keys(&mut ctx.runner, &ctx.args.id, &ctx.viewer, &bytes).await;
                            let _ = send_typed(&mut conn, &ctx.viewer).await;
                        }
                    }
                }
            }
            _ = async { match winch_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            } }, if !ctx.is_watch() && grid_owned.is_none() => {
                winch_deadline = None;
                let size = RawGuard::size(0).unwrap_or(last_taken);
                if size != last_taken {
                    last_taken = size;
                    ctx.local_size = size;
                    let args_id = ctx.args.id.clone();
                    let viewer = ctx.viewer.clone();
                    let _ = runner_takesize(&mut ctx.runner, &args_id, size, &viewer).await;
                }
            }
            _ = async { match sig_int.as_mut() { Some(s) => s.recv().await, None => std::future::pending::<Option<()>>().await } } => {
                return SessionResult::Detached;
            }
            _ = async { match sig_term.as_mut() { Some(s) => s.recv().await, None => std::future::pending::<Option<()>>().await } } => {
                return SessionResult::Detached;
            }
            _ = async { match sig_winch.as_mut() { Some(s) => s.recv().await, None => std::future::pending::<Option<()>>().await } }, if !ctx.is_watch() && grid_owned.is_none() => {
                winch_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(100));
            }
            _ = async { match esc_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            } } => {
                esc_deadline = None;
                if filter.has_pending() {
                    esc_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(20));
                    if let Some(bytes) = filter.take_expired_esc(Duration::from_millis(20)) {
                        esc_deadline = None;
                        if grid_owned.is_some() {
                            let size = RawGuard::size(0).unwrap_or(ctx.local_size);
                            let args_id = ctx.args.id.clone();
                            let viewer = ctx.viewer.clone();
                            match ctx.runner.call(&Request::TakeSize {
                                id: args_id,
                                cols: size.0,
                                rows: size.1,
                                owner: viewer,
                                force: true,
                            }).await {
                                Ok(_) => return SessionResult::GridOwned(grid_owned.unwrap_or_default()),
                                Err(_) => return SessionResult::Detached,
                            }
                        } else {
                            send_keys(&mut ctx.runner, &ctx.args.id, &ctx.viewer, &bytes).await;
                            let _ = send_typed(&mut conn, &ctx.viewer).await;
                        }
                    }
                }
            }
        }
    }
}

/// Send key bytes to the viewer's hidden tmux client (4 KiB chunks).
async fn send_keys(runner: &mut RunnerClient, id: &str, viewer: &str, bytes: &[u8]) {
    use base64::Engine as _;
    for chunk in bytes.chunks(4096) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = runner
            .call(&Request::Keys {
                id: id.to_string(),
                viewer: viewer.to_string(),
                input: KeyInput::Bytes { bytes_b64: b64 },
            })
            .await;
    }
}

/// The `typed` notification (the tap only sends `Clipboard` to recent
/// typers).
async fn send_typed(conn: &mut TapConn, viewer: &str) -> Result<(), std::io::Error> {
    let body = serde_json::to_string(&ControlMessage::Typed {
        viewer: viewer.to_string(),
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let env = crate::wire::encode_envelope(crate::wire::ENV_CONTROL, body.as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    conn.stream.write_all(&env).await
}

enum EnvelopeOutcome {
    Continue,
    Exited(Option<i32>),
    /// An Error frame from the tap (busy / offset-ahead): rejoin.
    DropAndResume,
    /// A data gap: close and resume from `applied`.
    GapResume,
}

/// Narrow view of [`AttachContext`] for frame handling (tests can build
/// this without a runner socket).
struct FrameCtx<'a> {
    viewer: &'a str,
    is_watch: bool,
    local_size: (u16, u16),
}

impl<'a> From<&'a AttachContext> for FrameCtx<'a> {
    fn from(ctx: &'a AttachContext) -> Self {
        FrameCtx {
            viewer: ctx.viewer.as_str(),
            is_watch: ctx.is_watch(),
            local_size: ctx.local_size,
        }
    }
}

fn handle_envelope(
    env: &crate::wire::Envelope,
    ctx: &AttachContext,
    rewriter: &mut LabelRewriter,
    state: &mut StreamState,
    banner_printed: &mut bool,
    sink: &mut Vec<u8>,
) -> EnvelopeOutcome {
    let fctx = FrameCtx::from(ctx);
    handle_envelope_with(&fctx, env, rewriter, state, banner_printed, sink)
}

fn handle_envelope_with(
    fctx: &FrameCtx<'_>,
    env: &crate::wire::Envelope,
    rewriter: &mut LabelRewriter,
    state: &mut StreamState,
    banner_printed: &mut bool,
    sink: &mut Vec<u8>,
) -> EnvelopeOutcome {
    match env.kind {
        crate::wire::ENV_TEXT => {
            let (id, body) = match crate::wire::parse_s2c_body(&env.body) {
                Ok(v) => v,
                Err(_) => return EnvelopeOutcome::Continue,
            };
            if id != fctx.viewer {
                return EnvelopeOutcome::Continue;
            }
            let text = match std::str::from_utf8(body) {
                Ok(t) => t,
                Err(_) => return EnvelopeOutcome::Continue,
            };
            match crate::wire::decode_text_frame(text) {
                Ok(TextFrame::Server(frame)) => {
                    apply_server_frame(frame, fctx, rewriter, state, banner_printed, sink)
                }
                Ok(TextFrame::Unknown(_)) | Ok(_) | Err(_) => EnvelopeOutcome::Continue,
            }
        }
        crate::wire::ENV_BINARY => {
            let (id, body) = match crate::wire::parse_s2c_body(&env.body) {
                Ok(v) => v,
                Err(_) => return EnvelopeOutcome::Continue,
            };
            if id != fctx.viewer {
                return EnvelopeOutcome::Continue;
            }
            let (offset, payload) = match crate::wire::decode_binary(body) {
                Ok(v) => v,
                Err(_) => return EnvelopeOutcome::Continue,
            };
            match decide_binary(state, offset) {
                BinaryDecision::Duplicate => EnvelopeOutcome::Continue,
                BinaryDecision::Gap => EnvelopeOutcome::GapResume,
                BinaryDecision::Accept => {
                    state.applied = Some(offset + payload.len() as u64);
                    write_rewritten(rewriter, payload, sink);
                    EnvelopeOutcome::Continue
                }
                BinaryDecision::PreSeed => {
                    if state.seed_remaining.is_none() {
                        // A binary before any seed (the tap's seed never
                        // arrived): resume to let it rejoin.
                        return EnvelopeOutcome::GapResume;
                    }
                    let remaining = state.seed_remaining.unwrap_or(0);
                    let used = remaining.min(payload.len() as u64);
                    if used == 0 {
                        return EnvelopeOutcome::Continue;
                    }
                    write_rewritten(rewriter, &payload[..used as usize], sink);
                    if remaining - used == 0 {
                        state.applied = state.seed_x;
                        state.seed_remaining = None;
                    } else {
                        state.seed_remaining = Some(remaining - used);
                    }
                    EnvelopeOutcome::Continue
                }
            }
        }
        _ => EnvelopeOutcome::Continue,
    }
}

fn apply_server_frame(
    frame: ServerFrame,
    ctx: &FrameCtx<'_>,
    rewriter: &mut LabelRewriter,
    state: &mut StreamState,
    banner_printed: &mut bool,
    sink: &mut Vec<u8>,
) -> EnvelopeOutcome {
    match frame {
        ServerFrame::Hello { epoch, .. } => {
            if state.epoch.as_deref() != Some(epoch.as_str()) {
                state.epoch = Some(epoch);
                state.applied = None;
                state.mseq = None;
                rewriter.reset_stream();
            }
            EnvelopeOutcome::Continue
        }
        ServerFrame::Profile { cols, rows, .. } => {
            rewriter.set_cols(cols);
            if !ctx.is_watch && !*banner_printed {
                let (lc, lr) = ctx.local_size;
                if (cols, rows) != (lc, lr) {
                    eprintln!(
                        "kb-term: grid size mismatch: session {cols}x{rows}, local pane is {lc}x{lr} (attach takes the grid)"
                    );
                }
                *banner_printed = true;
            }
            EnvelopeOutcome::Continue
        }
        ServerFrame::Imagedef { seq, id, w, h, .. } => {
            bump_mseq(state, seq);
            rewriter.add_image(id, w, h);
            EnvelopeOutcome::Continue
        }
        ServerFrame::Seed { offset, len } => {
            state.seed_remaining = Some(len);
            state.seed_x = Some(offset);
            // ImageDefs arrived before this Seed: keep them
            // (`reset_stream` preserves defs) so the seed labels keep
            // their pixel dims.
            rewriter.reset_stream();
            sink.extend_from_slice(b"\x1b[3J");
            EnvelopeOutcome::Continue
        }
        ServerFrame::Resize { seq, cols, .. } => {
            bump_mseq(state, seq);
            rewriter.set_cols(cols);
            EnvelopeOutcome::Continue
        }
        ServerFrame::Sizechanged { seq, cols, .. } => {
            bump_mseq(state, seq);
            rewriter.set_cols(cols);
            EnvelopeOutcome::Continue
        }
        ServerFrame::Gfxreset { seq, .. } => {
            bump_mseq(state, seq);
            rewriter.clear_images();
            EnvelopeOutcome::Continue
        }
        ServerFrame::Exit { seq, code, .. } => {
            bump_mseq(state, seq);
            EnvelopeOutcome::Exited(code)
        }
        ServerFrame::Clipboard { data_b64, .. } => {
            if let Some(osc52) = clipboard_osc52(&data_b64) {
                sink.extend_from_slice(&osc52);
            }
            EnvelopeOutcome::Continue
        }
        ServerFrame::Error { .. } => EnvelopeOutcome::DropAndResume,
    }
}

/// Advance the applied marker seq (`ResumeFrom.mseq`).
fn bump_mseq(state: &mut StreamState, seq: u64) {
    state.mseq = Some(state.mseq.map_or(seq, |m| m.max(seq)));
}

fn write_rewritten(rewriter: &mut LabelRewriter, payload: &[u8], sink: &mut Vec<u8>) {
    rewriter.feed(payload, sink);
}

/// Write the mirror's output bytes. An `Err` here (EPIPE: the terminal
/// went away) means the caller must break the pump and restore the tty
/// (spec §2.7): stdout errors are never swallowed.
async fn write_out(bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut stdout = tokio::io::stdout();
    stdout.write_all(bytes).await?;
    stdout.flush().await
}

/// Build the local OSC 52 sequence for a tap `Clipboard` frame. The
/// `data_b64` is attacker-visible terminal input: only strict canonical
/// base64 is forwarded; anything else (control bytes, partial padding,
/// non-alphabet chars) is dropped rather than written to the operator's
/// terminal. `None` = drop.
fn clipboard_osc52(data_b64: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .ok()?;
    // Canonical form: re-encoding must reproduce the input exactly
    // (rejects partial padding and non-canonical trailing bits).
    let canonical = base64::engine::general_purpose::STANDARD.encode(&decoded);
    if canonical != data_b64 {
        return None;
    }
    Some(format!("\x1b]52;c;{data_b64}\x07").into_bytes())
}

async fn runner_takesize(
    runner: &mut RunnerClient,
    id: &str,
    size: (u16, u16),
    owner: &str,
) -> Result<(), RunnerClientError> {
    let data = runner
        .call(&Request::TakeSize {
            id: id.to_string(),
            cols: size.0,
            rows: size.1,
            owner: owner.to_string(),
            force: false,
        })
        .await?;
    let _ = data;
    Ok(())
}

/// The stdin pump: one detached OS thread feeding bytes over a channel.
/// (Never a tokio blocking-pool read: a parked blocking read on stdin
/// would pin the runtime's shutdown, and the mirror must exit exactly
/// when the session ends.)
fn spawn_stdin() -> tokio::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match std::io::Read::read(&mut stdin, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

// ----------------------------------------------------------------- tap io

/// One tap connection.
struct TapConn {
    stream: tokio::net::UnixStream,
    decoder: crate::wire::EnvelopeDecoder,
    queue: std::collections::VecDeque<crate::wire::Envelope>,
}

impl TapConn {
    async fn read_next_envelope(
        &mut self,
    ) -> Result<Option<crate::wire::Envelope>, std::io::Error> {
        if let Some(e) = self.queue.pop_front() {
            return Ok(Some(e));
        }
        let mut buf = vec![0u8; 262_144];
        let n = self.stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        for e in self
            .decoder
            .push(&buf[..n])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        {
            self.queue.push_back(e);
        }
        Ok(self.queue.pop_front())
    }
}

async fn connect_tap(
    path: &std::path::Path,
    viewer: &str,
    resume: Option<ResumeInfo>,
) -> Result<TapConn, std::io::Error> {
    let mut stream = tokio::net::UnixStream::connect(path).await?;
    // Serialize the typed control message (no hand-built JSON that
    // could drift from the wire type).
    let msg = ControlMessage::ViewerOpen {
        viewer: viewer.to_string(),
        profile: crate::wire::Profile::Desktop,
        resume,
    };
    let body = serde_json::to_string(&msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let env = crate::wire::encode_envelope(crate::wire::ENV_CONTROL, body.as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    stream.write_all(&env).await?;
    Ok(TapConn {
        stream,
        decoder: crate::wire::EnvelopeDecoder::new(),
        queue: std::collections::VecDeque::new(),
    })
}

// ------------------------------------------------------------------- new/ls

pub async fn run_ls(args: LsArgs) -> u8 {
    let runner_sock = crate::runner_client::resolve_runner_socket(args.runner_sock.as_deref());
    let mut runner = match RunnerClient::connect(&runner_sock).await {
        Ok(c) => c,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    let rows = match list_sessions(&mut runner).await {
        Ok(r) => r,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    for line in render_ls_rows(&rows) {
        println!("{line}");
    }
    0
}

/// One rendered `ls` row.
fn render_ls_row(row: &serde_json::Value, out: &mut Vec<String>) {
    let id = row
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let harness = row
        .get("harness")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let size = match (
        row.get("cols").and_then(serde_json::Value::as_u64),
        row.get("rows").and_then(serde_json::Value::as_u64),
    ) {
        (Some(c), Some(r)) => format!("{c}x{r}"),
        _ => "-".to_string(),
    };
    let stale = row
        .get("stale")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let pane_dead = row
        .get("pane_dead")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let state = if stale {
        "stale".to_string()
    } else if pane_dead {
        match row.get("exit_code").and_then(serde_json::Value::as_i64) {
            Some(c) => format!("exited {c}"),
            None => "exited".to_string(),
        }
    } else {
        "running".to_string()
    };
    let owner = row.get("grid_owner").and_then(serde_json::Value::as_str);
    // Only this process's own mirror owner counts as `self`; another
    // `local:<pid>` (a different local attach) is foreign.
    let self_owner = fallback::local_owner(std::process::id());
    let grid = match owner {
        None => "none".to_string(),
        Some(o) if o == self_owner => "self".to_string(),
        Some(_) => "other".to_string(),
    };
    let bound = row
        .get("resume_sid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    out.push(format!("{id} {harness} {size} {state} {grid} {bound}"));
}

fn render_ls_rows(rows: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rows {
        render_ls_row(r, &mut out);
    }
    out
}

pub async fn run_new(args: NewArgs) -> u8 {
    let runner_sock = crate::runner_client::resolve_runner_socket(args.runner_sock.as_deref());
    let mut runner = match RunnerClient::connect(&runner_sock).await {
        Ok(c) => c,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    let harness = match crate::wire::Harness::ALL
        .iter()
        .find(|h| h.as_str() == args.harness)
    {
        Some(h) => *h,
        None => {
            eprintln!("kb-term: unknown harness {}", args.harness);
            return 2;
        }
    };
    // cwd: absolute; default the current directory.
    let cwd = match &args.cwd {
        Some(c) => {
            let p = std::path::Path::new(c);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("/"))
                    .join(p)
            }
        }
        None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")),
    };
    if !cwd.is_dir() {
        // A bad `--cwd` is a usage failure (exit 2), not an internal
        // error (1) or a runner failure (3).
        eprintln!(
            "kb-term: cwd {} is not an existing directory",
            cwd.display()
        );
        return 2;
    }
    for (v, name) in [(args.cols, "cols"), (args.rows, "rows")] {
        if let Some(n) = v {
            let (lo, hi) = if name == "cols" { (20, 500) } else { (5, 200) };
            if n < lo || n > hi {
                eprintln!("kb-term: {name} must be {lo}..={hi}");
                return 2;
            }
        }
    }
    let data = match runner
        .call(&Request::Launch {
            harness,
            argv: Some(vec![format!("/usr/local/bin/{}", harness.as_str())]),
            resume_sid: args.resume_sid.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
            cols: args.cols,
            rows: args.rows,
            env: None,
            exit_keys: None,
        })
        .await
    {
        Ok(d) => d,
        Err(e @ RunnerClientError::Unreachable(_)) => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kb-termd user unit may not be running (socket: {})",
                runner_sock.display()
            );
            return 3;
        }
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    let id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?")
        .to_string();
    if args.attach {
        drop(runner);
        let a = AttachArgs {
            id,
            watch: false,
            runner_sock: args.runner_sock.clone(),
            no_labels_dim: false,
        };
        run_attach(a).await
    } else {
        println!("{id}");
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_with_cap_and_resets_on_success() {
        let mut b = Backoff::new();
        // Zero jitter: 100, 200, 400, 800, 1600, 3200, then 5 s cap.
        let seq: Vec<u64> = (0..8).map(|_| b.next_delay(0).as_millis() as u64).collect();
        assert_eq!(seq, vec![100, 200, 400, 800, 1600, 3200, 5000, 5000]);
        // Injected jitter shifts the base but the cap still holds
        // (the total, jitter included, never exceeds 5 s).
        let mut b = Backoff::new();
        assert_eq!(b.next_delay(50).as_millis(), 150);
        for _ in 0..8 {
            b.next_delay(50);
        }
        assert_eq!(b.next_delay(50).as_millis(), 5000, "total capped at 5 s");
        // Reset restarts at 100 ms.
        b.reset();
        assert_eq!(b.next_delay(0).as_millis(), 100);
    }

    fn state_after_seed(x: u64) -> StreamState {
        StreamState {
            epoch: Some("00000001".into()),
            applied: Some(x),
            mseq: Some(2),
            seed_remaining: None,
            seed_x: Some(x),
        }
    }

    #[test]
    fn dedupe_and_gap_logic() {
        // Duplicate (< applied) dropped; exact accepted; ahead = gap.
        let s = state_after_seed(100);
        assert_eq!(decide_binary(&s, 99), BinaryDecision::Duplicate);
        assert_eq!(decide_binary(&s, 50), BinaryDecision::Duplicate);
        assert_eq!(decide_binary(&s, 100), BinaryDecision::Accept);
        assert_eq!(decide_binary(&s, 101), BinaryDecision::Gap);
        assert_eq!(decide_binary(&s, 200), BinaryDecision::Gap);

        // Before the seed, everything is PreSeed.
        let s = StreamState::default();
        assert_eq!(decide_binary(&s, 0), BinaryDecision::PreSeed);
        assert_eq!(decide_binary(&s, 1000), BinaryDecision::PreSeed);
    }

    #[test]
    fn resume_info_tracks_applied_and_mseq() {
        let s = state_after_seed(4242);
        let info = StreamState::resume_info(&s).expect("has applied+epoch");
        assert_eq!(info.offset, 4242);
        assert_eq!(info.mseq, 2);
        assert_eq!(info.epoch, "00000001");
        assert!(StreamState::resume_info(&StreamState::default()).is_none());
        // Applied without epoch -> no resume.
        let s = StreamState {
            applied: Some(9),
            ..StreamState::default()
        };
        assert!(StreamState::resume_info(&s).is_none());
    }
    #[test]
    fn ls_rows_render() {
        let self_owner = fallback::local_owner(std::process::id());
        let foreign_local = fallback::local_owner(std::process::id().wrapping_add(1).max(9999));
        assert_ne!(self_owner, foreign_local);
        let rows = vec![
            serde_json::json!({
                "id": "abcd1234", "harness": "omp", "cols": 201, "rows": 48,
                "pane_dead": false, "stale": false, "grid_owner": self_owner,
                "resume_sid": "deadbeefcafe", "exit_code": null
            }),
            serde_json::json!({
                "id": "efgh5678", "harness": "claude", "cols": 80, "rows": 24,
                "pane_dead": true, "stale": false, "grid_owner": foreign_local,
                "resume_sid": null, "exit_code": 3
            }),
            serde_json::json!({
                "id": "ijkl9012", "harness": null, "cols": null, "rows": null,
                "pane_dead": false, "stale": true, "grid_owner": null,
                "resume_sid": null, "exit_code": null
            }),
        ];
        assert_eq!(
            render_ls_rows(&rows),
            vec![
                "abcd1234 omp 201x48 running self deadbeefcafe",
                "efgh5678 claude 80x24 exited 3 other -",
                "ijkl9012 ? - stale none -",
            ]
        );
    }

    fn fctx() -> FrameCtx<'static> {
        FrameCtx {
            viewer: "v",
            is_watch: false,
            local_size: (201, 48),
        }
    }

    fn hello(epoch: &str) -> ServerFrame {
        ServerFrame::Hello {
            proto: crate::wire::PROTO.into(),
            session: "s".into(),
            epoch: epoch.into(),
            harness: crate::wire::Harness::Omp,
            bound_sid: None,
        }
    }

    #[test]
    fn epoch_change_restarts_with_fresh_ed3() {
        let mut rw = LabelRewriter::new(201, true);
        let mut state = StreamState::default();
        let mut banner = false;
        let mut sink: Vec<u8> = Vec::new();
        // Epoch 1: seed emits the leading ED3.
        apply_server_frame(
            hello("00000001"),
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        apply_server_frame(
            ServerFrame::Seed { offset: 0, len: 5 },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert_eq!(sink, b"\x1b[3J");
        // Epoch 2: the next Seed emits a fresh leading ED3.
        sink.clear();
        apply_server_frame(
            hello("00000002"),
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert_eq!(state.applied, None, "new epoch clears applied");
        apply_server_frame(
            ServerFrame::Seed { offset: 0, len: 5 },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert_eq!(sink, b"\x1b[3J", "fresh ED3 after epoch change");
        // Same epoch again: the Seed still leads with ED3 (a rejoin
        // always restarts at the seed step).
        sink.clear();
        apply_server_frame(
            hello("00000002"),
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        apply_server_frame(
            ServerFrame::Seed { offset: 0, len: 5 },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert_eq!(sink, b"\x1b[3J");
    }

    #[test]
    fn clipboard_validates_strict_base64() {
        // Canonical base64 is forwarded as OSC 52.
        assert_eq!(
            clipboard_osc52("aGVsbG8=").expect("valid"),
            b"\x1b]52;c;aGVsbG8=\x07",
            "valid clipboard forwards"
        );
        // Control bytes / invalid chars / partial padding / whitespace
        // are dropped (never reach the operator terminal).
        assert_eq!(
            clipboard_osc52("\x1b]52;c;evil\x07"),
            None,
            "ESC/BEL dropped"
        );
        assert_eq!(clipboard_osc52("aGVsbG8"), None, "partial padding dropped");
        assert_eq!(clipboard_osc52("aGVs bG8="), None, "whitespace dropped");
        assert_eq!(clipboard_osc52("****"), None, "non-alphabet dropped");
    }

    #[test]
    fn clipboard_frame_drops_invalid_base64() {
        let mut rw = LabelRewriter::new(201, true);
        let mut state = StreamState::default();
        let mut banner = false;
        let mut sink: Vec<u8> = Vec::new();
        apply_server_frame(
            ServerFrame::Clipboard {
                offset: 0,
                data_b64: "\x1b]52;evil\x07".into(),
            },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert!(sink.is_empty(), "invalid clipboard emits nothing");
        apply_server_frame(
            ServerFrame::Clipboard {
                offset: 0,
                data_b64: "aGVsbG8=".into(),
            },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        assert_eq!(sink, b"\x1b]52;c;aGVsbG8=\x07");
    }
}
