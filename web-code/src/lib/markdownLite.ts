// PRR-U2 (kb v0.39 "The PR Room," Report tab, Section 01 · Summary) — a
// minimal, dependency-free Markdown-SUBSET parser for `ReviewReport.summary`.
//
// Deviation note (see this unit's own report): the design doc says to "find
// and reuse the SPA's existing markdown renderer," but web-code has ZERO
// markdown dependency anywhere (`react-markdown` etc. live only in kb's
// OTHER, separate npm package — `web/`, not `web-code/` — and the two are
// different builds with different node_modules; nothing there is importable
// here). Rather than add a new runtime dependency mid-unit for one summary
// block, this is a small, pure, well-scoped parser covering exactly the
// shapes the generator's own report-pr.html summaries use (see
// design-server.md §4.1's `.v-body` prose): paragraphs, a bullet list,
// **bold**, and inline `code`. No headings/tables/nested lists/links — an
// honest subset, not a CommonMark implementation.
//
// Pure + total: any input produces SOME block list, never throws. Rendering
// (the actual JSX) lives in `components/reviews/ReportPanel.tsx` — this
// module only parses, so it can be unit-tested in the `node` vitest
// environment without a DOM (`vitest.config.ts`'s `include` is
// `src/**/*.test.ts` only, never `.tsx` — see that file's own doc).

export type InlineRun =
  | { kind: "text"; text: string }
  | { kind: "bold"; text: string }
  | { kind: "code"; text: string };

export type MarkdownBlock =
  | { kind: "paragraph"; runs: InlineRun[] }
  | { kind: "list"; items: InlineRun[][] }
  // V73-K2b — two block kinds a REVIEW DOCUMENT needs and a report summary
  // does not. Both are behind `MarkdownLiteOptions` and OFF by default, so
  // every pre-K2b caller's block stream is byte-identical.
  | { kind: "heading"; level: number; runs: InlineRun[] }
  | { kind: "code"; lang: string | null; text: string };

/// What a caller admits into its own block stream.
///
/// The default (everything `false`) is PRR-U2's original subset exactly —
/// paragraphs, bullet lists, `**bold**`, inline `` `code` ``. A review
/// document opts into the two extra kinds because its body genuinely has
/// them: chapters are `##` headings, and an agent citing a snippet writes a
/// fence. Making them options rather than unconditional keeps `ReportPanel`'s
/// rendering unchanged — a summary line that happens to start with `#` still
/// reads as prose there, which is what it always did.
export interface MarkdownLiteOptions {
  /// `#`…`######` at the start of a line become `heading` blocks.
  headings?: boolean;
  /// ```` ``` ````/`~~~` fences become `code` blocks, verbatim and unparsed.
  fences?: boolean;
}

/// Split one line/inline-run of text into `InlineRun`s on `**bold**` and
/// `` `code` `` spans. Delimiters must be non-empty and on the SAME line
/// (no multi-line spans — this is a subset, not a lexer with backtracking).
/// Unmatched/dangling delimiters degrade to plain text rather than eating
/// the rest of the line.
export function parseInline(text: string): InlineRun[] {
  const runs: InlineRun[] = [];
  let i = 0;
  let buf = "";
  function flush() {
    if (buf) {
      runs.push({ kind: "text", text: buf });
      buf = "";
    }
  }
  while (i < text.length) {
    if (text.startsWith("**", i)) {
      const end = text.indexOf("**", i + 2);
      if (end !== -1 && end > i + 2) {
        flush();
        runs.push({ kind: "bold", text: text.slice(i + 2, end) });
        i = end + 2;
        continue;
      }
    }
    if (text[i] === "`") {
      const end = text.indexOf("`", i + 1);
      if (end !== -1 && end > i + 1) {
        flush();
        runs.push({ kind: "code", text: text.slice(i + 1, end) });
        i = end + 1;
        continue;
      }
    }
    buf += text[i];
    i += 1;
  }
  flush();
  return runs;
}

const BULLET_PATTERN = /^\s*[-*]\s+(.*)$/;
const HEADING_PATTERN = /^(#{1,6})\s+(.*)$/;
const FENCE_PATTERN = /^(`{3,}|~{3,})\s*(\S*)/;

/// Parse a markdown-lite SOURCE string into blocks: consecutive `- `/`* `
/// lines group into one `list` block; every other non-blank run of lines
/// joins (space-separated) into one `paragraph` block. Blank lines separate
/// blocks. Total — an empty/whitespace-only source returns `[]`.
export function parseMarkdownLite(
  source: string,
  opts: MarkdownLiteOptions = {},
): MarkdownBlock[] {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  let para: string[] = [];
  let listItems: string[] = [];
  /// The open fence's marker + length, or `null`. A fence is closed by a run
  /// of the SAME character at least as long — CommonMark's rule, and the one
  /// `review_doc::refs`'s scanner already applies, so a ref inside a fence is
  /// invisible to BOTH sides rather than to one of them.
  let fence: { ch: string; len: number; lang: string | null; body: string[] } | null = null;

  function flushPara() {
    if (para.length > 0) {
      blocks.push({ kind: "paragraph", runs: parseInline(para.join(" ").trim()) });
      para = [];
    }
  }
  function flushList() {
    if (listItems.length > 0) {
      blocks.push({ kind: "list", items: listItems.map((it) => parseInline(it.trim())) });
      listItems = [];
    }
  }

  for (const raw of lines) {
    const line = raw;
    if (opts.fences) {
      if (fence) {
        const close = line.trimStart().match(FENCE_PATTERN);
        if (close && close[1][0] === fence.ch && close[1].length >= fence.len) {
          blocks.push({ kind: "code", lang: fence.lang, text: fence.body.join("\n") });
          fence = null;
        } else {
          fence.body.push(line);
        }
        continue;
      }
      const open = line.trimStart().match(FENCE_PATTERN);
      if (open) {
        flushPara();
        flushList();
        fence = {
          ch: open[1][0],
          len: open[1].length,
          lang: open[2] === "" ? null : open[2],
          body: [],
        };
        continue;
      }
    }
    if (opts.headings) {
      const h = line.match(HEADING_PATTERN);
      if (h) {
        flushPara();
        flushList();
        blocks.push({
          kind: "heading",
          level: h[1].length,
          runs: parseInline(h[2].trim()),
        });
        continue;
      }
    }
    if (line.trim() === "") {
      flushPara();
      flushList();
      continue;
    }
    const bullet = line.match(BULLET_PATTERN);
    if (bullet) {
      flushPara();
      listItems.push(bullet[1]);
      continue;
    }
    flushList();
    para.push(line.trim());
  }
  // An UNCLOSED fence is emitted anyway, with what it holds. Dropping it
  // would silently swallow the tail of the document; a document the daemon
  // accepted is one this side must render whole.
  if (fence) blocks.push({ kind: "code", lang: fence.lang, text: fence.body.join("\n") });
  flushPara();
  flushList();
  return blocks;
}
