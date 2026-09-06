// CodeMirror 6 theme + markdown syntax-highlight, mapped onto the kb design
// tokens (web/src/styles/tokens.css). Everything is expressed in CSS custom
// properties so light/dark themes follow the app automatically — CM6 never
// hardcodes a colour. Sizing (min-height, font-size per surface) lives in
// styles/editor.css via container queries; this file owns colour + texture.

import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

// Markdown token colours. Markers (`#`, `*`, `` ` ``) render dim so the prose
// reads cleanly even in raw "Write" mode; this is the syntax-highlight layer,
// distinct from the Phase-4 live-preview decorations that hide markers entirely.
export const kbHighlightStyle = HighlightStyle.define([
  { tag: t.heading1, fontSize: "1.45em", fontWeight: "700", color: "var(--fg)" },
  { tag: t.heading2, fontSize: "1.28em", fontWeight: "700", color: "var(--fg)" },
  { tag: t.heading3, fontSize: "1.14em", fontWeight: "600", color: "var(--fg)" },
  {
    tag: [t.heading4, t.heading5, t.heading6],
    fontWeight: "600",
    color: "var(--fg)",
  },
  { tag: t.strong, fontWeight: "700", color: "var(--fg)" },
  { tag: t.emphasis, fontStyle: "italic" },
  {
    tag: t.strikethrough,
    textDecoration: "line-through",
    color: "var(--fg-muted)",
  },
  { tag: [t.link, t.url], color: "var(--accent-soft)" },
  { tag: t.monospace, fontFamily: "var(--font-mono)", color: "var(--accent-soft)" },
  { tag: t.list, color: "var(--accent-soft)" },
  { tag: t.quote, color: "var(--fg-muted)", fontStyle: "italic" },
  // Markdown markup characters (the `*`/`#`/`` ` ``/`>` syntax).
  { tag: [t.meta, t.processingInstruction], color: "var(--fg-muted)" },
  { tag: t.contentSeparator, color: "var(--accent-bor)" },
]);

// Resolve whether the app is currently in dark mode from the <html>
// data-theme attribute (set by applyPrefs), following the OS for "system".
// Read at editor-mount time — editors are short-lived, so a theme switch
// while one is open is rare; the next mount picks up the new value. The
// colours themselves are token-driven and flip immediately regardless.
function resolveDark(): boolean {
  const attr = document.documentElement.getAttribute("data-theme");
  if (attr === "light") return false;
  if (attr === "dark") return true;
  return !window.matchMedia?.("(prefers-color-scheme: light)").matches;
}

// Base editor chrome. `&` is the editor root, `.cm-content` the editable area.
// `dark` only feeds CM6's built-in default fallbacks (the visible colours are
// all token-driven above); pass the resolved theme so light mode gets light
// defaults for anything we don't explicitly override.
export const makeEditorTheme = (dark: boolean) =>
  EditorView.theme(
  {
    "&": {
      color: "var(--fg)",
      backgroundColor: "var(--bg)",
      border: "1px solid var(--border)",
      borderRadius: "var(--radius-sm)",
    },
    "&.cm-focused": {
      outline: "none",
      borderColor: "var(--accent-bor)",
      boxShadow: "0 0 0 1px var(--accent-bor)",
    },
    ".cm-content": {
      fontFamily: "var(--font-sans)",
      padding: "8px",
      caretColor: "var(--accent)",
    },
    ".cm-scroller": { lineHeight: "1.55", fontFamily: "inherit" },
    ".cm-line": { padding: "0 2px" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
      backgroundColor: "color-mix(in srgb, var(--accent-soft) 26%, transparent)",
    },
    ".cm-activeLine": {
      backgroundColor: "color-mix(in srgb, var(--accent) 6%, transparent)",
    },
    ".cm-placeholder": { color: "var(--fg-muted)" },
    // Autocomplete (slash menu) popover.
    ".cm-tooltip": {
      border: "1px solid var(--border-hi)",
      borderRadius: "var(--radius-sm)",
      backgroundColor: "var(--elev-hi)",
      boxShadow: "var(--shadow)",
    },
    ".cm-tooltip.cm-tooltip-autocomplete > ul": {
      fontFamily: "var(--font-sans)",
      fontSize: "12px",
      maxHeight: "16em",
    },
    ".cm-tooltip-autocomplete ul li": {
      padding: "3px 8px",
      color: "var(--fg)",
    },
    ".cm-tooltip-autocomplete ul li[aria-selected]": {
      backgroundColor: "var(--accent-bg)",
      color: "var(--accent-fg)",
    },
    ".cm-completionLabel": { color: "inherit" },
    ".cm-completionDetail": {
      color: "var(--fg-muted)",
      fontStyle: "normal",
      fontFamily: "var(--font-mono)",
      marginLeft: "0.5em",
    },
  },
  { dark },
);

// One bundled extension: theme + syntax highlight. Built per editor mount so
// the CM `dark` flag matches the active app theme (see resolveDark above).
export const kbThemeExtension = () => [
  makeEditorTheme(resolveDark()),
  syntaxHighlighting(kbHighlightStyle),
];
