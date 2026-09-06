//! `kb-embedder` — sibling binary the daemon spawns to run fastembed/
//! ONNX at low CPU priority. Loads one model at startup, then serves
//! embedding (`--model`) OR reranking (`--reranker`) requests over
//! stdin/stdout NDJSON until EOF or `Request::Shutdown`.
//!
//! This is the ONLY binary that links ONNX Runtime (via the kb-core
//! `local-embedder` feature). The daemon/CLI/TUI drive it over IPC and
//! link zero onnxruntime themselves.
//!
//! Run directly only for diagnostics (`echo '...' | kb-embedder
//! --model X --cache Y`). `--download-only` loads (and thus downloads +
//! caches) the model then exits, which is how `kb model download` works.
//! Normal invocation is by the daemon, which sets `nice` on the child via
//! Unix `pre_exec` (see `kb_core::embed_ipc::IpcBackend::spawn`).

use anyhow::{anyhow, bail, Context, Result};
use kb_core::embed::{Embedder, Reranker};
use kb_core::embed_ipc::{Request, Response};
use std::io::{BufRead, BufReader, StdoutLock, Write};
use std::path::PathBuf;

/// What the subprocess loads: an embedding model or a reranker model.
enum Mode {
    Embed(String),
    Rerank(String),
}

impl Mode {
    fn model(&self) -> &str {
        match self {
            Mode::Embed(m) | Mode::Rerank(m) => m,
        }
    }
}

struct Args {
    mode: Mode,
    cache: PathBuf,
    /// Load (download + cache) the model, then exit 0 before the handshake.
    download_only: bool,
}

fn main() -> Result<()> {
    // tracing → stderr; the daemon inherits this so child diagnostic
    // logs land in the same place as the parent's.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KB_EMBEDDER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    tracing::info!(
        model = %args.mode.model(),
        cache = %args.cache.display(),
        download_only = args.download_only,
        "loading model",
    );

    match args.mode {
        Mode::Embed(model) => run_embed(&model, args.cache, args.download_only),
        Mode::Rerank(model) => run_rerank(&model, args.cache, args.download_only),
    }
}

/// Embedding mode: build the embedder, handshake `Ready{dim}`, then serve
/// `Embed` requests until EOF/Shutdown.
fn run_embed(model: &str, cache: PathBuf, download_only: bool) -> Result<()> {
    // The slow step (~3-10s for bge-small; longer if not cached yet).
    let mut embedder =
        Embedder::new(model, cache).with_context(|| format!("load embedder {model}"))?;
    if download_only {
        eprintln!("kb-embedder: model {model:?} ready in cache");
        return Ok(());
    }
    let dim = embedder.dim() as u32;
    let model_name = embedder.model_name().to_string();

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    handshake(&mut stdout, model_name, dim)?;

    serve(&mut stdout, |req, out| match req {
        Request::Embed { req_id, texts } => {
            let resp = match embedder.embed_batch(&texts) {
                Ok(vectors) => Response::EmbedOk { req_id, vectors },
                Err(e) => Response::Error {
                    req_id,
                    msg: e.to_string(),
                },
            };
            write_line(out, &resp)
        }
        Request::Rerank { req_id, .. } => write_line(
            out,
            &Response::Error {
                req_id,
                msg: "this is an embed-mode subprocess; rerank not supported".into(),
            },
        ),
        Request::Shutdown => Ok(ControlFlow::Stop),
    })
}

/// Reranking mode: build the reranker, handshake `Ready{dim:0}` (rerankers
/// have no embedding dimension), then serve `Rerank` requests.
fn run_rerank(model: &str, cache: PathBuf, download_only: bool) -> Result<()> {
    let mut reranker =
        Reranker::new(model, cache).with_context(|| format!("load reranker {model}"))?;
    if download_only {
        eprintln!("kb-embedder: reranker {model:?} ready in cache");
        return Ok(());
    }
    let model_name = reranker.model_name().to_string();

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    handshake(&mut stdout, model_name, 0)?;

    serve(&mut stdout, |req, out| match req {
        Request::Rerank {
            req_id,
            query,
            documents,
        } => {
            let resp = match reranker.rerank_scores(&query, &documents) {
                Ok(scores) => Response::RerankOk { req_id, scores },
                Err(e) => Response::Error {
                    req_id,
                    msg: e.to_string(),
                },
            };
            write_line(out, &resp)
        }
        Request::Embed { req_id, .. } => write_line(
            out,
            &Response::Error {
                req_id,
                msg: "this is a reranker-mode subprocess; embed not supported".into(),
            },
        ),
        Request::Shutdown => Ok(ControlFlow::Stop),
    })
}

/// Loop control: keep serving, or stop on Shutdown.
enum ControlFlow {
    Continue,
    Stop,
}

/// Write the `Ready` handshake and flush so the parent unblocks from its
/// `read_line`.
fn handshake(out: &mut StdoutLock<'_>, model: String, dim: u32) -> Result<()> {
    let ready = Response::Ready { model, dim };
    writeln!(out, "{}", serde_json::to_string(&ready).context("Ready")?)?;
    out.flush()?;
    Ok(())
}

fn write_line(out: &mut StdoutLock<'_>, resp: &Response) -> Result<ControlFlow> {
    writeln!(out, "{}", serde_json::to_string(resp)?)?;
    out.flush()?;
    Ok(ControlFlow::Continue)
}

/// Request loop — one JSON line per request. A malformed line yields an
/// `Error{req_id:0}` and continues. `handle` returns `Stop` to exit cleanly.
fn serve(
    out: &mut StdoutLock<'_>,
    mut handle: impl FnMut(Request, &mut StdoutLock<'_>) -> Result<ControlFlow>,
) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdin = BufReader::new(stdin.lock());
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = stdin.read_line(&mut buf)?;
        if n == 0 {
            tracing::info!("stdin EOF; exiting");
            return Ok(());
        }
        let req: Request = match serde_json::from_str(buf.trim()) {
            Ok(r) => r,
            Err(e) => {
                let err = Response::Error {
                    req_id: 0,
                    msg: format!("parse: {e}"),
                };
                writeln!(out, "{}", serde_json::to_string(&err)?)?;
                out.flush()?;
                continue;
            }
        };
        match handle(req, out)? {
            ControlFlow::Continue => {}
            ControlFlow::Stop => {
                tracing::info!("Shutdown request; exiting");
                return Ok(());
            }
        }
    }
}

/// Parse argv. Exactly one of `--model NAME` / `--reranker NAME` is required,
/// plus `--cache DIR`; `--download-only` is optional. Order doesn't matter.
fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut mode: Option<Mode> = None;
    let mut cache: Option<PathBuf> = None;
    let mut download_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--model needs a value"))?;
                if mode.is_some() {
                    bail!("--model and --reranker are mutually exclusive");
                }
                mode = Some(Mode::Embed(v));
            }
            "--reranker" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--reranker needs a value"))?;
                if mode.is_some() {
                    bail!("--model and --reranker are mutually exclusive");
                }
                mode = Some(Mode::Rerank(v));
            }
            "--cache" => {
                cache = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow!("--cache needs a value"))?,
                ));
            }
            "--download-only" => download_only = true,
            "--help" | "-h" => {
                eprintln!(
                    "kb-embedder (--model NAME | --reranker NAME) --cache DIR [--download-only]\n\
                     \n\
                     Reads Request envelopes (NDJSON) from stdin, writes Response\n\
                     envelopes to stdout. Normally spawned by the kb daemon.\n\
                     --download-only loads the model into the cache, then exits."
                );
                std::process::exit(0);
            }
            other => bail!("unknown arg: {other:?}"),
        }
    }
    let mode = mode.ok_or_else(|| anyhow!("one of --model / --reranker required"))?;
    let cache = cache.ok_or_else(|| anyhow!("--cache required"))?;
    Ok(Args {
        mode,
        cache,
        download_only,
    })
}
