//! `procrustes` — closed-form 2-D similarity alignment (rotate + uniform
//! scale + translate, optionally reflect) of one point set onto another.
//!
//! Why this exists: a corpus time-lapse replays successive atlas layouts
//! as frames. Two layouts computed from *nearly* the same embeddings can
//! come out arbitrarily rotated/mirrored relative to each other (PCA
//! eigenvectors have no canonical sign; UMAP-ish layouts have no canonical
//! orientation at all), so an unaligned time-lapse *spins* between frames
//! and every doc looks like it moved. Aligning frame N onto frame N−1
//! removes the part of the motion that is pure coordinate-frame churn and
//! leaves only the motion that means something.
//!
//! # Determinism is the whole point (invariant #7 · atlas.rs v0.7.1 "C2")
//!
//! The atlas layout is required to be **bit-identical across machines** —
//! the Docker image's libc differs from the dev box's. `atlas.rs` (see the
//! `next_unit_normal` comment around :545 and the
//! `pca_layout_golden_coords_pin_cross_libc_determinism` golden pin) had
//! to replace Box-Muller with Irwin-Hall for exactly this reason:
//! transcendental functions are libm-implementation-defined and glibc ≠
//! musl in the last ULP.
//!
//! **This module therefore uses only `+`, `-`, `*`, `/` and `sqrt`** —
//! every one of them correctly rounded by IEEE-754 on every platform. No
//! `atan2`, no `sin`, no `cos`, no `powf`, no `hypot`. The 2-D orthogonal
//! Procrustes problem has an exact closed form that yields `cosθ` and
//! `sinθ` *directly* as `a/r` and `b/r`; we never recover the angle θ and
//! re-trig it. Do not "simplify" this to `let theta = b.atan2(a)` — that
//! reintroduces the exact bug C2 fixed, and it will not fail to compile,
//! it will fail on someone else's machine six months from now.
//!
//! Accumulation happens in `f64` (also correctly rounded, and in a fixed
//! index order), with a single correctly-rounded narrowing to `f32` at the
//! end; the summation order is the caller's slice order, which is stable.
//!
//! # Honesty note: a residual always remains
//!
//! [`crate::atlas`]'s `normalise_to_unit` (atlas.rs:379-400) min-max-scales
//! the x axis and the y axis **independently**. That is an *anisotropic*
//! stretch: it squashes one axis by a different factor than the other. A
//! similarity transform (uniform scale + rotation + translation ± a
//! reflection) has no anisotropic component by construction, so **no
//! alignment produced here can fully undo it** and [`residual`] will be
//! non-zero between two real atlas frames even when the underlying
//! geometry is identical. That is expected and correct. Do not "fix" it by
//! adding a shear/anisotropic term: a full affine fit would happily
//! collapse a frame onto a line to minimise the residual, which is worse
//! than the honest leftover wobble. If it ever matters, the fix belongs
//! upstream in `normalise_to_unit` (scale both axes by ONE factor), not
//! here — and that is a layout change with its own golden pin to re-bless.

use serde::{Deserialize, Serialize};

/// A 2-D similarity transform: `p ↦ to_centroid + scale · M · (p − from_centroid)`
/// where `M` is the rotation `[[cos, −sin], [sin, cos]]`, or, when
/// [`Transform::reflect`] is set, the improper (mirroring) orthogonal
/// matrix `[[cos, sin], [sin, −cos]]`.
///
/// Both centroids are stored explicitly rather than being folded into a
/// single translation vector: the fit is *defined* around the centroids,
/// and keeping them separate avoids a cancellation that loses precision
/// when the point cloud sits far from the origin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// `cosθ`, obtained directly as `a / r` — never via an angle.
    pub cos: f32,
    /// `sinθ`, obtained directly as `b / r` — never via an angle.
    pub sin: f32,
    /// Uniform (isotropic) scale factor.
    pub scale: f32,
    /// When true the orthogonal part is improper: the transform mirrors.
    pub reflect: bool,
    /// Centroid of the `from` set (subtracted before the linear part).
    pub from_centroid: (f32, f32),
    /// Centroid of the `to` set (added after the linear part).
    pub to_centroid: (f32, f32),
}

impl Transform {
    /// The do-nothing transform. Returned for every degenerate input
    /// (see [`align`]) so callers never have to check for `NaN`.
    pub const IDENTITY: Transform = Transform {
        cos: 1.0,
        sin: 0.0,
        scale: 1.0,
        reflect: false,
        from_centroid: (0.0, 0.0),
        to_centroid: (0.0, 0.0),
    };

    /// True when every stored parameter is finite. [`align`] guarantees
    /// this for the transform it returns.
    pub fn is_finite(&self) -> bool {
        self.cos.is_finite()
            && self.sin.is_finite()
            && self.scale.is_finite()
            && self.from_centroid.0.is_finite()
            && self.from_centroid.1.is_finite()
            && self.to_centroid.0.is_finite()
            && self.to_centroid.1.is_finite()
    }

    /// Map one point. Pure `+ - * /` in `f32`.
    pub fn map(&self, p: (f32, f32)) -> (f32, f32) {
        let dx = p.0 - self.from_centroid.0;
        let dy = p.1 - self.from_centroid.1;
        let (rx, ry) = if self.reflect {
            (self.cos * dx + self.sin * dy, self.sin * dx - self.cos * dy)
        } else {
            (self.cos * dx - self.sin * dy, self.sin * dx + self.cos * dy)
        };
        (
            self.to_centroid.0 + self.scale * rx,
            self.to_centroid.1 + self.scale * ry,
        )
    }
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

/// Below this the denominator `Σ(x² + y²)` (or `r`) counts as zero. Matches
/// the `1e-12` epsilon `atlas::normalise_to_unit` uses for a degenerate
/// range, so the two modules agree on "this cloud has no extent".
const EPS: f64 = 1e-12;

/// Centroid of the first `n` points, accumulated in `f64` in slice order.
/// Returns `None` if `n == 0` or any coordinate is non-finite.
fn centroid(points: &[(f32, f32)], n: usize) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    for &(x, y) in &points[..n] {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        sx += x as f64;
        sy += y as f64;
    }
    let inv = 1.0 / (n as f64);
    Some((sx * inv, sy * inv))
}

/// Fit the similarity transform that best maps `from` onto `to` in the
/// least-squares sense (minimising `Σ |T(pᵢ) − qᵢ|²`).
///
/// Points are paired **by index**; if the slices differ in length the
/// extra tail of the longer one is ignored (pairing is the caller's
/// contract — see [`crate::atlas_field::disagreement`] for the id-joined
/// pairing the dual-field atlas uses).
///
/// # Method (exact closed form, no transcendentals)
///
/// Centre both sets on their centroids, giving `(xᵢ, yᵢ)` and `(uᵢ, vᵢ)`.
/// For the **proper rotation** branch accumulate
///
/// ```text
/// a = Σ(xᵢ·uᵢ + yᵢ·vᵢ)      b = Σ(xᵢ·vᵢ − yᵢ·uᵢ)      r = sqrt(a² + b²)
/// cosθ = a / r              sinθ = b / r              s = r / Σ(xᵢ² + yᵢ²)
/// ```
///
/// (`R·p · q` expands to `a·cosθ + b·sinθ`, whose maximum over the unit
/// circle is at `(a/r, b/r)` — that is the whole derivation.) For the
/// **reflection** branch the same algebra with `M = [[c, s], [s, −c]]`
/// gives `a' = Σ(xᵢ·uᵢ − yᵢ·vᵢ)`, `b' = Σ(yᵢ·uᵢ + xᵢ·vᵢ)`.
///
/// Both candidates are built and scored with [`residual`]; the lower
/// residual wins. **Tie-break: the non-reflected candidate.** (Reflection
/// must earn its place — a mirrored time-lapse frame is a bigger visual
/// lie than a slightly worse fit, and an exact tie happens for genuinely
/// symmetric inputs where either answer is equally right.)
///
/// # Degenerate inputs all return [`Transform::IDENTITY`]-shaped results
///
/// * empty input (either side) → [`Transform::IDENTITY`]
/// * a single pair → pure translation (`scale = 1`, no rotation)
/// * all `from` points coincident (`Σ(x² + y²) ≈ 0`) → pure translation
/// * `r ≈ 0` (e.g. all `to` points coincident, or perfectly orthogonal
///   clouds) → pure translation. The *true* least-squares optimum here is
///   `scale = 0`, i.e. collapse every point onto the target centroid; we
///   deliberately do not return that. A frame collapsed to a single dot is
///   useless as a time-lapse and useless as a disagreement map, so we keep
///   the transform invertible and let the residual be honest about it.
/// * any non-finite coordinate anywhere → [`Transform::IDENTITY`]
///
/// The returned transform always satisfies [`Transform::is_finite`].
pub fn align(from: &[(f32, f32)], to: &[(f32, f32)]) -> Transform {
    let n = from.len().min(to.len());
    if n == 0 {
        return Transform::IDENTITY;
    }
    let (Some(fc), Some(tc)) = (centroid(from, n), centroid(to, n)) else {
        // Non-finite input: refuse to fit rather than propagate NaN.
        return Transform::IDENTITY;
    };
    let translate_only = Transform {
        cos: 1.0,
        sin: 0.0,
        scale: 1.0,
        reflect: false,
        from_centroid: (fc.0 as f32, fc.1 as f32),
        to_centroid: (tc.0 as f32, tc.1 as f32),
    };
    if n == 1 {
        return translate_only;
    }

    // Accumulate the four cross-products + the `from` sum-of-squares in a
    // single fixed-order pass. f64 accumulators, f32 inputs: every op here
    // is IEEE-754 correctly rounded, so the sums are bit-identical on any
    // conforming platform.
    let mut a_rot = 0.0f64; // Σ(x·u + y·v)
    let mut b_rot = 0.0f64; // Σ(x·v − y·u)
    let mut a_ref = 0.0f64; // Σ(x·u − y·v)
    let mut b_ref = 0.0f64; // Σ(y·u + x·v)
    let mut sq = 0.0f64; // Σ(x² + y²)
                         // Index order (the caller's slice order) IS the summation order — see
                         // the module doc. The zip walks it identically; do not reorder.
    for (&(fx, fy), &(tx, ty)) in from[..n].iter().zip(&to[..n]) {
        let x = fx as f64 - fc.0;
        let y = fy as f64 - fc.1;
        let u = tx as f64 - tc.0;
        let v = ty as f64 - tc.1;
        a_rot += x * u + y * v;
        b_rot += x * v - y * u;
        a_ref += x * u - y * v;
        b_ref += y * u + x * v;
        sq += x * x + y * y;
    }
    if sq.is_nan() || sq <= EPS {
        // All `from` points coincide (or the sum went non-finite): there is
        // no orientation or scale to recover, only a translation.
        return translate_only;
    }

    let rot = candidate(a_rot, b_rot, sq, false, fc, tc);
    let refl = candidate(a_ref, b_ref, sq, true, fc, tc);
    match (rot, refl) {
        (None, None) => translate_only,
        (Some(t), None) => t,
        (None, Some(t)) => t,
        (Some(rot), Some(refl)) => {
            // Strictly lower residual wins; ties go to the proper rotation.
            if residual(&refl, from, to) < residual(&rot, from, to) {
                refl
            } else {
                rot
            }
        }
    }
}

/// Build one branch's candidate transform from its accumulated
/// cross-products. `None` when the branch is degenerate (`r ≈ 0`, or a
/// non-finite parameter fell out) — the caller falls back to translation.
fn candidate(
    a: f64,
    b: f64,
    sq: f64,
    reflect: bool,
    fc: (f64, f64),
    tc: (f64, f64),
) -> Option<Transform> {
    let r = (a * a + b * b).sqrt();
    if r.is_nan() || r <= EPS {
        return None;
    }
    let t = Transform {
        cos: (a / r) as f32,
        sin: (b / r) as f32,
        scale: (r / sq) as f32,
        reflect,
        from_centroid: (fc.0 as f32, fc.1 as f32),
        to_centroid: (tc.0 as f32, tc.1 as f32),
    };
    if t.is_finite() && t.scale > 0.0 {
        Some(t)
    } else {
        None
    }
}

/// Apply `t` to every point, in order. Length-preserving.
pub fn apply(t: &Transform, points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    points.iter().map(|&p| t.map(p)).collect()
}

/// Total **sum of squared distances** between `apply(t, from)` and `to`,
/// over index-paired points (extra tail of the longer slice ignored).
///
/// Sum, not mean and not RMS: it is the quantity [`align`] minimises, so
/// comparing two transforms with it is exactly comparing their fit. Divide
/// by `n` yourself if you want a per-point number. Accumulated in `f64`,
/// narrowed once at the end. Non-finite input contributes nothing rather
/// than poisoning the whole sum to `NaN`; an empty pairing scores `0.0`.
pub fn residual(t: &Transform, from: &[(f32, f32)], to: &[(f32, f32)]) -> f32 {
    let n = from.len().min(to.len());
    let mut acc = 0.0f64;
    for (&src, &dst) in from[..n].iter().zip(&to[..n]) {
        let p = t.map(src);
        let dx = p.0 as f64 - dst.0 as f64;
        let dy = p.1 as f64 - dst.1 as f64;
        let d = dx * dx + dy * dy;
        if d.is_finite() {
            acc += d;
        }
    }
    acc as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately irregular fixture — no symmetry, so the reflection
    /// branch is never an accidental tie.
    fn fixture() -> Vec<(f32, f32)> {
        vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 0.5),
            (0.25, 0.75),
            (-0.5, 0.125),
        ]
    }

    fn assert_close(got: &[(f32, f32)], want: &[(f32, f32)], tol: f32) {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                (g.0 - w.0).abs() <= tol && (g.1 - w.1).abs() <= tol,
                "point {i}: got {g:?} want {w:?} (tol {tol})"
            );
        }
    }

    #[test]
    fn identity_alignment_of_a_set_onto_itself() {
        let p = fixture();
        let t = align(&p, &p);
        assert!(!t.reflect, "self-alignment must not mirror");
        assert!((t.cos - 1.0).abs() < 1e-5, "cos = {}", t.cos);
        assert!(t.sin.abs() < 1e-5, "sin = {}", t.sin);
        assert!((t.scale - 1.0).abs() < 1e-5, "scale = {}", t.scale);
        assert!(residual(&t, &p, &p) < 1e-8);
        assert_close(&apply(&t, &p), &p, 1e-5);
    }

    #[test]
    fn pure_rotation_is_recovered_without_trig() {
        // 90° CCW: (x, y) → (−y, x). Exact in binary — no transcendental
        // needed to BUILD the fixture either.
        let p = fixture();
        let q: Vec<(f32, f32)> = p.iter().map(|&(x, y)| (-y, x)).collect();
        let t = align(&p, &q);
        assert!(!t.reflect);
        assert!(t.cos.abs() < 1e-5, "cos should be 0, got {}", t.cos);
        assert!((t.sin - 1.0).abs() < 1e-5, "sin should be 1, got {}", t.sin);
        assert!((t.scale - 1.0).abs() < 1e-5);
        assert!(
            residual(&t, &p, &q) < 1e-8,
            "residual {}",
            residual(&t, &p, &q)
        );
        assert_close(&apply(&t, &p), &q, 1e-5);
    }

    #[test]
    fn pure_scale_is_recovered() {
        let p = fixture();
        let q: Vec<(f32, f32)> = p.iter().map(|&(x, y)| (x * 3.0, y * 3.0)).collect();
        let t = align(&p, &q);
        assert!(!t.reflect);
        assert!((t.scale - 3.0).abs() < 1e-4, "scale = {}", t.scale);
        assert!((t.cos - 1.0).abs() < 1e-5);
        assert_close(&apply(&t, &p), &q, 1e-4);
    }

    #[test]
    fn pure_translation_is_recovered() {
        let p = fixture();
        let q: Vec<(f32, f32)> = p.iter().map(|&(x, y)| (x + 7.5, y - 2.25)).collect();
        let t = align(&p, &q);
        assert!(!t.reflect);
        assert!((t.scale - 1.0).abs() < 1e-5);
        assert!((t.cos - 1.0).abs() < 1e-5);
        assert!(t.sin.abs() < 1e-5);
        assert_close(&apply(&t, &p), &q, 1e-4);
    }

    #[test]
    fn rotation_plus_scale_plus_translation_round_trips() {
        // cos/sin from an exact Pythagorean triple (3,4,5) — again, no
        // trig anywhere in the test itself.
        let (c, s) = (0.6f32, 0.8f32);
        let p = fixture();
        let q: Vec<(f32, f32)> = p
            .iter()
            .map(|&(x, y)| (2.0 * (c * x - s * y) + 10.0, 2.0 * (s * x + c * y) - 4.0))
            .collect();
        let t = align(&p, &q);
        assert!(!t.reflect);
        assert!((t.cos - c).abs() < 1e-4, "cos = {}", t.cos);
        assert!((t.sin - s).abs() < 1e-4, "sin = {}", t.sin);
        assert!((t.scale - 2.0).abs() < 1e-4, "scale = {}", t.scale);
        assert!(residual(&t, &p, &q) < 1e-6);
    }

    #[test]
    fn reflected_set_takes_the_reflection_branch() {
        let p = fixture();
        // Mirror across the x axis, then rotate by (0.6, 0.8) and shift.
        let (c, s) = (0.6f32, 0.8f32);
        let q: Vec<(f32, f32)> = p
            .iter()
            .map(|&(x, y)| (c * x + s * y + 3.0, s * x - c * y + 1.0))
            .collect();
        let t = align(&p, &q);
        assert!(t.reflect, "a mirrored target must pick the reflect branch");
        assert!(
            residual(&t, &p, &q) < 1e-6,
            "residual {}",
            residual(&t, &p, &q)
        );
        assert_close(&apply(&t, &p), &q, 1e-4);
    }

    #[test]
    fn reflection_loses_the_tie_against_an_equal_rotation() {
        // Two points are always fittable BOTH ways with the same residual
        // (a 2-point cloud is symmetric about its own axis). The
        // documented tie-break says the proper rotation wins.
        let p = vec![(0.0, 0.0), (1.0, 0.0)];
        let q = vec![(0.0, 0.0), (0.0, 1.0)];
        let t = align(&p, &q);
        assert!(!t.reflect, "tie must resolve to the non-reflected branch");
        assert!(residual(&t, &p, &q) < 1e-8);
    }

    #[test]
    fn empty_input_is_identity() {
        assert_eq!(align(&[], &[]), Transform::IDENTITY);
        assert_eq!(align(&fixture(), &[]), Transform::IDENTITY);
        assert_eq!(align(&[], &fixture()), Transform::IDENTITY);
        assert!(apply(&Transform::IDENTITY, &[]).is_empty());
        assert_eq!(residual(&Transform::IDENTITY, &[], &[]), 0.0);
    }

    #[test]
    fn single_point_is_pure_translation() {
        let p = vec![(2.0f32, 3.0f32)];
        let q = vec![(-1.0f32, 10.0f32)];
        let t = align(&p, &q);
        assert!(t.is_finite());
        assert!(!t.reflect);
        assert_eq!(t.scale, 1.0);
        assert_eq!(t.cos, 1.0);
        assert_eq!(t.sin, 0.0);
        assert_close(&apply(&t, &p), &q, 1e-6);
    }

    #[test]
    fn all_coincident_from_points_is_pure_translation() {
        let p = vec![(1.0f32, 1.0f32); 4];
        let q = fixture()[..4].to_vec();
        let t = align(&p, &q);
        assert!(t.is_finite(), "degenerate fit must not be NaN: {t:?}");
        assert_eq!(t.scale, 1.0);
        assert!(!t.reflect);
        // Every source point maps to the target centroid.
        for got in apply(&t, &p) {
            assert!(got.0.is_finite() && got.1.is_finite());
        }
    }

    #[test]
    fn all_coincident_to_points_keeps_the_transform_invertible() {
        // r ≈ 0: the true optimum is scale = 0 (collapse). We document
        // and test the deliberate refusal to return a collapsing fit.
        let p = fixture();
        let q = vec![(5.0f32, 5.0f32); 5];
        let t = align(&p, &q);
        assert!(t.is_finite());
        assert_eq!(t.scale, 1.0, "must not collapse to a point");
        assert!(!t.reflect);
    }

    #[test]
    fn non_finite_input_yields_identity_not_nan() {
        let p = vec![(0.0f32, 0.0), (f32::NAN, 1.0), (2.0, 2.0)];
        let q = fixture()[..3].to_vec();
        assert_eq!(align(&p, &q), Transform::IDENTITY);
        let p2 = vec![(0.0f32, 0.0), (f32::INFINITY, 1.0), (2.0, 2.0)];
        assert_eq!(align(&p2, &q), Transform::IDENTITY);
        assert_eq!(align(&q, &p2), Transform::IDENTITY);
    }

    #[test]
    fn mismatched_lengths_pair_by_index_and_ignore_the_tail() {
        let p = fixture();
        let q: Vec<(f32, f32)> = p.iter().map(|&(x, y)| (x + 1.0, y + 1.0)).collect();
        let short = &q[..3];
        let t = align(&p, short);
        assert!(t.is_finite());
        assert!(residual(&t, &p, short) < 1e-6);
    }

    #[test]
    fn residual_is_the_sum_of_squared_distances() {
        // Identity transform, target offset by (3, 4) → 25 per point.
        let p = vec![(0.0f32, 0.0), (1.0, 1.0)];
        let q = vec![(3.0f32, 4.0), (4.0, 5.0)];
        assert!((residual(&Transform::IDENTITY, &p, &q) - 50.0).abs() < 1e-4);
    }

    #[test]
    fn alignment_is_bit_identical_across_runs() {
        let p = fixture();
        let (c, s) = (0.6f32, 0.8f32);
        let q: Vec<(f32, f32)> = p
            .iter()
            .map(|&(x, y)| (1.5 * (c * x - s * y) + 0.25, 1.5 * (s * x + c * y) - 0.75))
            .collect();
        let a = align(&p, &q);
        let b = align(&p, &q);
        assert_eq!(a.cos.to_bits(), b.cos.to_bits());
        assert_eq!(a.sin.to_bits(), b.sin.to_bits());
        assert_eq!(a.scale.to_bits(), b.scale.to_bits());
    }

    /// v0.7.1 "C2" mirror — see the module doc. `atlas.rs` pins the exact
    /// f32 bit patterns of its PCA layout so a transcendental sneaking
    /// back into the hot path (or an `unsafe` fast-math flag) fails CI on
    /// ONE box instead of silently diverging between glibc / musl.
    /// This is the same pin for the alignment path: `+ - * / sqrt` only,
    /// all IEEE-754 correctly rounded, so these bits hold everywhere.
    #[test]
    fn align_golden_bits_pin_cross_libc_determinism() {
        let p = fixture();
        let (c, s) = (0.6f32, 0.8f32);
        let q: Vec<(f32, f32)> = p
            .iter()
            .map(|&(x, y)| (1.5 * (c * x - s * y) + 0.25, 1.5 * (s * x + c * y) - 0.75))
            .collect();
        let t = align(&p, &q);
        assert_eq!(
            (
                t.cos.to_bits(),
                t.sin.to_bits(),
                t.scale.to_bits(),
                t.reflect,
                t.from_centroid.0.to_bits(),
                t.from_centroid.1.to_bits(),
                t.to_centroid.0.to_bits(),
                t.to_centroid.1.to_bits(),
            ),
            GOLDEN_TRANSFORM_BITS,
            "the fitted transform drifted from the pinned golden"
        );
        let mapped: Vec<(u32, u32)> = apply(&t, &p)
            .iter()
            .map(|p| (p.0.to_bits(), p.1.to_bits()))
            .collect();
        assert_eq!(
            mapped,
            GOLDEN_MAPPED_BITS.to_vec(),
            "the mapped coordinates drifted from the pinned golden"
        );
    }

    /// `(cos, sin, scale, reflect, from_cx, from_cy, to_cx, to_cy)` as raw
    /// `f32` bits. Cross-checked against the closed-form answer the fixture
    /// was BUILT from — `cos = 0.6` (`0x3F19999A`), `sin = 0.8`
    /// (`0x3F4CCCCD`), `scale = 1.5` (`0x3FC00000`), `from` centroid
    /// `(0.35, 0.275)`, `to` centroid `(0.235, −0.0825)` — so this pin
    /// records the *right* answer, not merely today's answer.
    const GOLDEN_TRANSFORM_BITS: (u32, u32, u32, bool, u32, u32, u32, u32) = (
        0x3F19999A, 0x3F4CCCCD, 0x3FC00000, false, 0x3EB33333, 0x3E8CCCCD, 0x3E70A3D8, 0xBDA8F5BD,
    );

    /// `apply(t, fixture())` as raw `f32` bits. Every entry was checked
    /// against the closed-form target the fixture was built from — the
    /// exact `q` values `(0.25, −0.75)`, `(1.15, 0.45)`, `(0.55, 0.9)`,
    /// `(−0.425, 0.225)`, `(−0.35, −1.2375)` — and agrees to under 1e-7,
    /// i.e. `f32` round-off. So a future diff here means the ALGORITHM
    /// moved, not that the pin was a snapshot of a wrong answer.
    const GOLDEN_MAPPED_BITS: [(u32, u32); 5] = [
        (0x3E800001, 0xBF400000),
        (0x3F933334, 0x3EE66665),
        (0x3F0CCCCE, 0x3F666666),
        (0xBED99998, 0x3E66666A),
        (0xBEB33334, 0xBF9E6666),
    ];

    #[test]
    fn transform_serializes_round_trip() {
        let t = Transform {
            cos: 0.6,
            sin: 0.8,
            scale: 2.0,
            reflect: true,
            from_centroid: (1.0, 2.0),
            to_centroid: (3.0, 4.0),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Transform = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
