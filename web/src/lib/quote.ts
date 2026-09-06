// W1.reader — copy-citation deep-links for kb-comments/1 rows.
//
// Pure (no DOM access beyond an injectable `origin` string, so the URL +
// markdown grammar is golden-test-pinned here). CommentsPanel's quiet "cite"
// action (data-kb-act="cite-comment") writes the result to the clipboard;
// this module only builds the string.
import type { Anchor } from "../api/client";
import { artifactHref } from "./artifactHref";
import { anchorLocationLabel } from "./commentFmt";
// W3.P-c — type-only: the payload grammar for a provenance register lives
// here (see `renderRegister` at the bottom); the STORE lives in
// `lib/registers.ts`. Type-only import, so this module stays pure and the
// two files have no runtime cycle.
import type { Ref } from "./registers";

/// A comment body can run to paragraphs; the citation blockquote is one
/// line, so quoted text is whitespace-collapsed and capped here.
export const QUOTE_MAX_CHARS = 200;

/// Collapse whitespace (including newlines) to single spaces, trim, and cap
/// at `max` characters (ellipsis on truncation). Exported for the golden
/// tests; also the substring used to build the `#:~:text=` fragment below.
export function normalizeQuote(text: string, max: number = QUOTE_MAX_CHARS): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  return `${flat.slice(0, max).trimEnd()}…`;
}

export type CiteCommentInput = {
  kb: string;
  /// `doc.source_relative` — the artifact permalink is path-based (Track U).
  sourceRelative: string;
  /// Artifact title (`ReviewFile.artifact.title`; caller falls back to the
  /// artifact id when blank, same as `buildClaudePrompt`/`buildMarkdown`).
  title: string;
  anchor: Anchor;
  commentId: string;
  body: string;
  /// Injectable for tests; defaults to `window.location.origin`.
  origin?: string;
};

/// The shareable permalink for one comment: the artifact URL (reusing
/// `artifactHref`'s query grammar — `?sec=` for a section anchor) plus
/// `&panel=comments&comment=<id>` so detail.tsx's one-shot deep-link opens
/// the panel and jumps straight to the comment's row, plus a best-effort
/// `#:~:text=` text fragment of the quoted body. The fragment is inert
/// inside the SPA's cross-origin iframe (harmless) but self-scrolls +
/// highlights the quote when the artifact is the top document — bare mode,
/// or a portable `kb comments export --embed` HTML file.
export function buildCiteUrl(input: CiteCommentInput): string {
  const sec = input.anchor.kind === "section" ? input.anchor.id : undefined;
  // artifactHref always emits at least `?panel=comments` here, so the base
  // is guaranteed to already carry a `?` — appending `&comment=` is safe.
  const base = artifactHref(input.kb, input.sourceRelative, {
    sec,
    panel: "comments",
  });
  const withComment = `${base}&comment=${encodeURIComponent(input.commentId)}`;
  const origin =
    input.origin ??
    (typeof window !== "undefined" ? window.location.origin : "");
  const quote = normalizeQuote(input.body);
  const fragment = quote ? `#:~:text=${encodeURIComponent(quote)}` : "";
  return `${origin}${withComment}${fragment}`;
}

/// The full markdown citation block for one comment row:
/// `> <quoted body>` + a blank-line-free attribution line linking back to
/// the comment's location (`buildCiteUrl` above).
export function buildCiteMarkdown(input: CiteCommentInput): string {
  const quote = normalizeQuote(input.body);
  const label = anchorLocationLabel({ anchor: input.anchor });
  const url = buildCiteUrl(input);
  return `> ${quote}\n— [${input.title} § ${label}](${url})`;
}

// W2.16 — highlight → cite. The bare-selection sibling of buildCiteUrl/
// buildCiteMarkdown above: no comment involved (no `commentId`, no
// `&panel=comments&comment=`), so the permalink carries `?sec=` instead
// (when the reader's TocSpy join has a nearest-known section — omitted,
// bare permalink, otherwise) and the label is always the quoted snippet
// (anchorLocationLabel's own selection case, reused verbatim — same
// "“…22 chars…”" shape CommentsPanel's cite already uses for a selection
// anchor, so the two cite affordances read identically).
export type SelectionCiteInput = {
  kb: string;
  /// `doc.source_relative` — same permalink grammar as CiteCommentInput.
  sourceRelative: string;
  /// Artifact title; caller falls back to the id when blank (same
  /// convention as CiteCommentInput.title / ProvenanceInput.title).
  title: string;
  anchor: Extract<Anchor, { kind: "selection" }>;
  /// Nearest section heading id active when the selection was made (a
  /// live TocSpy-style join — SelectionActions freezes it at capture
  /// time), or null/undefined when unknown (no headings, or the join
  /// hasn't fired yet). NOT stored on the anchor itself — display/link
  /// nicety only, never round-tripped through the list entry.
  sec?: string | null;
  /// Injectable for tests; defaults to `window.location.origin`.
  origin?: string;
};

/// The permalink for a bare selection highlight: `artifactHref`'s `?sec=`
/// (when known) plus the same best-effort `#:~:text=` fragment
/// `buildCiteUrl` uses — self-scrolls in bare mode / a portable export,
/// inert inside the SPA's cross-origin iframe.
export function buildSelectionCiteUrl(input: SelectionCiteInput): string {
  const base = artifactHref(input.kb, input.sourceRelative, {
    sec: input.sec ?? undefined,
  });
  const origin =
    input.origin ??
    (typeof window !== "undefined" ? window.location.origin : "");
  const quote = normalizeQuote(input.anchor.snippet);
  const fragment = quote ? `#:~:text=${encodeURIComponent(quote)}` : "";
  return `${origin}${base}${fragment}`;
}

/// The markdown citation block for a bare selection highlight — same
/// blockquote + attribution shape as `buildCiteMarkdown`, minus the
/// comment machinery.
export function buildSelectionCite(input: SelectionCiteInput): string {
  const quote = normalizeQuote(input.anchor.snippet);
  const label = anchorLocationLabel({ anchor: input.anchor });
  const url = buildSelectionCiteUrl(input);
  return `> ${quote}\n— [${input.title} § ${label}](${url})`;
}

// W2.6b — "y p" provenance yank (`lib/keymap.ts`'s REGISTRY carries the
// doc-only reader-scope binding for the `?` sheet; `routes/detail.tsx`'s
// existing keybind effect owns the actual keystroke and clipboard write —
// resolve() only ever dispatches `scope: "global"` entries). Reuses this
// module's injectable-`origin` grammar so the payload is golden-test-
// pinned like `buildCiteUrl`/`buildCiteMarkdown` above.
export type ProvenanceInput = {
  /// Falls back to the id at the call site when blank, same convention as
  /// `CiteCommentInput.title`.
  title: string;
  kb: string;
  sourceRelative: string;
  /// W3.P-c — now optional. The `y p` yank always has it; an artifact
  /// REGISTER migrated forward from a W2.6b mark never carried one (marks
  /// stored only the path), and printing "id: undefined" would be worse
  /// than omitting the line.
  id?: string | null;
  /// W3.P-c — the reader's active section heading id, when the capture
  /// site knew it. Rides `artifactHref`'s existing `?sec=` grammar so a
  /// pasted artifact register lands where it was taken from.
  sec?: string | null;
  /// The artifact's origin session id (`ArtifactSessionOut.authored`),
  /// when the reader has that data — `null`/`undefined` omits the line
  /// entirely rather than printing "origin session: null".
  sessionId?: string | null;
  /// Injectable for tests; defaults to `window.location.origin`.
  origin?: string;
};

/// One line each: title, permalink, and — only when known — the id and the
/// origin session id. No network calls of its own; every field is data
/// the reader already has in memory (recon §4: "a complete `"p` payload
/// with zero new fetches on the Detail route").
export function buildProvenanceBlock(input: ProvenanceInput): string {
  const origin =
    input.origin ??
    (typeof window !== "undefined" ? window.location.origin : "");
  const url = `${origin}${artifactHref(
    input.kb,
    input.sourceRelative,
    input.sec ? { sec: input.sec } : undefined,
  )}`;
  const lines = [input.title, url];
  if (input.id) lines.push(`id: ${input.id}`);
  if (input.sessionId) lines.push(`origin session: ${input.sessionId}`);
  return lines.join("\n");
}

// ── W3.P-c — provenance registers render through THIS module ──────────────
//
// `lib/registers.ts` owns the 26-slot store and the tagged `Ref` union;
// what a register looks like when PASTED is a citation-grammar question,
// so it lives here beside `buildCiteMarkdown`/`buildSelectionCite`/
// `buildProvenanceBlock` rather than growing a second payload grammar in
// the store module. Every kind's rendering is golden-pinned in
// `quote.test.ts`.

/// The permalink for a session register — the `/sessions?focus=<id>` deep
/// link the memory route already emits for "origin session" rows. Split
/// out (and exported) so the golden test pins the grammar once.
export function sessionFocusHref(sessionId: string): string {
  return `/sessions?focus=${encodeURIComponent(sessionId)}`;
}

/// Render one register's reference as pasteable text. Artifact and
/// selection kinds delegate to the two builders above verbatim — the whole
/// point of the subsumption is that `" a` and `y p` produce the SAME
/// payload for the same artifact. Session and commit kinds have no
/// existing builder, so they follow the same shape: a human label line,
/// then a URL line when one exists, then `key: value` lines.
export function renderRegister(ref: Ref, origin?: string): string {
  switch (ref.kind) {
    case "artifact":
      return buildProvenanceBlock({
        title: ref.title || ref.sourceRelative,
        kb: ref.kb,
        sourceRelative: ref.sourceRelative,
        id: ref.id,
        sec: ref.sec,
        sessionId: ref.sessionId,
        origin,
      });
    case "selection":
      return buildSelectionCite({
        kb: ref.kb,
        sourceRelative: ref.sourceRelative,
        title: ref.title || ref.sourceRelative,
        anchor: {
          kind: "selection",
          css_path: ref.cssPath,
          offset: ref.offset,
          snippet: ref.snippet,
        },
        sec: ref.sec,
        origin,
      });
    case "session": {
      const base =
        origin ?? (typeof window !== "undefined" ? window.location.origin : "");
      return [
        ref.title || ref.sessionId,
        `${base}${sessionFocusHref(ref.sessionId)}`,
        `session: ${ref.sessionId}`,
      ].join("\n");
    }
    case "commit": {
      // No URL: kb indexes no forge, and inventing one would be a lie.
      const lines = [ref.subject || ref.sha, `commit: ${ref.sha}`];
      if (ref.repo) lines.push(`repo: ${ref.repo}`);
      return lines.join("\n");
    }
  }
}
