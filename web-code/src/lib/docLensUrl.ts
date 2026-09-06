// DCB W2.B — URL builders + pure selection helpers for the doc↔code lens
// page (`routes/Lens.tsx` + `components/lens/*`). Mirrors `lib/setsUrl.ts`'s
// discipline (golden-pinned, one builder per route shape, never an ad hoc
// template string at a call site) and `lib/codeUrl.ts`'s per-segment
// `encodeURIComponent` convention for path segments.
//
// (reconciled: R2/D14) — the repo-scoped route carries the kb ARTIFACT ID,
// not a source-relative path, so `lensUrl`/`lensEntryUrl` take a plain
// `docId` segment (single `encodeURIComponent`, no splat). The PATH-
// addressed ramp (`lensEntryByPathUrl`) is the one builder here that still
// carries a multi-segment splat, mirroring `codeUrl.ts`'s own
// `encodePathSegments` (split on `/`, drop empty segments, encode each).

import type { CodeLensGroup, CodeLensOut, CodeLensRef } from "../api/types";
import { UNGROUPED_SEL, type GroupSelection } from "../components/lens/GroupRail";

const LENS_SEGMENT = "~lens";

/// `lensUrl(repo, kb, docId)` → `/r/{repo}/~lens/{kb}/{docId}` — the
/// repo-scoped lens route (`app.tsx`'s `/r/:repo/~lens/:kb/:docId`).
export function lensUrl(repo: string, kb: string, docId: string): string {
  return `/r/${encodeURIComponent(repo)}/${LENS_SEGMENT}/${encodeURIComponent(kb)}/${encodeURIComponent(docId)}`;
}

/// `lensEntryUrl(kb, docId)` → `/~lens/{kb}/{docId}` — the repo-LESS,
/// ID-addressed entry ramp (`LensEntry.tsx`). **This is the canonical
/// deep-link target for a caller that already holds the artifact id** (kb's
/// own reader always does — 13-w1d's Code section links here, R22).
export function lensEntryUrl(kb: string, docId: string): string {
  return `/${LENS_SEGMENT}/${encodeURIComponent(kb)}/${encodeURIComponent(docId)}`;
}

const BY_PATH_SEGMENT = "by-path";

/// `lensEntryByPathUrl(kb, path)` → `/~lens/{kb}/by-path/{path}` — the
/// repo-less, PATH-addressed entry ramp (`LensEntryByPath.tsx`, R2). `path`
/// is split on `/`, empty segments dropped, each segment individually
/// `encodeURIComponent`-ed — same convention `lib/codeUrl.ts`'s
/// `encodePathSegments` uses, so a literal `/` inside a filename can never
/// be confused with a path separator.
export function lensEntryByPathUrl(kb: string, path: string): string {
  const encodedPath = path
    .split("/")
    .filter((s) => s !== "")
    .map(encodeURIComponent)
    .join("/");
  return `/${LENS_SEGMENT}/${encodeURIComponent(kb)}/${BY_PATH_SEGMENT}/${encodedPath}`;
}

// --- selection helpers (§4.1/§5) --------------------------------------------
//
// `UNGROUPED_SEL`/`GroupSelection` are DEFINED in `components/lens/
// GroupRail.tsx` (that file's own doc has the full "why" — R9's no-wire-
// sentinel rule) and imported here; this module re-exports both so a
// caller that only needs the selection helpers (`useLensKeys.ts`) has one
// import path rather than two.
export { UNGROUPED_SEL, type GroupSelection };

/// The ref rows for one group selection — `null` (All) returns every ref,
/// [`UNGROUPED_SEL`] returns `group === null` refs, any other string an
/// exact `group` match. A display-time filter, never a count derivation
/// (`groups[].ref_count`/`ungrouped_count` on the wire already carry the
/// counts — see `GroupRail.tsx`'s own doc, R9).
export function refsForGroup(refs: CodeLensRef[], selection: GroupSelection): CodeLensRef[] {
  if (selection === null) return refs;
  if (selection === UNGROUPED_SEL) return refs.filter((r) => r.group === null);
  return refs.filter((r) => r.group === selection);
}

// --- pin pre-selection correction (§4.0/DCB-W2.B.R fix 4) -------------------

/// The pure correction decision behind `Lens.tsx`'s "correct to the doc's
/// pinned repo ONE TIME" effect (Decision 1: the last pick per doc
/// pre-selects the switcher next time). Returns the repo to navigate to, or
/// `null` when no correction should happen:
///
/// - `seeded` (this doc has already been resolved once this mount — see
///   `Lens.tsx`'s own per-`docId` guard, W2.B.R fix 7) ⇒ `null`, ALWAYS —
///   Decision 1 is a one-time nudge, never a standing override of a repo the
///   operator has since picked by hand.
/// - `pinned` is `null`/`undefined` (no scorecard resolved yet, or the doc
///   genuinely has no pin) ⇒ `null`.
/// - `pinned === current` (the URL already names the pinned repo) ⇒ `null`.
/// - otherwise ⇒ `pinned` (navigate there).
export function pinCorrection(
  pinned: string | null | undefined,
  current: string,
  seeded: boolean,
): string | null {
  if (seeded) return null;
  if (!pinned) return null;
  if (pinned === current) return null;
  return pinned;
}

// --- truncation/partial-resolution honesty caption (DCB-W2.B.R fix 3) ------

/// One caption line surfacing `codelens/1`'s `truncated`/`partial` signals
/// honestly — ported from kb's own `web/src/components/CodeRefsSection.tsx`
/// `truncationCaption` (W1.D.R #4), trimmed to the TWO signals `codelens/1`
/// itself carries (kb-code's own lens-level row cap, and the resolution
/// loop's deadline). Kb's version additionally folds in a THIRD,
/// coderef/1-only extraction-cap signal (`codeRefsTruncated`) that has no
/// analogue here — `codelens/1` is already the fully-resolved surface, not
/// a raw-extraction summary a second cap could apply to. `undefined` before
/// a repo is picked or while the lens query is still loading.
export function truncationCaption(lens: CodeLensOut | undefined): string | null {
  if (!lens) return null;
  const parts: string[] = [];
  if (lens.truncated) {
    parts.push(`showing ${lens.refs.length} of ${lens.counts.total}`);
  }
  if (lens.partial) {
    parts.push(`resolution incomplete (${lens.partial_reason ?? "budget"})`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

/// Step the group selection by `dir` (+1/-1), circularly, over
/// `[null, ...groups (ordinal order), UNGROUPED_SEL?]` — `UNGROUPED_SEL`
/// only joins the ring when `ungroupedCount > 0` (an empty Ungrouped
/// trailer is never a stop the `(`/`)` keys can land on). `groups` is
/// assumed already in the server's own `ordinal` order (never re-sorted
/// here — `GroupRail.tsx` doesn't re-sort it either).
export function stepGroup(
  groups: CodeLensGroup[],
  ungroupedCount: number,
  current: GroupSelection,
  dir: 1 | -1,
): GroupSelection {
  const ring: GroupSelection[] = [null, ...groups.map((g) => g.key)];
  if (ungroupedCount > 0) ring.push(UNGROUPED_SEL);
  if (ring.length === 0) return null;
  const idx = ring.indexOf(current);
  const from = idx === -1 ? 0 : idx;
  const next = (from + dir + ring.length) % ring.length;
  return ring[next];
}
