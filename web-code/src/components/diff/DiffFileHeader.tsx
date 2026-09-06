import type { ReactNode } from "react";
import { Icon } from "../icons";
import { diffStats } from "../../lib/diff";
import type { ParsedDiff } from "../../lib/diff";
import type { FindingSeverity } from "../../lib/diffFindings";
import type { DiffMode } from "../../lib/prefs";

export interface DiffFileHeaderProps {
  path: string;
  parsed: ParsedDiff;
  mode: DiffMode;
  onModeChange?: (mode: DiffMode) => void;
  headerSlot?: ReactNode;
  /// PRR-U3 — worst severity + total count among this file's findings
  /// (UNFILTERED by the overlay selector — a structural "this file has
  /// findings" summary, mirroring the Files tab). `null`/`0` renders
  /// nothing (named absence, not an empty badge).
  worstFindingSeverity?: FindingSeverity | null;
  findingCount?: number;
  /// PRR-U9 (design-addendum-2.md §D) — "2 errors · 5 warnings", lazily
  /// fetched (`lib/diagnostics.ts`'s `diagnosticsChipText`) when the file
  /// section first expands. `null`/absent renders nothing — same "named
  /// absence, not an empty badge" posture as the finding badge above; the
  /// chip is intentionally quiet for the loading/clean/refused states (the
  /// inspector Diagnostics card carries those, not the diff header).
  diagnosticsChip?: string | null;
  /// S2-C — clicking the chip toggles the SHARED `DiagnosticsCard`
  /// inspector open below the header (`routes/ReviewDiff.tsx`'s own doc);
  /// `undefined` renders the chip as a plain non-interactive `<span>`
  /// (Commit/Compare, which never fetch diagnostics at all).
  onDiagnosticsClick?: () => void;
  diagnosticsCardOpen?: boolean;
  /// PRR-F (design-ui.md §12.2, "Reviewer X-ray") — "12 callers · 2 in this
  /// diff", lazily fetched (`GET /api/reviews/{id}/impact?path=`) the SAME
  /// way `diagnosticsChip` is — once the file section expands. `null`/
  /// absent renders nothing (unsupported language, zero callers, or still
  /// loading — all named absence, not an empty chip).
  impactChip?: string | null;
  onImpactClick?: () => void;
}

/// Path + +/- stats + optional unified/split toggle. Reuses the existing
/// `.kbc-diff__header` / `__path` / `__stats` classes so `reader.css` and
/// the why-panel's header-hide rule keep applying.
export default function DiffFileHeader({
  path,
  parsed,
  mode,
  onModeChange,
  headerSlot,
  worstFindingSeverity,
  findingCount,
  diagnosticsChip,
  onDiagnosticsClick,
  diagnosticsCardOpen,
  impactChip,
  onImpactClick,
}: DiffFileHeaderProps) {
  const stats = diffStats(parsed);
  return (
    <div className="kbc-diff__header">
      <span className="kbc-diff__path">{path}</span>
      {!!worstFindingSeverity && !!findingCount && (
        <span
          className={`kbc-diff__finding-badge kbc-diff__finding-badge--${worstFindingSeverity}`}
          title={`${findingCount} finding${findingCount === 1 ? "" : "s"} in this file`}
          data-kbc-file-finding-severity={worstFindingSeverity}
          data-kbc-file-finding-count={findingCount}
        >
          <span className="kbc-diff__finding-dot" aria-hidden="true" />
          {findingCount}
        </span>
      )}
      {!!diagnosticsChip &&
        (onDiagnosticsClick ? (
          <button
            type="button"
            className="kbc-diff__diag-chip"
            title={`Diagnostics: ${diagnosticsChip} — click to view + quick fixes`}
            aria-expanded={diagnosticsCardOpen}
            onClick={onDiagnosticsClick}
            data-kbc-file-diagnostics-chip={diagnosticsChip}
          >
            {diagnosticsChip}
          </button>
        ) : (
          <span
            className="kbc-diff__diag-chip"
            title={`Diagnostics: ${diagnosticsChip}`}
            data-kbc-file-diagnostics-chip={diagnosticsChip}
          >
            {diagnosticsChip}
          </span>
        ))}
      {!!impactChip &&
        (onImpactClick ? (
          <button
            type="button"
            className="kbc-diff__impact-chip"
            title="Reviewer X-ray — click to view usages"
            onClick={onImpactClick}
            data-kbc-file-impact-chip={impactChip}
          >
            {impactChip}
          </button>
        ) : (
          <span className="kbc-diff__impact-chip" data-kbc-file-impact-chip={impactChip}>
            {impactChip}
          </span>
        ))}
      {headerSlot}
      <div className="kbc-diff__header-end">
        <span className="kbc-diff__stats">
          <span className="kbc-diff__additions">+{stats.additions}</span>{" "}
          <span className="kbc-diff__deletions">-{stats.deletions}</span>
        </span>
        {onModeChange && (
          <div className="kbc-diff__modes" role="group" aria-label="Diff layout">
            <button
              type="button"
              className={"kbc-diff__mode" + (mode === "unified" ? " is-active" : "")}
              aria-pressed={mode === "unified"}
              aria-label="Unified diff"
              title="Unified"
              data-kbc-diff-mode-toggle="unified"
              onClick={() => onModeChange("unified")}
            >
              <Icon.UnifiedView />
            </button>
            <button
              type="button"
              className={"kbc-diff__mode" + (mode === "split" ? " is-active" : "")}
              aria-pressed={mode === "split"}
              aria-label="Side-by-side diff"
              title="Side by side"
              data-kbc-diff-mode-toggle="split"
              onClick={() => onModeChange("split")}
            >
              <Icon.SplitView />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
