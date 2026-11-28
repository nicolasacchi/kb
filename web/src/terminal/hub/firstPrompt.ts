// HUBPROMPT — the optional first prompt of the launch flow. Pure rules only.
//
// The text is pasted into the terminal WITHOUT Enter once the terminal is live
// (the BHEAD `initialInput` ledger). A paste turns every line break into a CR,
// which is Enter when bracketed paste is off, so a multi-line text would run
// partway. It is therefore refused here with a caption instead of being
// silently reshaped; the paste layer flattens as a second guard.

export const FIRST_PROMPT_MAX = 2000;

const LINE_BREAK = /[\r\n]/;
// C0 controls and DEL except tab (line breaks are judged first).
const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;

/** The text that would be pasted: one trailing newline stripped. */
export const firstPromptText = (raw: string): string => raw.replace(/\r?\n$/, "");

/** Blank means "no prompt": nothing is stashed or pasted. */
export const firstPromptPresent = (raw: string): boolean => firstPromptText(raw).trim() !== "";

/** A visible caption when the text cannot be pasted safely, else null. */
export function firstPromptIssue(raw: string): string | null {
  const t = firstPromptText(raw);
  if (t.trim() === "") return null;
  if (LINE_BREAK.test(t)) {
    return "One line only: a line break would press Enter in the terminal. It is pasted, never sent.";
  }
  if (CONTROL.test(t)) return "Control characters cannot be pasted.";
  if (t.length > FIRST_PROMPT_MAX) return `Too long: at most ${FIRST_PROMPT_MAX} characters.`;
  return null;
}
