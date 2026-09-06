//! HTTP route handlers — topic 11 §B endpoints.

use std::path::Path;

/// Resolve the `{kb}` path segment to its [`KbContext`], or the problem+json
/// response to return on a malformed name (400) or unknown kb (404). D2 —
/// centralises the `KbName::new` + `kbs.get` preamble that was copy-pasted
/// across ~50 handlers (and had begun to drift). Returns the parsed `KbName`
/// too so handlers that log / format it don't re-parse. Usage:
/// `let (kb_name, ctx) = match resolve_kb(&state, &kb) { Ok(v) => v, Err(r) => return r };`
///
/// The `Err` variant is a fully-built `Response` (rather than a `kb_core::Error`)
/// so handlers can `return` it directly; that response is large, but the error
/// path is the rare malformed-name / unknown-kb case, so the
/// `result_large_err` cost is accepted over boxing it at every call site.
#[allow(clippy::result_large_err)]
pub(crate) fn resolve_kb<'a>(
    state: &'a crate::state::KbHandles,
    kb: &str,
) -> Result<
    (kb_core::types::KbName, &'a crate::state::KbContext),
    axum::response::Response<axum::body::Body>,
> {
    let kb_name = kb_core::types::KbName::new(kb)
        .map_err(|e| crate::middleware::error_to_problem_json(&e))?;
    match state.kbs.get(&kb_name) {
        Some(ctx) => Ok((kb_name, ctx)),
        None => Err(crate::middleware::error_to_problem_json(
            &kb_core::Error::NotFound(format!("kb {kb_name}")),
        )),
    }
}

/// Reject artifact ids that could traverse out of per-kb state dirs
/// (`.review/<id>.json`, attachment dirs) or inject FS metacharacters.
/// `kb_core::ids::ArtifactId` is hex (12 chars) but these API surfaces
/// accept any caller-provided id; allow alphanumerics + dash +
/// underscore + dot, reject `..` and leading/trailing dots. Shared by
/// the review, comments, attachments and history routes — `versions`
/// keeps its own STRICTER `is_hex_artifact_id` (exactly the 12-hex SHA
/// prefix), so don't "unify" that one into this.
pub(crate) fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !id.starts_with('.')
        && !id.ends_with('.')
}

/// Map a file extension to a static Content-Type. Shared by the SPA
/// ServeDir fallback (`spa`) and the artifact-subdomain server
/// (`artifact`) — one function so the two can't drift (they did: the
/// artifact copy was missing `mjs`/`ico`/`map`/`txt` and served them
/// as `application/octet-stream`).
pub(crate) fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Run per-corpus futures with bounded concurrency (≤ `cap` in flight),
/// collecting results in **submission order** (the order of `futs`).
///
/// Every federated ("scope=all" / fleet) read handler has the same shape: one
/// async call per corpus, results merged. Done serially the latency grows
/// linearly with corpus count. `buffered_join` runs up to `cap` of those
/// futures concurrently and yields their results in submission order — built on
/// `buffered`, NOT `buffer_unordered`, because the merge is order-sensitive:
/// RRF arm order, first-wins merge maps, and relevance-mode output all depend on
/// deterministic submission order (invariants 10/11). A completion-order collect
/// would silently flip them under load.
///
/// Callers build one boxed future per corpus (capturing `&ctx` plus any shared
/// borrows under a single lifetime — so no higher-ranked closure is needed) and
/// bake the kb name into each future's output so it travels with the result.
/// Each future returns its corpus's partial; the caller folds and DROPS failures,
/// preserving the per-corpus skip-on-error isolation every handler already has —
/// a future must NEVER `?`-propagate a per-corpus error (that would 500 the whole
/// fleet view).
///
/// Invariant 15: inside each future, take any `std::sync::Mutex` guard, extract
/// the owned value, and DROP the guard before the first `.await`.
pub(crate) async fn buffered_join<'f, T: Send + 'f>(
    futs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'f>>>,
    cap: usize,
) -> Vec<T> {
    use futures::stream::StreamExt;
    futures::stream::iter(futs)
        .buffered(cap.max(1))
        .collect()
        .await
}

/// Default concurrency cap for [`buffered_join`]: at most this many corpora
/// are queried at once, bounding the `spawn_blocking` pool + embedder IPC
/// pressure and keeping the daemon a fair neighbour when several share one
/// host. PF-R1 — this literal is now also the **default** of the operator-
/// configurable `[server] fanout_cap` (`crate::state::KbHandles::fanout_cap`,
/// resolved at boot/restart, see docs/configuration.md), so an unconfigured
/// daemon is byte-identical either way. Every federated `buffered_join` call
/// site across every file under `routes/` — INCLUDING `routes/sessions.rs`
/// (migrated last, ~48 call sites) — now reads `state.fanout_cap` instead of
/// this constant directly; the PF-R1 migration is complete, with no pending
/// exceptions. This constant remains the one true default value + the
/// literal `KbHandles::new()` seeds `fanout_cap` from before the real
/// config loads — never let it drift from `ServerSection::default_fanout_cap()`.
pub(crate) const FANOUT_CAP: usize = 8;

/// One boxed per-corpus future for [`buffered_join`], borrowing for `'a`. Saves
/// each federated handler from spelling out the `Pin<Box<dyn Future …>>` (which
/// trips `clippy::type_complexity`).
pub(crate) type CorpusFut<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub mod anchors;
pub mod artifact;
pub mod artifacts;
pub mod atlas;
pub mod atlas_field;
pub mod attachments;
pub mod boards;
pub mod capture;
pub mod coderefs;
pub mod comments;
pub mod compact;
pub mod config;
pub mod context;
pub mod daycard;
pub mod desk;
pub mod dispatch;
pub mod docs;
pub mod download;
pub mod drop;
pub mod echoes;
pub mod errors;
pub mod events;
pub mod exclusions;
pub mod facets;
pub mod folders;
pub mod graph;
pub mod health;
pub mod history;
pub mod identity;
pub mod inbox;
pub mod kbs;
pub mod links;
pub mod lists;
pub mod log_level;
pub mod lookup;
pub mod memory;
pub mod memory_links;
pub mod metrics;
pub mod notes;
pub mod prompt;
pub mod proposals;
pub mod quarantine;
pub mod reindex;
pub mod relocate;
pub mod resurface;
pub mod review;
pub mod saved_queries;
pub mod schema;
pub mod search;
pub mod sessions;
pub mod settings;
pub mod share;
pub mod shutdown;
pub mod slates;
pub mod slo;
pub mod sources;
pub mod spa;
pub mod stats;
pub mod tags;
pub mod timeline;
pub mod users;
pub mod versions;

#[cfg(test)]
mod tests {
    use super::is_safe_id;

    #[test]
    fn is_safe_id_accepts_hex_and_slugs() {
        assert!(is_safe_id("f5919686f042"));
        assert!(is_safe_id("kitchen-sink"));
        assert!(is_safe_id("multi_page-01"));
    }

    #[test]
    fn is_safe_id_rejects_traversal() {
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("../escape"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id(".hidden"));
        assert!(!is_safe_id("trailing."));
        assert!(!is_safe_id("foo..bar"));
    }

    #[test]
    fn guess_content_type_covers_pwa_manifest_and_icons() {
        use super::guess_content_type;
        use std::path::Path;
        // The PWA manifest must be served as application/manifest+json, NOT the
        // octet-stream fallback — the SPA handler (spa.rs) sets Content-Type
        // from this map, so a missing arm silently breaks PWA install.
        assert_eq!(
            guess_content_type(Path::new("manifest.webmanifest")),
            "application/manifest+json"
        );
        assert_eq!(guess_content_type(Path::new("icon-512.png")), "image/png");
        assert_eq!(
            guess_content_type(Path::new("logomark.svg")),
            "image/svg+xml"
        );
    }

    use super::buffered_join;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

    // invariant:28 submission-order
    #[tokio::test]
    async fn buffered_join_preserves_submission_order_not_completion_order() {
        // Earlier futures sleep LONGER, so completion order is the reverse of
        // submission order. `buffered` must still yield in submission order —
        // this is the determinism property the whole milestone rests on.
        let mut futs: Vec<BoxFut<u8>> = Vec::new();
        for k in 0u8..6 {
            let ms = (6 - k as u64) * 5;
            futs.push(Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                k
            }));
        }
        let out = buffered_join(futs, 8).await;
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn buffered_join_caps_concurrency() {
        // cap=3, N=6 → exactly two full barrier cycles of 3. A Barrier of size
        // `cap` only releases when `cap` futures are simultaneously in flight,
        // proving `buffered` runs `cap` concurrently; the timeout converts an
        // under-parallelised (would-deadlock) failure into a clean assertion.
        const CAP: usize = 3;
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(CAP));
        let mut futs: Vec<BoxFut<()>> = Vec::new();
        for _ in 0..(CAP * 2) {
            let inflight = inflight.clone();
            let max_seen = max_seen.clone();
            let barrier = barrier.clone();
            futs.push(Box::pin(async move {
                let cur = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(cur, Ordering::SeqCst);
                barrier.wait().await;
                inflight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        let res =
            tokio::time::timeout(std::time::Duration::from_secs(5), buffered_join(futs, CAP)).await;
        assert!(
            res.is_ok(),
            "buffered_join did not reach cap concurrency within timeout"
        );
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            CAP,
            "concurrency should reach exactly the cap"
        );
    }

    // invariant:28 fold-drop-isolation
    #[tokio::test]
    async fn buffered_join_fold_drops_failing_corpora() {
        // Each future returns (k, Result); the CALLER folds Ok and drops Err —
        // the per-corpus skip-on-error isolation every handler relies on.
        let mut futs: Vec<BoxFut<(u8, Result<u8, &'static str>)>> = Vec::new();
        for (k, ok) in [(0u8, true), (1, false), (2, true)] {
            futs.push(Box::pin(async move {
                (k, if ok { Ok(k) } else { Err("boom") })
            }));
        }
        let out = buffered_join(futs, 8).await;
        let survivors: Vec<u8> = out.into_iter().filter_map(|(_, r)| r.ok()).collect();
        assert_eq!(survivors, vec![0, 2]); // corpus 1 dropped; 0 and 2 survive, in order
    }
}
