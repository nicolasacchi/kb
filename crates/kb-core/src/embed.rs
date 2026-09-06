//! Embedding wrapper around `fastembed::TextEmbedding`. Lifted from
//! spike-fastembed (`spikes/fastembed/src/main.rs:97`,`127`,`157`,`191` —
//! the `InitOptions::new(model)` + `model.embed(vec![...], None)` pattern).
//!
//! Production differences from the spike:
//!
//! 1. **`with_cache_dir` is mandatory.** Spike-fastembed §"NEW finding 5":
//!    fastembed's default cache is `<cwd>/.fastembed_cache/`, not the XDG
//!    cache. Production constructs `InitOptions::new(...).with_cache_dir(
//!    paths.cache.join("models"))` at every `Embedder::new`. Without this,
//!    `kb daemon` invoked from different working directories would create
//!    parallel model caches.
//! 2. **Model registry up front.** A small const table maps user-facing
//!    names to `fastembed::EmbeddingModel` enum variants + the dim + the
//!    license + the on-disk size estimate. `kb model list` prints from this
//!    table; `kb model set NAME` validates against it; `kb-core::indexer`
//!    consults it for the embedding column dimension.
//! 3. **fp32 only — no quantised model is registered.** Spike-fastembed
//!    measured int8 *slower* than fp32 on Kaby Lake (no AVX-VNNI), so
//!    quantisation was never carried into production: `SUPPORTED_MODELS`
//!    below has no quantized entry and there is no `QuantizationMode`
//!    wiring. Adding one would need both a new `SUPPORTED_MODELS` row and
//!    that wiring — not just a config flag.
//! 4. **Batch cap at 32.** Spike measured throughput plateau at batch 32;
//!    larger batches thrash on Kaby Lake (RSS climbs to 1.3 GB at batch 32,
//!    killed at batch 128). `embed_batch` chunks any input larger than 32.
//!
//! Perf reality (per spike-fastembed §Measurements, host: i7-7700 Kaby Lake):
//! cold start ~3.4 s with cache; warm p50 ~170 ms / 500-token doc; throughput
//! ~5.6 emb/s regardless of batch size. A 50k-doc reindex = ~140 min on this
//! class of CPU. The indexer must surface progress via SSE
//! (`index.embedding` event before `index.file`) so the TUI / SPA show
//! granular state during the long pass.

use crate::{Error, Result};
// fastembed / ONNX Runtime is compiled in ONLY under the `local-embedder`
// feature, which only `kb-embedder` enables. The daemon, CLI and TUI build
// with this OFF and link zero onnxruntime — they drive embedding/reranking
// over IPC (see `embed_ipc`). Everything else in this module (the model
// registry, dims, the IPC `Embedder`/`RerankerClient` constructors) is
// always compiled and fastembed-free.
#[cfg(feature = "local-embedder")]
use fastembed::{
    EmbeddingModel, RerankInitOptions, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
};
use std::path::Path;
use std::path::PathBuf;

/// Spike-fastembed §"NEW finding 6" — batch cap. Larger batches thrash
/// memory bandwidth on pre-VNNI x86 without throughput benefit.
pub const MAX_BATCH_SIZE: usize = 32;

/// Hard byte cap applied to every text entering an embed call (2026-08-21
/// ci-host incident: a 292MB session HTML tokenized to ~11.7GB RSS and
/// memcg-OOM-killed the embedder hourly). BGE models truncate at 512
/// tokens, and no tokenizer needs >64 bytes/token, so any input this long
/// yields a byte-identical vector to the untruncated text — the cap only
/// removes bytes the model could never see.
pub const EMBED_INPUT_CAP_BYTES: usize = 32 * 1024;

/// Returns `text` unchanged when it fits within [`EMBED_INPUT_CAP_BYTES`];
/// otherwise the longest prefix at or under the cap that ends on a UTF-8
/// char boundary. Walks back from the cap index rather than slicing
/// blindly, so a multi-byte char straddling the cut never panics.
fn cap_embed_input(text: &str) -> &str {
    if text.len() <= EMBED_INPUT_CAP_BYTES {
        return text;
    }
    let mut end = EMBED_INPUT_CAP_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Mini-batch size the indexer embeds chunk vectors in when yielding to
/// waiting queries (query-priority lane). Small enough that a query waits at
/// most one mini-batch for the shared embedder, large enough to keep IPC
/// amortised. `Embedder::embed_batch` still re-chunks internally at
/// `MAX_BATCH_SIZE`; this only bounds how long the indexer holds the mutex
/// between yield points.
pub const INDEX_EMBED_MINI_BATCH: usize = 8;

// ── Query-priority lane (2026-07 tail-latency fix) ───────────────────────
//
// Queries (search/recall) and the indexer share ONE `Arc<Mutex<Embedder>>`
// per model. During a reindex the indexer can hold that mutex for a whole
// document's chunk embeds, so a latency-sensitive query embed on the SAME
// model queues behind it — a mechanism behind the 2026-07-03 kb.example.com recall
// stalls. This is a process-global, per-model count of in-flight QUERY embeds,
// keyed by the `&'static str` registry model name that BOTH sides already hold
// (`Embedder::model_name`). The query path brackets its embed with a
// `QueryLaneGuard`; the indexer polls `pending_query_count` between mini-batches
// and yields the mutex while it is non-zero, so a query never waits longer than
// one mini-batch. Keyed by model, so a corpus on its own model (e.g. the
// dedicated sessions `bge-small`) neither sees nor causes cross-model
// contention. Deliberately NOT threaded through the embedder handle: keying on
// the model name avoids changing `Arc<Mutex<Embedder>>` at its ~15 call sites.

fn pending_query_registry() -> &'static std::sync::Mutex<
    std::collections::HashMap<&'static str, std::sync::Arc<std::sync::atomic::AtomicUsize>>,
> {
    static REG: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<&'static str, std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        >,
    > = std::sync::OnceLock::new();
    REG.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn pending_counter(model: &'static str) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let mut reg = pending_query_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    reg.entry(model)
        .or_insert_with(|| std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)))
        .clone()
}

/// Count of QUERY embeds currently in flight (or waiting on the shared mutex)
/// for `model`. The indexer polls this between chunk mini-batches to decide
/// whether to yield the shared embedder to a waiting query.
pub fn pending_query_count(model: &'static str) -> usize {
    pending_counter(model).load(std::sync::atomic::Ordering::SeqCst)
}

/// RAII marker that a QUERY embed for `model` is contending for the shared
/// embedder: increments the per-model counter on construction, decrements on
/// drop, so [`pending_query_count`] is non-zero for exactly the window the
/// query holds/awaits the embedder mutex. Held across the query embed by
/// `kb-server`'s `embed_cache`.
#[must_use = "hold the guard across the query embed; dropping it clears the pending mark"]
pub struct QueryLaneGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl QueryLaneGuard {
    pub fn enter(model: &'static str) -> Self {
        let c = pending_counter(model);
        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(c)
    }
}

impl Drop for QueryLaneGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Static metadata about one supported embedding model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// User-facing name; matches what appears in `kb.toml [kb.<name>]
    /// embedding_model = "..."` and in `kb model list`.
    pub name: &'static str,
    /// Embedding dimension (drives the lance schema column width).
    pub dim: usize,
    /// SPDX licence identifier.
    pub license: &'static str,
    /// Approximate on-disk size of the model files (MB), for `kb model
    /// download` UX.
    pub approx_size_mb: u32,
    /// Whether this is the default for `kb add` / new kbs.
    pub default: bool,
    /// Per-model max sequence length (tokens), plumbed into fastembed's
    /// `TextInitOptions::with_max_length` at construction (`Embedder::
    /// with_info`, `local-embedder` only). `None` keeps fastembed's own
    /// per-model default (`text_embedding::DEFAULT_MAX_LENGTH` = 512,
    /// applied via `EmbeddingModel`'s blanket `HasMaxLength` impl —
    /// unchanged behavior for every model that doesn't set this).
    /// `Some(n)` overrides it. Only `jina-embeddings-v2-base-code` sets
    /// this today: its ALiBi long-context BERT backbone supports an
    /// 8192-token window, but fastembed's generic default would silently
    /// truncate it to 512 like every other registered model — discarding
    /// exactly the long-context headroom kb-code's semantic chunker
    /// (W2.3) exists to exploit (a chunk's header + body can run well
    /// past 512 tokens before the ~256-512-token merge cap even kicks in).
    pub max_length: Option<usize>,
}

/// Registry of supported models. Topic 02 §Decisions:
/// bge-small-en-v1.5 (default) + jina-v2-base-code (documented switch
/// candidate for code-heavy corpora). The bake-off milestone added
/// the natural step-ups in the same BGE family: bge-base (768) and
/// bge-large (1024). Both are MIT-licensed and supported by fastembed
/// 5.x; cache costs scale with `approx_size_mb`. Use `kb model list`
/// to see what's registered + which kbs reference each.
pub const SUPPORTED_MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "bge-small-en-v1.5",
        dim: 384,
        license: "MIT",
        approx_size_mb: 127,
        default: true,
        max_length: None,
    },
    ModelInfo {
        name: "bge-base-en-v1.5",
        dim: 768,
        license: "MIT",
        approx_size_mb: 440,
        default: false,
        max_length: None,
    },
    ModelInfo {
        name: "bge-large-en-v1.5",
        dim: 1024,
        license: "MIT",
        approx_size_mb: 1340,
        default: false,
        max_length: None,
    },
    ModelInfo {
        name: "jina-embeddings-v2-base-code",
        dim: 768,
        license: "Apache-2.0",
        approx_size_mb: 320,
        default: false,
        // W2.3 (kb-code semantic lane) — see the field doc: this model's
        // ALiBi backbone supports 8192 tokens; every other registered
        // model stays `None` (fastembed's own 512-token default).
        max_length: Some(8192),
    },
];

/// Look up a model by user-facing name.
pub fn model_info(name: &str) -> Option<&'static ModelInfo> {
    SUPPORTED_MODELS.iter().find(|m| m.name == name)
}

/// The default model (bge-small-en-v1.5) per topic 02 §Decisions.
pub fn default_model() -> &'static ModelInfo {
    SUPPORTED_MODELS
        .iter()
        .find(|m| m.default)
        .expect("at least one default model must be marked")
}

/// Map a registered model name to fastembed's enum variant. The mapping
/// lives in code (not the always-compiled `ModelInfo` table) so the daemon
/// can read model metadata without depending on `fastembed::EmbeddingModel`.
/// Kept in sync with `SUPPORTED_MODELS` by `every_registered_model_maps`.
#[cfg(feature = "local-embedder")]
fn fastembed_model(name: &str) -> Result<EmbeddingModel> {
    Ok(match name {
        "bge-small-en-v1.5" => EmbeddingModel::BGESmallENV15,
        "bge-base-en-v1.5" => EmbeddingModel::BGEBaseENV15,
        "bge-large-en-v1.5" => EmbeddingModel::BGELargeENV15,
        "jina-embeddings-v2-base-code" => EmbeddingModel::JinaEmbeddingsV2BaseCode,
        other => {
            return Err(Error::BadRequest(format!(
                "unknown embedding model: {other:?}"
            )))
        }
    })
}

/// Build the `TextInitOptions` `Embedder::with_info` hands to
/// `TextEmbedding::try_new` — factored out so the `max_length` plumb (see
/// `ModelInfo::max_length`'s doc) is pinnable by a cheap, network-free unit
/// test (`TextInitOptions`'s fields are `pub`, so the test reads
/// `opts.max_length` directly without ever constructing a real ONNX
/// session). `with_max_length` is called ONLY when `info.max_length` is
/// `Some` — never blanket — so every model without an override keeps
/// fastembed's own per-model default untouched.
#[cfg(feature = "local-embedder")]
fn build_text_init_options(
    info: &'static ModelInfo,
    cache_dir: PathBuf,
) -> Result<TextInitOptions> {
    let mut opts = TextInitOptions::new(fastembed_model(info.name)?).with_cache_dir(cache_dir);
    if let Some(max_length) = info.max_length {
        opts = opts.with_max_length(max_length);
    }
    Ok(opts)
}

/// Wrapper around `fastembed::TextEmbedding`. Constructed once per kb at
/// daemon startup; held inside `Arc<Mutex<Embedder>>` so the indexer (one
/// task per kb) and the search route (per-request) can serialise access.
///
/// `embed` is `&mut self` in fastembed 5.x — the `Mutex` is required, not
/// optional.
///
/// I1 — the embedder now has two backends. `Local` runs ONNX in-process
/// (the v0.0.1 shape; still used by `kb model download` and tests).
/// `Ipc` drives a sibling `kb-embedder` process via stdin/stdout NDJSON
/// — the daemon spawns this at startup so ONNX runs at nice 20 instead
/// of stealing every core from HTTP/SSE handlers. Callers see one
/// uniform `Embedder` type; the enum is hidden behind a private field.
pub struct Embedder {
    backend: EmbedderBackend,
    info: &'static ModelInfo,
}

/// I1 — backend dispatch. Local = legacy in-process embedding.
/// Ipc = subprocess client (see `crate::embed_ipc::IpcBackend`).
/// `Local` is boxed because `TextEmbedding` is ~1.2 KB; without the
/// indirection clippy flags the variant-size mismatch
/// (`large_enum_variant`) and every `Arc<Mutex<Embedder>>` on the
/// daemon would inherit the larger size for both arms.
enum EmbedderBackend {
    #[cfg(feature = "local-embedder")]
    Local(Box<TextEmbedding>),
    Ipc(crate::embed_ipc::IpcBackend),
}

impl Embedder {
    /// Construct an in-process embedder for `model_name`, downloading
    /// the model if not already cached at `cache_dir/<model-name>/`.
    /// The cache_dir MUST be supplied (XDG paths' `<cache>/models/` per
    /// `kb_core::paths`); without it fastembed creates
    /// `<cwd>/.fastembed_cache/`, defeating XDG.
    ///
    /// I1: this is the LOCAL (in-process) constructor. The daemon's
    /// per-kb startup uses `spawn_ipc` instead; `kb model download`
    /// and tests keep using `new`. Only compiled under `local-embedder`
    /// (i.e. inside `kb-embedder`); the daemon never links ONNX in-process.
    #[cfg(feature = "local-embedder")]
    pub fn new(model_name: &str, cache_dir: PathBuf) -> Result<Self> {
        let info = model_info(model_name).ok_or_else(|| {
            Error::BadRequest(format!(
                "unknown embedding model: {model_name:?}; \
                 known: {}",
                SUPPORTED_MODELS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        Self::with_info(info, cache_dir)
    }

    /// Construct from an already-resolved `ModelInfo`. Avoids the lookup if
    /// the caller already has the reference.
    #[cfg(feature = "local-embedder")]
    pub fn with_info(info: &'static ModelInfo, cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;
        let opts = build_text_init_options(info, cache_dir)?;
        let inner = TextEmbedding::try_new(opts)
            .map_err(|e| Error::Storage(format!("fastembed init {}: {e}", info.name)))?;
        Ok(Self {
            backend: EmbedderBackend::Local(Box::new(inner)),
            info,
        })
    }

    /// I1 — spawn a sibling `kb-embedder` subprocess and return an
    /// `Embedder` that drives it via stdin/stdout NDJSON. `nice` is
    /// applied before the child binary takes over so ONNX worker
    /// threads inherit the priority. Used by the daemon at per-kb
    /// startup so the indexer + search query embedding both run at
    /// the configured nice level without affecting HTTP latency.
    pub fn spawn_ipc(model_name: &str, cache_dir: PathBuf, nice: i32) -> Result<Self> {
        let info = model_info(model_name).ok_or_else(|| {
            Error::BadRequest(format!(
                "unknown embedding model: {model_name:?}; \
                 known: {}",
                SUPPORTED_MODELS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let ipc = crate::embed_ipc::IpcBackend::spawn(info.name, &cache_dir, nice)?;
        Ok(Self {
            backend: EmbedderBackend::Ipc(ipc),
            info,
        })
    }

    /// Embedding dimension for this model.
    pub fn dim(&self) -> usize {
        self.info.dim
    }

    /// Static model metadata.
    pub fn info(&self) -> &'static ModelInfo {
        self.info
    }

    /// User-facing model name.
    pub fn model_name(&self) -> &'static str {
        self.info.name
    }

    /// Embed a single text. The fastembed call always takes a Vec; this
    /// wraps + unwraps so callers don't have to. `text` is capped to
    /// [`EMBED_INPUT_CAP_BYTES`] FIRST, before the backend dispatch, so
    /// both the Local (fastembed) arm and the Ipc arm (the daemon's path,
    /// and — since `kb-embedder` itself constructs a Local `Embedder` —
    /// the subprocess side of that same IPC boundary) see only capped
    /// input.
    pub fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
        let text = cap_embed_input(text);
        match &mut self.backend {
            #[cfg(feature = "local-embedder")]
            EmbedderBackend::Local(inner) => {
                let mut out = inner
                    .embed(vec![text], None)
                    .map_err(|e| Error::Storage(format!("fastembed embed_one: {e}")))?;
                out.pop()
                    .ok_or_else(|| Error::Storage("fastembed returned empty result".into()))
            }
            EmbedderBackend::Ipc(ipc) => ipc.embed_one(text),
        }
    }

    /// Embed a batch. Every element is first capped to
    /// [`EMBED_INPUT_CAP_BYTES`] (no allocation when nothing is over the
    /// cap — the query path calls this hot with small texts). Then chunks
    /// at `MAX_BATCH_SIZE` (=32) per spike-fastembed finding (larger
    /// batches thrash on pre-VNNI x86 without throughput gain) — applied to
    /// BOTH backends, so an oversized batch can never reach the IPC
    /// subprocess unchunked either.
    pub fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let capped: Option<Vec<String>> = texts
            .iter()
            .any(|t| t.len() > EMBED_INPUT_CAP_BYTES)
            .then(|| {
                texts
                    .iter()
                    .map(|t| cap_embed_input(t).to_string())
                    .collect()
            });
        let texts: &[String] = capped.as_deref().unwrap_or(texts);

        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH_SIZE) {
            let batch_out = match &mut self.backend {
                #[cfg(feature = "local-embedder")]
                EmbedderBackend::Local(inner) => {
                    let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
                    inner
                        .embed(refs, None)
                        .map_err(|e| Error::Storage(format!("fastembed embed_batch: {e}")))?
                }
                EmbedderBackend::Ipc(ipc) => ipc.embed_batch(chunk)?,
            };
            out.extend(batch_out);
        }
        Ok(out)
    }

    /// Proactive liveness probe + recovery (v0.16 Q-track). For the IPC
    /// backend this detects an idle-dead subprocess (e.g. an OOM-kill
    /// between requests) and respawns it before the next embed —
    /// see [`crate::embed_ipc::IpcBackend::ensure_alive`]. The in-process
    /// `Local` backend has no subprocess to die, so it is always live.
    /// Returns whether the embedder is usable afterwards.
    pub fn ensure_alive(&mut self) -> bool {
        match &mut self.backend {
            #[cfg(feature = "local-embedder")]
            EmbedderBackend::Local(_) => true,
            EmbedderBackend::Ipc(ipc) => ipc.ensure_alive(),
        }
    }

    /// Subprocess respawns over this daemon's lifetime (0 for the
    /// in-process `Local` backend). A non-zero, climbing value is the
    /// "the embedder keeps crashing" signal the metrics tick surfaces.
    pub fn respawn_count(&self) -> u64 {
        match &self.backend {
            #[cfg(feature = "local-embedder")]
            EmbedderBackend::Local(_) => 0,
            EmbedderBackend::Ipc(ipc) => ipc.respawn_count(),
        }
    }
}

/// Convenience: resolve the model cache directory under the daemon's
/// `KbPaths.cache`. `<cache>/models/` matches topic 02 §Decisions
/// "XDG cache: ~/.cache/kb/models/".
pub fn models_cache_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("models")
}

// ---- SQ4 — cross-encoder reranker -------------------------------------

/// Static metadata about one supported cross-encoder reranker model.
/// Parallels [`ModelInfo`] but for the rerank stage; a reranker has no
/// embedding dimension (it scores query–document pairs directly).
#[derive(Debug, Clone)]
pub struct RerankerInfo {
    /// User-facing name; matches `kb.toml [kb.<name>] reranker_model`.
    pub name: &'static str,
    /// SPDX licence identifier.
    pub license: &'static str,
    /// Approximate on-disk size (MB) of the ONNX weights.
    pub approx_size_mb: u32,
    /// Whether this is the default when only `reranker = true`-style
    /// enablement is implied (currently informational).
    pub default: bool,
}

/// Registry of supported rerankers (fastembed 5.x, all gated on the
/// already-enabled `hf-hub` feature — no new dependency). bge-reranker-base
/// is the default: small enough to run on CPU, strong on technical English.
pub const SUPPORTED_RERANKERS: &[RerankerInfo] = &[
    RerankerInfo {
        name: "bge-reranker-base",
        license: "MIT",
        approx_size_mb: 1100,
        default: true,
    },
    RerankerInfo {
        name: "bge-reranker-v2-m3",
        license: "Apache-2.0",
        approx_size_mb: 2270,
        default: false,
    },
    RerankerInfo {
        name: "jina-reranker-v1-turbo-en",
        license: "Apache-2.0",
        approx_size_mb: 150,
        default: false,
    },
];

/// Look up a reranker by user-facing name.
pub fn reranker_info(name: &str) -> Option<&'static RerankerInfo> {
    SUPPORTED_RERANKERS.iter().find(|m| m.name == name)
}

/// Map a registered reranker name to fastembed's enum variant. As with
/// [`fastembed_model`], the mapping lives in code so the always-compiled
/// registry stays fastembed-free. Kept in sync with `SUPPORTED_RERANKERS`.
#[cfg(feature = "local-embedder")]
fn fastembed_reranker(name: &str) -> Result<RerankerModel> {
    Ok(match name {
        "bge-reranker-base" => RerankerModel::BGERerankerBase,
        "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
        other => {
            return Err(Error::BadRequest(format!(
                "unknown reranker model: {other:?}"
            )))
        }
    })
}

/// In-process cross-encoder reranker (fastembed `TextRerank`). Held by the
/// daemon behind `Arc<Mutex<Reranker>>` like the embedder; the search
/// route calls [`Reranker::rerank`] inside `spawn_blocking` so ONNX stays
/// off the async worker threads. Constructed only for kbs that opt in via
/// `reranker_model`; failure to load is non-fatal (the daemon logs and
/// disables reranking — search still works).
/// In-process reranker — only compiled under `local-embedder` (inside
/// `kb-embedder`). The daemon drives reranking over IPC via
/// [`crate::embed_ipc::RerankerClient`] instead of holding this directly.
#[cfg(feature = "local-embedder")]
pub struct Reranker {
    inner: TextRerank,
    info: &'static RerankerInfo,
}

#[cfg(feature = "local-embedder")]
impl Reranker {
    /// Construct an in-process reranker, downloading the model to
    /// `cache_dir` on first use (~1.1 GB for bge-reranker-base). Slow
    /// (ONNX load); call inside `spawn_blocking` from async contexts.
    pub fn new(model_name: &str, cache_dir: PathBuf) -> Result<Self> {
        let info = reranker_info(model_name).ok_or_else(|| {
            Error::BadRequest(format!(
                "unknown reranker model: {model_name:?}; known: {}",
                SUPPORTED_RERANKERS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        std::fs::create_dir_all(&cache_dir)?;
        let opts = RerankInitOptions::new(fastembed_reranker(info.name)?).with_cache_dir(cache_dir);
        let inner = TextRerank::try_new(opts)
            .map_err(|e| Error::Storage(format!("fastembed reranker init {}: {e}", info.name)))?;
        Ok(Self { inner, info })
    }

    /// User-facing model name.
    pub fn model_name(&self) -> &'static str {
        self.info.name
    }

    /// Score `documents` against `query`, returning one score per document
    /// in INPUT ORDER (no sort, no `top_n`). This is the shape the IPC
    /// protocol carries: the daemon-side [`crate::embed_ipc::RerankerClient`]
    /// re-derives the sorted `(index, score)` list. Empty input → empty vec.
    pub fn rerank_scores(&mut self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let docs: Vec<&str> = documents.iter().map(String::as_str).collect();
        // return_documents=false — only scores cross the wire.
        let results = self
            .inner
            .rerank(query, docs, false, None)
            .map_err(|e| Error::Storage(format!("fastembed rerank: {e}")))?;
        // fastembed returns results sorted by score; scatter back to input
        // order so `scores[i]` aligns with `documents[i]`.
        let mut scores = vec![0.0f32; documents.len()];
        for r in results {
            if let Some(slot) = scores.get_mut(r.index) {
                *slot = r.score;
            }
        }
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- cap_embed_input (2026-08-21 ci-host incident) -------------------------

    #[test]
    fn cap_embed_input_returns_unchanged_when_under_cap() {
        let text = "hello world";
        let capped = cap_embed_input(text);
        assert_eq!(capped, text);
        assert_eq!(capped.len(), text.len());
        assert!(
            std::ptr::eq(capped.as_ptr(), text.as_ptr()),
            "under-cap input must be returned as-is, no copy"
        );
    }

    #[test]
    fn cap_embed_input_truncates_oversized_ascii_to_exact_cap() {
        let text = "a".repeat(EMBED_INPUT_CAP_BYTES + 100);
        let capped = cap_embed_input(&text);
        assert_eq!(capped.len(), EMBED_INPUT_CAP_BYTES);
        assert!(text.starts_with(capped));
    }

    #[test]
    fn cap_embed_input_truncates_to_char_boundary_without_panicking() {
        // ASCII up to one byte short of the cap, then a 4-byte multi-byte
        // char straddling the naive cap index, then more text after it —
        // the naive byte index EMBED_INPUT_CAP_BYTES would land mid-char.
        let mut text = "a".repeat(EMBED_INPUT_CAP_BYTES - 1);
        text.push('\u{1F600}'); // 😀, 4 bytes in UTF-8
        text.push_str("tail text after the emoji");
        let capped = cap_embed_input(&text); // must not panic
        assert!(capped.len() <= EMBED_INPUT_CAP_BYTES);
        assert!(text.is_char_boundary(capped.len()));
    }

    #[test]
    fn cap_embed_input_capped_prefix_is_a_prefix_of_original() {
        let text = "x".repeat(EMBED_INPUT_CAP_BYTES * 2);
        let capped = cap_embed_input(&text);
        assert!(text.starts_with(capped));
    }

    // --- Query-priority lane primitive -----------------------------------
    // Distinct `&'static str` keys per test so the process-global registry
    // can't cross-contaminate under parallel execution.

    #[test]
    fn query_lane_guard_tracks_pending_count() {
        let model = "test-lane-track";
        assert_eq!(pending_query_count(model), 0);
        {
            let _g1 = QueryLaneGuard::enter(model);
            assert_eq!(pending_query_count(model), 1);
            {
                let _g2 = QueryLaneGuard::enter(model);
                assert_eq!(pending_query_count(model), 2);
            }
            assert_eq!(
                pending_query_count(model),
                1,
                "inner guard drop decremented"
            );
        }
        assert_eq!(pending_query_count(model), 0, "outer guard drop cleared it");
    }

    #[test]
    fn query_lane_counters_are_per_model() {
        let a = "test-lane-per-model-a";
        let b = "test-lane-per-model-b";
        let _g = QueryLaneGuard::enter(a);
        assert_eq!(pending_query_count(a), 1);
        assert_eq!(
            pending_query_count(b),
            0,
            "a query on model a must not register on model b (per-model contention)"
        );
    }

    // --- Pure registry tests (no live model download) ---------------------

    #[test]
    fn model_info_finds_supported() {
        let bge = model_info("bge-small-en-v1.5").expect("bge-small registered");
        assert_eq!(bge.dim, 384);
        assert_eq!(bge.license, "MIT");
        assert!(bge.default);

        let jina =
            model_info("jina-embeddings-v2-base-code").expect("jina-v2-base-code registered");
        assert_eq!(jina.dim, 768);
        assert_eq!(jina.license, "Apache-2.0");
        assert!(!jina.default);
    }

    #[test]
    fn model_info_returns_none_for_unknown() {
        assert!(model_info("does-not-exist").is_none());
        assert!(model_info("").is_none());
    }

    #[test]
    fn default_model_is_bge_small() {
        let d = default_model();
        assert_eq!(d.name, "bge-small-en-v1.5");
        assert_eq!(d.dim, 384);
    }

    #[test]
    fn exactly_one_default() {
        let n = SUPPORTED_MODELS.iter().filter(|m| m.default).count();
        assert_eq!(n, 1, "must be exactly one default model");
    }

    #[test]
    fn bge_base_registered_with_dim_768() {
        let m = model_info("bge-base-en-v1.5").expect("bge-base registered");
        assert_eq!(m.dim, 768);
        assert_eq!(m.license, "MIT");
        assert!(!m.default);
    }

    #[test]
    fn bge_large_registered_with_dim_1024() {
        let m = model_info("bge-large-en-v1.5").expect("bge-large registered");
        assert_eq!(m.dim, 1024);
        assert_eq!(m.license, "MIT");
        assert!(!m.default);
    }

    #[test]
    fn only_jina_code_sets_a_custom_max_length() {
        for m in SUPPORTED_MODELS {
            if m.name == "jina-embeddings-v2-base-code" {
                assert_eq!(m.max_length, Some(8192), "{} max_length", m.name);
            } else {
                assert_eq!(
                    m.max_length, None,
                    "{} must keep fastembed's own per-model default (max_length: None)",
                    m.name
                );
            }
        }
    }

    #[test]
    fn registered_model_names_are_unique() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for m in SUPPORTED_MODELS {
            assert!(seen.insert(m.name), "duplicate model name: {}", m.name);
        }
    }

    #[test]
    fn all_dims_divisible_by_16() {
        // Lance IvfPq index uses `n_subvectors = 16` by default. Any dim
        // not divisible by 16 makes `ensure_vector_index` fail on the
        // first reindex. 384 / 16 = 24, 768 / 16 = 48, 1024 / 16 = 64
        // — all clean. Defence-in-depth so a future model can't sneak
        // in with a 1536 or 100-dim variant that would break PQ.
        for m in SUPPORTED_MODELS {
            assert_eq!(
                m.dim % 16,
                0,
                "{} dim {} not divisible by 16 (IvfPq n_subvectors default)",
                m.name,
                m.dim
            );
        }
    }

    #[test]
    fn models_cache_dir_appends_models_segment() {
        use std::path::PathBuf;
        let root = PathBuf::from("/tmp/kb-cache");
        assert_eq!(models_cache_dir(&root), root.join("models"));
    }

    /// Pins the `max_length` plumb (`ModelInfo::max_length`'s doc) at the
    /// `TextInitOptions`-building layer — no ONNX session, no network, no
    /// model download: `build_text_init_options` returns a plain struct
    /// with `pub max_length: usize`, so this asserts the value directly.
    #[cfg(feature = "local-embedder")]
    #[test]
    fn max_length_plumbs_into_text_init_options_only_when_set() {
        // fastembed 5.13.4's own default (`text_embedding::
        // DEFAULT_MAX_LENGTH`, private to that crate, applied via
        // `EmbeddingModel`'s blanket `HasMaxLength` impl) — every model
        // without an explicit `ModelInfo::max_length` must see this
        // UNCHANGED default, never a blanket override.
        const FASTEMBED_DEFAULT_MAX_LENGTH: usize = 512;

        let tmp = tempfile::tempdir().unwrap();
        let bge = model_info("bge-small-en-v1.5").expect("bge-small registered");
        assert_eq!(bge.max_length, None);
        let bge_opts = build_text_init_options(bge, tmp.path().to_path_buf()).unwrap();
        assert_eq!(bge_opts.max_length, FASTEMBED_DEFAULT_MAX_LENGTH);

        let jina =
            model_info("jina-embeddings-v2-base-code").expect("jina-v2-base-code registered");
        assert_eq!(jina.max_length, Some(8192));
        let jina_opts = build_text_init_options(jina, tmp.path().to_path_buf()).unwrap();
        assert_eq!(jina_opts.max_length, 8192);
    }

    #[cfg(feature = "local-embedder")]
    #[test]
    fn embedder_new_rejects_unknown_model() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Embedder::new("not-a-real-model", tmp.path().to_path_buf())
            .err()
            .expect("should have failed for unknown model");
        assert!(err.to_string().contains("not-a-real-model"));
    }

    /// Guards the hand-maintained name→fastembed-enum maps against drift
    /// from the always-compiled registry tables. Only meaningful with the
    /// local backend compiled in.
    #[cfg(feature = "local-embedder")]
    #[test]
    fn every_registered_model_maps_to_a_fastembed_enum() {
        for m in SUPPORTED_MODELS {
            assert!(fastembed_model(m.name).is_ok(), "model {} unmapped", m.name);
        }
        for r in SUPPORTED_RERANKERS {
            assert!(
                fastembed_reranker(r.name).is_ok(),
                "reranker {} unmapped",
                r.name
            );
        }
    }

    // --- Live model tests (gated; require model download + onnxruntime) ---
    //
    // These tests download ~130 MB on first run (cached after) and require
    // ORT_DYLIB_PATH (or system onnxruntime). CI does NOT run them by
    // default; run locally with `cargo test --ignored -p kb-core`.

    #[cfg(feature = "local-embedder")]
    #[test]
    #[ignore = "downloads bge-small-en-v1.5 (~130 MB) and requires onnxruntime"]
    fn embed_one_produces_correct_dim() {
        let tmp = tempfile::tempdir().unwrap();
        let mut emb = Embedder::new("bge-small-en-v1.5", tmp.path().to_path_buf()).unwrap();
        let v = emb.embed_one("hello world").unwrap();
        assert_eq!(v.len(), 384);
        // Embeddings should be roughly L2-normalised (bge produces unit
        // vectors). Sum of squares ≈ 1.
        let norm_sq: f32 = v.iter().map(|x| x * x).sum();
        assert!((norm_sq - 1.0).abs() < 0.05, "norm² = {norm_sq}");
    }

    #[cfg(feature = "local-embedder")]
    #[test]
    #[ignore = "downloads bge-small-en-v1.5 (~130 MB) and requires onnxruntime"]
    fn embed_batch_handles_chunks_above_32() {
        let tmp = tempfile::tempdir().unwrap();
        let mut emb = Embedder::new("bge-small-en-v1.5", tmp.path().to_path_buf()).unwrap();
        let texts: Vec<String> = (0..50).map(|i| format!("doc number {i}")).collect();
        let out = emb.embed_batch(&texts).unwrap();
        assert_eq!(out.len(), 50);
        for v in &out {
            assert_eq!(v.len(), 384);
        }
    }

    #[cfg(feature = "local-embedder")]
    #[test]
    #[ignore = "downloads bge-small-en-v1.5 (~130 MB) and requires onnxruntime"]
    fn embed_one_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let mut emb = Embedder::new("bge-small-en-v1.5", tmp.path().to_path_buf()).unwrap();
        let a = emb.embed_one("repeat me").unwrap();
        let b = emb.embed_one("repeat me").unwrap();
        for (x, y) in a.iter().zip(b.iter()) {
            assert!(
                (x - y).abs() < 1e-5,
                "deterministic embed should be byte-equal"
            );
        }
    }

    /// P4b throughput verification (the roadmap's "verify on target hardware
    /// first"): is `embed_batch(N)` enough faster than `N × embed_one` to
    /// justify restructuring the per-doc cold-index pipeline into a batched
    /// one? Run with the production model:
    /// `cargo test -p kb-core --release bench_embed_batch_vs_one -- --ignored --nocapture`
    #[cfg(feature = "local-embedder")]
    #[test]
    #[ignore = "perf benchmark — uses the local bge model cache + onnxruntime"]
    fn bench_embed_batch_vs_one() {
        let cache =
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/kb/models");
        let model = std::env::var("KB_BENCH_MODEL").unwrap_or_else(|_| "bge-large-en-v1.5".into());
        let mut emb = Embedder::new(&model, cache).unwrap();
        let n = 32usize;
        let texts: Vec<String> = (0..n)
            .map(|i| format!("a representative artifact body number {i} with enough words to embed meaningfully across a few sentences"))
            .collect();
        let _ = emb.embed_one(&texts[0]).unwrap(); // warm the model

        let t0 = std::time::Instant::now();
        for t in &texts {
            let _ = emb.embed_one(t).unwrap();
        }
        let one = t0.elapsed();

        let t1 = std::time::Instant::now();
        let _ = emb.embed_batch(&texts).unwrap();
        let batch = t1.elapsed();

        eprintln!(
            "[P4b/{model}] {n}× embed_one: {one:?} ({:.1} emb/s) | embed_batch({n}): {batch:?} ({:.1} emb/s) | speedup {:.2}×",
            n as f64 / one.as_secs_f64(),
            n as f64 / batch.as_secs_f64(),
            one.as_secs_f64() / batch.as_secs_f64(),
        );
    }
}
