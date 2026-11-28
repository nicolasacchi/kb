// CMDPALETTE: a paste-mode payload goes to the pane as ONE line. With
// bracketed paste off, a newline in pasted text can press Enter, and a
// paste-mode entry must never do that. Run-mode text is never passed through
// here (CMDPURE already rejects control characters in run-mode args).

/** Collapse every run of line breaks (with the blanks around it) to one space, then trim. */
export function flattenToLine(text: string): string {
  return text.replace(/\s*[\r\n]+\s*/g, " ").trim();
}
