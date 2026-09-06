import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

// Query-stats context — the match count + ms timing + parser warnings a
// route reports for the current query, shown by the ContextLine (Band 2).
// Gallery populates it on every useDocs fetch; elsewhere it stays undefined
// and the context line hides the "N matches · in M ms" tail. Lives in its
// own module (split out of the old QueryRibbon) so the visual ContextLine
// and the routes that report stats don't import each other.

export type Stats = { total: number; ms: number; warnings: string[] };
type Ctx = { stats?: Stats; setStats: (s: Stats | undefined) => void };

const QueryStatsContext = createContext<Ctx>({
  stats: undefined,
  // eslint-disable-next-line @typescript-eslint/no-empty-function
  setStats: () => {},
});

export function QueryStatsProvider({ children }: { children: ReactNode }) {
  const [stats, setStats] = useState<Stats | undefined>(undefined);
  const value = useMemo(() => ({ stats, setStats }), [stats]);
  return (
    <QueryStatsContext.Provider value={value}>
      {children}
    </QueryStatsContext.Provider>
  );
}

/// One-shot per-route setter. Routes call this once per fetch result; the
/// context resets to undefined on unmount.
///
/// Callers pass a FRESH `{ total, ms, warnings }` object literal every render
/// (e.g. gallery/sessions from their useDocs result), so keying the effect on
/// the object identity re-ran it on EVERY commit → setStats →
/// QueryStatsProvider re-render → caller re-render → new object → ∞. It's an
/// async render→effect→setState loop (each cycle a separate scheduler task),
/// so React never trips "maximum update depth" — it just pins the CPU at a
/// steady ~200–400 renders/s. Re-key on the STABLE primitive fields so the
/// effect (and the context write) only fire on a real value change.
export function useReportQueryStats(stats: Stats | undefined) {
  const { setStats } = useContext(QueryStatsContext);
  const total = stats?.total;
  const ms = stats?.ms;
  const warnings = stats?.warnings;
  const warningsKey = warnings?.join("") ?? "";
  const stable = useMemo<Stats | undefined>(
    () =>
      total === undefined || ms === undefined
        ? undefined
        : { total, ms, warnings: warnings ?? [] },
    // `warnings` is tracked via `warningsKey`; depending on the array identity
    // would reintroduce the loop (callers rebuild the array each render).
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [total, ms, warningsKey],
  );
  useEffect(() => {
    setStats(stable);
    return () => setStats(undefined);
  }, [stable, setStats]);
}

export function useQueryStats(): Stats | undefined {
  return useContext(QueryStatsContext).stats;
}
