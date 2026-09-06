import { useDiagnostics } from "../../hooks/useDiagnostics";
import { rangeFromDiagnostic } from "../../lib/codeActions";
import { buildDiagnosticsView, formatSeveritySummary, severityLabel } from "../../lib/diagnostics";
import QuickFixes from "./QuickFixes";

export interface DiagnosticsCardProps {
  repo: string;
  path: string;
  /// Same-file jump — reuses `Reader.tsx`'s `jumpToLine` (the annotations/
  /// outline/bookmarks convention), never a full navigation `Link` the way
  /// `FrameworkCard`'s cross-file rows use `codeUrl`, since every diagnostic
  /// row is anchored to the CURRENTLY open file.
  onJumpLine: (line: number, lineEnd?: number) => void;
  /// S2-C — review scope for the "Fixes" affordance's suggestion creation
  /// (`components/provenance/QuickFixes.tsx`), threaded through ONLY by
  /// `routes/ReviewDiff.tsx`'s mount; the Reader's plain working-tree mount
  /// leaves both undefined.
  reviewId?: number;
  ps?: number;
}

/// PRR-U9 (design-addendum-2.md §D) — the reader inspector's Diagnostics
/// card: counts by severity + rows that jump to line. Mounted the same
/// always-visible way `FrameworkCard`/`CitedBy` are (`InspectorRailProps.
/// diagnosticsCard`, root CLAUDE.md invariant #30's "passport" precedent).
///
/// Named states (design-addendum-2 §D, `lib/diagnostics.ts`'s
/// `buildDiagnosticsView` owns the actual state-matrix logic):
/// - no provider covers this file's lang → renders NOTHING (`"absent"`,
///   mirrors `FrameworkCard`'s own "renders nothing while unfetched" —
///   here it's "renders nothing, ever, for this repo/file pairing").
/// - refused/unavailable → one caption line naming the reason (`"reason"`).
/// - provider ran, found nothing → "provider reports clean" (`"clean"`).
/// - provider ran, found rows → counts summary + a jump-to-line list.
export default function DiagnosticsCard({ repo, path, onJumpLine, reviewId, ps }: DiagnosticsCardProps) {
  const { data, isLoading, covered } = useDiagnostics(repo, path);
  const view = buildDiagnosticsView(covered, data, isLoading);

  if (view.kind === "absent") return null;

  return (
    <div className="kbc-diagnostics" data-kbc-diagnostics data-kbc-diagnostics-state={view.kind}>
      <div className="kbc-diagnostics__head">
        <span className="kbc-diagnostics__title">Diagnostics</span>
        {view.kind === "rows" && view.counts && (
          <span className="kbc-diagnostics__summary" data-kbc-diagnostics-summary>
            {formatSeveritySummary(view.counts)}
          </span>
        )}
      </div>
      {view.kind === "loading" && (
        <p className="kbc-diagnostics__hint" data-kbc-diagnostics-loading>
          Loading…
        </p>
      )}
      {view.kind === "reason" && (
        <p className="kbc-diagnostics__reason" data-kbc-diagnostics-reason>
          {view.reason}
        </p>
      )}
      {view.kind === "clean" && (
        <p className="kbc-diagnostics__clean" data-kbc-diagnostics-clean>
          provider reports clean
        </p>
      )}
      {view.kind === "rows" && view.rows && (
        <ul className="kbc-diagnostics__list">
          {view.rows.map((row, i) => {
            const sev = severityLabel(row.severity);
            const lineEnd = row.end_line > row.line ? row.end_line : undefined;
            return (
              <li key={`${row.line}:${row.col}:${i}`} className="kbc-diagnostics__row" data-kbc-diagnostics-row>
                <button
                  type="button"
                  className="kbc-diagnostics__jump"
                  onClick={() => onJumpLine(row.line, lineEnd)}
                  data-kbc-diagnostics-jump={row.line}
                  data-kbc-diagnostics-severity={sev}
                  title={row.message}
                >
                  <span className={`kbc-diagnostics__dot kbc-diagnostics__dot--${sev}`} aria-hidden="true" />
                  <span className="kbc-diagnostics__loc">
                    {row.line}
                    {row.col ? `:${row.col}` : ""}
                  </span>
                  <span className="kbc-diagnostics__message">{row.message}</span>
                  {(row.source || row.code != null) && (
                    <span className="kbc-diagnostics__meta">
                      {row.source}
                      {row.code != null ? ` ${String(row.code)}` : ""}
                    </span>
                  )}
                </button>
                <QuickFixes
                  repo={repo}
                  path={path}
                  range={rangeFromDiagnostic(row)}
                  reviewId={reviewId}
                  ps={ps}
                />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
