/* Compile-time drift guard between the hand-written types in client.ts
 * and the ts-rs-generated wire types in ./generated/ (regenerate with
 * `just types`; `just types-check` asserts the committed output is in
 * sync with the Rust structs).
 *
 * Two strengths, chosen per pair:
 *
 * - `Mutual` — the client type IS the wire type, field for field. Used
 *   where client.ts models exactly one endpoint's response. Catches
 *   both server-side drift AND client-side invention (a field the
 *   server never sent — `KbSummary.source_count` lived here for
 *   months).
 *
 * - `Satisfies` — every value the daemon can send is a valid value of
 *   the client type (wire ⊆ client). Used where the client type is
 *   deliberately wider: DocSummary also reads the lookup endpoint's
 *   page-bearing shape, and the review compose paths construct
 *   Anchor/Choice values where serde fills defaults, so requiring
 *   every wire field at construction time would be hostile. Still
 *   catches the dangerous direction: a removed/renamed/retyped field
 *   breaks the check.
 *
 * ReviewFile is checked with `schema` exempted — client.ts narrows it
 * to the literal "kb-comments/1" (the daemon validates exactly that on
 * save), which the generated `schema: string` can never satisfy. The
 * rest of the document shape is held to the usual bar.
 *
 * This module is types-only — `npm run typecheck` is the test; nothing
 * here survives into the bundle.
 */

import type { DocResponse } from "./generated/DocResponse";
import type { Anchor as AnchorWire } from "./generated/Anchor";
import type { NoteSummary as NoteSummaryWire } from "./generated/NoteSummary";
import type { Anchor, DocSummary } from "./client";
import type { NoteSummary } from "./notes";

type Satisfies<Wire, Client> = [Wire] extends [Client] ? true : false;
type Assert<T extends true> = T;

// DocSummary also reads the lookup endpoint's page-bearing shape, so it
// stays hand-written and wider; the wire row set must satisfy it.
export type _DocSummary = Assert<Satisfies<DocResponse, DocSummary>>;

// Anchor stays hand-written: the compose paths CONSTRUCT anchors and
// serde fills the defaulted fields, so requiring tag/snippet at
// construction time would be hostile. Every wire anchor must still be
// readable as the client type.
export type _Anchor = Assert<Satisfies<AnchorWire, Anchor>>;

// The deliberately-NARROWER client type runs the check the other
// way: every CLIENT value must be a valid wire value, so a wire-side
// rename/retype still breaks the build. (Narrower-than-wire is safe
// here by construction: notes.ts only reads the cross-kb route, which
// always sets `kb`.)
export type _NoteSummary = Assert<Satisfies<NoteSummary, NoteSummaryWire>>;
