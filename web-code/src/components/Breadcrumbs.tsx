import { useMemo } from "react";
import { Link } from "react-router-dom";
import type { Symbol } from "../api/types";
import { buildBreadcrumbs, crumbHref } from "../lib/breadcrumbs";
import type { PaneLoc } from "../lib/codeUrl";
import { ancestorChain, toStickySymbol } from "../lib/stickyContext";

export interface BreadcrumbsProps {
  repo: string;
  path: string;
  gitRef?: string;
  /// V3.N2 — current-file symbols for the cursor-line chain (optional).
  symbols?: Symbol[];
  /// 1-based cursor line; drives the symbol segment chain AND (V70-A3S)
  /// rides along on every crumb's own link so a breadcrumb click doesn't
  /// silently drop it.
  cursorLine?: number;
  /// V70-A3S — the live Wave E second-pane location, when a split is open
  /// (`Reader.tsx`'s `pane2Loc`). Threaded onto every crumb link so
  /// navigating via breadcrumbs doesn't silently close the split.
  pane2?: PaneLoc | null;
  /// Click a symbol segment → jump to its start line (same-file).
  onJumpSymbol?: (line: number) => void;
}

const SYMBOL_CAP = 3;

export default function Breadcrumbs({
  repo,
  path,
  gitRef,
  symbols,
  cursorLine,
  pane2,
  onJumpSymbol,
}: BreadcrumbsProps) {
  const crumbs = buildBreadcrumbs(repo, path);
  const symbolChain = useMemo(() => {
    if (!symbols || symbols.length === 0 || !cursorLine || cursorLine < 1) return [];
    return ancestorChain(symbols.map(toStickySymbol), cursorLine, SYMBOL_CAP);
  }, [symbols, cursorLine]);

  // Ellipsize overflow: when the full un-capped chain would be longer than
  // SYMBOL_CAP we already dropped outermost via ancestorChain; show a leading
  // "…" when the file has more nesting than we display (heuristic: a symbol
  // whose range contains cursor but is not in the displayed chain exists
  // only when there were > cap matches — reflected by a wider containing
  // symbol existing).
  const hasOverflow = useMemo(() => {
    if (!symbols || !cursorLine) return false;
    const full = ancestorChain(symbols.map(toStickySymbol), cursorLine, 99);
    return full.length > SYMBOL_CAP;
  }, [symbols, cursorLine]);

  return (
    <nav className="kbc-breadcrumbs" aria-label="Breadcrumb">
      {crumbs.map((c, i) => (
        <span key={c.path} className="kbc-breadcrumbs__segment">
          {i > 0 && <span className="kbc-breadcrumbs__sep">/</span>}
          {c.isCurrent ? (
            <span className="kbc-breadcrumbs__current" aria-current="page">
              {c.label}
            </span>
          ) : (
            <Link to={crumbHref(repo, c.path, { ref: gitRef, line: cursorLine, pane2 })}>{c.label}</Link>
          )}
        </span>
      ))}
      {symbolChain.length > 0 && (
        <span className="kbc-breadcrumbs__symbols" data-kbc-breadcrumbs-symbols>
          {hasOverflow && (
            <>
              <span className="kbc-breadcrumbs__sep">›</span>
              <span className="kbc-breadcrumbs__ellip" aria-hidden>
                …
              </span>
            </>
          )}
          {symbolChain.map((s) => (
            <span key={`${s.name}:${s.start_line}`} className="kbc-breadcrumbs__segment">
              <span className="kbc-breadcrumbs__sep">›</span>
              {onJumpSymbol ? (
                <button
                  type="button"
                  className="kbc-breadcrumbs__symbol"
                  onClick={() => onJumpSymbol(s.start_line)}
                  title={`${s.kind} · line ${s.start_line}`}
                  data-kbc-breadcrumb-symbol
                  data-kbc-breadcrumb-line={s.start_line}
                >
                  {s.name}
                </button>
              ) : (
                <span className="kbc-breadcrumbs__symbol">{s.name}</span>
              )}
            </span>
          ))}
        </span>
      )}
    </nav>
  );
}
