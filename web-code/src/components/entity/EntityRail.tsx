import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  fetchHierarchyCallees,
  fetchHierarchyCallers,
  fetchHierarchyTypes,
  fetchResolve,
} from "../../api/client";
import type { LensDeclaration, ResolveCandidate, Symbol } from "../../api/types";
import { isTypeIshKind } from "../../lib/hierarchyState";
import HistorySignals from "./HistorySignals";

function sessionDiffHref(sessionId: string): string {
  return `/session/${encodeURIComponent(sessionId)}/diff`;
}

export interface EntityRailProps {
  repo: string;
  path: string;
  /** 1-based cursor line (live). */
  cursorLine: number | null;
  /** 0-based UTF-16 col when known; defaults to 0. */
  cursorCol?: number | null;
  symbols: Symbol[];
  /** Optional lenses for the open file (usage counts without a second fetch). */
  lenses?: LensDeclaration[] | null;
  onOpenGraph?: (pos: { line: number; col: number; word: string }) => void;
  onJump?: (line: number) => void;
  onOpenImpact?: (pos: { line: number; col: number; word: string }) => void;
}

function symbolAtLine(symbols: Symbol[], line: number | null): Symbol | null {
  if (line == null || line < 1) return null;
  // Innermost covering symbol, prefer name start near cursor line.
  let best: Symbol | null = null;
  for (const s of symbols) {
    if (line >= s.line_start && line <= s.line_end) {
      if (!best || s.line_end - s.line_start < best.line_end - best.line_start) {
        best = s;
      }
    }
  }
  if (best) return best;
  // Fall back to nearest declaration on the line.
  return symbols.find((s) => s.line_start === line) ?? null;
}

function lensFor(lenses: LensDeclaration[] | null | undefined, sym: Symbol | null): LensDeclaration | null {
  if (!lenses || !sym) return null;
  return lenses.find((d) => d.line === sym.line_start && d.name === sym.name) ?? null;
}

function docExcerpt(doc: string | null | undefined, max = 280): string | null {
  if (!doc) return null;
  const t = doc.trim();
  if (t.length <= max) return t;
  return t.slice(0, max - 1) + "…";
}

/**
 * Entity inspector tab body (V3.1-H3b): live cursor symbol passport.
 * Hierarchy counts are lazy — fetched only while `visible`.
 * V3.2-B3 adds History signals for the CURRENT FILE (also lazy on tab).
 */
export default function EntityRail({
  repo,
  path,
  cursorLine,
  cursorCol = 0,
  symbols,
  lenses,
  onOpenGraph,
  onJump,
  onOpenImpact,
}: EntityRailProps) {
  // V70-A4 — MOUNTING is the gate. `InspectorRail` renders this body only
  // under the tabs that contain it (All and Understand), so being mounted
  // already means "the operator is looking at me"; the previous
  // `useInspectorTab("reader")` read here was the fragile second consumer
  // the layout recon flagged (docs/research/kb-code-v7-evidence/recon/
  // layout-rails-panels.md §6.7: read-once-on-mount, "correct today only
  // because EntityRail is conditionally rendered BY the tab check"). With
  // the Desk owning the rail tab, that read could go stale; deleting it
  // removes the second home for one piece of state rather than syncing it.
  const visible = true;

  const [debouncedLine, setDebouncedLine] = useState(cursorLine);
  const [debouncedCol, setDebouncedCol] = useState(cursorCol ?? 0);

  useEffect(() => {
    const t = window.setTimeout(() => {
      setDebouncedLine(cursorLine);
      setDebouncedCol(cursorCol ?? 0);
    }, 250);
    return () => window.clearTimeout(t);
  }, [cursorLine, cursorCol]);

  const covering = useMemo(
    () => symbolAtLine(symbols, debouncedLine),
    [symbols, debouncedLine],
  );

  const resolveQ = useQuery({
    queryKey: ["entity-resolve", repo, path, debouncedLine, debouncedCol],
    queryFn: () =>
      fetchResolve({
        repo,
        path,
        line: debouncedLine as number,
        col: debouncedCol || 0,
      }),
    enabled: visible && !!repo && !!path && debouncedLine != null && debouncedLine >= 1,
    staleTime: 30_000,
  });

  const candidate: ResolveCandidate | null = resolveQ.data?.candidates?.[0] ?? null;
  const ident = resolveQ.data?.ident ?? covering?.name ?? null;
  const kind = candidate?.kind ?? covering?.kind ?? null;
  const signature = candidate?.signature ?? covering?.signature ?? null;
  const container = candidate?.container ?? covering?.container ?? null;
  const doc = docExcerpt(candidate?.doc ?? covering?.doc ?? null);
  const lens = lensFor(lenses, covering);

  const defPath = candidate?.path ?? path;
  const defLine = candidate?.line ?? covering?.line_start ?? debouncedLine ?? 1;
  const isType = isTypeIshKind(kind);

  const callersQ = useQuery({
    queryKey: ["entity-callers", repo, defPath, defLine],
    queryFn: () => fetchHierarchyCallers({ repo, path: defPath, line: defLine, col: 0 }),
    enabled: visible && !!ident && !isType && !!repo,
    staleTime: 60_000,
  });
  const calleesQ = useQuery({
    queryKey: ["entity-callees", repo, defPath, defLine],
    queryFn: () => fetchHierarchyCallees({ repo, path: defPath, line: defLine, col: 0 }),
    enabled: visible && !!ident && !isType && !!repo,
    staleTime: 60_000,
  });
  const typesQ = useQuery({
    queryKey: ["entity-types", repo, ident, defPath],
    queryFn: () => fetchHierarchyTypes(repo, ident as string, defPath),
    enabled: visible && !!ident && isType && !!repo,
    staleTime: 60_000,
  });

  const noSymbol =
    !debouncedLine || (!covering && !ident && !resolveQ.isFetching);

  const usageExact = lens?.usages.exact;
  const usageLikely = lens?.usages.likely;
  const usageCand = lens?.usages.candidate;
  const usageTotal =
    usageExact != null
      ? (usageExact ?? 0) + (usageLikely ?? 0) + (usageCand ?? 0)
      : null;

  const callerCount = callersQ.data?.callers?.length ?? null;
  const calleeCount = calleesQ.data?.callees?.length ?? null;
  const implCount =
    lens?.implementors ??
    (typesQ.data ? typesQ.data.subtypes.length : null);

  const sessionId = lens?.session?.id ?? null;
  const authorKind = lens?.author?.kind ?? null;
  const authorLabel = lens?.author?.label ?? null;

  return (
    <div className="kbc-entity" data-kbc-entity>
      {noSymbol ? (
        <div className="kbc-inspector__hint" data-kbc-entity-empty>
          Place the cursor on a symbol to see its entity passport (signature, usages,
          call counts, introducing session).
        </div>
      ) : (
        <>
          <div className="kbc-entity__head">
            <span className="kbc-entity__name" data-kbc-entity-name>
              {ident ?? "…"}
            </span>
            {kind && (
              <span className="kbc-entity__kind" data-kbc-entity-kind>
                {kind}
              </span>
            )}
          </div>

          {signature && (
            <code className="kbc-entity__sig" data-kbc-entity-sig>
              {signature}
            </code>
          )}

          {container && (
            <div className="kbc-entity__row">
              <span className="kbc-entity__lab">in</span> {container}
            </div>
          )}

          {doc && (
            <pre className="kbc-entity__doc" data-kbc-entity-doc>
              {doc}
            </pre>
          )}

          <div className="kbc-entity__counts" data-kbc-entity-counts>
            {usageTotal != null && (
              <span title={`exact ${usageExact} · likely ${usageLikely} · candidate ${usageCand}`}>
                {usageTotal} usages
              </span>
            )}
            {implCount != null && <span>{implCount} impls</span>}
            {callerCount != null && <span>{callerCount} callers</span>}
            {calleeCount != null && <span>{calleeCount} callees</span>}
            {(callersQ.isFetching || calleesQ.isFetching || typesQ.isFetching) && (
              <span className="kbc-entity__muted">…</span>
            )}
          </div>

          {(authorKind || sessionId) && (
            <div className="kbc-entity__row" data-kbc-entity-author>
              <span className="kbc-entity__lab">author</span>{" "}
              {authorKind ?? "—"}
              {authorLabel ? ` · ${authorLabel}` : ""}
              {sessionId && (
                <>
                  {" · "}
                  <a href={sessionDiffHref(sessionId)} data-kbc-entity-session={sessionId}>
                    session {sessionId.slice(0, 8)}
                  </a>
                </>
              )}
            </div>
          )}

          <div className="kbc-entity__actions">
            {onOpenGraph && ident && (
              <button
                type="button"
                className="kbc-entity__btn"
                data-kbc-entity-graph
                onClick={() =>
                  onOpenGraph({
                    line: defLine,
                    col: 0,
                    word: ident,
                  })
                }
              >
                graph
              </button>
            )}
            {onOpenImpact && ident && (
              <button
                type="button"
                className="kbc-entity__btn"
                data-kbc-entity-impact
                onClick={() =>
                  onOpenImpact({
                    line: defLine,
                    col: 0,
                    word: ident,
                  })
                }
              >
                impact
              </button>
            )}
            {onJump && covering && (
              <button
                type="button"
                className="kbc-entity__btn"
                data-kbc-entity-jump
                onClick={() => onJump(covering.line_start)}
              >
                go to decl
              </button>
            )}
          </div>
        </>
      )}

      {!!path && (
        <HistorySignals repo={repo} path={path} visible={visible} />
      )}
    </div>
  );
}
