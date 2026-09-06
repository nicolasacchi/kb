// Pure helpers for PRR-U3 — the DiffThread finding variant, the diff
// overlay selector, and the composer's finding mode. Named `diffFindings`
// (not the more generic `findings`) to keep this unit's new file path
// unlikely to collide with a sibling builder's own report-tab findings
// module (both units touch "findings" vocabulary but own disjoint files).
//
// A finding IS a `ReviewComment` (design-ui.md §3: "one thread system, two
// voices" — the server sets `intent: "finding"` on the SAME underlying
// `annotations` row, see `crates/kb-code-server/src/annotations.rs`'s
// `INTENT_FINDING`). The two GETs (`.../comments`, `.../findings`) each
// surface that row from their own angle; this module JOINS them client-side
// by `ReviewFinding.annotation_id === ReviewComment.id` — a finding's
// annotation is always a TOP-LEVEL row (never a reply, per
// `review_findings.rs`'s "a finding's annotation is a 1:1 FK" doc), so the
// join is always against a thread id, never a reply id. No server change
// needed for the join itself; this is the shape the two existing routes
// already support.
//
// Severity/disposition vocab is the PLAN ARBITRATION vocabulary (overrides
// design-ui.md §3's table, which listed a 5-value severity and a 5-value
// disposition): severity = blocker|concern|ok (`store::is_valid_severity`),
// disposition = agree|dispute|waive|fix-later (`store::is_valid_disposition`).

import type { ReviewFinding } from "../api/types";

export type FindingSeverity = "blocker" | "concern" | "ok";
export type FindingDisposition = "agree" | "dispute" | "waive" | "fix-later";
/// PRR-U9 (design-addendum-2.md §D) added `"diagnostics"`: its own
/// mutually-exclusive lane (line-level lip diagnostics instead of comment/
/// finding threads), inserted before the terminal `"none"` so the `o`
/// cycle grows by one stop without reordering the existing four.
/// PRR-F (design-addendum-2.md §A) adds `"github"` the SAME way — one more
/// mutually-exclusive lane (GitHub-origin read-only cards instead of local
/// comment/finding threads), inserted between `"diagnostics"` and the
/// terminal `"none"` so the `o` cycle grows by one more stop without
/// reordering the existing five: all → findings → comments → diagnostics →
/// github → none.
export type OverlayMode = "all" | "findings" | "comments" | "diagnostics" | "github" | "none";

export const SEVERITIES: readonly FindingSeverity[] = ["blocker", "concern", "ok"];
export const DISPOSITIONS: readonly FindingDisposition[] = ["agree", "dispute", "waive", "fix-later"];
export const OVERLAY_MODES: readonly OverlayMode[] = [
  "all",
  "findings",
  "comments",
  "diagnostics",
  "github",
  "none",
];

const SEVERITY_RANK: Record<FindingSeverity, number> = { blocker: 0, concern: 1, ok: 2 };
/// Rank used for a value outside the closed set (forward-compat: an
/// unrecognized severity from a newer daemon sorts last, never throws).
const UNKNOWN_SEVERITY_RANK = 3;

export function isFindingSeverity(v: string): v is FindingSeverity {
  return v === "blocker" || v === "concern" || v === "ok";
}

export function isFindingDisposition(v: string): v is FindingDisposition {
  return v === "agree" || v === "dispute" || v === "waive" || v === "fix-later";
}

/// Lower rank = more severe. An unrecognized/missing severity sorts last.
export function severityRank(s: string | null | undefined): number {
  if (s && isFindingSeverity(s)) return SEVERITY_RANK[s];
  return UNKNOWN_SEVERITY_RANK;
}

const SEVERITY_LABELS: Record<FindingSeverity, string> = {
  blocker: "Blocker",
  concern: "Concern",
  ok: "OK",
};

/// Display label — an unrecognized value degrades to itself verbatim
/// (matches `lib/annotations.ts`'s `intentLabel` forward-compat posture).
export function severityLabel(s: string): string {
  return isFindingSeverity(s) ? SEVERITY_LABELS[s] : s;
}

const DISPOSITION_LABELS: Record<FindingDisposition, string> = {
  agree: "Agree",
  dispute: "Dispute",
  waive: "Waive",
  "fix-later": "Fix later",
};

export function dispositionLabel(d: string): string {
  return isFindingDisposition(d) ? DISPOSITION_LABELS[d] : d;
}

/// Hover/menu copy (design-ui.md §3's disposition semantics paragraph).
const DISPOSITION_HINTS: Record<FindingDisposition, string> = {
  agree: "fix it — becomes agent work",
  dispute: "opens a reply — thread state awaiting-agent",
  waive: "accepted risk — will publish as a note unless unmarked",
  "fix-later": "out of scope for now — tracked, not blocking",
};

export function dispositionHint(d: string): string {
  return isFindingDisposition(d) ? DISPOSITION_HINTS[d] : "";
}

// V70-A5 — `DISPOSITION_KEYS`/`dispositionForKey` lived here: a private
// `a/d/w/f` → verb table, the module's only piece of keyboard knowledge. It
// is gone. The four keys are now four registry rows
// (`diff.disposition.{agree,dispute,waive,fix-later}`, `when: diff.menu`), so
// the `?` sheet, the palette and the conflicts gate can all see them — and
// there is no longer a second place a fifth disposition key could be added
// without the sheet noticing.

// --- the join ---------------------------------------------------------

/// Index findings by their linked annotation (== thread) id. Last write
/// wins on a duplicate `annotation_id` (should not happen — the server's FK
/// is 1:1 — but a map build must still be total).
export function findingsByAnnotationId(
  findings: readonly ReviewFinding[],
): Map<string, ReviewFinding> {
  const m = new Map<string, ReviewFinding>();
  for (const f of findings) m.set(f.annotation_id, f);
  return m;
}

export function findingsBySlug(findings: readonly ReviewFinding[]): Map<string, ReviewFinding> {
  const m = new Map<string, ReviewFinding>();
  for (const f of findings) m.set(f.slug, f);
  return m;
}

// --- overlay filter -----------------------------------------------------

export function parseOverlayParam(v: string | null | undefined): OverlayMode {
  return v === "findings" ||
    v === "comments" ||
    v === "diagnostics" ||
    v === "github" ||
    v === "none"
    ? v
    : "all";
}

/// The `?overlay=` value to WRITE — omitted (`undefined`) for the default
/// `"all"`, same "omit the default" discipline `codeUrl.ts` uses throughout.
export function overlayParamValue(mode: OverlayMode): string | undefined {
  return mode === "all" ? undefined : mode;
}

export function nextOverlayMode(mode: OverlayMode): OverlayMode {
  const i = OVERLAY_MODES.indexOf(mode);
  return OVERLAY_MODES[(i + 1) % OVERLAY_MODES.length];
}

/// Whether a thread — a finding (`isFinding`) or a plain comment — renders
/// under the current overlay filter. `"diagnostics"`/`"github"` hide every
/// LOCAL thread (same as `"none"`) — each is its OWN lane (line-level lip
/// diagnostics / GitHub-origin cards occupy the gutter/rows instead), not
/// an additive toggle on top of findings/comments.
export function threadVisibleInOverlay(isFinding: boolean, overlay: OverlayMode): boolean {
  switch (overlay) {
    case "all":
      return true;
    case "none":
    case "diagnostics":
    case "github":
      return false;
    case "findings":
      return isFinding;
    case "comments":
      return !isFinding;
    default:
      return true;
  }
}

/// Whether GitHub-origin cards render under the current overlay filter —
/// the mirror of `threadVisibleInOverlay` for the `"github"` lane: visible
/// ONLY in that lane, same "own exclusive lane, not folded into `all`"
/// posture the diagnostics gutter marks already establish (`routes/
/// ReviewDiff.tsx`'s `diagByLine` is likewise gated on `overlay ===
/// "diagnostics"` exactly, never `"all"`).
export function githubThreadVisibleInOverlay(overlay: OverlayMode): boolean {
  return overlay === "github";
}

export interface OverlayCounts {
  findings: number;
  comments: number;
}

/// Total findings/comments counts among `threadIds` (UNFILTERED by overlay
/// — the toolbar shows what's available, not what's currently rendered).
export function countByOverlay(
  threadIds: readonly string[],
  findingsById: ReadonlyMap<string, ReviewFinding>,
): OverlayCounts {
  let findings = 0;
  let comments = 0;
  for (const id of threadIds) {
    if (findingsById.has(id)) findings++;
    else comments++;
  }
  return { findings, comments };
}

// --- file-header worst-severity dot --------------------------------------

/// The most severe severity among `findings`, or `null` when empty —
/// drives `DiffFileHeader`'s worst-severity dot.
export function worstSeverity(findings: readonly ReviewFinding[]): FindingSeverity | null {
  let best: FindingSeverity | null = null;
  for (const f of findings) {
    if (!isFindingSeverity(f.severity)) continue;
    if (best === null || severityRank(f.severity) < severityRank(best)) best = f.severity;
  }
  return best;
}

// --- finding view model (what DiffThread's finding variant renders) -------

export interface FindingHeaderView {
  severity: string;
  severityLabel: string;
  category: string;
  slug: string;
  /// `true` when this finding was import-authored (agent) — render the ✳
  /// mark before the author name. `false` (manual/human origin) renders
  /// the plain author string (design-ui.md §3: "✳ agent mark vs 'you' for
  /// manual origin").
  agentMark: boolean;
  author: string;
}

export function findingHeaderView(f: ReviewFinding): FindingHeaderView {
  return {
    severity: f.severity,
    severityLabel: severityLabel(f.severity),
    category: f.category,
    slug: f.slug,
    agentMark: f.origin !== "manual",
    author: f.author,
  };
}

export interface FindingDispositionChip {
  value: FindingDisposition;
  label: string;
  active: boolean;
}

/// The four disposition chips + which one (if any) is currently active.
export function findingDispositionChips(f: ReviewFinding): FindingDispositionChip[] {
  const active = f.disposition?.state;
  return DISPOSITIONS.map((value) => ({
    value,
    label: dispositionLabel(value),
    active: active === value,
  }));
}

// --- composer finding-mode POST shape ------------------------------------

export interface FindingComposerDraft {
  path: string;
  side: "old" | "new";
  line: number;
  severity: string;
  category: string;
  title: string;
  rationale: string;
  recommendation?: string;
}

/// Build the `POST /api/reviews/{id}/findings` body from a composer draft,
/// or `null` when incomplete — same "guard lives here, not repeated per
/// composer" discipline as `lib/reviewComments.ts`'s
/// `buildReviewCommentPayload`. Location is always `kind: "single"` at the
/// composer's own line (findings authored from the diff composer are never
/// path-less/general — design-addendum-2 §E's "findings are code-anchored"
/// rule, enforced here by construction rather than by a server 400).
/// `side: "old"` maps to `location.removed: true` (the anchor must read the
/// BASE blob, mirroring `build_finding_anchor`'s own `location.removed`
/// dispatch server-side).
export function buildManualFindingPayload(
  draft: FindingComposerDraft,
): { severity: string; category: string; location: { kind: "single"; path: string; lines: number[]; removed: boolean }; title: string; rationale: string; recommendation?: string } | null {
  const severity = draft.severity.trim();
  const category = draft.category.trim();
  const title = draft.title.trim();
  const rationale = draft.rationale.trim();
  if (!isFindingSeverity(severity)) return null;
  if (!category || !title || !rationale) return null;
  if (!Number.isFinite(draft.line) || draft.line < 1) return null;
  const recommendation = draft.recommendation?.trim();
  return {
    severity,
    category,
    location: {
      kind: "single",
      path: draft.path,
      lines: [draft.line],
      removed: draft.side === "old",
    },
    title,
    rationale,
    ...(recommendation ? { recommendation } : {}),
  };
}
