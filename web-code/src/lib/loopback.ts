// "Can THIS caller reach the daemon's loopback-only routes?" (V74-L2).
//
// `kbc-canvas/1`'s four mutations (`apply`/`accept`/`archive`/`DELETE`) ride
// the loopback-only sub-router, and D10's later graduation onto a named-family
// `[review] remote_mutations = ["review", "canvas"]` allowlist is its OWN unit
// — so there is no per-family capability bit for boards the way
// `IdentityOut.remote_mutations` carries one for reviews.
//
// What there IS is the daemon's own verdict about the CALLER:
// `GET /api/repos` answers `{ repos, loopback }`, derived server-side by
// `kb_server::middleware::is_loopback_origin` from the peer address and the
// trusted-proxy chain — the very predicate the mutation gate itself applies.
// That is a fact, not a guess, and it is what this module reads. Deriving it
// from `window.location.hostname` would have been a second, quietly different
// answer to a question the server already answers exactly.
//
// It decides what to OFFER, never what is allowed: every mutation still goes
// to the daemon, and a refusal is surfaced with the daemon's own message
// (`routes/BoardDetail.tsx`'s `refusalMessage`), following the
// `components/lens/Scorecard.tsx` precedent for a loopback-only action.

import type { ReposResponse } from "../api/types";

/// `true` only when the daemon SAID so. An older daemon that omits the field,
/// a failed fetch and an in-flight one all read `false` — the safe direction,
/// because hiding an affordance that would have worked costs a caption while
/// offering one that cannot costs a 404 the operator has to interpret.
export function isLoopbackCaller(repos: ReposResponse | undefined): boolean {
  return repos?.loopback === true;
}
