// Phase E4 ("kb-code v2 — The Operable Reader") — the single deep-link
// builder for reading sets: the sets list, one set's detail, and tour mode.
// Mirrors `lib/codeUrl.ts`'s discipline (golden-pinned, one builder, see
// that module's header doc + kb's own `galleryUrl`, root CLAUDE.md
// invariant #35) — every clickable surface that lands on a sets route (the
// reader's "+ Set" menu toast link, a set detail row's tour button,
// SessionDiff's "Save as reading set" success navigation) goes through
// these three functions, never an ad hoc template string.
//
// All three are repo-scoped, non-file sentinels — same footing as
// `~commit`/`~compare`/`~branches`/`~prs` (`lib/codeUrl.ts`'s own header
// doc): a fixed position right after `:repo`, registered as their own
// static `<Route>`s in `app.tsx`, never riding Reader's splat parsing the
// way `~diff`/`~story` do (those trail an arbitrary FILE path; a set has no
// file path of its own).

import { codeBasePath } from "./codeUrl";

const SETS_SEGMENT = "~sets";
const TOUR_SEGMENT = "~tour";
const WORKSPACES_SEGMENT = "~workspaces";

/// `setsUrl(repo)` → `/r/{repo}/~sets` — the reading-sets list.
export function setsUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/${SETS_SEGMENT}`;
}

/// `setUrl(repo, id)` → `/r/{repo}/~sets/{id}` — one set's detail page.
export function setUrl(repo: string, id: string): string {
  return `${setsUrl(repo)}/${encodeURIComponent(id)}`;
}

/// `tourUrl(repo, id, step?)` → `/r/{repo}/~sets/{id}/~tour[?step=N]` — tour
/// mode, stepping through the set's ordered spans. `step` is 1-based on the
/// wire (human-friendly — "step 3 of 8" in a shared link); omitted (or
/// non-positive) starts at the set's first span.
export function tourUrl(repo: string, id: string, step?: number): string {
  const base = `${setUrl(repo, id)}/${TOUR_SEGMENT}`;
  return step !== undefined && step > 0 ? `${base}?step=${step}` : base;
}

/// V70-A10 ("Workspaces v0") — `workspacesUrl(repo)` → `/r/{repo}/
/// ~workspaces` — the branch-view list of workspaces (grouped by `ref`),
/// optionally pre-filtered to one `ref` (a `~branches` row's "N
/// workspaces" chip, D26). Unlike `~sets`, there is no separate detail
/// route — the list page IS the only workspace surface (open navigates
/// straight to the reader, `?workspace=<id>`; rename/delete/duplicate are
/// row actions on this same page).
export function workspacesUrl(repo: string, ref?: string): string {
  const base = `${codeBasePath(repo, "")}/${WORKSPACES_SEGMENT}`;
  return ref ? `${base}?ref=${encodeURIComponent(ref)}` : base;
}
