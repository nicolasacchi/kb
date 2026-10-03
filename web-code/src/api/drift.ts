/* Compile-time drift guard between a hand-written type in types.ts that is
 * deliberately NOT a plain re-export of its ts-rs twin in ./generated/ and
 * that twin (mirrors web/src/api/drift.ts; `scripts/check-ts-shadows.sh`
 * accepts a shadow only when it is named here with an assertion, or sits on
 * its grandfather ratchet).
 *
 * - `Satisfies` — every value the daemon can send is a valid value of the
 *   hand type (wire ⊆ hand). Used where the hand type is deliberately wider.
 *
 * Types-only: `npm run build`'s tsc is the test; nothing here survives into
 * the bundle.
 */

import type { FramesResponse as FramesResponseWire } from "./generated/FramesResponse";
import type { ClaimOut as ClaimOutWire } from "./generated/ClaimOut";
import type { ClaimOut, FramesResponse } from "./types";

type Satisfies<Wire, Hand> = [Wire] extends [Hand] ? true : false;
type Assert<T extends true> = T;

// `FramesResponse` stays hand-written and WIDER: its `frames[].source` /
// `off_head` are plain strings here but closed unions (`FrameSource`,
// `OffHeadClass`) on the wire, and consumers index them with arbitrary
// strings. Every wire value must still read as the hand type.
export type _FramesResponse = Assert<Satisfies<FramesResponseWire, FramesResponse>>;

// `ClaimOut` stays hand-written and NARROWER: `subject_kind` / `kind` /
// `state` are closed unions here (`ClaimSubjectKind`, `ClaimKind`,
// `ClaimLadderState`) but plain strings on the wire, and `refs` is optional
// (older fixtures omit it; the routes always send it). So the check runs
// hand ⊆ wire: every hand value must be a valid wire value, field for field,
// with `refs` compared on its own below.
export type _ClaimOut = Assert<Satisfies<Omit<ClaimOut, "refs">, Omit<ClaimOutWire, "refs">>>;
export type _ClaimOutRefs = Assert<
  Satisfies<NonNullable<ClaimOut["refs"]>, ClaimOutWire["refs"]>
>;
