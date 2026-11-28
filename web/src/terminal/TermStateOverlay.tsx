// BSTATE — the calm state card over the terminal pane: first connect,
// reconnecting, ended (plus expired and blocked). Purely presentational: no
// fetch, no xterm, no focus() and no aria-live of its own (the panel owns the
// ONE polite announce region). The overlay is positioned over the pane's box
// by CSS; it never sizes the pane, so tmux geometry is never renegotiated.

import type { ReactNode } from "react";

export type OverlayKind = "connecting" | "reconnecting" | "expired" | "ended" | "blocked";

export interface TermStateOverlayProps {
  phase: string;
  caption: string;
  exitCode: number | null;
  blockedReason: string | null;
  /** True until the stream has been live once in this mount. */
  firstConnect: boolean;
  /** Skip the backoff now (the session's `kick()`). */
  onReconnect?: () => void;
  /** Leave the terminal (the panel's close). */
  onBack?: () => void;
  /** The phone type step. */
  compact: boolean;
}

/** Which overlay a phase shows; null = none (live and sleeping never cover the grid). */
export function overlayKind(phase: string, firstConnect: boolean): OverlayKind | null {
  switch (phase) {
    case "connecting":
      return firstConnect ? "connecting" : "reconnecting";
    case "reconnecting":
    case "expired":
    case "ended":
    case "blocked":
      return phase;
    default:
      return null;
  }
}

/** `exited (code N)` / `exited` / `ended`: the human words for how the session finished. */
export function exitText(exitCode: number | null): string {
  if (exitCode === null || exitCode === undefined) return "exited";
  return `exited (code ${exitCode})`;
}

/** The sentence the polite status region says for a phase ("" = nothing to say). */
export function announceText(phase: string, firstConnect: boolean, exitCode: number | null, blockedReason: string | null): string {
  switch (overlayKind(phase, firstConnect)) {
    case "connecting":
      return "Connecting";
    case "reconnecting":
      return "Reconnecting";
    case "expired":
      return "Session expired";
    case "ended":
      return `Session ended, ${exitText(exitCode)}`;
    case "blocked":
      return `Blocked${blockedReason ? `, ${blockedReason}` : ""}`;
    default:
      return "";
  }
}

export function TermStateOverlay(props: TermStateOverlayProps): ReactNode {
  const kind = overlayKind(props.phase, props.firstConnect);
  if (kind === null) return null;
  let title: string;
  let detail: string | null = null;
  let action: { label: string; run?: () => void } | null = null;
  switch (kind) {
    case "connecting":
      title = "Connecting";
      break;
    case "reconnecting":
      title = "Reconnecting";
      detail = props.phase === "reconnecting" ? props.caption : null;
      if (props.phase === "reconnecting") action = { label: "Reconnect now", run: props.onReconnect };
      break;
    case "expired":
      title = "Session expired";
      action = { label: "Reconnect now", run: props.onReconnect };
      break;
    case "ended":
      title = "Session ended";
      detail = exitText(props.exitCode);
      action = { label: "Back to terminals", run: props.onBack };
      break;
    default:
      title = "Blocked";
      detail = props.blockedReason ?? "blocked";
      action = { label: "Back to terminals", run: props.onBack };
  }
  const dim = kind === "ended" || kind === "blocked";
  return (
    <div
      className={`kb-term-state-overlay kb-term-state-overlay--${kind}${dim ? " kb-term-state-overlay--dim" : ""}${props.compact ? " kb-term-state-overlay--compact" : ""}`}
      data-testid="kb-term-overlay"
      data-overlay={kind}
    >
      <div className={`kb-term-state-overlay__card${action && action.run ? " kb-term-state-overlay__card--action" : ""}`}>
        <div className="kb-term-state-overlay__title">
          {kind === "connecting" || kind === "reconnecting" ? (
            <span className="kb-term-state-overlay__dot" aria-hidden="true" />
          ) : null}
          {title}
        </div>
        {detail ? (
          <div className="kb-term-state-overlay__detail" data-testid="kb-term-overlay-detail">
            {detail}
          </div>
        ) : null}
        {action && action.run ? (
          <button
            type="button"
            className="kb-term__btn kb-term-state-overlay__action"
            data-testid="kb-term-overlay-action"
            onClick={action.run}
          >
            {action.label}
          </button>
        ) : null}
      </div>
    </div>
  );
}
