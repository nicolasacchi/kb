import { useEffect, useMemo, useRef, useState } from "react";
import type { Symbol as CodeSymbol } from "../api/types";
import { subjectFor, type RailSubject } from "../desk/railSubject";

// V70-A4 — the right rail follows the caret, on a debounce, with a pin.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "The rail follows
// the caret (debounced ~250 ms) with an explicit pin".
//
// The debounce is the whole ergonomic question. Xcode's assistant editor
// re-subjected on every caret move and lost the reader's place; the
// research report calls that "the single most important rule in the
// whole design" (panel-layout-system.md §3.4). kb-code's answer is the
// milestone brief's: follow, but only after the caret has SETTLED, and
// give the human a pin that stops the following entirely.
//
// Pinning snapshots the subject at the moment the pin goes down — not a
// "path + line" to re-resolve later, the resolved subject itself — so a
// pinned rail keeps naming the same symbol even if the file scrolls,
// re-fetches, or the caret leaves the file altogether.

export const RAIL_SUBJECT_DEBOUNCE_MS = 250;

export interface UseRailSubjectInput {
  path: string | null;
  line: number | null;
  symbols: readonly CodeSymbol[];
  symbolsLoaded: boolean;
}

export interface UseRailSubjectResult {
  /// What the rail should RENDER: the pinned subject while pinned, the
  /// caret's own otherwise.
  subject: RailSubject | null;
  /// Where the caret actually is — always live, so a pinned header can
  /// name both ("📌 Order#total — caret is in Order#refund").
  caretSubject: RailSubject | null;
}

export function useRailSubject(
  input: UseRailSubjectInput,
  opts: { pinned: boolean; debounceMs?: number },
): UseRailSubjectResult {
  const debounceMs = opts.debounceMs ?? RAIL_SUBJECT_DEBOUNCE_MS;
  const [settledLine, setSettledLine] = useState<number | null>(input.line);

  // Only the LINE is debounced. A file open (path change) re-subjects
  // immediately: waiting 250ms there would leave the rail describing the
  // file the operator just navigated away from, which is a lie, not a
  // smoothing.
  const pathRef = useRef(input.path);
  useEffect(() => {
    if (pathRef.current !== input.path) {
      pathRef.current = input.path;
      setSettledLine(input.line);
      return;
    }
    if (settledLine === input.line) return;
    const t = window.setTimeout(() => setSettledLine(input.line), debounceMs);
    return () => window.clearTimeout(t);
  }, [input.path, input.line, settledLine, debounceMs]);

  const caretSubject = useMemo(
    () =>
      subjectFor({
        path: input.path,
        line: settledLine,
        symbols: input.symbols,
        symbolsLoaded: input.symbolsLoaded,
      }),
    [input.path, settledLine, input.symbols, input.symbolsLoaded],
  );

  // The frozen subject. Captured on the pin's rising edge and released
  // on its falling one — never recomputed while pinned.
  const [pinnedSubject, setPinnedSubject] = useState<RailSubject | null>(null);
  const wasPinned = useRef(false);
  useEffect(() => {
    if (opts.pinned && !wasPinned.current) setPinnedSubject(caretSubject);
    if (!opts.pinned && wasPinned.current) setPinnedSubject(null);
    wasPinned.current = opts.pinned;
  }, [opts.pinned, caretSubject]);

  return {
    subject: opts.pinned ? pinnedSubject : caretSubject,
    caretSubject,
  };
}
