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

/// `true` only when the daemon SAID so for THIS request. An older daemon
/// that omits the field, a failed identity fetch, and an in-flight one all
/// read `false` — the same safe-direction convention `lib/loopback.ts`'s
/// `isLoopbackCaller` documents: hiding an affordance that would have
/// worked costs a caption; offering one that cannot costs a 404 the
/// operator has to interpret. Never used to pre-empt a request the server
/// would actually admit — it is read FROM the server's own per-request
/// verdict, not guessed at client-side from `remote_mutations` + a
/// separately-fetched loopback bit.
export function useReviewMutationsAdmitted(): boolean {
  const identity = useIdentity();
  return identity.data?.review_mutations_admitted === true;
}
