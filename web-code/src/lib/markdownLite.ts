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
  | { kind: "list"; items: InlineRun[][] };

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

/// Parse a markdown-lite SOURCE string into blocks: consecutive `- `/`* `
/// lines group into one `list` block; every other non-blank run of lines
/// joins (space-separated) into one `paragraph` block. Blank lines separate
/// blocks. Total — an empty/whitespace-only source returns `[]`.
export function parseMarkdownLite(source: string): MarkdownBlock[] {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  let para: string[] = [];
  let listItems: string[] = [];

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
  flushPara();
  flushList();
  return blocks;
}
