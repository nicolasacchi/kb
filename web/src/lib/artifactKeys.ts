// READ1b — the key-forward lane's parent half.
//
// The artifact iframe is a separate document, so a reader key pressed while it
// holds focus never reaches this window's handlers (`keymap.ts`'s reader rows,
// QueueBar's n/p, detail.tsx's o/b/[/]/u). The daemon's in-iframe runtime
// therefore posts `{ kind: "kb:key", key }` for a CLOSED list of bare keys. The
// parent whitelists here, then replays the key as an ordinary keydown on
// `document.body`, so the EXISTING handlers (and their own guards) run exactly
// as if the key had been pressed on the page. Nothing new binds a key.

/// Keys the runtime may forward. Must equal `FWD_KEYS` in
/// `crates/kb-server/src/routes/artifact.rs` (a Rust test pins that side).
export const FORWARDED_KEYS: readonly string[] = [
  "r",
  "o",
  "b",
  "[",
  "]",
  "n",
  "p",
  "u",
];

/// The whitelisted key of a `kb:key` message, or null for anything else
/// (wrong kind, non-string, a key outside the list, or one with extra shape).
export function forwardedKey(msg: unknown): string | null {
  if (!msg || typeof msg !== "object") return null;
  const m = msg as { kind?: unknown; key?: unknown };
  if (m.kind !== "kb:key" || typeof m.key !== "string") return null;
  return FORWARDED_KEYS.includes(m.key) ? m.key : null;
}

/// Replay a forwarded key into this window's own keyboard handlers.
export function dispatchForwardedKey(key: string, doc: Document = document): void {
  doc.body.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
}

/// Route a `kb:key` message from ONE pane's frame: `r` toggles that pane's
/// reading mode directly (the frame it came from is unambiguous, so two panes
/// never both act); every other whitelisted key is replayed into the window's
/// handlers. Returns the key acted on, or null when the message was dropped.
export function routeForwardedKey(
  msg: unknown,
  toggleReading: () => void,
  replay: (key: string) => void = dispatchForwardedKey,
): string | null {
  const key = forwardedKey(msg);
  if (!key) return null;
  if (key === "r") toggleReading();
  else replay(key);
  return key;
}
