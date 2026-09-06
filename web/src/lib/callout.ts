// Pure mirror of the server's Obsidian-callout header parse
// (`kb_core::markdown::callout_header` + the default-title rule in
// `rewrite_callouts`). The SPA's native note view (`NoteMarkdown`'s
// `rehypeCallouts`) and the server's `rewrite_callouts` (the served-HTML
// iframe path) must agree, so a callout renders identically whether the
// SPA renders the note natively or falls back to the iframe.
//
// remark strips the `>` blockquote marker upstream, so this operates on
// the already-de-quoted first-line text of the blockquote's first
// paragraph (e.g. `[!warning] Heads up`), NOT the raw `> [!warning] …`
// source line the Rust side dequotes itself.
//
// Returns the FINAL { type, title } — `type` lowercased (it names the
// `kb-callout--<type>` class), `title` either the explicit title or the
// capitalised type as a default — or null when the line isn't a callout
// header. Pinned against the same fixtures as the Rust suite in
// `callout.test.ts` (sibling of `flipTask.test.ts`).

// A callout header: `[!type]`, an optional fold marker (`+`/`-`) flush
// against `]`, an optional single space, then the title. The type charset
// is ASCII alphanumeric + dash — deliberately NOT `\w` (which would also
// accept `_`), to match the server's `[a-zA-Z0-9-]` exactly; otherwise
// `[!my_type]` would render as a callout natively but as a plain
// blockquote in the iframe.
const CALLOUT_RE = /^\[!([A-Za-z0-9-]+)\]([+-]?)\s?(.*)$/s;

export type Callout = { type: string; title: string };

export function parseCallout(firstLine: string): Callout | null {
  const m = firstLine.match(CALLOUT_RE);
  if (!m) return null;
  const type = m[1].toLowerCase();
  // The fold marker (m[2]) is consumed and ignored — kb doesn't render
  // collapsible state. A title that legitimately starts with `-` survives
  // because the marker only matches flush against `]` (the title's leading
  // `-` sits after a space, so `([+-]?)` matches empty there).
  const titleRest = (m[3] ?? "").trim();
  const title = titleRest || type.charAt(0).toUpperCase() + type.slice(1);
  return { type, title };
}
