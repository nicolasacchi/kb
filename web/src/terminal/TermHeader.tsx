// BHEAD — the terminal header: ONE calm row for the desktop panel and the
// phone chrome. Pure presentational (no fetch, no xterm import): the panel
// owns every value and passes it down.
//
// Slots (typed extension points other units fill without editing the panel):
//   `actions`  left of the pane verbs (command palette, terminal switcher);
//   `extra`    phone only: the host's controls (End, switcher) in their own row;
//   `children` the pane verbs (Find, Full screen, Close; Read/A-/A+ on a phone);
//   `more`     secondary readouts (grid, font, cropped, typing) in a disclosure.
// The status chips (`StatusChips`) are the single home of the typed live status.

import type { ReactNode } from "react";
import type { StatusChip } from "./statusChips";

export type HeaderPhase = string;

/** The chips row. Renders nothing at all when there are no chips. */
export function StatusChips({ chips }: { chips: StatusChip[] }) {
  if (chips.length === 0) return null;
  return (
    <ul className="kb-term__chips" data-testid="kb-term-status-chips">
      {chips.map((c) => (
        <li key={c.id} className={`kb-term__schip kb-term__schip--${c.id}`} data-chip={c.id} title={c.title}>
          {c.label}
        </li>
      ))}
    </ul>
  );
}

export interface TermHeaderProps {
  harness: string;
  /** The terminal's working directory basename, when known. */
  project?: string;
  phase: HeaderPhase;
  /** Desktop only: the state caption. */
  caption?: string;
  /** The program's own OSC title, already sanitized (text only). */
  progTitle?: string;
  chips: StatusChip[];
  actions?: ReactNode;
  extra?: ReactNode;
  more?: ReactNode;
  compact: boolean;
  children?: ReactNode;
}

function StateDot({ phase, compact }: { phase: HeaderPhase; compact: boolean }) {
  return (
    <span
      className={`kb-term__state kb-term__state--${phase}${compact ? " kb-term__state--compact" : ""}`}
      data-testid="kb-term-state"
      data-phase={phase}
      title={phase}
    >
      <span className="kb-term__dot" aria-hidden="true" />
      {compact ? <span className="kb-term__sr">{phase}</span> : <span className="kb-term__state-word">{phase}</span>}
    </span>
  );
}

export function TermHeader(props: TermHeaderProps) {
  const { harness, project, phase, caption, progTitle, chips, actions, extra, more, compact, children } = props;
  const actionsEl = actions ? (
    <div className="kb-term__actions" data-testid="kb-term-actions">
      {actions}
    </div>
  ) : null;

  if (compact) {
    return (
      <>
        <div className="kb-term-phone__bar">
          <span className="kb-term__heading">
            <span className="kb-term-phone__title" data-testid="kb-term-phone-title">
              {harness}
            </span>
            {project ? (
              <span className="kb-term__project" data-testid="kb-term-project">
                {project}
              </span>
            ) : null}
          </span>
          <StateDot phase={phase} compact />
          <span className="kb-term-phone__spacer" />
          {children}
        </div>
        {actionsEl || extra ? (
          <div className="kb-term-phone__extra" data-testid="kb-term-phone-extra">
            {actionsEl}
            {extra}
          </div>
        ) : null}
      </>
    );
  }

  return (
    <div className="kb-term__bar" data-testid="kb-term-bar">
      <span className="kb-term__heading">
        <span className="kb-term__harness" data-testid="kb-term-harness">
          {harness}
        </span>
        {project ? (
          <span className="kb-term__project" data-testid="kb-term-project">
            {project}
          </span>
        ) : null}
      </span>
      <StateDot phase={phase} compact={false} />
      {progTitle ? (
        <span className="kb-term__progtitle" data-testid="kb-term-title" title={progTitle}>
          {progTitle}
        </span>
      ) : null}
      <StatusChips chips={chips} />
      <span className="kb-term__caption" data-testid="kb-term-caption">
        {caption}
      </span>
      {more ? (
        <details className="kb-term__more" data-testid="kb-term-more">
          <summary className="kb-term__btn">More</summary>
          <div className="kb-term__more-body">{more}</div>
        </details>
      ) : null}
      {actionsEl}
      {children}
    </div>
  );
}
