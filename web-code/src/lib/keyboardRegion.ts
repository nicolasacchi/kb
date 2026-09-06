// V70-H1 — the reader hosts two live keyboard surfaces at once (the CM6
// buffer and the file-tree chrome around it), and `Reader.tsx` needs to
// know which one owns the next keystroke. `KeyboardRegion` is that
// decision, made EXPLICIT state rather than inferred after the fact from
// a `focus`/`focusin` DOM event.
//
// The prior design inferred it purely from `focusin`: a bare
// `document.activeElement?.blur()` with nothing else explicitly focused
// fires NO `focus`/`focusin` event at all (`<body>` isn't natively
// focusable, so `document.activeElement` merely REPORTS it as the DOM
// spec's silent fallback) — and even on the rare occasion a real focus
// event eventually did land, it raced the dock's async expand, the tree's
// virtualizer, and CM6's own focus handling. That produced a genuine
// flake, not a reliably-broken-or-reliably-fine bug: two consecutive e2e
// runs of the identical code path could disagree.
//
// The fix splits the derivation in two:
// - the app action that MOVES keyboard intent (`Ctrl-w h`, a pane click,
//   …) sets `KeyboardRegion` SYNCHRONOUSLY — that write IS the scope
//   switch, not a hoped-for side effect of some other DOM operation;
// - `nextKeyboardRegion` (below) is a CONFIRMATION over `focusin` events,
//   never the sole source: it moves the region when a `focusin` target
//   unambiguously names the buffer or the tree, and — critically — never
//   regresses on an unnamed target (`<body>`, or anything else). That
//   "nothing to infer from" case is exactly what used to strand the
//   region silently.
export type KeyboardRegion = "buffer" | "tree";

/// A minimal, testable shape for a `focusin` event's target — real
/// `HTMLElement`s satisfy this; so does a plain mock in a unit test.
export interface RegionTarget {
  closest(selector: string): unknown;
}

const BUFFER_SELECTOR = ".kbc-codeview";
const TREE_SELECTOR = "[data-kbc-tree]";

/// Pure — no DOM reads beyond `target.closest(...)`, no React. `current`
/// is returned UNCHANGED whenever `target` doesn't unambiguously name the
/// buffer or the tree (a null target, or a `focusin` landing on `<body>`
/// or some third, unrelated element).
export function nextKeyboardRegion(current: KeyboardRegion, target: RegionTarget | null): KeyboardRegion {
  if (!target || typeof target.closest !== "function") return current;
  if (target.closest(BUFFER_SELECTOR)) return "buffer";
  if (target.closest(TREE_SELECTOR)) return "tree";
  return current;
}
