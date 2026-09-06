// MI-W4.2a — the /memory row's per-memory injection sparkline: a coarse
// per-week histogram of how many times a `kb-recall` hook actually
// injected THIS memory (`RecallHit.recall_weekly`, only populated when the
// request opted in via `with_weekly=true` — see `fetchRecall`). This is a
// server-side AGGREGATE, not a full per-event timeline (the design brief's
// own sanctioned fallback: "if per-injection timestamps are not on the
// wire, add a server-side aggregate rather than faking it").
//
// `weekly[0]` is THIS week, `weekly[weekly.length-1]` is the oldest
// (`N+ weeks ago`) catch-all bucket — reversed here so the bars read
// oldest-to-newest left-to-right, the natural timeline direction.

export interface InjectionHistogramProps {
  weekly: number[];
  /** CT-C5 (V0037) — how many of this memory's injections a LATER turn in
   * the recalling session went on to EXPLICITLY reference (the memory's id
   * or title, named verbatim) — `RecallHit.recall_used_count`, always
   * `<= ` the summed `weekly` total. This is a lower bound on usefulness,
   * not a full one: an agent can act on a recalled fact without ever
   * naming it, and that reads identically to "not referenced" here too —
   * spelled out in the caption's own title tooltip so the number is never
   * mistaken for "acted on". Optional/defaults to 0 so existing callers
   * (and this component's own pre-CT-C5 tests) keep compiling unchanged. */
  usedCount?: number;
}

export default function InjectionHistogram({ weekly, usedCount = 0 }: InjectionHistogramProps) {
  const total = weekly.reduce((a, b) => a + b, 0);
  if (weekly.length === 0 || total === 0) {
    return (
      <span className="kb-injecthist kb-injecthist--empty" data-testid="injection-histogram-empty">
        no injections
      </span>
    );
  }
  const oldestFirst = [...weekly].reverse();
  const max = Math.max(1, ...oldestFirst);
  return (
    <span className="kb-injecthist-wrap">
      <span
        className="kb-injecthist"
        data-testid="injection-histogram"
        title={`${total} injection${total === 1 ? "" : "s"} over the last ${weekly.length} weeks`}
      >
        {oldestFirst.map((c, i) => (
          <span
            key={i}
            className={`kb-injecthist__bar${c > 0 ? " on" : ""}`}
            style={{ height: `${c > 0 ? 4 + (c / max) * 14 : 2}px` }}
            data-testid="injection-histogram-bar"
          />
        ))}
      </span>
      <span
        className="kb-injecthist__used"
        data-testid="injection-histogram-used"
        title="&quot;Referenced&quot; counts only an EXPLICIT later mention of this memory's id or title. An agent can act on a recalled fact without ever naming it — that case reads as unreferenced too, so this is a lower bound on usefulness, not a full measure of it."
      >
        {total} recall{total === 1 ? "" : "s"} · {usedCount} referenced
      </span>
    </span>
  );
}
