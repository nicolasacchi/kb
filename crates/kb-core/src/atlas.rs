//! Atlas — 2-D layout of artifact embeddings + k-means cluster labels.
//!
//! v0.5 ships TWO layout algorithms with a unified `compute_layout`:
//!
//! 1. **UMAP-ish (`umap_layout`)** — brute-force KNN (k=15), random
//!    init, 200 SGD iterations with attractive (positive-edge) +
//!    repulsive (negative-sample) forces. Closer to LargeVis than
//!    canonical UMAP (no fuzzy simplicial set construction; cross-
//!    entropy loss replaced with a force-directed approximation),
//!    but produces visibly better cluster separation than PCA on
//!    embedding distributions where the principal components don't
//!    align with semantic structure. Tried first.
//!
//! 2. **PCA fallback (`pca_layout`)** — the v0.3 path. Center →
//!    top-2 power iteration → project → normalise. Deterministic +
//!    cheap (~O(nd) per iteration). Used when the operator picks
//!    `layout = "pca"` in kb.toml OR when UMAP returns degenerate
//!    output (any NaN, all points coincident, fewer than 3 distinct
//!    coordinates).
//!
//! Clusters: seeded k-means (Lloyd's, ≤12 clusters). Empty clusters
//! reseed to a random unassigned point on every iteration (v0.4
//! deferred). Run on the original embeddings, not the projected
//! coords — semantic similarity lives in the high-dim space.
//!
//! Determinism: same `(embeddings, k_clusters, seed)` → same
//! `Vec<AtlasPoint>` across machines (invariant #7). The whole path is
//! IEEE-754 add/sub/mul/div/sqrt only — no transcendentals, which are
//! libm-implementation-defined and would drift glibc↔musl. CI
//! verifies via `deterministic_under_same_seed`,
//! `umap_layout_bit_identical_across_runs_on_large_corpus`, and the
//! pinned-bits golden `pca_layout_golden_coords_pin_cross_libc_determinism`.
//!
//! Numeric strategy: f32 throughout. Output coordinates normalised
//! to `[0, 1]` so the SPA can scale into any viewport without math.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtlasPoint {
    pub x: f32,
    pub y: f32,
    pub cluster: i16,
}

/// PCA convergence iterations per component. 100 is overkill for
/// 384-dim embeddings but keeps determinism stable across machines.
const PCA_ITERS: usize = 100;
/// Max k-means iterations before bail. Lloyd's converges fast for
/// well-separated clusters; 50 is a generous cap.
const KMEANS_ITERS: usize = 50;
/// Cluster id 0 is reserved for "uncategorised" (single-doc kbs, etc.).
pub const MAX_CLUSTERS: usize = 12;
/// UMAP-ish KNN neighborhood size. 15 is the umap-learn default and
/// the v0.5 KNN code is brute-force O(n²) — fine for ≤1000-doc kb.
const UMAP_K: usize = 15;
/// P3 — corpus-size cap for the UMAP path. Its brute-force KNN is
/// O(n²·d) (each of n rows scans all n for nearest neighbours), so it
/// walls hard once a kb grows past a few thousand docs — at the 50k
/// ceiling the S-milestone raised it to, the KNN alone is ~1e15 float
/// ops. Above this cap `compute_layout_with` falls back to the
/// deterministic O(n·d) PCA path (overriding an explicit `layout =
/// "umap"` — availability over the layout preference; below the cap
/// UMAP is honoured). 4000 keeps a recompute to a few seconds while
/// preserving UMAP for every typical corpus. Determinism (invariant #3)
/// also rules out an approximate-NN swap — ANN ordering isn't
/// bit-reproducible across builds — so PCA is the only safe fallback.
const UMAP_MAX_N: usize = 4000;
/// SGD iterations for the UMAP-ish layout. 200 is plenty for the
/// embedding scales kb sees (4-1000 docs).
const UMAP_ITERS: usize = 200;
/// Negative samples per positive-edge update. UMAP-learn defaults to
/// 5; we follow.
const UMAP_NEG_SAMPLES: usize = 5;
/// Initial learning rate for the UMAP SGD. Decays linearly to 0.
const UMAP_LR_INIT: f32 = 1.0;

/// Operator-overrideable layout choice. `Umap` is the v0.5 default;
/// kb.toml `[kb.foo.atlas] layout = "pca"` flips back to the v0.3
/// PCA path when UMAP's output isn't suitable for a particular kb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Umap,
    Pca,
}

impl LayoutKind {
    /// Parse the kb.toml string. Unknown values fall back to Umap +
    /// log a warning at the call site.
    pub fn from_config(s: Option<&str>) -> Self {
        match s.map(|x| x.to_ascii_lowercase()) {
            Some(ref v) if v == "pca" => LayoutKind::Pca,
            _ => LayoutKind::Umap,
        }
    }
}

/// Compute the atlas layout. Returns one `AtlasPoint` per input
/// embedding, in the same order. `k_clusters` is clamped to
/// `[1, MAX_CLUSTERS]` and to `embeddings.len()`. `seed` controls
/// k-means initialisation + UMAP/PCA random init.
///
/// Equivalent to `compute_layout_with(embeddings, k_clusters, seed,
/// LayoutKind::Umap)`. Use the `_with` variant for explicit control.
pub fn compute_layout(embeddings: &[Vec<f32>], k_clusters: usize, seed: u64) -> Vec<AtlasPoint> {
    compute_layout_with(embeddings, k_clusters, seed, LayoutKind::Umap)
}

/// `compute_layout` with explicit layout choice. UMAP tries first
/// (when chosen) + falls back to PCA on degenerate output. PCA is
/// always deterministic given (embeddings, seed).
pub fn compute_layout_with(
    embeddings: &[Vec<f32>],
    k_clusters: usize,
    seed: u64,
    layout: LayoutKind,
) -> Vec<AtlasPoint> {
    let n = embeddings.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![AtlasPoint {
            x: 0.5,
            y: 0.5,
            cluster: 0,
        }];
    }
    let d = embeddings[0].len();
    if d == 0 {
        // Degenerate case — empty vectors.
        return embeddings
            .iter()
            .map(|_| AtlasPoint {
                x: 0.5,
                y: 0.5,
                cluster: 0,
            })
            .collect();
    }

    // Compute the 2-D coords first; clusters are independent.
    let mut rng = SeededRng::new(seed);
    // P3 — cap the O(n²·d) UMAP path. Above UMAP_MAX_N, take the
    // deterministic O(n·d) PCA route instead (even when the operator
    // asked for UMAP). Decided here, before the match, so the fallback
    // takes the clean `Pca` arm (rng = seed, NOT the seed+1 the
    // degenerate-UMAP fallback uses) and is therefore bit-identical to
    // an explicit `LayoutKind::Pca` request — keeps the cap off the
    // determinism golden path.
    let effective_layout = if layout == LayoutKind::Umap && n > UMAP_MAX_N {
        tracing::info!(
            n,
            cap = UMAP_MAX_N,
            "atlas: corpus exceeds the UMAP cap; using PCA to avoid the O(n²) brute-force KNN"
        );
        LayoutKind::Pca
    } else {
        layout
    };
    let coords = match effective_layout {
        LayoutKind::Umap => umap_layout(embeddings, &mut rng).unwrap_or_else(|| {
            // Same RNG state continuation isn't load-bearing here — PCA
            // re-seeds its own iteration vectors.
            let mut pca_rng = SeededRng::new(seed.wrapping_add(1));
            pca_layout(embeddings, &mut pca_rng)
        }),
        LayoutKind::Pca => pca_layout(embeddings, &mut rng),
    };

    // K-means on the original (uncentered) embeddings — clusters are
    // about semantic similarity in the original space, not in the
    // projected one.
    let k = k_clusters.clamp(1, MAX_CLUSTERS).min(n);
    let labels = kmeans_lloyd(embeddings, k, &mut rng);

    coords
        .into_iter()
        .zip(labels)
        .map(|((x, y), cluster)| AtlasPoint {
            x,
            y,
            cluster: cluster as i16,
        })
        .collect()
}

/// PCA path lifted unchanged from v0.3. Returns `[(x, y); n]` —
/// k-means runs separately on the original embeddings.
fn pca_layout(embeddings: &[Vec<f32>], rng: &mut SeededRng) -> Vec<(f32, f32)> {
    let n = embeddings.len();
    let d = embeddings[0].len();

    // Center the embeddings (subtract per-dim mean).
    let mut mean = vec![0.0f32; d];
    for e in embeddings {
        for (i, v) in e.iter().enumerate() {
            mean[i] += v;
        }
    }
    for v in &mut mean {
        *v /= n as f32;
    }
    let centered: Vec<Vec<f32>> = embeddings
        .iter()
        .map(|e| e.iter().zip(&mean).map(|(a, b)| a - b).collect())
        .collect();

    let v1 = power_iteration(&centered, d, rng, None);
    let v2 = power_iteration(&centered, d, rng, Some(&v1));

    let mut xs: Vec<f32> = centered.iter().map(|e| dot(e, &v1)).collect();
    let mut ys: Vec<f32> = centered.iter().map(|e| dot(e, &v2)).collect();
    normalise_to_unit(&mut xs);
    normalise_to_unit(&mut ys);

    xs.into_iter().zip(ys).collect()
}

/// UMAP-ish layout: brute-force KNN graph + force-directed SGD.
/// Returns `None` when the result is degenerate (any NaN, all
/// coincident, or n < 3 — for tiny corpora the PCA path is enough).
/// Caller falls back to PCA on `None`.
fn umap_layout(embeddings: &[Vec<f32>], rng: &mut SeededRng) -> Option<Vec<(f32, f32)>> {
    let n = embeddings.len();
    if n < 3 {
        return None;
    }
    let k = UMAP_K.min(n - 1);

    // 1. Brute-force KNN. For each i, find the k nearest j by
    //    Euclidean distance (squared, since we only use ordering).
    let mut neighbors: Vec<Vec<usize>> = Vec::with_capacity(n);
    for (i, ei) in embeddings.iter().enumerate() {
        let mut dists: Vec<(usize, f32)> = embeddings
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, ej)| (j, sq_dist(ei, ej)))
            .collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        neighbors.push(dists.into_iter().take(k).map(|(j, _)| j).collect());
    }
    // O(1) membership for the negative-sampling skip below — the inner
    // loop would otherwise do a linear `Vec::contains` per sample
    // (v0.7.1 H4). Iteration order still comes from `neighbors`, so this
    // doesn't perturb determinism (invariant #7).
    let neighbor_sets: Vec<std::collections::HashSet<usize>> = neighbors
        .iter()
        .map(|ns| ns.iter().copied().collect())
        .collect();

    // 2. Initialise 2-D coords randomly in [-0.5, 0.5].
    let mut coords: Vec<(f32, f32)> = (0..n)
        .map(|_| (rng.next_unit() - 0.5, rng.next_unit() - 0.5))
        .collect();

    // 3. SGD optimization. For each iteration, scan every (i, j) edge
    //    in the KNN graph; pull (attract) the pair closer; sample
    //    UMAP_NEG_SAMPLES random non-neighbors of i; push (repel)
    //    those. Learning rate decays linearly.
    for iter in 0..UMAP_ITERS {
        let lr = UMAP_LR_INIT * (1.0 - iter as f32 / UMAP_ITERS as f32);
        for (i, neighs) in neighbors.iter().enumerate() {
            for &j in neighs {
                let (xi, yi) = coords[i];
                let (xj, yj) = coords[j];
                let (dx, dy) = (xj - xi, yj - yi);
                let dist2 = dx * dx + dy * dy + 1e-3;
                let attr = lr / (1.0 + dist2);
                coords[i].0 += attr * dx;
                coords[i].1 += attr * dy;
                coords[j].0 -= attr * dx;
                coords[j].1 -= attr * dy;
            }
            // Negative samples — random points that aren't i's
            // neighbours. Push i away from each.
            for _ in 0..UMAP_NEG_SAMPLES {
                let neg = (rng.next_u32() as usize) % n;
                if neg == i || neighbor_sets[i].contains(&neg) {
                    continue;
                }
                let (xi, yi) = coords[i];
                let (xn, yn) = coords[neg];
                let (dx, dy) = (xn - xi, yn - yi);
                let dist2 = dx * dx + dy * dy + 1e-3;
                let rep = lr / (dist2 * (1.0 + dist2));
                coords[i].0 -= rep * dx;
                coords[i].1 -= rep * dy;
            }
        }
    }

    // 4. Normalise to [0, 1] + check for degeneracy.
    let mut xs: Vec<f32> = coords.iter().map(|(x, _)| *x).collect();
    let mut ys: Vec<f32> = coords.iter().map(|(_, y)| *y).collect();
    if xs.iter().chain(ys.iter()).any(|v| v.is_nan()) {
        return None;
    }
    normalise_to_unit(&mut xs);
    normalise_to_unit(&mut ys);
    // Guard against the "all points landed at the same spot" case —
    // normalise_to_unit returns 0.5 for every point in that case.
    // v0.7.1 H4: quantise at 1e-6, not the old 1e-3 — at 1e-3 a
    // valid-but-tight layout of near-identical artifacts (e.g. the
    // 4-6-doc canon corpus) fell under the `< 3` distinct-buckets
    // threshold and was wrongly discarded for the PCA path even though
    // UMAP had converged fine.
    let distinct = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| ((x * 1e6) as i32, (y * 1e6) as i32))
        .collect::<std::collections::HashSet<_>>()
        .len();
    if distinct < 3 {
        return None;
    }
    Some(xs.into_iter().zip(ys).collect())
}

// --- linear algebra helpers ----------------------------------------------

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(v: &[f32]) -> f32 {
    dot(v, v).sqrt()
}

fn normalise(v: &mut [f32]) {
    let n = norm(v);
    if n > 1e-12 {
        for x in v {
            *x /= n;
        }
    }
}

/// Power iteration for the top eigenvector of the covariance matrix
/// `Cov = X^T X / (n - 1)`, where `X` is the centered embedding matrix.
/// We never materialise `Cov`; instead each iteration computes
/// `v_{k+1} = X^T (X v_k)`, which costs `O(n d)` instead of `O(d^2)`.
///
/// `deflate_against`: when computing the second component, project the
/// candidate vector orthogonal to the first principal component each
/// iteration (Gram-Schmidt deflation).
fn power_iteration(
    centered: &[Vec<f32>],
    d: usize,
    rng: &mut SeededRng,
    deflate_against: Option<&Vec<f32>>,
) -> Vec<f32> {
    let mut v: Vec<f32> = (0..d).map(|_| rng.next_unit_normal()).collect();
    normalise(&mut v);
    for _ in 0..PCA_ITERS {
        // u = X v  (n-vector of projections)
        let u: Vec<f32> = centered.iter().map(|row| dot(row, &v)).collect();
        // v_next = X^T u  (d-vector)
        let mut next = vec![0.0f32; d];
        for (i, row) in centered.iter().enumerate() {
            let ui = u[i];
            for (j, x) in row.iter().enumerate() {
                next[j] += ui * x;
            }
        }
        if let Some(prev) = deflate_against {
            // Subtract projection onto prev so we converge to a
            // direction orthogonal to it.
            let proj = dot(&next, prev);
            for (a, b) in next.iter_mut().zip(prev) {
                *a -= proj * b;
            }
        }
        normalise(&mut next);
        v = next;
    }
    v
}

fn normalise_to_unit(values: &mut [f32]) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in values.iter() {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    let range = max - min;
    if range < 1e-12 {
        for v in values {
            *v = 0.5;
        }
    } else {
        for v in values {
            *v = (*v - min) / range;
        }
    }
}

// --- k-means (Lloyd's) ---------------------------------------------------

fn kmeans_lloyd(points: &[Vec<f32>], k: usize, rng: &mut SeededRng) -> Vec<usize> {
    let n = points.len();
    if k <= 1 {
        return vec![0; n];
    }
    let d = points[0].len();
    // Initialise centroids by sampling k distinct points.
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    let mut taken = std::collections::HashSet::new();
    while centroids.len() < k {
        let i = (rng.next_u32() as usize) % n;
        if taken.insert(i) {
            centroids.push(points[i].clone());
        }
    }
    let mut labels = vec![0usize; n];
    for _ in 0..KMEANS_ITERS {
        let mut changed = false;
        // Assign each point to the nearest centroid.
        for (i, p) in points.iter().enumerate() {
            let best = nearest_centroid(p, &centroids);
            if labels[i] != best {
                labels[i] = best;
                changed = true;
            }
        }
        // Recompute centroids.
        let mut sums: Vec<Vec<f32>> = vec![vec![0.0; d]; k];
        let mut counts = vec![0u32; k];
        for (i, p) in points.iter().enumerate() {
            let c = labels[i];
            for (j, x) in p.iter().enumerate() {
                sums[c][j] += x;
            }
            counts[c] += 1;
        }
        for c in 0..k {
            if counts[c] > 0 {
                let n = counts[c] as f32;
                for x in &mut sums[c] {
                    *x /= n;
                }
                centroids[c] = std::mem::take(&mut sums[c]);
            } else {
                // v0.5 — reseed empty clusters to a random unassigned
                // point. Without this, the cluster stays orphaned for
                // the rest of the run + the kb loses one of its
                // available colors.
                let mut tries = 0;
                loop {
                    let i = (rng.next_u32() as usize) % n;
                    // Prefer a point that isn't already serving as
                    // someone else's centroid; bail after 8 tries to
                    // avoid infinite loops on tiny corpora.
                    let already_centroid = centroids
                        .iter()
                        .enumerate()
                        .any(|(other, ce)| other != c && ce == &points[i]);
                    if !already_centroid || tries >= 8 {
                        centroids[c] = points[i].clone();
                        // Also flip its label so the next iteration's
                        // assign step sees the new centroid as having
                        // at least one member.
                        labels[i] = c;
                        changed = true;
                        break;
                    }
                    tries += 1;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // v0.7.1 H3 — one final assign pass so `labels` is consistent with
    // the final `centroids`. The loop's last iteration may have reseeded
    // an empty cluster (mutating `centroids` + a single label) with no
    // following assign step, and a non-converged exit leaves `labels`
    // one recompute stale. A pure O(nk) pass closes both gaps; the
    // reseeded centroid IS one of the points, so that point re-binds to
    // it (distance 0) and the cluster keeps at least one member.
    for (i, p) in points.iter().enumerate() {
        labels[i] = nearest_centroid(p, &centroids);
    }
    labels
}

/// Index of the centroid nearest to `p` (squared-Euclidean). Ties go to
/// the lowest index — `<` is strict, so the first minimum wins.
fn nearest_centroid(p: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut best = 0usize;
    let mut best_dist = f32::INFINITY;
    for (c, centroid) in centroids.iter().enumerate() {
        let dist = sq_dist(p, centroid);
        if dist < best_dist {
            best_dist = dist;
            best = c;
        }
    }
    best
}

fn sq_dist(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

// --- seeded RNG ----------------------------------------------------------

/// Tiny deterministic PRNG (xorshift64). We only need seeded uniforms +
/// approximate normals for power-iteration init + k-means seed picks;
/// pulling in `rand` is overkill for this volume of randomness.
struct SeededRng {
    state: u64,
}

impl SeededRng {
    fn new(seed: u64) -> Self {
        // xorshift64 doesn't tolerate seed=0; smear with a constant.
        Self {
            state: seed.wrapping_add(0x9e3779b97f4a7c15),
        }
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        (x >> 32) as u32
    }

    fn next_unit(&mut self) -> f32 {
        // Map u32 → [0, 1).
        (self.next_u32() as f32) / (u32::MAX as f32 + 1.0)
    }

    /// Approximate standard normal via the Irwin-Hall construction: the
    /// sum of 12 uniforms on `[0, 1)` has mean 6 and variance 1, so
    /// `sum - 6` approximates `N(0, 1)`.
    ///
    /// v0.7.1 C2: this replaced a Box-Muller implementation that used
    /// `f32::ln` + `f32::cos`. Transcendental functions are
    /// libm-implementation-defined (glibc ≠ musl in the last
    /// ULP), so the power-iteration seed — and therefore the entire PCA
    /// layout — was NOT bit-identical across machines, breaking
    /// invariant #7 (the Docker image's libc differs from the dev box).
    /// Irwin-Hall uses only IEEE-754 addition, which IS correctly
    /// rounded everywhere. The approximation is plenty for seeding power
    /// iteration: it just needs a non-degenerate random direction, and
    /// 100 iterations converge regardless of the exact seed.
    fn next_unit_normal(&mut self) -> f32 {
        let mut sum = 0.0f32;
        for _ in 0..12 {
            sum += self.next_unit();
        }
        sum - 6.0
    }
}

// --- topology helpers ----------------------------------------------------

/// Reasonable cluster count for `n` artifacts. Caps at `MAX_CLUSTERS`;
/// uses √n as a heuristic; floors at 1.
pub fn suggested_k(n: usize) -> usize {
    let s = (n as f64).sqrt() as usize;
    s.clamp(1, MAX_CLUSTERS)
}

/// Default RNG seed for atlas recompute. Stable across processes so
/// `kb atlas recompute --kb foo` produces the same layout twice. Can
/// be overridden per kb later (v0.4 may surface a kb.toml field).
pub const DEFAULT_SEED: u64 = 0x006b_6261_746c_6173; // "kbatlas" ascii

/// Recompute the atlas for a single kb. Reads all (id, embedding)
/// pairs via the storage handle, computes the layout, writes back via
/// `update_atlas`, and returns a summary the caller can drop into the
/// `atlas.recompute.complete` SSE payload.
///
/// Caller is expected to emit `atlas.recompute.start` before invoking
/// and `atlas.recompute.complete` (with this struct's fields) after.
/// Splitting the events from the work keeps `kb_core::atlas` free of
/// `events::EventBus` (which lives outside this module).
///
/// Equivalent to `recompute_for_kb_with(storage, None, None)`. Use the
/// `_with` variant for per-kb operator overrides (cluster count + layout).
///
/// `computed_at` stamps the c-TF-IDF label rows this also writes (W1.B) —
/// see the `_with` doc comment for why it's a caller-supplied argument
/// rather than a clock read in here.
pub async fn recompute_for_kb(
    storage: &crate::storage::StorageHandle,
    computed_at: i64,
) -> crate::Result<RecomputeReport> {
    recompute_for_kb_with(storage, None, LayoutKind::Umap, computed_at).await
}

/// `recompute_for_kb` with per-kb overrides:
/// - `k_override = Some(N)` clamps the cluster count to N (otherwise √n).
/// - `layout` picks UMAP or PCA explicitly.
///
/// Both are typically threaded from `kb.toml [kb.foo.atlas]` by the
/// caller (kb-server's atlas route handler).
///
/// `computed_at` (unix seconds) stamps the W1.B c-TF-IDF cluster labels
/// this also (re)computes once the layout lands — see the module doc on
/// `crate::atlas_labels` for the math. It's threaded in by the caller
/// (kb-server's route handler already reads `chrono::Utc::now()` around
/// every other mutation) rather than read here, keeping this whole
/// function — and the pure `atlas_labels::compute` it calls — free of a
/// clock (crates/kb-core/CLAUDE.md invariant #3: no clock in the
/// deterministic compute path). Label (re)computation is best-effort: a
/// failure here is logged and swallowed, never fails an atlas
/// recompute whose coordinates/clusters already landed successfully.
///
/// The same `computed_at` also stamps the W3 T-a TIME-LAPSE FRAME this
/// records (`record_atlas_snapshot` — read its doc comment for the two
/// honesty caveats: frames start empty and cannot be backfilled, and cluster
/// ids renumber between frames). Frame recording is best-effort on exactly
/// the same terms as the labels.
pub async fn recompute_for_kb_with(
    storage: &crate::storage::StorageHandle,
    k_override: Option<usize>,
    layout: LayoutKind,
    computed_at: i64,
) -> crate::Result<RecomputeReport> {
    let started = std::time::Instant::now();
    let pairs = storage.list_embeddings().await?;
    if pairs.is_empty() {
        return Ok(RecomputeReport {
            points: 0,
            clusters: 0,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }
    let rows = layout_rows(pairs, k_override, layout);
    if rows.is_empty() {
        return Ok(RecomputeReport {
            points: 0,
            clusters: 0,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }
    let points = rows.len();
    let clusters = rows
        .iter()
        .map(|(_, _, _, c)| *c)
        .collect::<std::collections::HashSet<_>>()
        .len();
    // Snapshot the geometry BEFORE `rows` moves into `update_atlas` below —
    // this is exactly the zip-aligned assignment W1.B's labels and W3's
    // time-lapse frame need, so no second `list_embeddings`/layout pass is
    // required.
    let frame_points = frame_points_from(&rows);
    let cluster_by_id: Vec<(String, i16)> = frame_points
        .iter()
        .map(|p| (p.artifact_id.clone(), p.cluster))
        .collect();
    storage.update_atlas(rows).await?;
    refresh_atlas_labels(storage, &cluster_by_id, computed_at).await;
    record_atlas_snapshot(storage, frame_points, layout_label(layout), computed_at).await;
    Ok(RecomputeReport {
        points,
        clusters,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// The deterministic layout KERNEL: `(id, embedding)` pairs in, the
/// `(id, x, y, cluster)` rows a recompute would write out. Extracted from
/// [`recompute_for_kb_with`] (which now calls it) so the W3 reconstruction
/// backfill lays out a HISTORICAL SUBSET of the corpus through the exact
/// same code path — a reconstructed frame is therefore bit-identical to what
/// a real recompute of that subset would have produced, with no second
/// layout implementation to drift.
///
/// Two invariant-carrying steps live here, in this order:
///
/// 1. **GC-B1 — canonicalise input order** at the boundary into the
///    order-sensitive `compute_layout` (a reverse permutation moves every
///    point up to 0.81 on the normalised [0,1] canvas; k-means seeds
///    centroids by array position, so any reorder can renumber clusters —
///    docs/research/atlas-input-order-determinism-2026-07.html). Artifact
///    ids are source-relative path hashes (root invariant #27), so sorting
///    by id makes this pipeline's input canonical regardless of the
///    filesystem walk / lance scan order that produced `pairs` — this fn
///    does NOT rely on `list_embeddings` alone doing it (belt-and-braces:
///    the research's own recommendation is to pin the order at THIS
///    boundary, not trust an upstream contract).
/// 2. **M1 — drop rows whose embedding contains any NaN or non-finite
///    value** BEFORE handing them to `compute_layout`. Pre-fix the k-means
///    reseed loop did `ce == &points[i]` (Vec<f32> equality), which is
///    false for NaN bits, and `dist < best_dist` silently misroutes
///    NaN-poisoned vectors into cluster 0. The atlas-determinism invariant
///    (crates/kb-core/CLAUDE.md #3) requires identical inputs → identical
///    output across machines; NaN inputs violate that even on the same libc
///    because comparison ordering is implementation-defined. Filtering at
///    the boundary keeps the deterministic code path pure-finite.
///
/// Returns an empty vec when nothing survives the finite filter (or `pairs`
/// was empty) — callers treat that as "no layout", never as an error.
/// Clock-free and storage-free by construction.
pub fn layout_rows(
    mut pairs: Vec<crate::storage::lance::EmbeddingPair>,
    k_override: Option<usize>,
    layout: LayoutKind,
) -> Vec<(String, f32, f32, i16)> {
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let (ids, embeddings): (Vec<String>, Vec<Vec<f32>>) = pairs
        .into_iter()
        .filter(|(id, emb)| {
            let ok = emb.iter().all(|v| v.is_finite());
            if !ok {
                tracing::warn!(
                    artifact_id = %id,
                    "atlas: dropping row with non-finite embedding"
                );
            }
            ok
        })
        .unzip();
    if embeddings.is_empty() {
        return Vec::new();
    }
    let k = k_override.unwrap_or_else(|| suggested_k(embeddings.len()));
    let layout_pts = compute_layout_with(&embeddings, k, DEFAULT_SEED, layout);
    ids.into_iter()
        .zip(layout_pts)
        .map(|(id, p)| (id, p.x, p.y, p.cluster))
        .collect()
}

/// The algorithm label stamped on a recorded frame. This is the layout the
/// caller REQUESTED — `compute_layout_with` may internally fall back to PCA
/// (degenerate UMAP output, or a corpus over `UMAP_MAX_N`), and that fallback
/// is deliberately not surfaced here: the label documents the configured
/// pipeline, not the per-run branch taken inside it.
fn layout_label(layout: LayoutKind) -> &'static str {
    match layout {
        LayoutKind::Umap => "umap",
        LayoutKind::Pca => "pca",
    }
}

/// Convert the `(id, x, y, cluster)` tuples about to be written to lance into
/// time-lapse frame points.
fn frame_points_from(
    rows: &[(String, f32, f32, i16)],
) -> Vec<crate::storage::sqlite::AtlasFramePoint> {
    rows.iter()
        .map(|(id, x, y, c)| crate::storage::sqlite::AtlasFramePoint {
            artifact_id: id.clone(),
            x: *x,
            y: *y,
            cluster: *c,
        })
        .collect()
}

/// W3 T-a — best-effort capture of one atlas TIME-LAPSE FRAME (V0028): the
/// `(id, x, y, cluster)` geometry a recompute/recluster just committed, kept
/// in the sqlite `atlas_snapshots` + `atlas_snapshot_points` side tables so
/// the corpus map's drift can be replayed.
///
/// Same log-and-swallow posture as `refresh_atlas_labels`, and for the same
/// reason: a frame is an additive, derived, disposable record. A frame-write
/// failure must NEVER fail a recompute whose coordinates already landed in
/// lance. Writing a frame does NOT bump the storage-actor index generation
/// (invariant #15) — it never touches the lance row-set, exactly like
/// `update_atlas` and `set_atlas_labels`.
///
/// `created_at` is CALLER-SUPPLIED, threaded from the route handler exactly
/// like `computed_at`: no clock may enter the deterministic layout path
/// (crates/kb-core/CLAUDE.md invariant #3).
///
/// Two honesty caveats every consumer of these frames must carry:
///
/// 1. **Frames start EMPTY, and TRUE history cannot be recovered.** kb
///    retains no past layout and no past embedding — the lance `atlas_x`/
///    `atlas_y`/`atlas_cluster` columns hold exactly one (the current)
///    layout, and re-embedding today's text reproduces today's map, not
///    yesterday's. So the `recorded` time-lapse ships BLIND: it is watchable
///    only after recomputes accumulate going forward.
///    [`backfill_reconstructed_frames`] can seed a *reconstruction* — today's
///    embeddings laid out over the doc subset that existed at each past cut
///    point — but those frames are stamped
///    [`crate::storage::sqlite::FrameProvenance::Reconstructed`] precisely
///    because they answer a DIFFERENT question ("where would these docs have
///    sat, if I had run the atlas then, knowing what I know now") and must
///    never be presented as first-hand history.
/// 2. **Cluster ids RENUMBER between frames.** `kmeans_lloyd` seeds centroids
///    by array POSITION and reseeds empty clusters randomly, so "cluster 3" in
///    consecutive frames names unrelated groups. A consumer must remap colours
///    per frame (match on the frame's label terms or on centroids) — never
///    animate a colour keyed on the raw cluster id.
async fn record_atlas_snapshot(
    storage: &crate::storage::StorageHandle,
    points: Vec<crate::storage::sqlite::AtlasFramePoint>,
    layout: &str,
    created_at: i64,
) {
    let frame = crate::storage::sqlite::NewAtlasFrame {
        created_at_unix: created_at,
        layout: layout.to_string(),
        provenance: crate::storage::sqlite::FrameProvenance::Recorded,
    };
    match storage.atlas_frame_insert(frame, points).await {
        Ok(Some(id)) => tracing::debug!(frame_id = id, layout, "atlas: recorded time-lapse frame"),
        // Not an error: a recompute that reproduced bit-identical coordinates
        // is not a new frame (see `Db::atlas_frame_insert`'s dedup).
        Ok(None) => tracing::debug!(layout, "atlas: layout unchanged; no new time-lapse frame"),
        Err(e) => tracing::warn!(
            error = %e,
            "atlas: failed to record the time-lapse frame (atlas coords/clusters are unaffected)"
        ),
    }
}

// --- W3 T-d: the RECONSTRUCTED backfill --------------------------------
//
// A time-lapse whose frames only start accruing after the feature ships is
// blind on day one. This backfill answers the honest, weaker question
// instead: **"where would the docs that existed at time T have sat, if I had
// run the atlas then, knowing what I know now?"** — today's embeddings, laid
// out over the doc subset whose `mtime_unix` is at or before each cut point,
// through the SAME `layout_rows` kernel a live recompute uses.
//
// It is NOT history, and every frame it writes says so: `provenance =
// 'reconstructed'` on the row, surfaced verbatim by `GET /atlas/history`,
// `kb atlas history|show|backfill` and the SPA scrubber. True historical
// layouts are impossible (see `record_atlas_snapshot`'s caveat 1) — nothing
// retains a past layout or a past embedding, and the lance row holds only
// the CURRENT `atlas_x`/`atlas_y`/`atlas_cluster`, overwritten wholesale by
// `update_atlas`.
//
// The time axis is **`mtime_unix`, never `indexed_at_unix`**: the latter is
// `unix_now()` at index time, so a single `kb reindex` stamps the whole
// corpus with today and every cut point would return the entire corpus (12
// identical frames — a lie that also costs 12 layouts).

/// One reconstruction cut point: a past instant, and how many docs existed
/// at (or before) it. Produced by [`plan_reconstruction`] and printed by the
/// CLI BEFORE any work runs, so the operator sees what they are about to
/// spend.
///
/// `doc_count` counts docs by **mtime alone**, over whatever universe the
/// caller passed in. The frame actually written at this cut can be SMALLER:
/// only docs that also carry an embedding can be laid out. The two numbers
/// are reported separately rather than reconciled — see
/// [`ReconstructedFrame::points`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconstructionCut {
    pub cut_unix: i64,
    pub doc_count: usize,
}

/// Plan `frames` evenly-spaced cut points over the `mtimes` window, oldest
/// first — pure, clock-free and storage-free, so it's directly unit-testable
/// and can run synchronously in a route handler before it spawns the work.
///
/// Rules, all deliberate:
/// - Empty `mtimes` (or `frames == 0`) ⇒ an empty plan: a corpus with no
///   timestamps has no time axis, and that's an honest empty, not an error.
/// - Cut `i` (1-based) sits at `min + (max - min) * i / frames`, so the LAST
///   cut is always exactly `max` — the final reconstructed frame covers the
///   whole corpus and is therefore directly comparable to a live recompute.
/// - A degenerate window (`min == max`: every doc shares one mtime) collapses
///   to a SINGLE cut. N copies of the same subset would be N bit-identical
///   layouts, i.e. N-1 wasted layouts that the coord-hash dedup would then
///   drop anyway.
/// - Integer rounding can land two cuts on the same second on a narrow
///   window; duplicates are collapsed for the same reason.
pub fn plan_reconstruction(mtimes: &[i64], frames: usize) -> Vec<ReconstructionCut> {
    if mtimes.is_empty() || frames == 0 {
        return Vec::new();
    }
    let mut sorted: Vec<i64> = mtimes.to_vec();
    sorted.sort_unstable();
    let min = sorted[0];
    let max = sorted[sorted.len() - 1];

    let mut cuts: Vec<i64> = Vec::with_capacity(frames);
    if min == max {
        cuts.push(max);
    } else {
        let span = (max - min) as i128;
        for i in 1..=frames as i128 {
            let cut = min + (span * i / frames as i128) as i64;
            if cuts.last() != Some(&cut) {
                cuts.push(cut);
            }
        }
    }

    cuts.into_iter()
        .map(|cut_unix| ReconstructionCut {
            cut_unix,
            // `sorted` is ascending ⇒ partition_point is the count of
            // mtimes <= cut (a doc "existed at" the cut).
            doc_count: sorted.partition_point(|&m| m <= cut_unix),
        })
        .collect()
}

/// What happened at one cut point. `Written` is the only outcome that
/// produced a row; the rest are reported, never swallowed, so
/// `kb atlas backfill` can say WHY a cut produced no frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    Written {
        frame_id: i64,
    },
    /// No doc at this cut point carried a usable embedding — nothing to lay
    /// out (typical for the earliest cuts on a corpus indexed without an
    /// embedding model).
    SkippedEmpty,
    /// The geometry is bit-identical to a frame the kb already holds — the
    /// idempotence rule (re-running backfill must not duplicate frames) and
    /// the same "a still that changed nothing is not a frame" rule
    /// `Db::atlas_frame_insert` applies to recorded frames.
    SkippedDuplicate,
    /// The insert itself failed. Frames are additive and disposable, so one
    /// failure is reported and the run continues with the next cut.
    Failed(String),
}

/// One cut point's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructedFrame {
    pub cut_unix: i64,
    /// Docs existing at this cut by mtime (from the plan).
    pub doc_count: usize,
    /// Points actually laid out — docs at this cut that ALSO had a usable
    /// (finite) embedding. `<= doc_count`, and the honest number: a doc kb
    /// never embedded cannot be placed on a map.
    pub points: usize,
    pub outcome: FrameOutcome,
}

/// The whole backfill run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillReport {
    pub frames: Vec<ReconstructedFrame>,
    pub written: usize,
    pub skipped: usize,
    pub duration_ms: u64,
}

/// How many stored frames the idempotence pre-check reads. Generous
/// headroom over `DEFAULT_ATLAS_FRAMES_KEEP` (24) so this constant never
/// needs to move if that one does.
const BACKFILL_EXISTING_FRAME_SCAN: u32 = 500;

/// W3 T-d — write one `provenance = 'reconstructed'` frame per cut point.
///
/// For each cut (oldest first) this lays out the docs whose `mtime_unix` is
/// at or before it, using **today's embeddings** and the SAME
/// [`layout_rows`] kernel `recompute_for_kb_with` runs, then inserts through
/// the SAME [`crate::storage::sqlite::Db::atlas_frame_insert`] path recorded
/// frames use — so the coord-hash dedup and the prune-to-24 retention bound
/// apply identically. Nothing here touches lance: the live
/// `atlas_x`/`atlas_y`/`atlas_cluster` columns (and therefore the map the
/// SPA draws) are left exactly as they were. It also writes no
/// `atlas_labels` — those describe the CURRENT clustering, and a
/// reconstruction must not overwrite them.
///
/// **Idempotence rule: skip on coord_hash.** Every stored frame's
/// `coord_hash` is read up front and each candidate's hash is checked
/// against that set (plus the hashes this run itself writes), so re-running
/// the backfill on an unchanged corpus writes ZERO new frames. This is a
/// superset of the insert path's own dedup, which only compares against the
/// NEWEST frame — and a backfill's frames are back-dated, so they are never
/// the newest row and would otherwise not dedup against each other at all.
/// Deleting-then-rewriting reconstructed frames was the alternative; it was
/// rejected because it destroys rows an operator may have already scrubbed
/// through and would churn frame ids on every run.
///
/// `on_frame` is called once per cut with that cut's result, in order, so
/// the caller can emit SSE progress (`atlas.snapshot.recorded`) live. It is
/// sync on purpose — `EventBus::emit` is sync, and kb-core holds no bus
/// (no clock, no bus in the deterministic path).
///
/// `mtime_by_id` is supplied by the caller (the route already holds the
/// memoised full-corpus doc scan, invariant #15) — this fn does one
/// `list_embeddings` and one `atlas_frames` read of its own, and no more.
pub async fn backfill_reconstructed_frames(
    storage: &crate::storage::StorageHandle,
    mtime_by_id: &std::collections::HashMap<String, i64>,
    cuts: &[ReconstructionCut],
    k_override: Option<usize>,
    layout: LayoutKind,
    mut on_frame: impl FnMut(&ReconstructedFrame),
) -> crate::Result<BackfillReport> {
    let started = std::time::Instant::now();
    let pairs = storage.list_embeddings().await?;
    // Seed the idempotence set from every frame the kb already holds —
    // recorded ones included: if a reconstruction happens to reproduce a
    // recorded frame's exact geometry, it is that frame, and a duplicate row
    // claiming weaker provenance would only muddy the scrubber.
    let mut seen: std::collections::HashSet<String> = storage
        .atlas_frames(BACKFILL_EXISTING_FRAME_SCAN)
        .await?
        .into_iter()
        .map(|f| f.coord_hash)
        .collect();

    let mut frames: Vec<ReconstructedFrame> = Vec::with_capacity(cuts.len());
    let mut written = 0usize;
    let mut skipped = 0usize;
    for cut in cuts {
        // Today's embeddings, restricted to the docs that existed then. A
        // doc with no mtime at all can't be placed on the time axis and is
        // excluded from every cut (not silently folded into the oldest).
        let subset: Vec<crate::storage::lance::EmbeddingPair> = pairs
            .iter()
            .filter(|(id, _)| {
                mtime_by_id
                    .get(id)
                    .is_some_and(|&mtime| mtime <= cut.cut_unix)
            })
            .cloned()
            .collect();
        let rows = layout_rows(subset, k_override, layout);
        let points = frame_points_from(&rows);
        let frame = if points.is_empty() {
            ReconstructedFrame {
                cut_unix: cut.cut_unix,
                doc_count: cut.doc_count,
                points: 0,
                outcome: FrameOutcome::SkippedEmpty,
            }
        } else {
            let hash = crate::storage::sqlite::atlas_frame_coord_hash(&points);
            let n = points.len();
            if seen.contains(&hash) {
                ReconstructedFrame {
                    cut_unix: cut.cut_unix,
                    doc_count: cut.doc_count,
                    points: n,
                    outcome: FrameOutcome::SkippedDuplicate,
                }
            } else {
                let new_frame = crate::storage::sqlite::NewAtlasFrame {
                    created_at_unix: cut.cut_unix,
                    layout: layout_label(layout).to_string(),
                    provenance: crate::storage::sqlite::FrameProvenance::Reconstructed,
                };
                let outcome = match storage.atlas_frame_insert(new_frame, points).await {
                    Ok(Some(frame_id)) => {
                        seen.insert(hash);
                        FrameOutcome::Written { frame_id }
                    }
                    // The insert path's own newest-frame dedup fired.
                    Ok(None) => {
                        seen.insert(hash);
                        FrameOutcome::SkippedDuplicate
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            cut_unix = cut.cut_unix,
                            "atlas: failed to write a reconstructed frame"
                        );
                        FrameOutcome::Failed(e.to_string())
                    }
                };
                ReconstructedFrame {
                    cut_unix: cut.cut_unix,
                    doc_count: cut.doc_count,
                    points: n,
                    outcome,
                }
            }
        };
        if matches!(frame.outcome, FrameOutcome::Written { .. }) {
            written += 1;
        } else {
            skipped += 1;
        }
        on_frame(&frame);
        frames.push(frame);
    }

    Ok(BackfillReport {
        frames,
        written,
        skipped,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// W1.B — best-effort (re)compute + persist of the deterministic c-TF-IDF
/// cluster labels for `cluster_by_id` (the `(id, cluster)` assignments an
/// atlas recompute/recluster just landed). Fetches the cheap per-doc
/// metadata (`title`/`summary`/`tags` — the existing `list_docs` slim
/// projection, no second embedding scan or lance `body` read), joins it to
/// the cluster assignment by id, runs the pure `atlas_labels::compute`, and
/// replaces the kb's `atlas_labels` sqlite table wholesale.
///
/// Failure anywhere in this path (a `list_docs`/`set_atlas_labels` error) is
/// logged and swallowed — labels are an additive SPA affordance; they must
/// never fail a recompute/recluster whose atlas coordinates/clusters
/// already committed successfully. Writing labels does NOT bump the
/// storage-actor index generation (invariant #15) — same rule as
/// `update_atlas` itself, since neither touches the lance row-set.
async fn refresh_atlas_labels(
    storage: &crate::storage::StorageHandle,
    cluster_by_id: &[(String, i16)],
    computed_at: i64,
) {
    if let Err(e) = refresh_atlas_labels_inner(storage, cluster_by_id, computed_at).await {
        tracing::warn!(
            error = %e,
            "atlas: failed to (re)compute cluster labels (atlas coords/clusters are unaffected)"
        );
    }
}

async fn refresh_atlas_labels_inner(
    storage: &crate::storage::StorageHandle,
    cluster_by_id: &[(String, i16)],
    computed_at: i64,
) -> crate::Result<()> {
    let docs = storage.list_docs(u32::MAX).await?;
    let by_id: std::collections::BTreeMap<&str, &crate::storage::lance::DocSummary> =
        docs.iter().map(|d| (d.id.as_str(), d)).collect();
    let label_docs: Vec<crate::atlas_labels::LabelDoc> = cluster_by_id
        .iter()
        .filter_map(|(id, cluster)| {
            by_id
                .get(id.as_str())
                .map(|d| crate::atlas_labels::LabelDoc {
                    id: id.clone(),
                    cluster: *cluster,
                    title: d.title.clone(),
                    summary: d.summary.clone().unwrap_or_default(),
                    tags: d.tags.clone(),
                })
        })
        .collect();
    let clusters = crate::atlas_labels::compute(&label_docs);
    let mut label_rows: Vec<crate::storage::sqlite::AtlasLabelRow> = Vec::new();
    for c in &clusters {
        for (i, term) in c.terms.iter().enumerate() {
            label_rows.push(crate::storage::sqlite::AtlasLabelRow {
                cluster: c.cluster,
                rank: (i + 1) as i64,
                term: term.term.clone(),
                tf: term.tf,
                ft: term.ft,
                score: term.score,
                computed_at,
            });
        }
    }
    storage.set_atlas_labels(label_rows).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecomputeReport {
    pub points: usize,
    pub clusters: usize,
    pub duration_ms: u64,
}

/// Recluster an existing atlas without re-running UMAP/PCA. Reads the
/// (id, embedding, atlas_x, atlas_y) rows that already have coords,
/// re-runs k-means on the embeddings (cluster identity lives in the
/// original high-dim space, not the 2-D projection — see the comment
/// on `compute_layout_with`), preserves x/y verbatim, and writes back
/// the new labels.
///
/// Use case: operator wants to try `--k 6` after a recompute landed
/// with the √n default of 4, without paying the UMAP O(n²) KNN +
/// 200 SGD-iteration cost again. Rows that don't already have atlas
/// coords are skipped — reclustering can't fabricate a layout from
/// nothing, and we don't want a partial recluster silently to wipe
/// the `atlas_cluster` column on never-laid-out rows.
///
/// Emits no events itself; the caller wraps with
/// `atlas.recluster.start` / `atlas.recluster.complete`.
///
/// `computed_at` (unix seconds) restamps the W1.B c-TF-IDF cluster labels —
/// cluster membership can change on a recluster even though x/y don't, so
/// labels are refreshed here too — and it stamps the W3 T-a time-lapse frame
/// this records (labelled `recluster`: same coordinates, new grouping). See
/// `recompute_for_kb_with`'s doc comment for why this is caller-supplied
/// rather than read from a clock in here.
pub async fn recluster_for_kb_with(
    storage: &crate::storage::StorageHandle,
    k_override: Option<usize>,
    computed_at: i64,
) -> crate::Result<RecomputeReport> {
    let started = std::time::Instant::now();
    let mut pairs = storage.list_embeddings().await?;
    // GC-B1 — `kmeans_lloyd` seeds centroids by array position (same
    // order-sensitivity as `compute_layout`), so canonicalise here too;
    // see the matching comment in `recompute_for_kb_with`.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    // u32::MAX is overkill but `list_docs_with_atlas` takes a limit;
    // any cap < n would silently drop rows + the recluster would
    // mis-match `pairs`. The lance scan is fine with a saturating limit.
    let docs = storage.list_docs_with_atlas(u32::MAX).await?;

    // Build id → (x, y) for rows that already have a layout. Rows
    // without coords get dropped (see fn-level docs).
    let coords: std::collections::HashMap<String, (f32, f32)> = docs
        .iter()
        .filter_map(|d| match (d.atlas_x, d.atlas_y) {
            (Some(x), Some(y)) => Some((d.id.clone(), (x, y))),
            _ => None,
        })
        .collect();

    let (ids, embeddings): (Vec<String>, Vec<Vec<f32>>) = pairs
        .into_iter()
        .filter(|(id, emb)| coords.contains_key(id) && emb.iter().all(|v| v.is_finite()))
        .unzip();

    if embeddings.is_empty() {
        return Ok(RecomputeReport {
            points: 0,
            clusters: 0,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    let n = embeddings.len();
    let k = k_override
        .unwrap_or_else(|| suggested_k(n))
        .clamp(1, MAX_CLUSTERS)
        .min(n);
    let mut rng = SeededRng::new(DEFAULT_SEED);
    let labels = kmeans_lloyd(&embeddings, k, &mut rng);

    let rows: Vec<(String, f32, f32, i16)> = ids
        .iter()
        .zip(labels.iter())
        .map(|(id, &c)| {
            let (x, y) = coords[id];
            (id.clone(), x, y, c as i16)
        })
        .collect();

    let points = rows.len();
    let clusters = labels
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .len();
    let frame_points = frame_points_from(&rows);
    let cluster_by_id: Vec<(String, i16)> = frame_points
        .iter()
        .map(|p| (p.artifact_id.clone(), p.cluster))
        .collect();
    storage.update_atlas(rows).await?;
    refresh_atlas_labels(storage, &cluster_by_id, computed_at).await;
    // A recluster preserves x/y and only reassigns clusters, so the frame it
    // records is a "same map, new grouping" still. Its label is `recluster`
    // rather than umap/pca precisely because the projection wasn't re-run —
    // the coordinates were inherited from whichever earlier layout produced
    // them.
    record_atlas_snapshot(storage, frame_points, "recluster", computed_at).await;
    Ok(RecomputeReport {
        points,
        clusters,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// `recluster_for_kb_with` with the kb's √n default cluster count.
pub async fn recluster_for_kb(
    storage: &crate::storage::StorageHandle,
    computed_at: i64,
) -> crate::Result<RecomputeReport> {
    recluster_for_kb_with(storage, None, computed_at).await
}

/// Sort `(idx, point)` pairs into a stable order — useful for tests
/// that need deterministic comparison without caring about the
/// original input order.
#[cfg(test)]
fn sorted_by_x(points: &[AtlasPoint]) -> Vec<(usize, AtlasPoint)> {
    use std::cmp::Ordering;
    let mut indexed: Vec<_> = points.iter().enumerate().map(|(i, p)| (i, *p)).collect();
    indexed.sort_by(|a, b| a.1.x.partial_cmp(&b.1.x).unwrap_or(Ordering::Equal));
    indexed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MI test-hardening (2026-08) — serializes just the seven lance-heaviest
    /// tests below (each does several real `StorageActor::upsert_doc` lance
    /// writes) against each other, so they never run their merge_inserts
    /// concurrently within this test binary.
    ///
    /// Root cause (static analysis of the installed lance-4.0.0/
    /// lance-datafusion-4.0.0 sources, cross-checked against kb-cli's own
    /// `main()` doc comment below, which independently names this exact
    /// panic text as a previously-encountered production issue): `upsert_
    /// doc`'s `merge_insert` routes through lance's `update_fragments`,
    /// which calls
    /// `lance_datafusion::exec::get_session_context(&LanceExecutionOptions
    /// { use_spilling: true, target_partition: Some(cpus.min(8)), .. })`.
    /// That function caches ONE `SessionContext` per resolved options tuple
    /// in a `static` `OnceLock<Mutex<HashMap<..>>>` INSIDE the
    /// `lance-datafusion` crate — a process-global singleton, not
    /// per-call — so every test in this binary whose merge_insert resolves
    /// to the same tuple (they all do: same env, same CPU count) shares the
    /// exact same `FairSpillPool`, hardcoded to `DEFAULT_LANCE_MEM_POOL_SIZE`
    /// = 100 MiB when `LANCE_MEM_POOL_SIZE` is unset
    /// (lance-datafusion-4.0.0/src/exec.rs). DataFusion's `ExternalSorter`
    /// reserves a FIXED `sort_spill_reservation_bytes`-derived amount up
    /// front per partition (up to 8 partitions here), *regardless of actual
    /// data volume* — which is exactly why the reported shortfall ("Failed
    /// to allocate additional 552.6 KB … 365.7 KB remain available") is
    /// byte-IDENTICAL on every occurrence: both numbers are functions of
    /// fixed config constants and of how many of THESE SPECIFIC tests
    /// happen to be mid-merge_insert at once, not of machine load or
    /// dataset size — a handful of tiny 4-8-doc fixtures already reserve
    /// most of that 100 MiB just in spill headroom.
    ///
    /// kb-cli's `main()` already carries the real fix for this exact error
    /// in production (`LANCE_BYPASS_SPILLING=1` by default — its doc
    /// comment cites this identical panic message), but that only runs for
    /// the `kb` binary's own process; `cargo test -p kb-core` never executes
    /// `kb_cli::main()`, so this binary never gets that default. We do not
    /// set the env var here instead: doing so would mean calling the
    /// now-`unsafe` `std::env::set_var` from a `cargo test` binary that runs
    /// many tests concurrently on separate OS threads by default — the
    /// exact unsynchronized-env-mutation hazard `set_var` became `unsafe`
    /// to warn about (`main()`'s call is sound only because it runs before
    /// any other code touches the environment, which no test binary can
    /// guarantee). Serializing the actual contenders for the shared pool
    /// sidesteps that hazard entirely, and mirrors the identical,
    /// already-shipped fix for this crate's sibling integration-test family
    /// (`crates/kb-server/tests/common/mod.rs`'s `atlas_lance_lock` — same
    /// root cause, same day, different binary so it can't share the static).
    ///
    /// Other tests in this module also drive `StorageActor::upsert_doc`
    /// against lance and are theoretically exposed to the same pool, but
    /// only these seven have been observed to lose the race; if one of the
    /// others starts flaking too, add it to the guarded list rather than
    /// re-diagnosing from scratch.
    ///
    /// Honesty note for the next person: this specific race did NOT
    /// reproduce on the box this fix was written on, across 8 attempts
    /// (`--test-threads=1`, the default runner ×6, `--test-threads=32`, and
    /// the full unfiltered 1659-test `kb-core` suite matching the reported
    /// baseline) — it is a genuine, low-probability scheduling race, not a
    /// deterministic failure, and CI's container-cgroup CPU throttling
    /// (this host also runs several `--cpus 3`-limited GH Actions runners)
    /// plausibly makes the unlucky interleaving far more likely there than
    /// on an idle bare-metal dev box. The fix does not depend on
    /// reproducing it: a mutex that serializes these seven tests removes
    /// the concurrent-access precondition the error message and the lance
    /// source prove is possible, regardless of how often any one
    /// environment happens to hit it.
    fn lance_heavy_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn fixture_embeddings() -> Vec<Vec<f32>> {
        // 6 points: 3 near (1, 0, 0...), 3 near (0, 1, 0...). Should
        // separate into two clusters along the first principal component.
        vec![
            vec![1.0, 0.0, 0.0, 0.1],
            vec![0.9, 0.05, 0.0, 0.1],
            vec![1.1, -0.05, 0.0, 0.1],
            vec![0.0, 1.0, 0.0, 0.1],
            vec![0.05, 0.95, 0.0, 0.1],
            vec![-0.05, 1.1, 0.0, 0.1],
        ]
    }

    /// Pinned `(x.to_bits(), y.to_bits())` for
    /// `compute_layout_with(fixture_embeddings(), 2, 42, Pca)`. See
    /// `pca_layout_golden_coords_pin_cross_libc_determinism`. Bootstrap:
    /// set to `&[]`, run the test, copy the `got` vector from the
    /// failure output.
    const GOLDEN_PCA_FIXTURE: &[(u32, u32)] = &[
        (1064261537, 1056964611),
        (1063169858, 1065353216),
        (1065353216, 0),
        (1032183408, 1059207245),
        (1038018962, 1059095112),
        (0, 1042114877),
    ];

    #[test]
    fn empty_input_yields_empty_output() {
        let pts = compute_layout(&[], 3, 42);
        assert!(pts.is_empty());
    }

    #[test]
    fn single_point_lands_at_centre() {
        let pts = compute_layout(&[vec![1.0, 2.0, 3.0]], 3, 42);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].x, 0.5);
        assert_eq!(pts[0].y, 0.5);
        assert_eq!(pts[0].cluster, 0);
    }

    #[test]
    fn deterministic_under_same_seed() {
        let emb = fixture_embeddings();
        let a = compute_layout(&emb, 2, 42);
        let b = compute_layout(&emb, 2, 42);
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(&b) {
            assert!((pa.x - pb.x).abs() < 1e-6);
            assert!((pa.y - pb.y).abs() < 1e-6);
            assert_eq!(pa.cluster, pb.cluster);
        }
    }

    #[test]
    fn coords_normalised_to_unit_range() {
        let emb = fixture_embeddings();
        let pts = compute_layout(&emb, 2, 42);
        for p in &pts {
            assert!(p.x >= 0.0 && p.x <= 1.0, "x={} out of range", p.x);
            assert!(p.y >= 0.0 && p.y <= 1.0, "y={} out of range", p.y);
        }
    }

    #[test]
    fn distinct_clusters_for_separable_points() {
        let emb = fixture_embeddings();
        let pts = compute_layout(&emb, 2, 42);
        // First 3 should share a cluster; last 3 should share a different one.
        let c_first = pts[0].cluster;
        let c_last = pts[3].cluster;
        assert_ne!(c_first, c_last, "groups should land in different clusters");
        assert_eq!(pts[1].cluster, c_first);
        assert_eq!(pts[2].cluster, c_first);
        assert_eq!(pts[4].cluster, c_last);
        assert_eq!(pts[5].cluster, c_last);
    }

    #[test]
    fn separable_points_spread_along_x() {
        // The first principal component of fixture_embeddings is along
        // the diagonal between (1,0,0,0) and (0,1,0,0). Sorting by x
        // should keep the two groups separated.
        let emb = fixture_embeddings();
        let pts = compute_layout(&emb, 2, 42);
        let sorted = sorted_by_x(&pts);
        let lower_half: std::collections::HashSet<usize> =
            sorted[..3].iter().map(|(i, _)| *i).collect();
        let upper_half: std::collections::HashSet<usize> =
            sorted[3..].iter().map(|(i, _)| *i).collect();
        // One half should be {0, 1, 2}, the other {3, 4, 5}.
        let group_a: std::collections::HashSet<usize> = (0..3).collect();
        let group_b: std::collections::HashSet<usize> = (3..6).collect();
        assert!(
            (lower_half == group_a && upper_half == group_b)
                || (lower_half == group_b && upper_half == group_a),
            "PCA failed to separate the groups: lower={lower_half:?} upper={upper_half:?}"
        );
    }

    #[test]
    fn k_clamped_to_max_clusters() {
        let emb: Vec<_> = (0..20).map(|i| vec![i as f32, 0.0]).collect();
        let pts = compute_layout(&emb, 100, 42);
        for p in &pts {
            assert!(
                (p.cluster as usize) < MAX_CLUSTERS,
                "cluster {} exceeds MAX_CLUSTERS",
                p.cluster
            );
        }
    }

    #[test]
    fn k_clamped_to_n() {
        let emb = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        // Asking for 10 clusters with 2 points should yield ≤2 distinct ids.
        let pts = compute_layout(&emb, 10, 42);
        let distinct: std::collections::HashSet<i16> = pts.iter().map(|p| p.cluster).collect();
        assert!(distinct.len() <= 2);
    }

    #[test]
    fn suggested_k_caps_at_max_clusters() {
        assert_eq!(suggested_k(0), 1);
        assert_eq!(suggested_k(1), 1);
        assert_eq!(suggested_k(4), 2);
        assert_eq!(suggested_k(100), 10);
        assert_eq!(suggested_k(1_000), MAX_CLUSTERS);
    }

    #[test]
    fn different_seeds_may_produce_different_clusters_but_same_count() {
        let emb = fixture_embeddings();
        let a = compute_layout(&emb, 2, 42);
        let b = compute_layout(&emb, 2, 7);
        // We don't assert layouts diverge — for separable data the
        // converged result should be similar regardless of seed. We DO
        // assert both runs produced full results in the same shape.
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn power_iteration_recovers_dominant_direction() {
        // 1-D-dominated data: all points lie roughly on x-axis.
        let emb: Vec<Vec<f32>> = (0..10).map(|i| vec![i as f32, 0.01, 0.01]).collect();
        let pts = compute_layout_with(&emb, 1, 42, LayoutKind::Pca);
        // x coords should be monotonic-ish in input index after PCA.
        // PCA component sign is arbitrary, so the axis may come out
        // ascending OR descending in input order — either proves the
        // dominant direction was recovered. (Pre-v0.7.1 this test
        // assumed one sign; the C2 RNG-seed change flipped it.)
        let mut ascending = 0;
        for i in 0..pts.len() - 1 {
            if pts[i].x <= pts[i + 1].x {
                ascending += 1;
            }
        }
        assert!(
            ascending >= 7 || ascending <= 2,
            "PCA didn't recover dominant axis ordering: {ascending}/9 ascending"
        );
    }

    // --- v0.5 P1 — real UMAP --------------------------------------------

    #[test]
    fn layout_kind_from_config_defaults_umap() {
        assert_eq!(LayoutKind::from_config(None), LayoutKind::Umap);
        assert_eq!(LayoutKind::from_config(Some("umap")), LayoutKind::Umap);
        assert_eq!(LayoutKind::from_config(Some("pca")), LayoutKind::Pca);
        assert_eq!(LayoutKind::from_config(Some("PCA")), LayoutKind::Pca);
        // Unknown → Umap (the default).
        assert_eq!(LayoutKind::from_config(Some("tsne")), LayoutKind::Umap);
    }

    #[test]
    fn umap_layout_deterministic_under_same_seed() {
        let emb = fixture_embeddings();
        let a = compute_layout_with(&emb, 2, 42, LayoutKind::Umap);
        let b = compute_layout_with(&emb, 2, 42, LayoutKind::Umap);
        for (pa, pb) in a.iter().zip(&b) {
            assert!(
                (pa.x - pb.x).abs() < 1e-6,
                "x mismatch: {} vs {}",
                pa.x,
                pb.x
            );
            assert!(
                (pa.y - pb.y).abs() < 1e-6,
                "y mismatch: {} vs {}",
                pa.y,
                pb.y
            );
            assert_eq!(pa.cluster, pb.cluster);
        }
    }

    #[test]
    fn umap_layout_separates_two_groups() {
        // Same fixture as the PCA test: 6 points in 2 groups. UMAP
        // should also yield distinct clusters via k-means + a non-
        // collapsed 2-D layout.
        let emb = fixture_embeddings();
        let pts = compute_layout_with(&emb, 2, 42, LayoutKind::Umap);
        let c_first = pts[0].cluster;
        let c_last = pts[3].cluster;
        assert_ne!(
            c_first, c_last,
            "UMAP path should still cluster the two groups separately"
        );
        // Coords should not all collapse to (0.5, 0.5).
        let distinct: std::collections::HashSet<_> = pts
            .iter()
            .map(|p| ((p.x * 100.0) as i32, (p.y * 100.0) as i32))
            .collect();
        assert!(distinct.len() >= 3, "UMAP collapsed: {distinct:?}");
    }

    #[test]
    fn umap_falls_back_to_pca_on_degenerate_input() {
        // All-zero embeddings — UMAP can't extract any neighborhood
        // structure (every distance is 0), so the post-iteration
        // distinct-coords check fails + the caller falls back to PCA.
        // PCA on all-zero centered data also degenerates but doesn't
        // panic; the test asserts the call returns without crashing
        // + every point is non-NaN.
        let emb: Vec<Vec<f32>> = (0..6).map(|_| vec![0.0; 8]).collect();
        let pts = compute_layout_with(&emb, 2, 42, LayoutKind::Umap);
        assert_eq!(pts.len(), 6);
        for p in &pts {
            assert!(!p.x.is_nan() && !p.y.is_nan(), "NaN coord: {p:?}");
            assert!(p.x >= 0.0 && p.x <= 1.0);
            assert!(p.y >= 0.0 && p.y <= 1.0);
        }
    }

    #[test]
    fn umap_under_3_points_falls_back_to_pca() {
        // n=2: UMAP returns None (nothing to KNN against); PCA fills in.
        let emb = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let pts = compute_layout_with(&emb, 2, 42, LayoutKind::Umap);
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn pca_path_explicitly_selectable_via_layout_kind() {
        // On a 51-doc corpus with real cluster structure UMAP succeeds
        // (it doesn't fall back), so `LayoutKind::Pca` vs `Umap` is a
        // genuine algorithm comparison — unlike the 6-doc fixture, where
        // UMAP collapses + falls back to PCA and the two paths only
        // differed by an artifact of incomplete power-iteration
        // convergence (the C2 RNG change improved convergence and
        // exposed that the old comparison was meaningless).
        let emb = synthetic_clusters(17, 1);
        let umap_pts = compute_layout_with(&emb, 3, 42, LayoutKind::Umap);
        let pca_a = compute_layout_with(&emb, 3, 42, LayoutKind::Pca);
        let pca_b = compute_layout_with(&emb, 3, 42, LayoutKind::Pca);
        assert_eq!(umap_pts.len(), 51);
        assert_eq!(pca_a.len(), 51);
        // The PCA path is deterministic + produces a valid layout.
        for (a, b) in pca_a.iter().zip(&pca_b) {
            assert_eq!(a.x.to_bits(), b.x.to_bits());
            assert_eq!(a.y.to_bits(), b.y.to_bits());
        }
        for p in &pca_a {
            assert!(!p.x.is_nan() && !p.y.is_nan(), "NaN coord: {p:?}");
            assert!((0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y));
        }
        // The two algorithms genuinely diverge on structured data.
        let any_diff = umap_pts
            .iter()
            .zip(&pca_a)
            .any(|(u, p)| (u.x - p.x).abs() > 1e-3 || (u.y - p.y).abs() > 1e-3);
        assert!(
            any_diff,
            "UMAP + PCA produced identical coords (suspicious)"
        );
    }

    #[test]
    fn kmeans_reseeds_empty_clusters() {
        // Two tightly-clustered groups + ask for 4 clusters. Without
        // reseeding, two of the 4 centroids would orphan after the
        // first iteration; with reseed, ≥3 cluster ids appear in the
        // labels. v0.7.1 H3's final assign pass keeps `labels`
        // consistent with the final centroids (a last-iteration reseed
        // used to leave them out of sync) and the partition stable.
        let emb: Vec<Vec<f32>> = vec![
            vec![1.0, 0.0, 0.0],
            vec![1.05, 0.0, 0.0],
            vec![1.0, 0.05, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 1.05, 0.0],
            vec![0.05, 1.0, 0.0],
        ];
        let pts = compute_layout_with(&emb, 4, 42, LayoutKind::Pca);
        let used: std::collections::HashSet<i16> = pts.iter().map(|p| p.cluster).collect();
        // We don't insist all 4 are used (some might still be empty
        // after reseed; bail-after-8-tries), but at least 3 should be.
        assert!(
            used.len() >= 3,
            "expected ≥3 cluster ids used after reseed, got {used:?}"
        );
        // H3 — the reseed-heavy path is deterministic: identical inputs
        // produce identical cluster labels, run to run.
        let again = compute_layout_with(&emb, 4, 42, LayoutKind::Pca);
        for (a, b) in pts.iter().zip(&again) {
            assert_eq!(a.cluster, b.cluster, "reseed partition not deterministic");
        }
    }

    // --- v0.7 P3 — UMAP convergence regression on synthetic corpora ------

    /// Build `per_cluster * 3` synthetic 384-d embeddings: three
    /// well-separated clusters, each with its signal concentrated in a
    /// different third of the dimensions plus small isotropic noise.
    /// Deterministic from `seed`; every vector is L2-normalised to match
    /// real bge-small output. Used to verify UMAP + k-means recover a
    /// balanced partition on corpora bigger than the 6-doc fixture.
    fn synthetic_clusters(per_cluster: usize, seed: u64) -> Vec<Vec<f32>> {
        const DIM: usize = 384;
        let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as i64 as f64 / i64::MAX as f64) as f32
        };
        let mut out = Vec::with_capacity(per_cluster * 3);
        for cluster in 0..3usize {
            for _ in 0..per_cluster {
                let mut v = vec![0.0f32; DIM];
                for (i, x) in v.iter_mut().enumerate() {
                    *x = next() * 0.08; // small isotropic noise
                    if i / (DIM / 3) == cluster {
                        *x += 1.0; // strong per-cluster signal
                    }
                }
                let norm = v.iter().map(|a| a * a).sum::<f32>().sqrt().max(1e-9);
                for a in &mut v {
                    *a /= norm;
                }
                out.push(v);
            }
        }
        out
    }

    fn cluster_counts(pts: &[AtlasPoint]) -> std::collections::HashMap<i16, usize> {
        let mut m = std::collections::HashMap::new();
        for p in pts {
            *m.entry(p.cluster).or_insert(0usize) += 1;
        }
        m
    }

    #[test]
    fn umap_balances_three_clusters_at_50_docs() {
        // 51 docs, 3 ground-truth clusters of 17. k=3 → k-means should
        // recover all three with a near-even split. Data seed 1 is a
        // robust regression anchor: it lands a clean partition across
        // every k-means seed tried (seed 42 on some other data seeds
        // hits a degenerate single-init local optimum — k-means is
        // init-sensitive; this test pins a known-good combination).
        let emb = synthetic_clusters(17, 1);
        let pts = compute_layout(&emb, 3, 42);
        assert_eq!(pts.len(), 51);
        let counts = cluster_counts(&pts);
        assert_eq!(counts.len(), 3, "expected 3 clusters, got {counts:?}");
        for (&c, &n) in &counts {
            assert!(
                (13..=21).contains(&n),
                "cluster {c} holds {n}/51 — partition not balanced ({counts:?})"
            );
        }
    }

    #[test]
    fn umap_balances_three_clusters_at_200_docs() {
        // 201 docs, 3 ground-truth clusters of 67. Same balance check at
        // a larger scale — the brute-force KNN is O(n²) but 201² is
        // trivially within the CI budget.
        let emb = synthetic_clusters(67, 7);
        let pts = compute_layout(&emb, 3, 42);
        assert_eq!(pts.len(), 201);
        let counts = cluster_counts(&pts);
        assert_eq!(counts.len(), 3, "expected 3 clusters, got {counts:?}");
        for (&c, &n) in &counts {
            assert!(
                (50..=84).contains(&n),
                "cluster {c} holds {n}/201 — partition not balanced ({counts:?})"
            );
        }
    }

    #[test]
    fn pca_layout_golden_coords_pin_cross_libc_determinism() {
        // v0.7.1 C2 — invariant #7 says the layout is bit-identical
        // ACROSS MACHINES. The run-twice tests can't catch a libc-
        // dependent drift (they run on one box). This pins the exact
        // f32 bit patterns of the PCA path so any drift — a transcendental
        // sneaking back into the hot path, an `unsafe` fast-math flag,
        // a different `next_unit_normal` — fails CI. The PCA path is now
        // add/sub/mul/div/sqrt only, all IEEE-754 correctly-rounded, so
        // these bits should hold on glibc and musl alike.
        let pts = compute_layout_with(&fixture_embeddings(), 2, 42, LayoutKind::Pca);
        let got: Vec<(u32, u32)> = pts.iter().map(|p| (p.x.to_bits(), p.y.to_bits())).collect();
        let want: Vec<(u32, u32)> = GOLDEN_PCA_FIXTURE.to_vec();
        assert_eq!(got, want, "PCA layout drifted from the pinned golden");
    }

    #[test]
    fn umap_layout_bit_identical_across_runs_on_large_corpus() {
        // Invariant #7 — `compute_layout` must be deterministic. Verify
        // it on a 200-doc corpus, not just the 6-doc fixture.
        let emb = synthetic_clusters(67, 7);
        let a = compute_layout(&emb, 3, 42);
        let b = compute_layout(&emb, 3, 42);
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(&b) {
            assert_eq!(pa.x.to_bits(), pb.x.to_bits(), "x not bit-identical");
            assert_eq!(pa.y.to_bits(), pb.y.to_bits(), "y not bit-identical");
            assert_eq!(pa.cluster, pb.cluster);
        }
    }

    #[test]
    fn umap_above_cap_falls_back_to_pca() {
        // P3 — n > UMAP_MAX_N: the brute-force-KNN O(n²) UMAP is skipped
        // for the deterministic O(n·d) PCA path even though Umap was
        // requested. The fallback must be BIT-IDENTICAL to an explicit
        // PCA request (the cap routes through the clean `Pca` arm, so it
        // stays off the determinism golden path). d=2 keeps the
        // over-cap fixture cheap while still exceeding the cap on n.
        let n = UMAP_MAX_N + 1;
        let emb: Vec<Vec<f32>> = (0..n).map(|i| vec![i as f32, (i % 7) as f32]).collect();
        let capped = compute_layout_with(&emb, 4, 42, LayoutKind::Umap);
        let pca = compute_layout_with(&emb, 4, 42, LayoutKind::Pca);
        assert_eq!(capped.len(), n);
        for (c, p) in capped.iter().zip(&pca) {
            assert_eq!(c.x.to_bits(), p.x.to_bits(), "capped UMAP x != PCA x");
            assert_eq!(c.y.to_bits(), p.y.to_bits(), "capped UMAP y != PCA y");
            assert_eq!(c.cluster, p.cluster, "capped UMAP cluster != PCA cluster");
        }
        // Sanity: below the cap UMAP is still UMAP (the cap didn't disable
        // it wholesale) — on structured data the two paths genuinely
        // diverge (mirrors pca_path_explicitly_selectable_via_layout_kind).
        let small = synthetic_clusters(17, 1); // 51 docs, well under the cap
        let umap_small = compute_layout_with(&small, 3, 42, LayoutKind::Umap);
        let pca_small = compute_layout_with(&small, 3, 42, LayoutKind::Pca);
        let differs = umap_small
            .iter()
            .zip(&pca_small)
            .any(|(u, p)| u.x.to_bits() != p.x.to_bits() || u.y.to_bits() != p.y.to_bits());
        assert!(differs, "below the cap UMAP should still differ from PCA");
    }

    #[test]
    fn umap_falls_back_cleanly_on_all_identical_embeddings() {
        // Every vector identical → no neighborhood structure → UMAP
        // degenerates → PCA fallback. Must not panic; coords stay in
        // range and non-NaN.
        let emb: Vec<Vec<f32>> = (0..20).map(|_| vec![0.3f32; 64]).collect();
        let pts = compute_layout(&emb, 3, 42);
        assert_eq!(pts.len(), 20);
        for p in &pts {
            assert!(!p.x.is_nan() && !p.y.is_nan(), "NaN coord: {p:?}");
            assert!(p.x >= 0.0 && p.x <= 1.0 && p.y >= 0.0 && p.y <= 1.0);
        }
    }

    #[test]
    fn umap_survives_nan_poisoned_embedding() {
        // A NaN slips into one vector (a corrupt embed row). The layout
        // path must not panic and must still return one point per input
        // — graceful degradation, not a crash.
        let mut emb = synthetic_clusters(7, 9); // 21 docs
        emb[3][10] = f32::NAN;
        let pts = compute_layout(&emb, 3, 42);
        assert_eq!(pts.len(), emb.len());
    }

    // --- recompute_for_kb integration ------------------------------------

    fn fake_embedding(seed: u64) -> Vec<f32> {
        // Deterministic 384-d vector keyed on seed; matches bge-small dim.
        let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut out = Vec::with_capacity(384);
        for _ in 0..384 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = (state as i64 as f64) / (i64::MAX as f64);
            out.push(f as f32);
        }
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for v in &mut out {
            *v /= norm;
        }
        out
    }

    #[tokio::test]
    async fn recompute_for_kb_writes_atlas_columns() {
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();

        // 6 docs with 2 visually-distinct embedding clusters.
        for i in 0..6 {
            let id = format!("doc{i}");
            let path = format!("/tmp/{id}.html");
            let mut doc = Doc::placeholder(id.clone(), path);
            doc.title = id;
            doc.embedding = Some(fake_embedding(if i < 3 { 1 } else { 2 } * 1000 + i as u64));
            h.upsert_doc(doc).await.unwrap();
        }

        let report = recompute_for_kb(&h, 1_700_000_000).await.unwrap();
        assert_eq!(report.points, 6);
        assert!(report.clusters >= 1 && report.clusters <= MAX_CLUSTERS);

        let docs = h.list_docs_with_atlas(100).await.unwrap();
        assert_eq!(docs.len(), 6);
        for d in &docs {
            assert!(d.atlas_x.is_some(), "{} missing atlas_x", d.id);
            assert!(d.atlas_y.is_some(), "{} missing atlas_y", d.id);
            assert!(d.atlas_cluster.is_some(), "{} missing cluster", d.id);
            let x = d.atlas_x.unwrap();
            let y = d.atlas_y.unwrap();
            assert!((0.0..=1.0).contains(&x), "x out of range: {x}");
            assert!((0.0..=1.0).contains(&y), "y out of range: {y}");
        }
    }

    // --- W1.B: atlas labels wiring ---------------------------------------

    #[tokio::test]
    async fn recompute_for_kb_with_writes_atlas_labels() {
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();

        // Two thematic groups so both clusters get distinct, meaningful
        // title tokens (not just "doc0".."doc5" placeholders).
        let titles = [
            "rust ownership borrow checker",
            "rust lifetimes memory safety",
            "rust cargo build system",
            "python duck typing dynamic",
            "python generators async runtime",
            "python packaging pip wheel",
        ];
        for (i, title) in titles.iter().enumerate() {
            let id = format!("doc{i}");
            let path = format!("/tmp/{id}.html");
            let mut doc = Doc::placeholder(id.clone(), path);
            doc.title = title.to_string();
            doc.embedding = Some(fake_embedding(if i < 3 { 1 } else { 2 } * 1000 + i as u64));
            h.upsert_doc(doc).await.unwrap();
        }

        recompute_for_kb_with(&h, Some(2), LayoutKind::Pca, 1_700_000_000)
            .await
            .unwrap();

        let labels = h.atlas_labels().await.unwrap();
        assert!(!labels.is_empty(), "expected at least one label row");
        for row in &labels {
            assert_eq!(row.computed_at, 1_700_000_000);
            assert!(row.rank >= 1 && row.rank <= 5);
            assert!(row.tf > 0.0);
            assert!(row.ft >= row.tf);
        }

        // Reclustering restamps computed_at without touching x/y (already
        // covered by `recluster_preserves_xy_and_only_rewrites_cluster`).
        recluster_for_kb_with(&h, Some(3), 1_800_000_000)
            .await
            .unwrap();
        let relabeled = h.atlas_labels().await.unwrap();
        assert!(!relabeled.is_empty());
        for row in &relabeled {
            assert_eq!(row.computed_at, 1_800_000_000);
        }
    }

    // --- W3 T-a: atlas time-lapse frame wiring ----------------------------

    #[tokio::test]
    async fn recompute_and_recluster_record_time_lapse_frames() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();

        // A fresh kb has NO frames — the time-lapse ships blind; nothing in
        // the system retains a past layout, so there is nothing to backfill.
        assert!(h.atlas_frames(100).await.unwrap().is_empty());

        for i in 0..6u64 {
            let id = format!("doc{i}");
            let mut doc = Doc::placeholder(id.clone(), format!("/tmp/{id}.html"));
            let group: u64 = if i < 3 { 1 } else { 2 };
            doc.embedding = Some(fake_embedding(group * 1000 + i));
            h.upsert_doc(doc).await.unwrap();
        }

        recompute_for_kb_with(&h, Some(2), LayoutKind::Pca, 1_700_000_000)
            .await
            .unwrap();
        let frames = h.atlas_frames(100).await.unwrap();
        assert_eq!(frames.len(), 1, "the recompute must record one frame");
        assert_eq!(frames[0].created_at_unix, 1_700_000_000);
        assert_eq!(frames[0].layout, "pca");
        assert_eq!(frames[0].provenance, "recorded");
        assert_eq!(frames[0].point_count, 6);
        assert_eq!(frames[0].cluster_count, 2);

        let pts = h.atlas_frame_points(frames[0].id).await.unwrap();
        assert_eq!(pts.len(), 6);
        // Frame geometry matches what actually landed in lance.
        let docs = h.list_docs_with_atlas(u32::MAX).await.unwrap();
        for p in &pts {
            let d = docs.iter().find(|d| d.id == p.artifact_id).unwrap();
            assert_eq!(d.atlas_x, Some(p.x));
            assert_eq!(d.atlas_y, Some(p.y));
        }

        // An identical recompute changes nothing, so it is NOT a frame.
        recompute_for_kb_with(&h, Some(2), LayoutKind::Pca, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(
            h.atlas_frames(100).await.unwrap().len(),
            1,
            "a recompute that changed nothing must not add a still"
        );

        // A recluster reassigns clusters at the same coordinates → a frame,
        // labelled `recluster` (the projection wasn't re-run).
        recluster_for_kb_with(&h, Some(3), 1_800_000_000)
            .await
            .unwrap();
        let frames = h.atlas_frames(100).await.unwrap();
        assert_eq!(frames.len(), 2);
        // Newest first.
        assert_eq!(frames[0].created_at_unix, 1_800_000_000);
        assert_eq!(frames[0].layout, "recluster");
        assert_eq!(frames[0].cluster_count, 3);
    }

    /// A frame-write failure must never fail a recompute whose coordinates
    /// already landed — the hook is log-and-swallow. We can't easily inject a
    /// sqlite error, so we pin the weaker but load-bearing half: a recompute
    /// over a kb whose frames are already at the retention bound still
    /// succeeds and still returns an accurate report.
    #[tokio::test]
    async fn frame_recording_never_fails_a_recompute() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        for i in 0..4u64 {
            let id = format!("doc{i}");
            let mut doc = Doc::placeholder(id.clone(), format!("/tmp/{id}.html"));
            doc.embedding = Some(fake_embedding(i));
            h.upsert_doc(doc).await.unwrap();
        }
        // Fill past the keep bound with synthetic frames.
        for i in 0..(crate::storage::sqlite::DEFAULT_ATLAS_FRAMES_KEEP as i64 + 3) {
            h.atlas_frame_insert(
                crate::storage::sqlite::NewAtlasFrame {
                    created_at_unix: 1_600_000_000 + i,
                    layout: "umap".into(),
                    provenance: crate::storage::sqlite::FrameProvenance::Recorded,
                },
                vec![crate::storage::sqlite::AtlasFramePoint {
                    artifact_id: "synthetic".into(),
                    x: i as f32,
                    y: 0.0,
                    cluster: 0,
                }],
            )
            .await
            .unwrap();
        }
        assert_eq!(
            h.atlas_frames(1000).await.unwrap().len(),
            crate::storage::sqlite::DEFAULT_ATLAS_FRAMES_KEEP
        );

        let report = recompute_for_kb_with(&h, Some(2), LayoutKind::Pca, 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(report.points, 4);
        // Still bounded — the insert self-prunes.
        assert_eq!(
            h.atlas_frames(1000).await.unwrap().len(),
            crate::storage::sqlite::DEFAULT_ATLAS_FRAMES_KEEP
        );
    }

    #[tokio::test]
    async fn recompute_for_kb_handles_empty_kb() {
        use crate::storage::actor::StorageActor;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();

        let report = recompute_for_kb(&h, 1_700_000_000).await.unwrap();
        assert_eq!(report.points, 0);
        assert_eq!(report.clusters, 0);
    }

    #[tokio::test]
    async fn recluster_preserves_xy_and_only_rewrites_cluster() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        for i in 0..6 {
            let id = format!("doc{i}");
            let path = format!("/tmp/{id}.html");
            let mut doc = Doc::placeholder(id.clone(), path);
            doc.title = id;
            doc.embedding = Some(fake_embedding(if i < 3 { 1 } else { 2 } * 1000 + i as u64));
            h.upsert_doc(doc).await.unwrap();
        }
        // Initial recompute lays out coords + clusters.
        let _ = recompute_for_kb(&h, 1_700_000_000).await.unwrap();
        let before = h.list_docs_with_atlas(100).await.unwrap();
        let coords_before: std::collections::HashMap<_, _> = before
            .iter()
            .map(|d| (d.id.clone(), (d.atlas_x, d.atlas_y)))
            .collect();

        // Recluster with k=4 — coords stay, labels can change.
        let report = recluster_for_kb_with(&h, Some(4), 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(report.points, 6);
        assert!(report.clusters >= 1 && report.clusters <= 4);

        let after = h.list_docs_with_atlas(100).await.unwrap();
        for d in &after {
            let (bx, by) = coords_before[&d.id];
            assert_eq!(d.atlas_x, bx, "x drifted for {}", d.id);
            assert_eq!(d.atlas_y, by, "y drifted for {}", d.id);
        }
    }

    #[tokio::test]
    async fn recluster_skips_rows_without_existing_coords() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        // No recompute first — rows have embeddings but no atlas coords.
        let mut a = Doc::placeholder("a", "/tmp/a.html");
        a.embedding = Some(fake_embedding(11));
        h.upsert_doc(a).await.unwrap();

        let report = recluster_for_kb(&h, 1_700_000_000).await.unwrap();
        assert_eq!(report.points, 0);
        assert_eq!(report.clusters, 0);
    }

    #[tokio::test]
    async fn recompute_for_kb_skips_docs_without_embeddings() {
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();

        // 2 with embeddings, 1 without.
        let mut a = Doc::placeholder("a", "/tmp/a.html");
        a.embedding = Some(fake_embedding(11));
        h.upsert_doc(a).await.unwrap();
        let mut b = Doc::placeholder("b", "/tmp/b.html");
        b.embedding = Some(fake_embedding(22));
        h.upsert_doc(b).await.unwrap();
        h.upsert_doc(Doc::placeholder("c", "/tmp/c.html"))
            .await
            .unwrap(); // no embedding

        let report = recompute_for_kb(&h, 1_700_000_000).await.unwrap();
        // list_embeddings filters out null-embedding rows (lance's
        // FixedSizeList outer-nullable scan), so only 2 points.
        assert_eq!(report.points, 2);
    }

    // --- GC-B1: atlas input-order determinism -----------------------------

    #[test]
    fn compute_layout_pipeline_is_order_independent_after_id_sort() {
        use crate::test_support::assert_order_independent;
        // Mirrors the exact GC-B1 fix in `recompute_for_kb_with`: sort the
        // (id, embedding) pairs by id before handing them to the
        // order-sensitive `compute_layout`. A permutation probe (docs/
        // research/atlas-input-order-determinism-2026-07.html §2) found a
        // reverse permutation of the raw input moves every point (up to
        // 0.81 on the normalised canvas) — this is the "permanent test"
        // that research recommended, run over N seeded shuffles via the
        // shared harness rather than one hand-picked reversal.
        let rows: Vec<(String, Vec<f32>)> = fixture_embeddings()
            .into_iter()
            .enumerate()
            .map(|(i, emb)| (format!("doc-{i}"), emb))
            .collect();
        assert_order_independent(&rows, 12, |mut pairs| {
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let (ids, embeddings): (Vec<String>, Vec<Vec<f32>>) = pairs.into_iter().unzip();
            let pts = compute_layout(&embeddings, 2, 42);
            ids.into_iter()
                .zip(pts)
                .map(|(id, p)| (id, p.x.to_bits(), p.y.to_bits(), p.cluster))
                .collect::<Vec<_>>()
        });
    }

    #[test]
    #[should_panic(expected = "order-canonicalizing")]
    fn compute_layout_is_order_sensitive_without_the_sort() {
        use crate::test_support::assert_order_independent;
        // Negative control: the identical pipeline minus the `sort_by`
        // line panics — proving the sort above is load-bearing (this is
        // the mechanism the permutation probe found), not incidental. If
        // this test ever stops panicking, `compute_layout` itself changed
        // to be order-insensitive and the defensive sorts in
        // `recompute_for_kb_with`/`recluster_for_kb_with` may be
        // reviewable (not this test's call — flag it).
        let rows: Vec<(String, Vec<f32>)> = fixture_embeddings()
            .into_iter()
            .enumerate()
            .map(|(i, emb)| (format!("doc-{i}"), emb))
            .collect();
        assert_order_independent(&rows, 12, |pairs| {
            let (ids, embeddings): (Vec<String>, Vec<Vec<f32>>) = pairs.into_iter().unzip();
            let pts = compute_layout(&embeddings, 2, 42);
            ids.into_iter()
                .zip(pts)
                .map(|(id, p)| (id, p.x.to_bits(), p.y.to_bits(), p.cluster))
                .collect::<Vec<_>>()
        });
    }

    #[tokio::test]
    async fn recompute_for_kb_atlas_is_independent_of_insertion_order() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        async fn build_and_recompute(
            order: &[usize],
        ) -> std::collections::BTreeMap<String, (u32, u32, i16)> {
            let tmp = tempfile::tempdir().unwrap();
            let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
                .await
                .unwrap();
            for &i in order {
                let id = format!("doc{i}");
                let path = format!("/tmp/{id}.html");
                let mut doc = Doc::placeholder(id.clone(), path);
                doc.title = id;
                doc.embedding = Some(fake_embedding(if i < 3 { 1 } else { 2 } * 1000 + i as u64));
                h.upsert_doc(doc).await.unwrap();
            }
            recompute_for_kb(&h, 1_700_000_000).await.unwrap();
            h.list_docs_with_atlas(100)
                .await
                .unwrap()
                .into_iter()
                .map(|d| {
                    (
                        d.id,
                        (
                            d.atlas_x.unwrap().to_bits(),
                            d.atlas_y.unwrap().to_bits(),
                            d.atlas_cluster.unwrap(),
                        ),
                    )
                })
                .collect()
        }

        // Real end-to-end regression (not the pure re-implementation
        // above): each doc is its own `upsert_doc` call — its own tiny
        // lance fragment/merge_insert commit — so forward vs reverse
        // INSERTION order exercises the exact "list_embeddings scan order
        // != id order" gap the research found, through the actual
        // `list_embeddings` + `recompute_for_kb_with` production code.
        let forward = build_and_recompute(&[0, 1, 2, 3, 4, 5]).await;
        let reverse = build_and_recompute(&[5, 4, 3, 2, 1, 0]).await;
        assert_eq!(
            forward, reverse,
            "atlas coords/cluster must not depend on insertion order (GC-B1)"
        );
    }

    // --- W3 T-d: the RECONSTRUCTED backfill ------------------------------

    #[test]
    fn plan_reconstruction_spaces_cuts_evenly_and_ends_at_the_newest_mtime() {
        // Window [100, 1100]; 4 frames ⇒ cuts every 250s, last == max.
        let mtimes = vec![100, 400, 700, 1100];
        let cuts = plan_reconstruction(&mtimes, 4);
        assert_eq!(
            cuts.iter().map(|c| c.cut_unix).collect::<Vec<_>>(),
            vec![350, 600, 850, 1100]
        );
        // doc_count = docs whose mtime is at or before the cut.
        assert_eq!(
            cuts.iter().map(|c| c.doc_count).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn plan_reconstruction_is_honest_empty_and_collapses_degenerate_windows() {
        assert!(plan_reconstruction(&[], 8).is_empty());
        assert!(plan_reconstruction(&[100, 200], 0).is_empty());
        // Every doc shares one mtime: N identical subsets would be N
        // bit-identical layouts, so the plan collapses to a single cut.
        let cuts = plan_reconstruction(&[500, 500, 500], 8);
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0].cut_unix, 500);
        assert_eq!(cuts[0].doc_count, 3);
        // A window narrower than the frame count can't produce more cuts
        // than it has distinct seconds — integer rounding collapses the
        // duplicates rather than asking for 8 layouts of 3 distinct subsets.
        let cuts = plan_reconstruction(&[10, 12], 8);
        assert_eq!(
            cuts.iter().map(|c| c.cut_unix).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
    }

    #[test]
    fn plan_reconstruction_ignores_input_order() {
        let a = plan_reconstruction(&[1100, 100, 700, 400], 4);
        let b = plan_reconstruction(&[100, 400, 700, 1100], 4);
        assert_eq!(a, b);
    }

    /// The whole point of routing the backfill through `layout_rows`: a
    /// reconstructed FULL-corpus frame must be bit-identical to what a real
    /// recompute of that same doc set produces — no second layout impl.
    #[tokio::test]
    async fn reconstructed_full_corpus_frame_is_bit_identical_to_a_recompute() {
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        let mut mtime_by_id = std::collections::HashMap::new();
        for i in 0..6u64 {
            let id = format!("doc{i}");
            let mut doc = Doc::placeholder(id.clone(), format!("/tmp/{id}.html"));
            doc.embedding = Some(fake_embedding(if i < 3 { 1000 + i } else { 2000 + i }));
            doc.mtime_unix = 1_700_000_000 + i as i64 * 100;
            mtime_by_id.insert(id, doc.mtime_unix);
            h.upsert_doc(doc).await.unwrap();
        }

        recompute_for_kb_with(&h, Some(2), LayoutKind::Pca, 1_700_000_500)
            .await
            .unwrap();
        let recorded = h.atlas_frames(100).await.unwrap();
        assert_eq!(recorded.len(), 1);
        let recorded_pts = h.atlas_frame_points(recorded[0].id).await.unwrap();

        // ONE cut, at the newest mtime ⇒ the whole corpus.
        let cuts = vec![ReconstructionCut {
            cut_unix: 1_700_000_500,
            doc_count: 6,
        }];
        let report = backfill_reconstructed_frames(
            &h,
            &mtime_by_id,
            &cuts,
            Some(2),
            LayoutKind::Pca,
            |_| {},
        )
        .await
        .unwrap();
        // Bit-identical geometry ⇒ the coord-hash dedup refuses to store a
        // weaker-provenance copy of a frame the kb already holds.
        assert_eq!(report.written, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.frames[0].outcome, FrameOutcome::SkippedDuplicate);
        assert_eq!(report.frames[0].points, recorded_pts.len());
        // And the identity itself, checked directly through the kernel.
        let pairs = h.list_embeddings().await.unwrap();
        let rows = layout_rows(pairs, Some(2), LayoutKind::Pca);
        for (id, x, y, cluster) in &rows {
            let p = recorded_pts.iter().find(|p| &p.artifact_id == id).unwrap();
            assert_eq!(p.x.to_bits(), x.to_bits());
            assert_eq!(p.y.to_bits(), y.to_bits());
            assert_eq!(p.cluster, *cluster);
        }
    }

    #[tokio::test]
    async fn backfill_writes_reconstructed_frames_and_is_idempotent() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        let mut mtimes: Vec<i64> = Vec::new();
        let mut mtime_by_id = std::collections::HashMap::new();
        for i in 0..8u64 {
            let id = format!("doc{i}");
            let mut doc = Doc::placeholder(id.clone(), format!("/tmp/{id}.html"));
            doc.embedding = Some(fake_embedding(if i < 4 { 1000 + i } else { 2000 + i }));
            doc.mtime_unix = 1_700_000_000 + i as i64 * 1000;
            mtimes.push(doc.mtime_unix);
            mtime_by_id.insert(id, doc.mtime_unix);
            h.upsert_doc(doc).await.unwrap();
        }

        let cuts = plan_reconstruction(&mtimes, 4);
        assert_eq!(cuts.len(), 4);
        let mut progress: Vec<i64> = Vec::new();
        let report =
            backfill_reconstructed_frames(&h, &mtime_by_id, &cuts, Some(2), LayoutKind::Pca, |f| {
                progress.push(f.cut_unix)
            })
            .await
            .unwrap();
        assert_eq!(report.written, 4, "{:?}", report.frames);
        // The progress callback fires once per cut, in plan order.
        assert_eq!(
            progress,
            cuts.iter().map(|c| c.cut_unix).collect::<Vec<_>>()
        );

        let frames = h.atlas_frames(100).await.unwrap();
        assert_eq!(frames.len(), 4);
        for f in &frames {
            assert_eq!(
                f.provenance, "reconstructed",
                "every backfilled frame must say so"
            );
            assert_eq!(f.layout, "pca");
        }
        // Frames grow with the corpus: the oldest cut holds the fewest points.
        let mut by_time = frames.clone();
        by_time.sort_by_key(|f| f.created_at_unix);
        assert!(by_time[0].point_count < by_time[3].point_count);
        assert_eq!(by_time[3].point_count, 8, "the last cut covers everything");
        // The time axis is mtime — the newest frame sits at the newest mtime,
        // not at "now".
        assert_eq!(by_time[3].created_at_unix, 1_700_007_000);

        // The lance atlas columns are untouched — a reconstruction never
        // moves the live map.
        let docs = h.list_docs_with_atlas(u32::MAX).await.unwrap();
        assert!(docs.iter().all(|d| d.atlas_x.is_none()));

        // IDEMPOTENCE: re-running writes nothing new (skip-on-coord_hash).
        let again = backfill_reconstructed_frames(
            &h,
            &mtime_by_id,
            &cuts,
            Some(2),
            LayoutKind::Pca,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(again.written, 0);
        assert_eq!(again.skipped, 4);
        assert!(again
            .frames
            .iter()
            .all(|f| f.outcome == FrameOutcome::SkippedDuplicate));
        assert_eq!(h.atlas_frames(100).await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn backfill_reports_cuts_with_no_embedded_docs_instead_of_writing_empties() {
        let _lance_guard = lance_heavy_test_lock().lock().await;
        use crate::storage::actor::StorageActor;
        use crate::storage::schema::Doc;

        let tmp = tempfile::tempdir().unwrap();
        let h = StorageActor::spawn(tmp.path().join("lance"), tmp.path().join("idx.db"), None)
            .await
            .unwrap();
        // One un-embedded doc, early; one embedded doc, late.
        let mut early = Doc::placeholder("early".to_string(), "/tmp/early.html".to_string());
        early.mtime_unix = 1_700_000_000;
        h.upsert_doc(early).await.unwrap();
        let mut late = Doc::placeholder("late".to_string(), "/tmp/late.html".to_string());
        late.embedding = Some(fake_embedding(7));
        late.mtime_unix = 1_700_001_000;
        h.upsert_doc(late).await.unwrap();

        let mtime_by_id: std::collections::HashMap<String, i64> = [
            ("early".to_string(), 1_700_000_000),
            ("late".to_string(), 1_700_001_000),
        ]
        .into_iter()
        .collect();
        let cuts = plan_reconstruction(&[1_700_000_000, 1_700_001_000], 2);
        let report =
            backfill_reconstructed_frames(&h, &mtime_by_id, &cuts, None, LayoutKind::Pca, |_| {})
                .await
                .unwrap();
        assert_eq!(report.frames.len(), 2);
        // The first cut has a doc but no embedding to place it with — an
        // explicit skip, never a silent zero-point frame.
        assert_eq!(report.frames[0].doc_count, 1);
        assert_eq!(report.frames[0].points, 0);
        assert_eq!(report.frames[0].outcome, FrameOutcome::SkippedEmpty);
        assert!(matches!(
            report.frames[1].outcome,
            FrameOutcome::Written { .. }
        ));
        assert_eq!(h.atlas_frames(100).await.unwrap().len(), 1);
    }
}
