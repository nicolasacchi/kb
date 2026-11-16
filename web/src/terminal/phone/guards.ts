// T18 — the Claude background guard.
//
// On an EMPTY Claude prompt the `Left` arrow and the `/bg` command hand the
// conversation to Claude's background supervisor. The SPA cannot see the
// prompt, so the guard asks on every `Left` and on every message that is
// exactly `/bg`. This is a convention guard against a misfire, not a security
// boundary: a pasted `/bg` followed by Enter on the key bar is not guarded
// (the SPA cannot see what was pasted by then).

export interface ConfirmCopy {
  title: string;
  body: string;
  confirmLabel: string;
  danger: true;
}

/** What the operator is about to send. */
export type GuardAction = { kind: "key"; key: string } | { kind: "text"; text: string };

const COPY: ConfirmCopy = {
  title: "Move this conversation to Claude's background?",
  body:
    "On an empty prompt this key/command hands the conversation to Claude's background supervisor. You will have to bring it back from the Claude CLI.",
  confirmLabel: "Send anyway",
  danger: true,
};

/** The confirm copy when `action` needs one on `harness`, else null. */
export function needsConfirm(harness: string, action: GuardAction): ConfirmCopy | null {
  if (harness !== "claude") return null;
  if (action.kind === "key") return action.key === "Left" ? COPY : null;
  return action.text.trim() === "/bg" ? COPY : null;
}
