import type { ReactNode } from "react";
import "./crossingchip.css";

export type CrossingTone = "ok" | "warn";

export interface CrossingChipProps {
  /** Glyph, rendered first (decorative; the label carries the meaning). */
  icon?: ReactNode;
  /** Visible text, e.g. "Cited by 2 docs". Hidden at <=860px, icon + count remain. */
  label: string;
  /** Short count shown beside the icon when the label collapses. */
  count?: number | string;
  /** Accessible name; defaults to `label`. */
  ariaLabel?: string;
  /** Tooltip; the only text a disabled chip carries when `label` is hidden. */
  title?: string;
  /** Render as `<a rel="noreferrer">` (a plain, shareable navigation). */
  href?: string;
  /** With `href`: open in a new tab (`target="_blank"`). */
  newTab?: boolean;
  /** Render as a `<button>` (used when there is no single destination). */
  onPress?: () => void;
  /** `warn` is carried in text (`data-tone` + the label), never by colour alone. */
  tone?: CrossingTone;
  testId?: string;
}

/**
 * Presentational chip for the Spine `crossing` slot. Three render modes:
 * `href` -> anchor, `onPress` -> button, neither -> a disabled button that
 * carries `title`. Collapses to icon + count at <=860px with a 44px target.
 */
export function CrossingChip({
  icon,
  label,
  count,
  ariaLabel,
  title,
  href,
  newTab,
  onPress,
  tone = "ok",
  testId = "crossing-chip",
}: CrossingChipProps) {
  const body = (
    <>
      {icon ? (
        <span className="kb-crossing__icon" aria-hidden="true">
          {icon}
        </span>
      ) : null}
      {count !== undefined ? <span className="kb-crossing__count">{count}</span> : null}
      <span className="kb-crossing__label">{label}</span>
    </>
  );
  const common = {
    className: "kb-crossing",
    "data-kb-crossing": "",
    "data-tone": tone,
    "data-testid": testId,
    "aria-label": ariaLabel ?? label,
    title,
  };
  if (href) {
    return (
      <a {...common} href={href} rel="noreferrer" target={newTab ? "_blank" : undefined}>
        {body}
      </a>
    );
  }
  if (onPress) {
    return (
      <button {...common} type="button" onClick={onPress}>
        {body}
      </button>
    );
  }
  return (
    <button {...common} type="button" disabled>
      {body}
    </button>
  );
}
