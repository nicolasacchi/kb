// V80-F2 — the loopback PRE-PROBE. Standing v0.37 deferral: `VerdictBar`,
// the diff finding composer and the disposition controls all used to learn
// they were refused only AFTER a submit hit `review_mutations_gate`'s
// byte-identical 404 (`crates/kb-code-server/src/review_gate.rs`). The
// human should know BEFORE — a caption, and the control disabled with a
// reason, never hidden (root CLAUDE.md's honesty rule: "a filter names what
// it hides").
//
// This hook is the ONE place that reads the probe: the daemon computes
// `IdentityOut.review_mutations_admitted` PER REQUEST from the exact same
// peer classification the gate itself runs (loopback → true; non-loopback →
// `[review] remote_mutations`), so it can never disagree with what the
// gate would actually do for THIS caller on the very next request. Riding
// `useIdentity()` (mounted once at the app root, `staleTime: Infinity`)
// means every consumer reads the SAME cached fetch rather than each
// re-deriving the field.

import { useIdentity } from "./useIdentity";

/// Shown as the inline caption beside a disabled control AND as that
/// control's `title` tooltip — one string, so hovering says the same thing
/// reading says. Names both remedies the gate itself offers.
export const REVIEW_MUTATIONS_ADMITTED_HINT =
  "review writes are loopback-only on this daemon — open kb-code on the box, or set [review] remote_mutations = true";

/// V80-F2b (a real compatibility bug found post-merge): a consumer must
/// disable ONLY on an EXPLICIT `false`. `admitted` is `true` whenever the
/// daemon has not said otherwise — an older daemon that omits the field
/// (a rolling deploy, mixed fleet), a failed identity fetch, and an
/// in-flight one all read `admitted: true` here, never `false`. Getting
/// this backwards (V80-F2's original shape: absent read as `false`) is a
/// REAL regression against an old server — it pre-empts a write the
/// daemon would actually have admitted, on nothing more than "I don't yet
/// know". `unknown` names the reason: it is `true` exactly when
/// `admitted: true` was NOT read from an explicit server `true` — a
/// consumer wanting to distinguish "confirmed admitted" from "unconfirmed,
/// optimistically admitted" may read it, but neither currently needs to:
/// the pre-existing post-submit 404 latch (`VerdictBar`'s `refused` state
/// and its siblings) is what discovers an unknown-but-actually-refused
/// write, exactly as it did before this field existed at all.
export interface ReviewMutationsAdmitted {
  /// Whether a control should render enabled. `false` if AND ONLY IF the
  /// daemon's own `GET /api/identity` explicitly said
  /// `review_mutations_admitted: false` for THIS request.
  admitted: boolean;
  /// `true` when the daemon has not told us either way yet — `admitted`
  /// is optimistic in this case, never a confirmed fact.
  unknown: boolean;
}

export function useReviewMutationsAdmitted(): ReviewMutationsAdmitted {
  const identity = useIdentity();
  const raw = identity.data?.review_mutations_admitted;
  if (raw === false) return { admitted: false, unknown: false };
  return { admitted: true, unknown: raw === undefined };
}
