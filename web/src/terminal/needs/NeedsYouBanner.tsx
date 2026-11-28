// CONVNEED — the needs-you banner of the open terminal. Derived from the shared
// live-status query (blocked flag, tool, reason) and, only while blocked, the
// shared Conversation query (the pending ask). No new fetch, route, stream or
// poll; nothing is read from terminal bytes. Text is rendered as text nodes.
// The banner is state, not an event: it cannot be dismissed and it unmounts the
// moment the session is no longer blocked. No xterm import.

import type { ReactElement } from "react";
import { useConversation } from "../../hooks/useConversation";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { useTermRail } from "../rail/context";
import { deriveNeeds, type NeedsKind } from "./needsYou";
import "./needsYou.css";

const KIND_LABEL: Record<NeedsKind, string> = {
  question: "Question",
  plan: "Plan approval",
  permission: "Permission",
  attention: "Needs you",
};

export const FRESH_CAPTURE = "From the last capture";
export const FRESH_PENDING = "Question text appears after the next capture";

export function NeedsYouBanner({ onJump }: { onJump: () => void }): ReactElement | null {
  const { sid } = useTermRail();
  const { rows } = useLiveSessions();
  const row = sid ? rows.find((r) => r.session_id === sid) : undefined;
  const blocked = row?.blocked === true;
  const conv = useConversation(blocked ? sid : undefined);
  const needs = deriveNeeds(blocked ? row : undefined, conv.data);
  if (!needs) return null;
  return (
    <section
      className="kb-needs"
      aria-label="Needs you"
      data-testid="needs-you"
      data-kind={needs.kind}
      data-source={needs.source}
    >
      <div className="kb-needs__main">
        <p className="kb-needs__label">
          <span>{KIND_LABEL[needs.kind]}</span>
          {needs.tool ? (
            <span className="kb-needs__tool" data-testid="needs-you-tool">
              {needs.tool}
            </span>
          ) : null}
        </p>
        <p className="kb-needs__text" data-testid="needs-you-text">
          {needs.text}
        </p>
        {needs.options.length > 0 ? (
          <p className="kb-needs__opts" data-testid="needs-you-options">
            {needs.options.join(" / ")}
          </p>
        ) : null}
        {needs.lastLine ? (
          <p className="kb-needs__last" data-testid="needs-you-last">
            {needs.lastLine}
          </p>
        ) : null}
        <p className="kb-needs__fresh" data-testid="needs-you-fresh">
          {needs.source === "capture" ? FRESH_CAPTURE : FRESH_PENDING}
        </p>
      </div>
      <button type="button" className="kb-needs__jump" data-testid="needs-you-jump" onClick={onJump}>
        Show in Conversation
      </button>
    </section>
  );
}
