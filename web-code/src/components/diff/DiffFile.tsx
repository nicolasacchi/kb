import { useMemo, type ReactNode } from "react";
import type { GithubThread } from "../../api/types";
import { useIsMobile } from "../../hooks/useIsMobile";
import type { ParsedDiff } from "../../lib/diff";
import type { DiagnosticGutterMark } from "../../lib/diagnostics";
import type { DiffHighlights } from "../../lib/diffHighlight";
import { worstSeverity } from "../../lib/diffFindings";
import type { DiffMode } from "../../lib/prefs";
import type { DiffCommentsApi } from "../../lib/reviewComments";
import DiffFileHeader from "./DiffFileHeader";
import type { HunkView } from "./HunkStrip";
import SplitHunks from "./SplitHunks";
import UnifiedHunks from "./UnifiedHunks";

export interface DiffFileProps {
  repo?: string;
  path: string;
  parsed: ParsedDiff;
  mode: DiffMode;
  onModeChange?: (mode: DiffMode) => void;
  /// Forwarded to UnifiedHunks for the existing diff-comment button.
  sha?: string;
  headerSlot?: ReactNode;
  /// V4.D2 — per-side highlight maps. `null` / omitted = plain text.
  highlights?: DiffHighlights | null;
  /// V4.C4 — review-scoped threads. Absent on Commit/Compare.
  comments?: DiffCommentsApi | null;
  /// PRR-U9 — "2 errors · 5 warnings" (design-addendum-2.md §D). Absent on
  /// Commit/Compare (only `routes/ReviewDiff.tsx` fetches diagnostics).
  diagnosticsChip?: string | null;
  /// PRR-U9 — per-NEW-side-line severity marks, passed ONLY while the
  /// review diff's overlay selector is in the `"diagnostics"` lane
  /// (`routes/ReviewDiff.tsx` computes this — `DiffFile` itself doesn't
  /// know about overlay modes, it just renders whatever map it's given, or
  /// nothing when `null`/absent).
  diagnosticsByLine?: Map<number, DiagnosticGutterMark> | null;
  /// S2-C — clicking `diagnosticsChip` toggles this open; `diagnosticsCard`
  /// is the SHARED `DiagnosticsCard` element itself (rendered by the
  /// caller, `null` while collapsed) so `DiffFile` stays free of any
  /// `useDiagnostics`/quick-fixes knowledge of its own — same "caller
  /// builds the ReactNode, this component just places it" split
  /// `InspectorRail`'s own `diagnosticsCard` slot already establishes.
  onDiagnosticsClick?: () => void;
  diagnosticsCardOpen?: boolean;
  diagnosticsCard?: ReactNode | null;
  /// PRR-F (design-ui.md §12.2, "Reviewer X-ray") chip text + click handler
  /// — see `DiffFileHeader`'s own doc.
  impactChip?: string | null;
  onImpactClick?: () => void;
  /// PRR-F (design-addendum-2.md §A) — GitHub-origin threads for THIS file,
  /// indexed by the SAME `threadLineKey` grammar `comments.byLine` uses
  /// (`lib/githubThreads.ts`'s `indexGithubThreadsByLine`), passed ONLY
  /// while the overlay selector is in the `"github"` lane — same "own
  /// exclusive lane" gating `diagnosticsByLine` already establishes.
  githubByLine?: Map<string, GithubThread[]> | null;
  githubOrphans?: GithubThread[];
  /// V73-K2a — per-hunk state (`HunkStrip`'s own `HunkView` doc), forwarded
  /// verbatim to whichever renderer is mounted. `DiffFile` computes none of
  /// it: one derivation in `FileDiffBody`, two renderers — the same "one
  /// projection, two renderers" rule kbc-tree/1 states.
  hunkViews?: readonly HunkView[] | null;
  onHunkFold?: (hunkIdx: number) => void;
  onHunkViewed?: (hunkIdx: number) => void;
  onHunkExpand?: (hunkIdx: number, dir: "up" | "down") => void;
  currentHunk?: number | null;
  /// V73-K2c (kbc-hunk-turns/1) — see `UnifiedHunks`/`SplitHunks`'s own doc;
  /// forwarded verbatim to whichever renderer is mounted.
  onHunkTurns?: (hunkIdx: number) => void;
  turnsOpenId?: string | null;
  turnsPanel?: ReactNode | null;
  /// V80-M1 — the caption shown when `parsed.hunks.length === 0` and no
  /// synthetic whole-file body was built (deleted / binary / absent-at-tip
  /// / still loading — `lib/diffContext.ts`'s `emptyDiffView`). Defaults to
  /// the pre-existing literal text, so every caller that does not pass this
  /// (Commit/Compare/SessionDiff, and the review diff before this unit)
  /// renders byte-identically.
  emptyNote?: ReactNode;
  /// V80-M1 — shown ABOVE the hunk renderer, ONLY while a synthetic
  /// whole-file body (`hunkViews` non-empty despite zero real hunks) is
  /// actually rendering. `undefined` shows nothing — no existing caller
  /// (none passes `hunkViews` for a zero-hunk file) is affected.
  wholeFileNote?: ReactNode;
}

/// Orchestrator every call site uses: header (path / stats / optional
/// mode toggle) then unified or split hunks. Mobile (≤860px) always
/// renders unified; the split toggle is also CSS-hidden at that
/// breakpoint.
export default function DiffFile({
  repo,
  path,
  parsed,
  mode,
  onModeChange,
  sha,
  headerSlot,
  highlights,
  comments,
  diagnosticsChip,
  diagnosticsByLine,
  onDiagnosticsClick,
  diagnosticsCardOpen,
  diagnosticsCard,
  impactChip,
  onImpactClick,
  githubByLine,
  githubOrphans,
  hunkViews,
  onHunkFold,
  onHunkViewed,
  onHunkExpand,
  currentHunk,
  onHunkTurns,
  turnsOpenId,
  turnsPanel,
  emptyNote,
  wholeFileNote,
}: DiffFileProps) {
  const isMobile = useIsMobile();
  const effectiveMode: DiffMode = isMobile ? "unified" : mode;

  // PRR-U3 — this file's findings (unfiltered by overlay — a structural
  // summary, see `DiffFileHeader`'s own doc), for the worst-severity dot +
  // count badge.
  const fileFindings = useMemo(
    () =>
      comments
        ? [...comments.findingsById.values()].filter((f) => f.location.path === path)
        : [],
    [comments, path],
  );
  const worstFindingSeverity = fileFindings.length > 0 ? worstSeverity(fileFindings) : null;
  const findingCount = fileFindings.length;

  if (parsed.binary) {
    return (
      <div className="kbc-diff kbc-diff--binary" data-kbc-diff-mode="binary">
        <DiffFileHeader
          path={path}
          parsed={parsed}
          mode={effectiveMode}
          onModeChange={onModeChange}
          headerSlot={headerSlot}
          worstFindingSeverity={worstFindingSeverity}
          findingCount={findingCount}
          diagnosticsChip={diagnosticsChip}
          onDiagnosticsClick={onDiagnosticsClick}
          diagnosticsCardOpen={diagnosticsCardOpen}
          impactChip={impactChip}
          onImpactClick={onImpactClick}
        />
        Binary file differs
      </div>
    );
  }

  // V80-M1 — a caller (`FileDiffBody`) that built a synthetic whole-file
  // hunk hands it here ALREADY FOLDED into `parsed` (its own doc: the
  // caller passes the EFFECTIVE parse, real or synthetic, as this prop) —
  // so `parsed.hunks.length === 0` alone still tells the two branches
  // apart, exactly as it did before this unit; no caller (this one
  // included) ever needs a second flag. `wholeFileNote` is gated on being
  // non-empty ALONE — every pre-existing caller leaves it `undefined`, so
  // a real multi-hunk diff never shows it.
  if (parsed.hunks.length === 0) {
    return (
      <div className="kbc-diff kbc-diff--empty" data-kbc-diff-mode="empty">
        <DiffFileHeader
          path={path}
          parsed={parsed}
          mode={effectiveMode}
          onModeChange={onModeChange}
          headerSlot={headerSlot}
          worstFindingSeverity={worstFindingSeverity}
          findingCount={findingCount}
          diagnosticsChip={diagnosticsChip}
          onDiagnosticsClick={onDiagnosticsClick}
          diagnosticsCardOpen={diagnosticsCardOpen}
          impactChip={impactChip}
          onImpactClick={onImpactClick}
        />
        <span data-kbc-diff-empty-note>{emptyNote ?? "No textual difference"}</span>
      </div>
    );
  }

  return (
    <div className="kbc-diff" data-kbc-diff-mode={effectiveMode}>
      <DiffFileHeader
        path={path}
        parsed={parsed}
        mode={effectiveMode}
        onModeChange={isMobile ? undefined : onModeChange}
        headerSlot={headerSlot}
        worstFindingSeverity={worstFindingSeverity}
        findingCount={findingCount}
        diagnosticsChip={diagnosticsChip}
        onDiagnosticsClick={onDiagnosticsClick}
        diagnosticsCardOpen={diagnosticsCardOpen}
        impactChip={impactChip}
        onImpactClick={onImpactClick}
      />
      {diagnosticsCardOpen && diagnosticsCard}
      {wholeFileNote && (
        <p className="kbc-diff__wholefile-note" role="status" data-kbc-diff-wholefile-note>
          {wholeFileNote}
        </p>
      )}
      {effectiveMode === "split" ? (
        <SplitHunks
          path={path}
          parsed={parsed}
          highlights={highlights}
          comments={comments}
          diagnosticsByLine={diagnosticsByLine}
          githubByLine={githubByLine}
          githubOrphans={githubOrphans}
          hunkViews={hunkViews}
          onHunkFold={onHunkFold}
          onHunkViewed={onHunkViewed}
          onHunkExpand={onHunkExpand}
          currentHunk={currentHunk}
          onHunkTurns={onHunkTurns}
          turnsOpenId={turnsOpenId}
          turnsPanel={turnsPanel}
        />
      ) : (
        <UnifiedHunks
          path={path}
          parsed={parsed}
          repo={repo}
          sha={sha}
          highlights={highlights}
          diagnosticsByLine={diagnosticsByLine}
          comments={comments}
          githubByLine={githubByLine}
          githubOrphans={githubOrphans}
          hunkViews={hunkViews}
          onHunkFold={onHunkFold}
          onHunkViewed={onHunkViewed}
          onHunkExpand={onHunkExpand}
          currentHunk={currentHunk}
          onHunkTurns={onHunkTurns}
          turnsOpenId={turnsOpenId}
          turnsPanel={turnsPanel}
        />
      )}
    </div>
  );
}
