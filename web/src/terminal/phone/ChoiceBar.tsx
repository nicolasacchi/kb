// T18 — the choice bar: answers for a session that is waiting on the operator.
//
// Visible ONLY while the bound session is blocked, and "blocked" comes from
// beats alone (the shared live-sessions query: no new query, no new SSE kind,
// no polling) — never from the terminal's bytes. Hidden means it renders
// nothing at all, so it takes no layout. The AUTHORITATIVE check is on the
// server (`enter` refuses 409 while blocked); here Enter is a KEY, because
// answering a blocked dialog IS pressing Enter.

import { useState } from "react";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { toast } from "../../lib/toast";
import { DriveError, sendKey, type Drive } from "./driveApi";
import { CHOICE_ACTIONS, CHOICE_DIGITS_MORE, CHOICE_DIGITS_PRIMARY, type KeyName } from "./keys";

export interface ChoiceBarProps {
  drive: Drive;
  onTyped?: () => void;
}

export function ChoiceBar({ drive, onTyped }: ChoiceBarProps) {
  const { rows } = useLiveSessions();
  const [more, setMore] = useState(false);
  const row = rows.find((r) => r.term === drive.termId);
  if (!row || !row.blocked) return null;

  const send = (key: KeyName): void => {
    sendKey(drive, key).then(
      (r) => {
        if (r.kind === "sent") onTyped?.();
      },
      (e: unknown) => toast.err(e instanceof DriveError ? e.detail : "Could not reach the terminal"),
    );
  };

  const btn = (key: KeyName, label: string, aria: string) => (
    <button
      key={key}
      type="button"
      tabIndex={-1}
      className="kb-drive__key kb-drive__key--choice"
      data-testid={`kb-drive-choice-${key}`}
      aria-label={aria}
      onPointerDown={(e) => {
        e.preventDefault();
        send(key);
      }}
      onMouseDown={(e) => e.preventDefault()}
    >
      {label}
    </button>
  );

  const waitingOn = row.blocked_tool ?? row.detail_reason ?? "a prompt";
  return (
    <div className="kb-drive__choice" data-testid="kb-drive-choicebar" role="group" aria-label="Answer the prompt">
      <div className="kb-drive__choice-caption" data-testid="kb-drive-choice-caption" role="status">
        {`Waiting on you: ${waitingOn}`}
      </div>
      <div className="kb-drive__keys">
        {CHOICE_DIGITS_PRIMARY.map((k) => btn(k, k, `Choose ${k}`))}
        {more ? CHOICE_DIGITS_MORE.map((k) => btn(k, k, `Choose ${k}`)) : null}
        <button
          type="button"
          tabIndex={-1}
          className="kb-drive__key"
          data-testid="kb-drive-choice-more"
          aria-expanded={more}
          aria-label={more ? "Fewer choices" : "More choices"}
          onPointerDown={(e) => {
            e.preventDefault();
            setMore((v) => !v);
          }}
          onMouseDown={(e) => e.preventDefault()}
        >
          {more ? "−" : "5-9"}
        </button>
        {CHOICE_ACTIONS.map((k) => btn(k.name, k.label, k.aria))}
      </div>
    </div>
  );
}
