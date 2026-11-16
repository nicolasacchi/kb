// T18 — Esc inside the terminal belongs to the terminal, never to the Sheet.
//
// The predicate the phone Sheet hands its `ignoreEscape`: an Escape keydown
// whose target is anywhere inside the terminal root (the xterm helper
// textarea, the Drive composer) must not dismiss the sheet. It lives here,
// away from the xterm-importing panel, so the Drive tests can exercise the
// real predicate against a real Sheet.

export function terminalOwnsEscape(e: KeyboardEvent): boolean {
  return e.target instanceof Element && e.target.closest("[data-kb-term-root]") !== null;
}
