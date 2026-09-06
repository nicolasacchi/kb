// CM6 extension: renders `lib/linkifyScan.ts`'s tokens as clickable spans
// (`.kbc-link-token`, underline-on-hover — see `styles/reader.css`) inside
// comment/string text, and dispatches a click to whichever callback matches
// the token's kind. Tokens are precomputed ONCE per file load (pure —
// `scanLinkTokens`) and carried in via a `Facet`, the exact same shape
// `highlightField.ts` uses for syntax-highlight ranges — no per-view
// mutable state is needed here (unlike the line gutters' hover/click marker
// maps), since a token's position/kind never changes after the file loads
// (the reader has no editor, ever).
//
// `handlersRef` is a caller-owned `{ current: LinkifyCallbacks }` ref, read
// at CLICK time rather than captured when this extension is built — same
// "ref, not a closure" idiom `editor/lineGutter.ts`'s `LineGutterHandlers`
// uses, and for the identical reason: `CodeView` creates this extension
// exactly ONCE per `blobHash` (inside the view-creation `useEffect`), but
// `Reader`'s own `navigate`/`window.open` closures are re-created every
// render — the ref lets the extension always call the LATEST one without
// needing the whole `EditorView` recreated just because a prop's identity
// changed.
//
// Plain click activates a token (no Cmd/Ctrl modifier required) — the
// buffer is read-only, so there is no competing "place a text cursor here
// to start typing" gesture a bare click needs to be disambiguated against
// (unlike a normal code editor, where Cmd/Ctrl-click is reserved so a plain
// click can still just move the caret).

import { Facet, StateField, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
import type { LinkToken } from "../lib/linkifyScan";

export interface LinkifyCallbacks {
  onOpenUrl?(url: string): void;
  onOpenPath?(path: string): void;
  onOpenSession?(sessionId: string): void;
  /// Wave C — a bare commit sha inside a comment/string (`lib/
  /// linkifyScan.ts`'s `kind: "sha"`).
  onOpenSha?(sha: string): void;
}

const linkTokensFacet = Facet.define<LinkToken[], LinkToken[]>({
  combine: (values) => values[0] ?? [],
});

function buildDecorations(tokens: LinkToken[]): DecorationSet {
  const marks = tokens
    .filter((t) => t.to > t.from)
    .map((t) =>
      Decoration.mark({
        class: "kbc-link-token",
        attributes: { "data-kbc-link-kind": t.kind },
      }).range(t.from, t.to),
    );
  return Decoration.set(marks, true);
}

const linkifyField = StateField.define<DecorationSet>({
  create(state) {
    return buildDecorations(state.facet(linkTokensFacet));
  },
  update(deco, tr) {
    // Read-only reader (see `highlightField.ts`'s identical note) — `tr.
    // changes` is always empty in practice; `.map` keeps this correct
    // rather than merely convenient if that invariant is ever relaxed.
    return deco.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

/// A per-file token list is small (comment/string spans only) — a linear
/// scan is fine, no need for the binary-search machinery `decorations.ts`'s
/// byte mapper uses for the much larger "every highlight span in the file"
/// set.
function tokenAt(view: EditorView, pos: number): LinkToken | null {
  const tokens = view.state.facet(linkTokensFacet);
  return tokens.find((t) => pos >= t.from && pos < t.to) ?? null;
}

/// Build the linkify extension for one file load. `tokens` is this file's
/// precomputed scan result (`CodeView` memoizes it alongside the highlight
/// `ranges`); `handlersRef` is read at click time (see the module doc).
export function linkifyExtension(tokens: LinkToken[], handlersRef: { current: LinkifyCallbacks }): Extension {
  return [
    linkTokensFacet.of(tokens),
    linkifyField,
    EditorView.domEventHandlers({
      mousedown(event, view) {
        if (event.button !== 0) return false; // left-click only
        const target = event.target as HTMLElement | null;
        if (!target?.closest(".kbc-link-token")) return false;
        const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
        if (pos == null) return false;
        const token = tokenAt(view, pos);
        if (!token) return false;
        const cb = handlersRef.current;
        if (token.kind === "url") cb.onOpenUrl?.(token.value);
        else if (token.kind === "path") cb.onOpenPath?.(token.value);
        else if (token.kind === "session") cb.onOpenSession?.(token.value);
        else cb.onOpenSha?.(token.value);
        event.preventDefault();
        return true;
      },
    }),
  ];
}
