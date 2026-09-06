// V70-A4 — what the main region can show.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "Center modes:
// reader · diff · dossier · board · dashboard · results", and §D1:
// "Reader → diff → dossier → board → dashboard. … Any new surface lands
// in an existing region (CI-checked by the landmark golden)."
//
// The union is declared WHOLE here even though only `reader` is
// implemented, because it is the thing the landmark golden iterates: the
// e2e spec loops `SHIPPED_CENTER_MODES` and asserts the region set is
// identical for each. Today that loop runs once; when `diff` lands it
// runs twice with no spec edit, which is the point of writing the loop
// before the second mode exists.

export type CenterMode = "reader" | "diff" | "dossier" | "board" | "dashboard" | "results";

export const CENTER_MODES: readonly CenterMode[] = [
  "reader",
  "diff",
  "dossier",
  "board",
  "dashboard",
  "results",
];

/// The modes a route can actually mount today. Deliberately separate
/// from `CENTER_MODES`: the vocabulary is fixed now so the shell's slot
/// contract is fixed now, but claiming a mode is shipped before it is
/// would make the landmark golden green over nothing.
///
/// V72-G1.2 added `dossier` — `entity/1`'s page, mounted by the SAME
/// `/r/:repo/*` route the reader owns when the location carries `?ent=`
/// (D1: "any new surface lands in an existing REGION"). It is deliberately
/// NOT a new route: the dossier is a different CENTER over the same shell,
/// so the dock, the rail, the drawer and both stripes are the ones that
/// were already there, and `e2e/desk-landmarks.spec.ts` proves it by
/// asserting the identical region set for both modes.
export const SHIPPED_CENTER_MODES: readonly CenterMode[] = ["reader", "dossier"];

export function isShippedCenterMode(m: string): m is CenterMode {
  return (SHIPPED_CENTER_MODES as readonly string[]).includes(m);
}
