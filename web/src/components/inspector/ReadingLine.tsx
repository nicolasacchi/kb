import type { ReadingSummary } from "../../api/reading";

// OL R2 — ONE reading line: estimated read time and the reader's own
// progress together. `.kb-pinsp__readby` stays on the line (e2e hook). With a
// persisted reading summary the line carries scrolled/read/active/visits; with
// only the live in-session `progress` it carries the live percentage.

/// `372000` → `6m 12s`.
export function fmtReadDur(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export default function ReadingLine({
  readMin,
  progress,
  summary,
}: {
  readMin: number | null;
  progress?: { pct: number; isDone: boolean };
  summary?: ReadingSummary | null;
}) {
  const seen = summary && summary.visit_count > 0 ? summary : null;
  const parts: React.ReactNode[] = [];
  if (readMin !== null) {
    parts.push(
      <span key="est">
        est. <b>{readMin} min</b>
      </span>,
    );
  }
  if (seen) {
    parts.push(
      <span key="scrolled">
        <b className="kb-pinsp__pct">{seen.completion_pct}%</b> scrolled
      </span>,
      <span key="read">
        <b>{seen.read_pct}%</b> read
      </span>,
      <span key="active">
        active {fmtReadDur(seen.active_ms_total)} · {seen.visit_count} visit
        {seen.visit_count === 1 ? "" : "s"}
      </span>,
    );
  } else if (progress) {
    parts.push(
      <span key="read">
        <b className="kb-pinsp__pct">{progress.pct}%</b> read
      </span>,
    );
  }
  return (
    <>
      <h4>Reading</h4>
      <div className="kb-pinsp__reading kb-pinsp__readby">
        {parts.map((p, i) => (
          <span key={i} className="kb-pinsp__reading-part">
            {i > 0 && <span className="kb-pinsp__sep"> · </span>}
            {p}
          </span>
        ))}
      </div>
      {progress && (
        <div className="kb-pinsp__progress" aria-hidden>
          <i style={{ width: `${progress.pct}%` }} />
        </div>
      )}
      {seen?.stopped_at && (
        <div className="kb-pinsp__readby kb-pinsp__readby--stop">
          stopped at “{seen.stopped_at.text}” (~{seen.stopped_at.pct}%)
        </div>
      )}
    </>
  );
}
