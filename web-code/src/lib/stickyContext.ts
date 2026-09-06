// Pure helper for sticky context lines + symbol breadcrumbs (V3.N2).
// Given a flat symbol list and a 1-based line, returns the ancestor chain
// of symbols whose range CONTAINS that line, outermost → innermost.
// Cap drops outermost entries when the chain is longer than `cap`.

export interface StickySymbol {
  name: string;
  kind: string;
  start_line: number;
  end_line: number;
}

export interface StickyAncestor {
  name: string;
  kind: string;
  start_line: number;
  end_line: number;
}

const DEFAULT_CAP = 4;

/// Symbols whose `[start_line, end_line]` contains `line` (inclusive both
/// ends), sorted outermost→innermost (widest span first; ties broken by
/// later start then earlier end so nesting order stays stable). When more
/// than `cap` match, drop from the OUTER end (keep the innermost `cap`).
export function ancestorChain(
  symbols: readonly StickySymbol[],
  line: number,
  cap: number = DEFAULT_CAP,
): StickyAncestor[] {
  if (symbols.length === 0 || line < 1 || cap <= 0) return [];

  const containing = symbols.filter((s) => line >= s.start_line && line <= s.end_line);
  if (containing.length === 0) return [];

  // Outermost first: largest span, then earlier start, then later end.
  containing.sort((a, b) => {
    const spanA = a.end_line - a.start_line;
    const spanB = b.end_line - b.start_line;
    if (spanB !== spanA) return spanB - spanA;
    if (a.start_line !== b.start_line) return a.start_line - b.start_line;
    return b.end_line - a.end_line;
  });

  const mapped: StickyAncestor[] = containing.map((s) => ({
    name: s.name,
    kind: s.kind,
    start_line: s.start_line,
    end_line: s.end_line,
  }));

  if (mapped.length <= cap) return mapped;
  // Drop outermost beyond cap — keep the last `cap` (innermost).
  return mapped.slice(mapped.length - cap);
}

/// Sticky-stack variant of `ancestorChain`: only ancestors whose SIGNATURE
/// has scrolled off the top (`start_line < line`) may pin. An ancestor whose
/// start IS the first visible line is still on screen — pinning it would
/// double-render the signature AND overlay the real line, stealing its
/// pointer events (this exact bug broke 11 e2e specs on 2026-07-31: a
/// symbol starting at line 1 pinned while line 1 was visible, and every
/// `.cm-line` first() click hit the sticky button instead). Breadcrumbs
/// keep `ancestorChain`'s inclusive containment — a cursor ON a signature
/// line should still show that symbol in the crumb.
export function stickyChain(
  symbols: readonly StickySymbol[],
  firstVisibleLine: number,
  cap: number = DEFAULT_CAP,
): StickyAncestor[] {
  return ancestorChain(symbols, firstVisibleLine, cap).filter(
    (s) => s.start_line < firstVisibleLine,
  );
}

/// Map a wire `Symbol` (line_start/line_end) into the sticky helper shape.
export function toStickySymbol(s: {
  name: string;
  kind: string;
  line_start: number;
  line_end: number;
}): StickySymbol {
  return {
    name: s.name,
    kind: s.kind,
    start_line: s.line_start,
    end_line: s.line_end,
  };
}
