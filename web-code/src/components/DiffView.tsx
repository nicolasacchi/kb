import { useMemo, useState } from "react";
import DiffFile from "./diff/DiffFile";
import { useDiff } from "../hooks/useDiff";
import { useDiffHighlights } from "../hooks/useDiffHighlights";
import { parseUnifiedDiff } from "../lib/diff";
import { loadDiffMode, saveDiffMode } from "../lib/prefs";
import type { DiffMode } from "../lib/prefs";

export interface DiffViewProps {
  repo: string;
  path: string;
  from: string;
  to?: string;
}

/// `GET /api/diff?repo=&path=&from=&to=` fetch+parse wrapper. Rendering
/// lives in `DiffFile` (V4.D1): unified or side-by-side from the same
/// `ParsedDiff`. This wrapper keeps its no-comments contract — it never
/// forwards `repo`/`sha` — so ReviewDetail and the reader `~diff` route
/// stay comment-free (V4.C4 wires review comments separately).
export default function DiffView({ repo, path, from, to }: DiffViewProps) {
  const { data, isLoading, error } = useDiff(repo, path, from, to);
  const parsed = useMemo(() => (data ? parseUnifiedDiff(data.diff) : null), [data]);
  const [mode, setMode] = useState<DiffMode>(() => loadDiffMode());
  const hasRemoves = useMemo(
    () => (parsed ? parsed.hunks.some((h) => h.lines.some((l) => l.kind === "remove")) : false),
    [parsed],
  );
  const highlights = useDiffHighlights(
    parsed ? repo : undefined,
    parsed ? path : undefined,
    { oldSha: from, newSha: to },
    { hasRemoves, parsed },
  );

  if (isLoading) return <div className="kbc-diff kbc-diff--loading">Loading diff…</div>;
  if (error) return <div className="kbc-diff kbc-diff--error">Failed to load diff</div>;
  if (!parsed) return null;

  return (
    <DiffFile
      path={path}
      parsed={parsed}
      mode={mode}
      onModeChange={(next) => {
        saveDiffMode(next);
        setMode(next);
      }}
      highlights={highlights}
    />
  );
}
