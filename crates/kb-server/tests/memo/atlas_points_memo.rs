//! M-a (v0.28+) — `GET /api/kb/{kb}/atlas/points` generation-keyed memo.
//! Boots the in-process daemon (the `facets_memo.rs` convention) with
//! `[server] metrics = true` so `/api/metrics`'s
//! `detailed.pipeline.storage` block exposes the storage actor's
//! per-`StorageKind` call counts — `list_docs_with_atlas` falls through to
//! `StorageKind::Read` (see `kb_core::metrics::StorageKind::from_msg`'s
//! catch-all), so a stable "read" count across two back-to-back
//! `/atlas/points` calls at the SAME index generation proves the second
//! call hit the memo instead of re-scanning the whole corpus again.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn doc(title: &str, category: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title>\
         <meta name=\"kb-category\" content=\"{category}\"></head>\
         <body><h1>{title}</h1><p>body text for {title}</p></body></html>"
    )
}

/// Tempdir corpus with two categorized docs (`alpha` / `beta`). Metrics are
/// enabled so `/api/metrics` surfaces the storage-actor read count.
async fn boot() -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.html"), doc("Doc A", "alpha")).unwrap();
    std::fs::write(source.join("b.html"), doc("Doc B", "beta")).unwrap();

    let daemon_name = format!(
        "test-atlas-points-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source.clone(),
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection {
            metrics: true,
            ..Default::default()
        },
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    (tmp, addr, source)
}

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

async fn atlas_points(client: &reqwest::Client, addr: std::net::SocketAddr) -> serde_json::Value {
    client
        .get(url(addr, "/api/kb/smoke/atlas/points"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// PF-F1 — `GET /docs?projection=atlas`, no `?offset=` so the response is
/// the legacy bare-array shape (every doc fits under `DEFAULT_PAGE_LIMIT`
/// on this fixture's tiny corpus, so no pagination concern either).
async fn atlas_projection_docs(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
) -> serde_json::Value {
    client
        .get(url(addr, "/api/kb/smoke/docs?projection=atlas"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Storage actor's cumulative "read" `StorageKind` count. `list_docs_with_
/// atlas` falls through `StorageKind::from_msg`'s catch-all, so this
/// counter only climbs when the atlas-points memo misses and re-scans —
/// PF-F1: so does `StorageMsg::AtlasSnapshotsList` (`atlas_frames`), which
/// `atlas_docs_cached`'s frame-freshness check now reads on EVERY call
/// (hit or miss), so a hit costs +1 here rather than +0 pre-PF-F1. See
/// `atlas_points_memo_hits_then_invalidates_on_new_doc`'s doc comment.
async fn read_count(client: &reqwest::Client, addr: std::net::SocketAddr) -> u64 {
    let body: serde_json::Value = client
        .get(url(addr, "/api/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let storage = body["detailed"]["pipeline"]["storage"]
        .as_array()
        .expect("detailed metrics must be enabled");
    storage
        .iter()
        .find(|v| v["kind"] == "read")
        .and_then(|v| v["count"].as_u64())
        .expect("a 'read' StorageKind entry")
}

async fn doc_ids(client: &reqwest::Client, addr: std::net::SocketAddr) -> Vec<String> {
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=100"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body.iter()
        .filter_map(|d| d["id"].as_str().map(str::to_string))
        .collect()
}

async fn wait_for_docs(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    what: &str,
    pred: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    common::poll_until(what, || async {
        let ids = doc_ids(client, addr).await;
        pred(&ids).then_some(ids)
    })
    .await
}

/// A second `/atlas/points` call at the same index generation must hit the
/// memo (byte-identical body, no full-corpus rescan), and a subsequent doc
/// upsert (which bumps the generation, invariant #15) must force a rebuild
/// that surfaces the new doc.
///
/// PF-F1 note on the read-count deltas below: a hit is no longer FREE the
/// way it was pre-PF-F1 — `atlas_docs_cached` now ALSO reads the newest
/// atlas-frame id on every call (the second half of its two-key freshness
/// check, needed so a recompute/recluster — which never bumps `generation`,
/// see `AtlasPointsCache`'s doc comment — can't hide behind this memo
/// forever). That read is one small indexed `SELECT … LIMIT 1`
/// (`StorageMsg::AtlasSnapshotsList`), which — like `list_docs_with_atlas`
/// — falls through `StorageKind::from_msg`'s catch-all to `Read`, so a hit
/// now costs exactly ONE "read" count tick and a miss costs exactly TWO
/// (the frame check plus the full rescan). The delta between the hit-delta
/// and the miss-delta is what actually proves the expensive scan was
/// skipped, so that's what's asserted below rather than an absolute zero.
#[tokio::test]
async fn atlas_points_memo_hits_then_invalidates_on_new_doc() {
    // MI test-hardening (2026-08) — serialize this binary's atlas tests:
    // each does a full corpus scan + PCA, the heaviest lance/datafusion
    // workload in this crate's suite, and running several concurrently has
    // produced memory-pool exhaustion under host load. See `common`'s
    // module doc for the full rationale + scope.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    // Warm the memo.
    let first = atlas_points(&client, addr).await;
    assert_eq!(first["total"].as_u64(), Some(2));
    let points = first["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);

    let after_first = read_count(&client, addr).await;

    // Same generation AND same atlas frame (no recompute happened): the
    // second call must be a memo hit — byte-identical body, and the ONLY
    // storage read is the cheap frame-freshness check (+1), never a second
    // full `list_docs_with_atlas` rescan (which would also show up as +1,
    // but stacked on top — see the miss delta below, which is +2).
    let second = atlas_points(&client, addr).await;
    let after_second = read_count(&client, addr).await;
    assert_eq!(second, first, "identical response on a memo hit");
    let hit_delta = after_second - after_first;
    assert_eq!(
        hit_delta, 1,
        "a repeat /atlas/points call at the same (generation, atlas frame) \
         must add exactly one storage read (the frame-freshness check), \
         never a second full corpus scan"
    );

    // Add a third doc — an UpsertDoc bumps the generation (invariant #15),
    // which must force the atlas-points memo to rebuild and pick up the
    // new doc rather than serving the stale 2-point snapshot.
    std::fs::write(source.join("c.html"), doc("Doc C", "gamma")).unwrap();
    wait_for_docs(&client, addr, "third doc indexed", |ids| ids.len() == 3).await;

    let third = atlas_points(&client, addr).await;
    assert_eq!(
        third["total"].as_u64(),
        Some(3),
        "the new doc must appear after the generation bump: {third}"
    );
    let after_third = read_count(&client, addr).await;
    let miss_delta = after_third - after_second;
    assert!(
        miss_delta > hit_delta,
        "the generation bump must force a rescan (memo miss, +2: frame \
         check + full scan) — strictly more storage reads than a same-\
         generation hit (+1: frame check only). hit_delta={hit_delta} \
         miss_delta={miss_delta}"
    );
}

/// The whole point of this route: it must return every doc in the corpus,
/// not just the paged gallery's default 200-doc window (the measured bug
/// this endpoint exists to fix). A small corpus can't reproduce >200 docs
/// in a fast test, so this pins the *shape* guarantee instead — `total`
/// always equals `points.len()`, and every cluster count in `clusters`
/// sums to at most `total` (never inflated, never silently dropped).
#[tokio::test]
async fn atlas_points_total_matches_points_len_and_cluster_counts_are_bounded() {
    // See `atlas_points_memo_hits_then_invalidates_on_new_doc` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    let body = atlas_points(&client, addr).await;
    let total = body["total"].as_u64().unwrap();
    let points = body["points"].as_array().unwrap();
    assert_eq!(total, points.len() as u64);

    let clusters = body["clusters"].as_array().unwrap();
    let cluster_sum: u64 = clusters.iter().map(|c| c["count"].as_u64().unwrap()).sum();
    assert!(
        cluster_sum <= total,
        "cluster counts must never exceed the total point count"
    );
}

// --- PF-F1: `?projection=atlas` reuses the SAME memo as `/atlas/points` ---

/// PF-F1 — before this fix `GET /docs?projection=atlas` ran its own
/// uncached `list_docs_with_atlas` scan on every request ("P3 owns atlas
/// perf" — the deferral this fixes). It now shares `routes::atlas::
/// atlas_docs_cached`, the SAME per-(kb, generation, atlas-frame) memo
/// `GET /atlas/points` warms — one memo, many consumers, not a second full
/// scan. Warming via ONE route must satisfy the OTHER for free.
///
/// The proof avoids hardcoding either route's exact per-request read count
/// (`?projection=atlas` also pays for its own unrelated `edge_counts()` +
/// `first_seen_for_ids()` reads, constant per request but not zero — call
/// that constant `k`): a cross-route hit (`/atlas/points` warms, then
/// `?projection=atlas` reads) must cost the SAME as a same-route repeat
/// (`?projection=atlas` twice), since both are hits against the identical
/// warm memo (`k + 1`, the `+1` being the shared frame-freshness check).
/// That equality alone isn't quite enough to rule out "neither route
/// caches at all" (then EVERY call costs the same flat `k + scan`, so the
/// two deltas would ALSO happen to match) — so a third measurement, after
/// a doc write forces a genuine memo MISS, closes the gap: a real hit must
/// cost strictly LESS than a real miss (`k + 1` vs `k + 2`, the full
/// `list_docs_with_atlas` rescan). An uncached implementation would show
/// all three deltas equal; a per-route-cached (not shared) implementation
/// would show the cross-route delta match the miss delta, not the hit
/// delta. Only "one shared, correctly-invalidated memo" produces the
/// pattern asserted here.
#[tokio::test]
async fn atlas_projection_docs_route_shares_the_atlas_points_memo() {
    // See `atlas_points_memo_hits_then_invalidates_on_new_doc` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    // Warm the shared memo via `/atlas/points`.
    let via_points = atlas_points(&client, addr).await;
    assert_eq!(via_points["total"].as_u64(), Some(2));
    let after_points = read_count(&client, addr).await;

    // Cross-route hit: `?projection=atlas` must return the same 2 docs
    // without a fresh scan (the memo `/atlas/points` just warmed).
    let via_docs = atlas_projection_docs(&client, addr).await;
    let via_docs_arr = via_docs
        .as_array()
        .expect("bare array shape without ?offset=");
    assert_eq!(
        via_docs_arr.len(),
        2,
        "?projection=atlas must return every doc: {via_docs}"
    );
    let after_docs = read_count(&client, addr).await;
    let cross_route_hit_delta = after_docs - after_points;

    // Same-route repeat: a guaranteed hit against `?projection=atlas`'s own
    // just-warmed cache entry.
    let via_docs_repeat = atlas_projection_docs(&client, addr).await;
    assert_eq!(
        via_docs_repeat, via_docs,
        "a repeat ?projection=atlas call at the same generation must be \
         byte-identical"
    );
    let after_docs_repeat = read_count(&client, addr).await;
    let same_route_hit_delta = after_docs_repeat - after_docs;

    assert_eq!(
        cross_route_hit_delta, same_route_hit_delta,
        "warming via /atlas/points then hitting ?projection=atlas \
         (cross_route_hit_delta={cross_route_hit_delta}) must cost exactly \
         what two consecutive ?projection=atlas hits cost \
         (same_route_hit_delta={same_route_hit_delta})"
    );

    // Force a genuine miss: an UpsertDoc bumps the generation (invariant
    // #15), so this call MUST rescan.
    std::fs::write(source.join("c.html"), doc("Doc C", "gamma")).unwrap();
    wait_for_docs(&client, addr, "third doc indexed", |ids| ids.len() == 3).await;

    let via_docs_after_write = atlas_projection_docs(&client, addr).await;
    assert_eq!(
        via_docs_after_write.as_array().unwrap().len(),
        3,
        "the new doc must appear: {via_docs_after_write}"
    );
    let after_write = read_count(&client, addr).await;
    let miss_delta = after_write - after_docs_repeat;

    assert!(
        miss_delta > same_route_hit_delta,
        "a memo MISS (after a doc write) must cost strictly more than a \
         HIT — otherwise this test can't tell 'shares one memo' apart from \
         'neither route caches at all'. same_route_hit_delta=\
         {same_route_hit_delta} miss_delta={miss_delta}"
    );
}

/// PF-F1 — mirrors `atlas_points_memo_hits_then_invalidates_on_new_doc` for
/// the NEW `?projection=atlas` consumer of the shared memo: a doc write
/// (which bumps the generation, invariant #15) must force a rebuild that
/// surfaces the new doc, never a stale 2-doc snapshot.
#[tokio::test]
async fn atlas_projection_docs_invalidates_on_new_doc() {
    // See `atlas_points_memo_hits_then_invalidates_on_new_doc` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    let first = atlas_projection_docs(&client, addr).await;
    assert_eq!(
        first.as_array().unwrap().len(),
        2,
        "?projection=atlas must return every doc: {first}"
    );

    std::fs::write(source.join("c.html"), doc("Doc C", "gamma")).unwrap();
    wait_for_docs(&client, addr, "third doc indexed", |ids| ids.len() == 3).await;

    let second = atlas_projection_docs(&client, addr).await;
    assert_eq!(
        second.as_array().unwrap().len(),
        3,
        "the new doc must appear after the generation bump, not a stale \
         2-doc snapshot: {second}"
    );
}

// --- PF-F1: the stale-coords fix — an atlas recompute invalidates --------
//
// A REAL coordinate change needs a real embedder (`list_embeddings()` must
// be non-empty for `recompute_for_kb_with` to ever call `update_atlas` /
// write a new `atlas_snapshots` frame), which this crate's test harness
// deliberately doesn't wire up — see `atlas_recompute_emits_start_and_
// complete`'s doc comment in `end_to_end.rs`: "this fixture corpus has no
// embedder wired up, so `atlas_recompute` always sees an empty
// `list_embeddings()` and returns early... a `record_atlas_snapshot` call
// (and therefore any real frame) is unreachable from this suite." That
// same comment names the established split for this exact gap: the
// populated (real frame) path is proven at the PURE level —
// `atlas_cache_is_fresh_requires_both_generation_and_frame_id_to_match` in
// `routes::atlas::tests` (kb-server/src/routes/atlas.rs) pins that a
// changed `atlas_frame_id` (what a real recompute produces) misses the
// memo even at an unchanged `generation`, which is the entire PF-F1 fix.
// What CAN run here, against this fixture, is the other half of the
// contract: a recompute that changes NOTHING (no embeddings to lay out)
// must not spuriously invalidate the memo either.

/// PF-F1 — a `POST /atlas/recompute` that finds no embeddings is a TRUE
/// no-op (early return before `update_atlas`/`record_atlas_snapshot`); the
/// atlas-docs memo's new frame-id key must correctly recognise "nothing
/// changed" and keep hitting, not spuriously rescan on every recompute
/// call regardless of outcome.
#[tokio::test]
async fn atlas_recompute_with_no_embeddings_does_not_invalidate_the_memo() {
    // See `atlas_points_memo_hits_then_invalidates_on_new_doc` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    let first = atlas_points(&client, addr).await;

    let resp = client
        .post(url(addr, "/api/kb/smoke/atlas/recompute"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    // Let the spawned (no-op) task finish — same wait `atlas_recompute_
    // emits_start_and_complete` uses in end_to_end.rs. The recompute path
    // itself makes a few incidental "read"-classified storage calls of its
    // OWN (`count_rows`, `newest_frame_id`, `list_embeddings` — none of
    // them `atlas_docs_cached`), so the baseline for isolating the NEXT
    // `atlas_points` call's own cost is taken AFTER settling, not before.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_recompute = read_count(&client, addr).await;

    let second = atlas_points(&client, addr).await;
    assert_eq!(
        second, first,
        "a no-op recompute (no embeddings to lay out) must not change the \
         atlas response: {second}"
    );
    let after_second = read_count(&client, addr).await;
    assert_eq!(
        after_second - after_recompute,
        1,
        "a no-op recompute writes no new atlas frame, so the next \
         /atlas/points call must still be a memo HIT (+1: frame-freshness \
         check only), never a spurious +2 rescan"
    );
}
