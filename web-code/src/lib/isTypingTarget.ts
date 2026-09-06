// V70-A3S — the shared "is the operator actively typing here" predicate.
// Every keydown-listening surface in this app has independently duplicated
// the `tagName === "INPUT" || tagName === "TEXTAREA" || isContentEditable`
// triad (Reader.tsx, ReviewDetail.tsx, ReviewDiff.tsx's `isGatedTarget`,
// Canvas.tsx, Browser.tsx, Tour.tsx, useLensKeys.ts, StoryPlayer.tsx) — this
// is the ONE place a NEW global listener should reach for (starting with
// app.tsx's Cmd/Ctrl+K launcher, which had no guard at all and stole
// Ctrl-K — kill-to-EOL on Linux/GTK — out of composers and the CM6
// suggestion editor). The pre-existing duplicates are left untouched (out
// of this unit's file scope); a future cleanup can point them here.

/// True when `target` is somewhere the operator is actively typing text: a
/// plain `<input>`/`<textarea>`, any contenteditable element (the native
/// `isContentEditable` property already walks up through nested inline
/// elements to the nearest `contenteditable` ancestor, so a click deep
/// inside a rich-text body still reports `true`), or inside an EDITABLE
/// CodeMirror 6 instance.
///
/// The CM6 check is deliberately its OWN branch rather than relying on
/// `isContentEditable` alone: CM6's `.cm-content` carries
/// `contenteditable="true"` only when the editor is writable — the
/// reader's READ-ONLY buffer is ALSO a `.cm-editor` (so Ctrl-K must keep
/// working while just browsing code), but the suggestion composer's
/// editable instance is not. Checking `.cm-editor` → its own
/// `[contenteditable="true"]` descendant (rather than trusting
/// `isContentEditable` on the exact event target) also covers a click that
/// lands on a non-editable child of an editable editor (e.g. a gutter
/// widget) without over-matching the read-only reader.
export function isTypingTarget(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  if (!t) return false;
  if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable) return true;
  if (typeof t.closest !== "function") return false;
  const cm = t.closest(".cm-editor");
  if (!cm) return false;
  return cm.querySelector('[contenteditable="true"]') !== null;
}
