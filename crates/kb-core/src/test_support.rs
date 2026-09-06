//! GC-B1 — shared test-only helpers for order-independence assertions.
//!
//! Several kb-core pipelines are documented as deterministic given a FIXED
//! input order (`atlas::compute_layout`, the ranked lance read paths in
//! `storage::lance`) but historically the order itself was supplied by an
//! unpinned filesystem walk / lance physical scan
//! (docs/research/search-determinism-settling-window-2026-07.html,
//! docs/research/atlas-input-order-determinism-2026-07.html). This module
//! is the reusable harness both fixes are tested against: run a
//! caller-supplied pipeline stage over N seeded shuffles of its own input
//! rows and assert every shuffle reproduces the same canonical output —
//! catching a regression where the canonicalizing sort gets dropped.
//!
//! `#[cfg(test)]`-only (see the `pub(crate)` gate in `lib.rs`); never
//! reachable from non-test code.

/// Tiny deterministic PRNG (splitmix64) for the shuffle below. Same
/// "no `rand` dep for a handful of draws" rationale as atlas's own
/// `SeededRng` (a xorshift64 living in `atlas.rs`); this is a separate,
/// test-only copy rather than a shared dependency so production code
/// never depends on test-only helpers (and vice versa).
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        // splitmix64 tolerates seed=0 fine (unlike atlas's xorshift64),
        // but smear anyway for a better first draw.
        Self(seed.wrapping_add(0x9e3779b97f4a7c15))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Fisher-Yates, in place.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        let n = items.len();
        for i in (1..n).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// Run `pipeline` over `seeds` distinct shuffles of `rows` and assert
/// every shuffle yields the SAME output as the unshuffled baseline call —
/// i.e. `pipeline` is order-independent (its own canonicalizing sort, if
/// any, is doing its job).
///
/// - `rows`: the un-canonicalized input set the caller wants to stress —
///   e.g. a plausible alternate insertion/scan order.
/// - `pipeline`: takes an owned (possibly shuffled) `Vec<T>` and returns a
///   comparable summary `K`. The caller is responsible for making `K`
///   itself order-independent where the underlying operation legitimately
///   permutes (e.g. match atlas points back to stable artifact ids before
///   comparing, rather than comparing `Vec<AtlasPoint>` positionally).
/// - `seeds`: how many distinct shuffles to try (0 is a valid no-op; each
///   seed is deterministic so a failure is reproducible from the seed
///   index in the panic message).
///
/// A single fresh `SplitMix64` per seed keeps shuffles independent of
/// each other and of `seeds`' own iteration order.
pub(crate) fn assert_order_independent<T, K, F>(rows: &[T], seeds: u64, mut pipeline: F)
where
    T: Clone,
    K: PartialEq + std::fmt::Debug,
    F: FnMut(Vec<T>) -> K,
{
    let baseline = pipeline(rows.to_vec());
    for seed in 0..seeds {
        let mut shuffled = rows.to_vec();
        SplitMix64::new(seed ^ 0xD1B5_4A32_5F42_4A7F).shuffle(&mut shuffled);
        let got = pipeline(shuffled);
        assert_eq!(
            got, baseline,
            "pipeline output differs after shuffle seed {seed} — an order-canonicalizing \
             sort is missing or was dropped (GC-B1 order-independence contract)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_is_a_permutation_not_a_reorder_to_self() {
        let mut rng = SplitMix64::new(1);
        let mut items: Vec<u32> = (0..10).collect();
        let original = items.clone();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort();
        assert_eq!(
            sorted, original,
            "shuffle must preserve the element multiset"
        );
    }

    #[test]
    fn different_seeds_produce_different_shuffles() {
        let mut a: Vec<u32> = (0..20).collect();
        let mut b = a.clone();
        SplitMix64::new(1).shuffle(&mut a);
        SplitMix64::new(2).shuffle(&mut b);
        assert_ne!(
            a, b,
            "distinct seeds should (overwhelmingly likely) diverge"
        );
    }

    #[test]
    fn helper_passes_for_a_genuinely_order_independent_pipeline() {
        let rows = vec![3, 1, 4, 1, 5, 9, 2, 6];
        assert_order_independent(&rows, 8, |mut v| {
            v.sort();
            v
        });
    }

    #[test]
    #[should_panic(expected = "order-canonicalizing")]
    fn helper_catches_an_order_sensitive_pipeline() {
        // No internal sort — the identity fn is exactly what the fixes in
        // this step replace; the helper must fail on it.
        let rows = vec![3, 1, 4, 1, 5, 9, 2, 6];
        assert_order_independent(&rows, 8, |v| v);
    }
}
