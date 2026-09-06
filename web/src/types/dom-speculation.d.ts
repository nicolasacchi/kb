// Ambient decl for the Speculation Rules API's prerendering surface — not
// yet in TypeScript's bundled DOM lib (as of the `lib: ["ES2022", "DOM", …]`
// pinned in tsconfig.json). Used by detail.tsx's prerender beacon guard
// (invariants #8/#19): a prerendered document must not INSERT a `history`
// row or seed reading state before the browser actually activates it as the
// visible page (a browser may prerender several candidate pages and discard
// all but one). No import/export here — like `global.d.ts`, this is a plain
// ambient script so its `interface` merges straight into the global `lib.dom`
// types without a `declare global` wrapper.

interface Document {
  /** True while this document is being prerendered; false/absent once the
   *  browser activates it. */
  prerendering?: boolean;
}

interface DocumentEventMap {
  /** Fires once, on the document that was prerendering, when the browser
   *  activates it as the user-visible page. */
  prerenderingchange: Event;
}
