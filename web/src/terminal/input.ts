// FUTERMFULL — the desktop input state and the copy/paste key table. Pure.

/** The chip beside the panel title: where keys go right now. */
export function inputLabel(focused: boolean): "typing" | "click to type" {
  return focused ? "typing" : "click to type";
}

export interface KeyLike {
  type: string;
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/**
 * What a key does about the clipboard:
 * - "copy": copy the selection, send nothing (Ctrl/Cmd+C WITH a selection;
 *   Ctrl/Cmd+Shift+C always);
 * - "paste": leave the key to the browser's paste event, send nothing
 *   (Ctrl/Cmd+Shift+V always; a plain Ctrl/Cmd+V is the browser's too);
 * - "pass": everything else, notably Ctrl+C WITHOUT a selection, which is ^C.
 */
export function clipboardAction(ev: KeyLike, hasSelection: boolean): "copy" | "paste" | "pass" {
  if (ev.type !== "keydown" || ev.altKey) return "pass";
  const mod = ev.ctrlKey || ev.metaKey;
  if (!mod) return "pass";
  const k = ev.key.toLowerCase();
  if (k === "c") {
    if (ev.shiftKey) return "copy";
    return hasSelection ? "copy" : "pass";
  }
  if (k === "v" && ev.shiftKey) return "paste";
  return "pass";
}

/** The view is scrolled up from the live bottom. */
export function scrolledUp(buf: { viewportY: number; baseY: number }): boolean {
  return buf.viewportY < buf.baseY;
}
