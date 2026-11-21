//! `kb-term attach|new|ls|kill`: the host-local mirror client.
//!
//! The mirror is a normal tap viewer (`<run-dir>/s/<id>/tap.sock`):
//! it clears the visible screen and the scrollback itself ([`JOIN_CLEAR`]),
//! then writes the twin SEED, then the live stream from the exact offset
//! the seed was taken at — the whole session in native scrollback. Picture rows become dim one-row
//! labels from [`label::LabelRewriter`]. Runner control (list, launch,
//! take-size, the hidden-client keys path) goes through
//! [`crate::runner_client`]; the tap socket carries only the stream
//! (viewer-open + the `kb-term/1` frames).

pub mod fallback;
pub mod graphics;
pub mod label;
pub mod status;
mod tables;
#[cfg(test)]
mod testscreen;
pub mod tty;
pub mod width;

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::runner_client::{run_dir_of, tap_socket_path, RunnerClient, RunnerClientError};
use crate::wire::{ControlMessage, KeyInput, Request, ResumeInfo, ServerFrame, TextFrame};

use label::LabelRewriter;
use status::StatusLine;
use tty::{InputFilter, RawGuard, StdinDecision, RESTORE_BYTES};

/// What every join writes before its seed. The seed paints rows from the
/// top without erasing, so whatever the outer terminal showed (the shell
/// line that started the attach, a previous join's rows, a notice) would
/// survive under a shorter row. SGR is reset first so the erase fills with
/// the default background; the scroll region and origin mode are reset so
/// `H` means the screen's top-left; `2J` erases the visible screen and `3J`
/// the scrollback (the mirror owns the scrollback, see the module doc).
pub const JOIN_CLEAR: &[u8] = b"\x1b[0m\x1b[?6l\x1b[r\x1b[H\x1b[2J\x1b[3J";

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
    nanos ^ u64::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

// ---------------------------------------------------------- stream state

/// The client-side stream state: the applied-offset counter and marker
/// bookkeeping. The contract is brief §2.5: on a full join the mirror
/// writes [`JOIN_CLEAR`] (reset attributes, scroll region and origin mode,
/// home, erase the screen, erase the scrollback), then the seed, then live
/// data from the seed's offset. T7's seed relation (mirroring
/// `tap/src/viewers.ts` `joinNow`): the seed is captured at `X = ring.head`, the seed
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
    /// The size of the newest `resize`/`sizechanged` marker not yet looked
    /// at by the pump (an owning attach checks it against its last take).
    size_marker: Option<(u16, u16)>,
    /// The grid the screen was painted for (the join's `profile`).
    grid: Option<(u16, u16)>,
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
    /// How the next join starts after a repaint (see [`Preset`]).
    preset: Option<Preset>,
    /// The grid of the last repaint this attach asked for. A repaint for
    /// the same grid twice in a row is never repeated (a loop guard: a real
    /// tap's new profile already carries the new grid).
    last_repaint: Option<(u16, u16)>,
}

/// How a repaint join starts: the grid ownership is already known, so the
/// join neither takes the size again nor asks who owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Preset {
    /// This attach owns the grid (its own take just changed the size).
    Own,
    /// Someone else owns the grid: watch behind the status line.
    Watching(String),
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
        preset: None,
        last_repaint: None,
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
    /// The grid changed under the painted screen: re-join with a fresh
    /// seed (which clears the screen first) so the mirror equals the
    /// session again.
    Repaint(Preset),
    /// The tap connection died (retry with resume + backoff).
    Error(&'static str),
    /// The hidden-client keys path rejected key bytes (e.g. `no-viewer`
    /// after a runner restart). Continuing would run inputless, so the
    /// attach fails instead.
    KeysFailed,
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
    // rejects `keys` for a viewer that never opened (`no-viewer`), so
    // a failed open must fail the attach instead of running inputless.
    if let Err(e) = ctx
        .runner
        .call(&Request::ViewerOpen {
            id: ctx.args.id.clone(),
            viewer: ctx.viewer.clone(),
        })
        .await
    {
        eprintln!("kb-term: viewer_open: {e}");
        if let Some(g) = raw.as_mut() {
            g.restore();
        }
        let mut stdout = tokio::io::stdout();
        let _ = stdout.write_all(RESTORE_BYTES).await;
        let _ = stdout.flush().await;
        return if e.code() == Some("invalid-id") { 4 } else { 1 };
    }
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
                banner_printed = false;
                continue 'outer;
            }
            SessionResult::Repaint(preset) => {
                state = StreamState::default();
                rewriter.reset();
                ctx.preset = Some(preset);
                continue 'outer;
            }
            SessionResult::KeysFailed => {
                break 'outer SessionResult::KeysFailed;
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
    // Flush the rewriter's held tail (a trigger-bearing row with no
    // trailing LF, and any partial-trigger prefix) so it still reaches
    // the scrollback before the tty is restored.
    let mut tail = Vec::new();
    rewriter.flush(&mut tail);
    if !tail.is_empty() {
        let _ = write_out(&tail).await;
    }
    if let Some(g) = raw.as_mut() {
        g.restore();
    }
    let mut stdout = tokio::io::stdout();
    let _ = stdout.write_all(RESTORE_BYTES).await;
    let _ = stdout.flush().await;
    match result {
        SessionResult::Normal(_)
        | SessionResult::Detached
        | SessionResult::GridOwned(_)
        | SessionResult::Repaint(_) => 0,
        SessionResult::Error(_) | SessionResult::KeysFailed => 1,
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
    stdin_rx: Option<&mut tokio::sync::mpsc::Receiver<Vec<u8>>>,
    filter: &mut InputFilter,
) -> SessionResult {
    let mut status = StatusLine::default();
    let result = run_one_join_inner(
        ctx,
        rewriter,
        state,
        resume,
        banner_printed,
        backoff,
        stdin_rx,
        filter,
        &mut status,
    )
    .await;
    // However the join ends, the notice row is erased: it was never part
    // of the mirror, and a detach must hand the terminal back clean.
    if status.text().is_some() {
        let _ = write_out(&status::clear_bytes(outer_size(ctx))).await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_one_join_inner(
    ctx: &mut AttachContext,
    rewriter: &mut LabelRewriter,
    state: &mut StreamState,
    resume: Option<ResumeInfo>,
    banner_printed: &mut bool,
    backoff: &mut Backoff,
    mut stdin_rx: Option<&mut tokio::sync::mpsc::Receiver<Vec<u8>>>,
    filter: &mut InputFilter,
    status: &mut StatusLine,
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
    // The one-row notice on the outer terminal's last row (never the
    // mirror's cursor: that would overwrite mirrored rows).
    let preset = ctx.preset.take();
    if let Some(Preset::Watching(owner)) = &preset {
        status.set(Some(fallback::grid_status(Some(owner))));
        grid_owned = Some(owner.clone());
    }
    if !ctx.is_watch() && preset.is_none() {
        let args_id = ctx.args.id.clone();
        let viewer = ctx.viewer.clone();
        match runner_takesize(&mut ctx.runner, &args_id, ctx.local_size, &viewer).await {
            Ok(()) => {}
            Err(RunnerClientError::Code { code, message }) if code == "grid-owned" => {
                let owner = fallback::owner_from_message(&message);
                status.set(Some(fallback::grid_status(Some(&owner))));
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
    // While watching under another owner, ask the runner now and then so
    // the status line can say "press any key" once the web releases.
    let mut owner_poll = tokio::time::interval(Duration::from_secs(2));
    owner_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                        if !sink.is_empty() {
                            // The stream's bytes, then (when the stream stopped
                            // at ground) the status line again: the stream may
                            // have scrolled or overwritten the last row.
                            let repaint = status.after_write(&sink, outer_size(ctx));
                            sink.extend_from_slice(&repaint);
                            if write_out(&sink).await.is_err() {
                                return SessionResult::Detached;
                            }
                        }
                        // A grid marker: the screen was painted for another grid
                        // unless it equals the profile's. Re-join (a fresh seed
                        // clears the screen first) so the mirror equals the
                        // session. An owning attach that did not take this size
                        // asks who owns the grid now (the web outranks a local
                        // attach): if someone else does, it re-joins as a watch
                        // behind the status line and never retakes on its own.
                        if let Some(marker) = state.size_marker.take() {
                            let painted = state.grid;
                            let repeated = ctx.last_repaint == Some(marker);
                            if !ctx.is_watch() && !repeated {
                                if let Some(owner) = grid_owned.clone() {
                                    if painted.is_some_and(|g| g != marker) {
                                        ctx.last_repaint = Some(marker);
                                        return SessionResult::Repaint(Preset::Watching(owner));
                                    }
                                } else if marker == last_taken {
                                    if painted.is_some_and(|g| g != marker) {
                                        ctx.last_repaint = Some(marker);
                                        return SessionResult::Repaint(Preset::Own);
                                    }
                                } else if let Some(owner) = lost_grid_owner(ctx).await {
                                    ctx.last_repaint = Some(marker);
                                    return SessionResult::Repaint(Preset::Watching(owner));
                                }
                            }
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
                        // Only a bare ESC can ever idle-flush; arming on
                        // a partial CSI would wake the pump forever.
                        if filter.pending_is_bare_esc() {
                            esc_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(20));
                        }
                        if bytes.is_empty() {
                            continue;
                        }
                        if grid_owned.is_some() {
                            // Fallback take key: NOT forwarded; force the
                            // size back and restart the attach.
                            let owner = grid_owned.clone().unwrap_or_default();
                            match fallback_take_key(ctx, &owner).await {
                                FallbackKey::Taken => return SessionResult::GridOwned(owner),
                                // The web owns the grid: the key is dropped,
                                // nothing is taken, the attach keeps watching.
                                FallbackKey::Held(now_owner) => {
                                    status.set(Some(fallback::grid_status(Some(&now_owner))));
                                    if write_out(&status.paint_now(outer_size(ctx))).await.is_err() {
                                        return SessionResult::Detached;
                                    }
                                    grid_owned = Some(now_owner);
                                }
                                FallbackKey::Failed => return SessionResult::Detached,
                            }
                        } else if let Err(e) =
                            send_keys(&mut ctx.runner, &ctx.args.id, &ctx.viewer, &bytes).await
                        {
                            eprintln!("kb-term: keys: {e}");
                            return SessionResult::KeysFailed;
                        } else {
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
                    // A refused window-size take means someone else (the web)
                    // took the grid meanwhile: watch behind the banner instead
                    // of fighting or failing silently.
                    if let Err(RunnerClientError::Code { code, message }) =
                        runner_takesize(&mut ctx.runner, &args_id, size, &viewer).await
                    {
                        if code == "grid-owned" {
                            let owner = fallback::owner_from_message(&message);
                            status.set(Some(fallback::grid_status(Some(&owner))));
                            if write_out(&status.paint_now(outer_size(ctx))).await.is_err() {
                                return SessionResult::Detached;
                            }
                            grid_owned = Some(owner);
                        }
                    }
                }
            }
            _ = owner_poll.tick(), if grid_owned.is_some() => {
                // Who owns the grid now? (None = released.) Only the text
                // changes; a change of owner repaints the status line.
                if let Some(cur) = current_owner(ctx).await.filter(|c| c.as_deref() != Some(ctx.viewer.as_str())) {
                    let text = fallback::grid_status(cur.as_deref());
                    if status.text() != Some(text.as_str()) {
                        status.set(Some(text));
                        if write_out(&status.paint_now(outer_size(ctx))).await.is_err() {
                            return SessionResult::Detached;
                        }
                    }
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
                // Only a bare ESC can idle-flush; a dangling partial CSI
                // waits for more bytes (arming again would wake forever).
                if filter.pending_is_bare_esc() {
                    if let Some(bytes) = filter.take_expired_esc(Duration::from_millis(20)) {
                        if grid_owned.is_some() {
                            let owner = grid_owned.clone().unwrap_or_default();
                            match fallback_take_key(ctx, &owner).await {
                                FallbackKey::Taken => return SessionResult::GridOwned(owner),
                                // The web owns the grid: the key is dropped,
                                // nothing is taken, the attach keeps watching.
                                FallbackKey::Held(now_owner) => {
                                    status.set(Some(fallback::grid_status(Some(&now_owner))));
                                    if write_out(&status.paint_now(outer_size(ctx))).await.is_err() {
                                        return SessionResult::Detached;
                                    }
                                    grid_owned = Some(now_owner);
                                }
                                FallbackKey::Failed => return SessionResult::Detached,
                            }
                        } else if let Err(e) =
                            send_keys(&mut ctx.runner, &ctx.args.id, &ctx.viewer, &bytes).await
                        {
                            eprintln!("kb-term: keys: {e}");
                            return SessionResult::KeysFailed;
                        } else {
                            let _ = send_typed(&mut conn, &ctx.viewer).await;
                        }
                    } else {
                        // Not yet idle long enough: re-arm once.
                        esc_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(20));
                    }
                }
            }
        }
    }
}

/// What a keypress in the grid-owned fallback did.
enum FallbackKey {
    /// The grid was taken back: restart the join.
    Taken,
    /// Someone the attach must not fight (the web) owns the grid: the key
    /// was dropped; keep watching. Carries the current owner.
    Held(String),
    /// The runner call failed: detach.
    Failed,
}

/// The fallback's take key. Asks the runner who owns the grid FIRST: a web
/// owner is never taken from (one rate-limited notice, nothing forwarded);
/// with the web gone (owner none) or another local owner, the key takes the
/// size back exactly as before. The runner enforces the same precedence, so
/// a race with a web take is refused there with `grid-owned`.
async fn fallback_take_key(ctx: &mut AttachContext, remembered_owner: &str) -> FallbackKey {
    let id = ctx.args.id.clone();
    let mut owner = remembered_owner.to_string();
    if let Ok(data) = ctx.runner.call(&Request::Grid { id: id.clone() }).await {
        owner = data
            .get("owner")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if fallback::is_web_owner(&owner) {
        return FallbackKey::Held(owner);
    }
    let size = RawGuard::size(0).unwrap_or(ctx.local_size);
    match ctx
        .runner
        .call(&Request::TakeSize {
            id,
            cols: size.0,
            rows: size.1,
            owner: ctx.viewer.clone(),
            force: true,
        })
        .await
    {
        Ok(_) => {
            ctx.local_size = size;
            FallbackKey::Taken
        }
        Err(RunnerClientError::Code { code, message }) if code == "grid-owned" => {
            FallbackKey::Held(fallback::owner_from_message(&message))
        }
        Err(_) => FallbackKey::Failed,
    }
}

/// The outer terminal's current size (the status line goes on its last
/// row): the live tty size, else the size at start.
fn outer_size(ctx: &AttachContext) -> (u16, u16) {
    RawGuard::size(0).unwrap_or(ctx.local_size)
}

/// The grid's current owner as the runner reports it: `Some(None)` = no
/// owner, `Some(Some(o))` = owned by `o`, `None` = the runner could not say.
async fn current_owner(ctx: &mut AttachContext) -> Option<Option<String>> {
    let data = ctx
        .runner
        .call(&Request::Grid {
            id: ctx.args.id.clone(),
        })
        .await
        .ok()?;
    Some(
        data.get("owner")
            .and_then(|v| v.as_str())
            .filter(|o| !o.is_empty())
            .map(str::to_string),
    )
}

/// An owning attach saw a grid marker for a size it did not take:
/// returns the new owner when the grid is now someone else's, `None` when
/// this attach still owns it (or the runner could not say).
async fn lost_grid_owner(ctx: &mut AttachContext) -> Option<String> {
    let data = ctx
        .runner
        .call(&Request::Grid {
            id: ctx.args.id.clone(),
        })
        .await
        .ok()?;
    let owner = data.get("owner").and_then(|v| v.as_str())?;
    if owner == ctx.viewer {
        None
    } else {
        Some(owner.to_string())
    }
}

/// Send key bytes to the viewer's hidden tmux client (4 KiB chunks).
/// The first chunk the runner rejects fails the send (the caller
/// surfaces it and stops instead of running inputless).
async fn send_keys(
    runner: &mut RunnerClient,
    id: &str,
    viewer: &str,
    bytes: &[u8],
) -> Result<(), RunnerClientError> {
    use base64::Engine as _;
    for chunk in bytes.chunks(4096) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        runner
            .call(&Request::Keys {
                id: id.to_string(),
                viewer: viewer.to_string(),
                input: KeyInput::Bytes { bytes_b64: b64 },
            })
            .await?;
    }
    Ok(())
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
            state.grid = Some((cols, rows));
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
            sink.extend_from_slice(JOIN_CLEAR);
            EnvelopeOutcome::Continue
        }
        ServerFrame::Resize {
            seq, cols, rows, ..
        } => {
            bump_mseq(state, seq);
            state.size_marker = Some((cols, rows));
            rewriter.set_cols(cols);
            EnvelopeOutcome::Continue
        }
        ServerFrame::Sizechanged {
            seq, cols, rows, ..
        } => {
            bump_mseq(state, seq);
            state.size_marker = Some((cols, rows));
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

// ------------------------------------------------------------ new/ls/kill

/// Arguments for `kb-term kill`.
#[derive(Debug, Clone)]
pub struct KillArgs {
    /// Session id.
    pub id: String,
    /// Kill grace in seconds (the runner clamps it; `None` = its default).
    pub grace_secs: Option<u64>,
    /// Runner socket path (flag/env override).
    pub runner_sock: Option<String>,
}

/// The one line `kb-term kill` prints for a successful runner reply.
fn render_kill_reply(id: &str, reply: &serde_json::Value) -> String {
    if reply
        .get("stale_removed")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return format!("removed stale session {id} (no holder answered)");
    }
    let forced = reply
        .get("forced")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let code = reply.get("exit_code").and_then(serde_json::Value::as_i64);
    match (forced, code) {
        (true, _) => format!("killed {id} (forced)"),
        (false, Some(c)) => format!("killed {id} (exit {c})"),
        (false, None) => format!("killed {id}"),
    }
}

/// The exit code for a runner error on `kill`: 4 not found (same as
/// `attach`), 2 an id the runner refuses as malformed, 1 anything else.
fn kill_error_exit(code: Option<&str>) -> u8 {
    match code {
        Some("not-found") => 4,
        Some("invalid-id") => 2,
        _ => 1,
    }
}

/// `kb-term kill <id>`: the runner's graceful kill op (exit keys, then the
/// whole-holder stop). For an id whose holder no longer answers the runner
/// removes that session's leftover socket and directory. Exit codes: 0 done;
/// 1 other failure; 2 usage (bad id); 3 runner unreachable; 4 not found.
pub async fn run_kill(args: KillArgs) -> u8 {
    run_kill_with(args, None).await
}

/// The default call budget of a kill: the grace (`None` means the CLI's own
/// default of 60 s; the runner clamps any grace to 60 s, so this does too)
/// plus 30 s for the stop path. Pure, so the formula is unit-tested.
fn kill_call_budget(grace_secs: Option<u64>) -> Duration {
    Duration::from_secs(grace_secs.unwrap_or(60).min(60) + 30)
}

/// The kill runs exit keys, waits up to the grace and then stops the holder,
/// so the default 10 s per-call deadline is far too short: the budget is
/// [`kill_call_budget`]. `budget` overrides that (tests).
async fn run_kill_with(args: KillArgs, budget: Option<Duration>) -> u8 {
    if !crate::wire::is_valid_term_id(&args.id) {
        eprintln!("kb-term: invalid session id");
        return 2;
    }
    let runner_sock = crate::runner_client::resolve_runner_socket(args.runner_sock.as_deref());
    let unreachable = |e: &RunnerClientError| {
        eprintln!("kb-term: {e}");
        eprintln!(
            "kb-term: the kb-termd user unit may not be running (socket: {})",
            runner_sock.display()
        );
        3
    };
    let mut runner = match RunnerClient::connect(&runner_sock).await {
        Ok(c) => c,
        Err(e @ RunnerClientError::Unreachable(_)) => return unreachable(&e),
        Err(e) => {
            eprintln!("kb-term: {e}");
            return 1;
        }
    };
    runner.set_timeout(budget.unwrap_or_else(|| kill_call_budget(args.grace_secs)));
    match runner
        .call(&Request::Kill {
            id: args.id.clone(),
            grace_secs: args.grace_secs,
        })
        .await
    {
        Ok(reply) => {
            println!("{}", render_kill_reply(&args.id, &reply));
            0
        }
        Err(e @ RunnerClientError::Unreachable(_)) if runner.last_call_timed_out() => {
            eprintln!("kb-term: {e}");
            eprintln!(
                "kb-term: the kill outcome is unknown (the runner may still be working); \
                 check `kb-term ls`"
            );
            1
        }
        Err(e @ RunnerClientError::Unreachable(_)) => unreachable(&e),
        Err(e) => {
            eprintln!("kb-term: {e}");
            kill_error_exit(e.code())
        }
    }
}

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
            handoff_from: None,
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
    #[test]
    fn kill_reply_lines_and_exit_codes() {
        use serde_json::json;
        assert_eq!(
            render_kill_reply("abcd1234", &json!({"exit_code": 0, "forced": false})),
            "killed abcd1234 (exit 0)"
        );
        assert_eq!(
            render_kill_reply("abcd1234", &json!({"exit_code": null, "forced": true})),
            "killed abcd1234 (forced)"
        );
        assert_eq!(
            render_kill_reply(
                "abcd1234",
                &json!({"exit_code": null, "forced": false, "stale_removed": true})
            ),
            "removed stale session abcd1234 (no holder answered)"
        );
        assert_eq!(kill_error_exit(Some("not-found")), 4);
        assert_eq!(kill_error_exit(Some("invalid-id")), 2);
        assert_eq!(kill_error_exit(Some("bad-request")), 1);
        assert_eq!(kill_error_exit(None), 1);
    }

    /// A malformed id never reaches the runner: exit 2 with no socket
    /// dialled (the path below does not exist).
    #[tokio::test]
    async fn kill_rejects_a_bad_id_before_dialling() {
        for bad in ["", "../x", "a/b", "ABC", "x y", "a\u{0}b"] {
            let code = run_kill(KillArgs {
                id: bad.to_string(),
                grace_secs: None,
                runner_sock: Some("/nonexistent-kb-term-test/runner.sock".to_string()),
            })
            .await;
            assert_eq!(code, 2, "id {bad:?}");
        }
        // A valid id with no runner is exit 3, not 2.
        let code = run_kill(KillArgs {
            id: "abcd1234".to_string(),
            grace_secs: None,
            runner_sock: Some("/nonexistent-kb-term-test/runner.sock".to_string()),
        })
        .await;
        assert_eq!(code, 3);
    }

    use super::*;

    /// A runner that answers Kill only after a delay longer than the
    /// client's call deadline must still yield exit 0 when the kill budget
    /// covers it (before the fix the flat 10 s deadline made this exit 3).
    #[tokio::test]
    async fn kill_budget_outlasts_a_slow_runner_reply() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("r.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (st, _) = listener.accept().await.unwrap();
            let (r, mut w) = st.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            let reply = serde_json::json!({"v":1,"id":v["id"],"status":"ok",
                "data":{"exit_code":0,"forced":false}});
            w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        });
        let args = |s: &std::path::Path| KillArgs {
            id: "abcd1234".to_string(),
            grace_secs: None,
            runner_sock: Some(s.display().to_string()),
        };
        let code = run_kill_with(args(&sock), Some(Duration::from_secs(5))).await;
        assert_eq!(code, 0);
        server.await.unwrap();

        // A budget shorter than the reply is an UNKNOWN outcome (exit 1),
        // never the "unit not running" exit 3.
        let sock2 = dir.path().join("r2.sock");
        let listener = tokio::net::UnixListener::bind(&sock2).unwrap();
        let server = tokio::spawn(async move {
            let (st, _) = listener.accept().await.unwrap();
            let (r, _w) = st.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
        });
        let code = run_kill_with(args(&sock2), Some(Duration::from_millis(150))).await;
        assert_eq!(code, 1);
        server.await.unwrap();
    }

    /// The DEFAULT kill call budget (the `kill_budget_*` test overrides it,
    /// so this is the only place the formula is exercised): grace + 30 s,
    /// the grace clamped to 60 s like the runner clamps it, no overflow.
    #[test]
    fn the_default_kill_call_budget_is_the_clamped_grace_plus_thirty_seconds() {
        let secs = |g| kill_call_budget(g).as_secs();
        assert_eq!(secs(None), 90, "no grace = the 60 s default + 30");
        assert_eq!(secs(Some(0)), 30);
        assert_eq!(secs(Some(5)), 35);
        assert_eq!(secs(Some(60)), 90);
        assert_eq!(secs(Some(61)), 90, "clamped to 60 like the runner");
        assert_eq!(secs(Some(u64::MAX)), 90, "no overflow");
    }

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
            size_marker: None,
            grid: None,
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
            viewer: None,
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
        assert_eq!(sink, JOIN_CLEAR);
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
        assert_eq!(sink, JOIN_CLEAR, "fresh clear after epoch change");
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
        assert_eq!(sink, JOIN_CLEAR);
    }

    #[test]
    fn a_join_paint_erases_what_it_paints_over() {
        use super::testscreen::Screen;
        let mut scr = Screen::new(80, 10);
        // What the outer terminal showed before the join: the shell line
        // that started the attach, then two old, longer mirror rows.
        scr.feed(b"$ kb-term attach abc12345 --runner-sock /tmp/some/long/path/runner.sock\r\n");
        scr.feed(b"an old mirrored row that is much longer than the new one\r\n");
        scr.feed(b"second old row with a long tail xxxxxxxxxxxxxxxxxxxxxxxx\r\n");
        // The join: a Seed frame, then the (shorter) seed bytes.
        let mut rw = LabelRewriter::new(80, true);
        let mut state = StreamState::default();
        let mut banner = true;
        let mut sink: Vec<u8> = Vec::new();
        apply_server_frame(
            hello("00000001"),
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        apply_server_frame(
            ServerFrame::Seed { offset: 0, len: 17 },
            &fctx(),
            &mut rw,
            &mut state,
            &mut banner,
            &mut sink,
        );
        write_rewritten(&mut rw, b"short\r\nrow two\r\n$ ", &mut sink);
        scr.feed(&sink);
        assert_eq!(scr.row(0), "short", "no tail of the old shell line");
        assert_eq!(scr.row(1), "row two", "no tail of the old mirror row");
        assert_eq!(scr.row(2), "$");
        for i in 3..10 {
            assert_eq!(scr.row(i), "", "row {i} is erased");
        }
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
