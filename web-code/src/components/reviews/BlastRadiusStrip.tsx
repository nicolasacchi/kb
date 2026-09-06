import { useMemo, useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { Icon } from "../icons";
import { fetchDiff, fetchFile, fetchImpactAnalysis } from "../../api/client";
import type { ImpactAnalysisOut, Symbol } from "../../api/types";
import { changedNewLines, symbolsInChangedHunks } from "../../lib/blastRadius";
import { parseUnifiedDiff } from "../../lib/diff";

export interface BlastRadiusStripProps {
  repo: string;
  path: string;
  baseSha: string;
  tipSha: string;
  /** Optional: open full impact panel in reader (host may navigate). */
  onOpenImpact?: (sym: { name: string; line: number; path: string }) => void;
}

function summaryLine(out: ImpactAnalysisOut): string {
  const exact = out.direct_exact.length;
  const likely = out.direct_likely.length;
  const tests = out.tests.length;
  return `impacted: ${exact} exact · ${likely} likely · ${tests} tests`;
}

/**
 * Lazy blast-radius strip for a review file row (V3.1-H3b R8).
 * Nothing is fetched until the strip is expanded.
 */
export default function BlastRadiusStrip({
  repo,
  path,
  baseSha,
  tipSha,
  onOpenImpact,
}: BlastRadiusStripProps) {
  const [open, setOpen] = useState(false);

  // Lazy: nothing fetches until the strip is expanded (R8).
  const diffQ = useQuery({
    queryKey: ["diff", repo, path, baseSha, tipSha],
    queryFn: () => fetchDiff(repo, path, baseSha, tipSha),
    enabled: open && !!repo && !!path && !!baseSha && !!tipSha,
  });
  const fileQ = useQuery({
    queryKey: ["file", repo, path, tipSha],
    queryFn: () => fetchFile(repo, path, tipSha),
    enabled: open && !!repo && !!path && !!tipSha,
  });

  const targets: Symbol[] = useMemo(() => {
    if (!open || !diffQ.data || !fileQ.data) return [];
    const parsed = parseUnifiedDiff(diffQ.data.diff);
    const lines = changedNewLines(parsed);
    return symbolsInChangedHunks(fileQ.data.symbols ?? [], lines, 3);
  }, [open, diffQ.data, fileQ.data]);

  const impactQs = useQueries({
    queries: targets.map((s) => ({
      queryKey: ["impact-analysis", repo, path, s.line_start, s.col_start, s.name],
      queryFn: () =>
        fetchImpactAnalysis({
          repo,
          path,
          line: s.line_start,
          col: s.col_start || 0,
          ref: tipSha,
        }),
      enabled: open && targets.length > 0,
      staleTime: 60_000,
    })),
  });

  const loading =
    open && (diffQ.isLoading || fileQ.isLoading || impactQs.some((q) => q.isLoading));

  return (
    <div className="kbc-blast" data-kbc-blast={path}>
      <button
        type="button"
        className="kbc-blast__toggle"
        data-kbc-blast-toggle
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        <Icon.Chevron className={open ? "kbc-twisty is-open" : "kbc-twisty"} /> blast radius
      </button>
      {open && (
        <div className="kbc-blast__body" data-kbc-blast-body>
          {loading && <div className="kbc-blast__hint">Analyzing…</div>}
          {!loading && (diffQ.error || fileQ.error) && (
            <div className="kbc-blast__hint kbc-blast__hint--err">
              Couldn’t load diff/symbols for blast radius.
            </div>
          )}
          {!loading && !diffQ.error && !fileQ.error && targets.length === 0 && (
            <div className="kbc-blast__hint" data-kbc-blast-empty>
              No resolvable changed symbols in this file’s hunks.
            </div>
          )}
          {!loading &&
            targets.map((s, i) => {
              const q = impactQs[i];
              const data = q?.data;
              return (
                <div key={`${s.name}-${s.line_start}`} className="kbc-blast__row" data-kbc-blast-sym={s.name}>
                  <span className="kbc-blast__sym">
                    {s.name}
                    <span className="kbc-blast__line">:{s.line_start}</span>
                  </span>
                  {q?.isError && (
                    <span className="kbc-blast__hint kbc-blast__hint--err">impact failed</span>
                  )}
                  {data && (
                    <>
                      <span className="kbc-blast__summary">{summaryLine(data)}</span>
                      <button
                        type="button"
                        className="kbc-blast__open"
                        data-kbc-blast-open={s.name}
                        onClick={(e) => {
                          e.stopPropagation();
                          onOpenImpact?.({ name: s.name, line: s.line_start, path });
                        }}
                      >
                        open impact panel
                      </button>
                    </>
                  )}
                </div>
              );
            })}
        </div>
      )}
    </div>
  );
}
