// Phase E4 ("kb-code v2 — The Operable Reader") — the scrubber shell shared
// by StoryPlayer (Phase C7) and tour mode (`routes/Tour.tsx`): prev/next
// buttons, a dot-per-step (or a range slider above `dotCap` steps), a
// "N/total" counter, and an optional autoplay toggle. Pulled out of
// `StoryPlayer.tsx` verbatim — same markup, same classes, same data hooks —
// rather than duplicated for Tour's own footer.
//
// `prefix` drives BOTH the CSS class names (`${prefix}__scrubber`,
// `${prefix}__step-btn`, `${prefix}__dots`, `${prefix}__dot`,
// `${prefix}__slider`, `${prefix}__counter`, `${prefix}__autoplay`) and the
// `data-${prefix}-*` e2e hooks. StoryPlayer passes `"kbc-story"` — preserving
// every selector `e2e/story.spec.ts` already pins byte-for-byte (this
// extraction changes NOTHING about that component's rendered DOM) — while
// Tour mode passes `"kbc-tour"` for its own, independently-styled hooks
// (`styles/sets.css`).
export interface StepperBarProps {
  prefix: string;
  /// Already clamped to `[0, total - 1]` — callers only mount this once
  /// `total > 0`.
  index: number;
  total: number;
  /// Above this many steps, a range slider replaces the per-step dots.
  /// Defaults to 30 (StoryPlayer's original `STORY_DOT_CAP`).
  dotCap?: number;
  /// A stable key for step `i`'s dot — becomes both its React `key` and its
  /// `data-${prefix}-dot` value (a commit sha for story mode, a span's
  /// ordinal for tour mode).
  stepKey: (i: number) => string;
  /// `title`/`aria-label` text for step `i`'s dot (a commit subject, a
  /// span's `path:lines`).
  stepLabel: (i: number) => string;
  /// `aria-label` on the dots' `role="group"` container (plural — "story
  /// steps"/"tour steps").
  groupAriaLabel: string;
  /// `aria-label` on the range-slider fallback (singular — "story step"/
  /// "tour step").
  sliderAriaLabel: string;
  onStep: (i: number) => void;
  /// Story mode's play/pause toggle. Omitted entirely (not just hidden) for
  /// tour mode, which has no autoplay per the milestone brief.
  autoplay?: { on: boolean; onToggle: () => void };
}

const DEFAULT_DOT_CAP = 30;

export default function StepperBar({
  prefix,
  index,
  total,
  dotCap = DEFAULT_DOT_CAP,
  stepKey,
  stepLabel,
  groupAriaLabel,
  sliderAriaLabel,
  onStep,
  autoplay,
}: StepperBarProps) {
  const atFirst = index === 0;
  const atLast = index === total - 1;

  return (
    <div className={`${prefix}__scrubber`} {...{ [`data-${prefix}-scrubber`]: true }}>
      <button
        type="button"
        className={`${prefix}__step-btn`}
        onClick={() => onStep(index - 1)}
        disabled={atFirst}
        aria-label="previous step"
        {...{ [`data-${prefix}-prev`]: true }}
      >
        ◀
      </button>

      {total <= dotCap ? (
        <div className={`${prefix}__dots`} role="group" aria-label={groupAriaLabel}>
          {Array.from({ length: total }, (_, i) => (
            <button
              key={stepKey(i)}
              type="button"
              className={`${prefix}__dot` + (i === index ? " is-current" : "")}
              onClick={() => onStep(i)}
              title={stepLabel(i)}
              aria-label={`step ${i + 1}: ${stepLabel(i)}`}
              aria-current={i === index}
              {...{ [`data-${prefix}-dot`]: stepKey(i) }}
            />
          ))}
        </div>
      ) : (
        <input
          type="range"
          className={`${prefix}__slider`}
          min={0}
          max={total - 1}
          value={index}
          onChange={(e) => onStep(Number(e.target.value))}
          aria-label={sliderAriaLabel}
          {...{ [`data-${prefix}-slider`]: true }}
        />
      )}

      <button
        type="button"
        className={`${prefix}__step-btn`}
        onClick={() => onStep(index + 1)}
        disabled={atLast}
        aria-label="next step"
        {...{ [`data-${prefix}-next`]: true }}
      >
        ▶
      </button>

      <span className={`${prefix}__counter`} {...{ [`data-${prefix}-counter`]: true }}>
        {index + 1}/{total}
      </span>

      {autoplay && (
        <button
          type="button"
          className={`${prefix}__autoplay` + (autoplay.on ? " is-on" : "")}
          onClick={autoplay.onToggle}
          aria-pressed={autoplay.on}
          aria-label={autoplay.on ? "pause autoplay" : "play autoplay"}
          {...{ [`data-${prefix}-autoplay`]: true }}
        >
          {autoplay.on ? "⏸" : "⏵"}
        </button>
      )}
    </div>
  );
}
