import { useMemo } from "react";
import type { Symbol } from "../api/types";
import { stickyChain, toStickySymbol } from "../lib/stickyContext";

export interface StickyContextProps {
  symbols: Symbol[];
  /// 1-based first visible line of the editor viewport.
  firstVisibleLine: number;
  onJump: (line: number) => void;
  /// When false, render nothing (reader pref off).
  enabled?: boolean;
}

const STICKY_CAP = 4;

/// V3.N2 — thin stack pinned above the CodeView viewport showing the
/// ancestor symbol chain for the first visible line. Click → jump to that
/// symbol's start line.
export default function StickyContext({
  symbols,
  firstVisibleLine,
  onJump,
  enabled = true,
}: StickyContextProps) {
  const chain = useMemo(() => {
    if (!enabled || symbols.length === 0 || firstVisibleLine < 1) return [];
    return stickyChain(symbols.map(toStickySymbol), firstVisibleLine, STICKY_CAP);
  }, [symbols, firstVisibleLine, enabled]);

  if (!enabled || chain.length === 0) return null;

  return (
    <div className="kbc-sticky-context" data-kbc-sticky-context aria-label="sticky context">
      {chain.map((a) => (
        <button
          key={`${a.kind}:${a.name}:${a.start_line}`}
          type="button"
          className="kbc-sticky-context__row"
          onClick={() => onJump(a.start_line)}
          title={`${a.kind} ${a.name} · line ${a.start_line}`}
          data-kbc-sticky-row
          data-kbc-sticky-line={a.start_line}
        >
          <span className="kbc-sticky-context__kind">{a.kind}</span>
          <span className="kbc-sticky-context__name">{a.name}</span>
        </button>
      ))}
    </div>
  );
}
