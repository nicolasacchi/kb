// G5 — reading-progress chip shared between gallery Card and the
// FloatingPill sibling popover. `compact` shrinks the bar from 50px to
// 36px so popover rows stay tight. When isDone (pct >= 95% server-side
// threshold; see useReadingProgress), the bar+% is replaced by a green
// ✓ so finished artifacts are glanceable.
export default function ReadingChip({
  pct,
  isDone,
  compact,
}: {
  pct: number;
  isDone: boolean;
  compact?: boolean;
}) {
  if (isDone) {
    return (
      <span
        className="reading-chip reading-chip--done"
        title="fully read"
        aria-label="fully read"
      >
        ✓
      </span>
    );
  }
  return (
    <span
      className={`reading-chip${compact ? " reading-chip--compact" : ""}`}
      title={`${pct}% read`}
      aria-label={`${pct}% read`}
    >
      <span className="reading-chip__bar">
        <span
          className="reading-chip__fill"
          style={{ width: `${pct}%` }}
        />
      </span>
      <span className="reading-chip__pct">{pct}%</span>
    </span>
  );
}
