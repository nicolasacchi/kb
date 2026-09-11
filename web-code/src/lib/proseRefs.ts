// kbc-prose/1 (V76-B3) — the SPA half of the prose-ref overlay.
//
// The daemon owns the grammar (`crates/kb-code-server/src/prose_refs.rs`,
// golden-pinned in `grammar/prose_refs.golden.json`). This module NEVER
// re-parses path/symbol/finding tokens: it consumes the wire's `refs`
// array, slices the source at the UTF-16 spans the daemon already minted,
// and produces a render tree the `ProseBlock` component paints. Markdown
// structure still comes from `markdownLite` (the SPA's only Markdown
// path) — overlay is a second pass over that tree, not a second parser.
//
// Spans are UTF-16 code units because JS string indexes are UTF-16 (the
// same reason the server converts once, crate invariant 16(a)).
//
// A ref that sits inside a `[text](dest)` Markdown link is never turned
// into a second `<a>` (the no-double-linking rule). An `orphan` is never
// an `<a>` either — dotted underline + caption, never a dead link.

import type { MarkdownLiteOptions } from "./markdownLite";
import { codeUrl, entityUrl, findingUrl, symbolUrl } from "./codeUrl";

export interface ProseSpan {
  start: number;
  end: number;
}

export interface ProseRefResolution {
  state: string;
  path?: string;
  line?: number;
  ent?: string;
  caption?: string;
}

export interface ProseRef {
  kind: string;
  span: ProseSpan;
  text: string;
  path?: string;
  line_start?: number;
  line_end?: number;
  lines?: string;
  container?: string;
  member?: string;
  slug?: string;
  resolution?: ProseRefResolution;
}

export interface FieldRefs {
  refs: ProseRef[];
  truncated: boolean;
}

export interface ProseHrefCtx {
  repo: string;
  reviewId?: number;
}

/** One inline piece after overlay. `href` is set only when this piece is a live link. */
export interface OverlayNode {
  t: "text" | "bold" | "code" | "ref";
  text: string;
  /** Present on `ref` nodes. */
  kind?: string;
  href?: string | null;
  trust?: string;
  className?: string;
  title?: string;
  /** `code`/`bold` wrapper the ref sat inside. */
  wrap?: "code" | "bold";
  /** True when the ref overlapped a Markdown `[text](dest)` and was left unlinked. */
  skippedLink?: boolean;
  /** Peek caption (path:line / symbol / orphan reason). */
  peek?: string;
  /** Finding slug, when `kind === "finding"`. */
  slug?: string;
}

export type ProseTreeBlock =
  | { kind: "paragraph"; nodes: OverlayNode[] }
  | { kind: "list"; items: OverlayNode[][] }
  | { kind: "heading"; level: number; nodes: OverlayNode[] }
  | { kind: "code"; lang: string | null; text: string };

export interface ProseTree {
  blocks: ProseTreeBlock[];
  truncated: boolean;
}

export function emptyFieldRefs(): FieldRefs {
  return { refs: [], truncated: false };
}

export function asFieldRefs(raw: FieldRefs | ProseRef[] | null | undefined): FieldRefs {
  if (!raw) return emptyFieldRefs();
  if (Array.isArray(raw)) return { refs: raw, truncated: false };
  return { refs: raw.refs ?? [], truncated: !!raw.truncated };
}

/** `[text](dest)` ranges in UTF-16 indexes — same shape the daemon's structural pre-pass uses. */
export function markdownLinkRanges(text: string): [number, number][] {
  const ranges: [number, number][] = [];
  let i = 0;
  while (i < text.length) {
    if (text[i] === "[") {
      const close = text.indexOf("](", i + 1);
      if (close !== -1) {
        const paren = text.indexOf(")", close + 2);
        if (paren !== -1) {
          ranges.push([i, paren + 1]);
          i = paren + 1;
          continue;
        }
      }
    }
    i += 1;
  }
  return ranges;
}

function inRanges(ranges: [number, number][], pos: number): boolean {
  return ranges.some(([s, e]) => pos >= s && pos < e);
}

function spanInsideLink(span: ProseSpan, ranges: [number, number][]): boolean {
  if (ranges.length === 0) return false;
  // Fully inside, or the start sits inside — either way a second <a> would
  // nest or collide with the Markdown link.
  return inRanges(ranges, span.start) || ranges.some(([s, e]) => span.start < e && span.end > s);
}

export function trustOf(ref: ProseRef): string {
  const state = ref.resolution?.state;
  if (state === "exact" || state === "likely" || state === "candidate" || state === "orphan") {
    return state;
  }
  return "candidate";
}

export function proseRefClass(ref: ProseRef, opts?: { skippedLink?: boolean }): string {
  const trust = trustOf(ref);
  const bits = ["kbc-prose-ref", `kbc-prose-ref--${trust}`, `kbc-trust-${trust}`];
  if (opts?.skippedLink || trust === "orphan") bits.push("kbc-prose-ref--nolink");
  return bits.join(" ");
}

function lineSel(ref: ProseRef): number | { start: number; end: number } | undefined {
  const start = ref.resolution?.line ?? ref.line_start;
  if (start == null) return undefined;
  const end = ref.line_end;
  if (end != null && end !== start) return { start, end };
  return start;
}

export function proseRefHref(ref: ProseRef, ctx: ProseHrefCtx): string | null {
  const state = ref.resolution?.state;
  if (state === "orphan") return null;
  if (ref.kind === "code") return null;

  if (ref.kind === "finding") {
    const slug = ref.slug ?? (ref.text.startsWith("f-") ? ref.text : undefined);
    if (!slug || ctx.reviewId == null) return null;
    return findingUrl(ctx.repo, ctx.reviewId, slug);
  }

  const path = ref.resolution?.path ?? ref.path;
  const line = lineSel(ref);

  if (ref.kind === "path") {
    if (!path) return null;
    return codeUrl({ repo: ctx.repo, path, line });
  }

  if (ref.kind === "symbol" || ref.kind === "call") {
    if (ref.resolution?.ent) {
      return entityUrl(ctx.repo, ref.resolution.ent, {
        path: path ?? "",
        line,
      });
    }
    if (path) {
      return symbolUrl(ctx.repo, ref.text, { fallbackPath: path, fallbackLine: line });
    }
    // A candidate/likely symbol with no path still has a name — land on
    // the repo root with ?sym= so the reader can resolve it, rather than
    // inventing a file.
    if (ref.kind === "symbol") {
      return symbolUrl(ctx.repo, ref.text, { fallbackPath: "" });
    }
    return null;
  }

  return null;
}

export function proseRefPeek(ref: ProseRef): string {
  const cap = ref.resolution?.caption;
  if (ref.resolution?.state === "orphan") return cap ?? "no honest match";
  if (ref.kind === "path") {
    const path = ref.resolution?.path ?? ref.path ?? ref.text;
    const line = ref.resolution?.line ?? ref.line_start;
    return line != null ? `${path}:${line}` : path;
  }
  if (ref.kind === "finding") return ref.slug ?? ref.text;
  if (ref.kind === "symbol" || ref.kind === "call") {
    return ref.resolution?.ent ?? ref.text;
  }
  return cap ?? ref.text;
}

interface SourcedInline {
  kind: "text" | "bold" | "code";
  text: string;
  start: number;
  end: number;
}

/** `parseInline` plus UTF-16 offsets into `source` at `base`. */
export function parseInlineSourced(text: string, base = 0): SourcedInline[] {
  const runs: SourcedInline[] = [];
  let i = 0;
  let buf = "";
  let bufStart = 0;
  function flush(end: number) {
    if (buf) {
      runs.push({ kind: "text", text: buf, start: base + bufStart, end: base + end });
      buf = "";
    }
  }
  while (i < text.length) {
    if (text.startsWith("**", i)) {
      const end = text.indexOf("**", i + 2);
      if (end !== -1 && end > i + 2) {
        flush(i);
        runs.push({
          kind: "bold",
          text: text.slice(i + 2, end),
          start: base + i + 2,
          end: base + end,
        });
        i = end + 2;
        continue;
      }
    }
    if (text[i] === "`") {
      const end = text.indexOf("`", i + 1);
      if (end !== -1 && end > i + 1) {
        flush(i);
        runs.push({
          kind: "code",
          text: text.slice(i + 1, end),
          start: base + i + 1,
          end: base + end,
        });
        i = end + 1;
        continue;
      }
    }
    if (!buf) bufStart = i;
    buf += text[i];
    i += 1;
  }
  flush(i);
  return runs;
}

function overlayable(refs: ProseRef[]): ProseRef[] {
  return refs
    .filter((r) => r.kind !== "code")
    .slice()
    .sort((a, b) => a.span.start - b.span.start || a.span.end - b.span.end);
}

function refNode(ref: ProseRef, text: string, ctx: ProseHrefCtx, skippedLink: boolean, wrap?: "code" | "bold"): OverlayNode {
  const href = skippedLink ? null : proseRefHref(ref, ctx);
  const trust = trustOf(ref);
  return {
    t: "ref",
    text,
    kind: ref.kind,
    href,
    trust,
    className: proseRefClass(ref, { skippedLink }),
    title: ref.resolution?.caption,
    wrap,
    skippedLink: skippedLink || undefined,
    peek: proseRefPeek(ref),
    slug: ref.slug ?? (ref.kind === "finding" ? ref.text : undefined),
  };
}

export function overlayRun(
  run: SourcedInline,
  refs: ProseRef[],
  linkRanges: [number, number][],
  ctx: ProseHrefCtx,
): OverlayNode[] {
  const wrap: "code" | "bold" | undefined = run.kind === "text" ? undefined : run.kind;
  const hits = overlayable(refs).filter((r) => r.span.start < run.end && r.span.end > run.start);
  if (hits.length === 0) {
    return [{ t: run.kind, text: run.text }];
  }
  const out: OverlayNode[] = [];
  let pos = run.start;
  for (const ref of hits) {
    const s = Math.max(ref.span.start, run.start);
    const e = Math.min(ref.span.end, run.end);
    if (s < pos) continue;
    if (s > pos) {
      out.push({ t: run.kind, text: run.text.slice(pos - run.start, s - run.start) });
    }
    const piece = run.text.slice(s - run.start, e - run.start);
    const skipped = spanInsideLink(ref.span, linkRanges);
    out.push(refNode(ref, piece, ctx, skipped, wrap));
    pos = e;
  }
  if (pos < run.end) {
    out.push({ t: run.kind, text: run.text.slice(pos - run.start) });
  }
  return out;
}

function overlayText(text: string, base: number, refs: ProseRef[], linkRanges: [number, number][], ctx: ProseHrefCtx): OverlayNode[] {
  const runs = parseInlineSourced(text, base);
  const nodes: OverlayNode[] = [];
  for (const run of runs) nodes.push(...overlayRun(run, refs, linkRanges, ctx));
  return nodes;
}

interface LineSlice {
  start: number;
  text: string;
}

type SourcedBlock =
  | { kind: "paragraph"; lines: LineSlice[] }
  | { kind: "list"; items: LineSlice[] }
  | { kind: "heading"; level: number; line: LineSlice }
  | { kind: "code"; lang: string | null; text: string };

const BULLET_PATTERN = /^\s*[-*]\s+(.*)$/;
const HEADING_PATTERN = /^(#{1,6})\s+(.*)$/;
const FENCE_PATTERN = /^(`{3,}|~{3,})\s*(\S*)/;

/**
 * Walk `source` the same way `parseMarkdownLite` groups blocks, but keep
 * per-line UTF-16 offsets so overlay spans still hit the original string
 * (joining paragraph lines with a space would shift them).
 */
export function parseSourcedBlocks(source: string, opts: MarkdownLiteOptions = {}): SourcedBlock[] {
  const blocks: SourcedBlock[] = [];
  let para: LineSlice[] = [];
  let listItems: LineSlice[] = [];
  let fence: { ch: string; len: number; lang: string | null; body: string[] } | null = null;
  let offset = 0;
  const text = source;

  function flushPara() {
    if (para.length > 0) {
      blocks.push({ kind: "paragraph", lines: para });
      para = [];
    }
  }
  function flushList() {
    if (listItems.length > 0) {
      blocks.push({ kind: "list", items: listItems });
      listItems = [];
    }
  }

  while (offset < text.length) {
    let nl = text.indexOf("\n", offset);
    const crlf = nl > 0 && text[nl - 1] === "\r" ? 1 : 0;
    if (nl === -1) nl = text.length;
    const lineStart = offset;
    const lineEnd = nl - crlf;
    const line = text.slice(lineStart, lineEnd);
    const next = nl === text.length ? text.length : nl + 1;

    if (opts.fences) {
      if (fence) {
        const close = line.trimStart().match(FENCE_PATTERN);
        if (close && close[1][0] === fence.ch && close[1].length >= fence.len) {
          blocks.push({ kind: "code", lang: fence.lang, text: fence.body.join("\n") });
          fence = null;
        } else {
          fence.body.push(line);
        }
        offset = next;
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
        offset = next;
        continue;
      }
    }
    if (opts.headings) {
      const h = line.match(HEADING_PATTERN);
      if (h) {
        flushPara();
        flushList();
        const body = h[2];
        const bodyStart = lineStart + line.indexOf(body);
        blocks.push({
          kind: "heading",
          level: h[1].length,
          line: { start: bodyStart, text: body },
        });
        offset = next;
        continue;
      }
    }
    if (line.trim() === "") {
      flushPara();
      flushList();
      offset = next;
      continue;
    }
    const bullet = line.match(BULLET_PATTERN);
    if (bullet) {
      flushPara();
      const body = bullet[1];
      const bodyStart = lineStart + line.indexOf(body);
      listItems.push({ start: bodyStart, text: body });
      offset = next;
      continue;
    }
    flushList();
    const trimmed = line.trim();
    const tStart = lineStart + line.indexOf(trimmed);
    para.push({ start: tStart, text: trimmed });
    offset = next;
  }
  if (fence) blocks.push({ kind: "code", lang: fence.lang, text: fence.body.join("\n") });
  flushPara();
  flushList();
  return blocks;
}

function joinLineNodes(lines: LineSlice[], refs: ProseRef[], linkRanges: [number, number][], ctx: ProseHrefCtx): OverlayNode[] {
  const nodes: OverlayNode[] = [];
  lines.forEach((ln, i) => {
    if (i > 0) nodes.push({ t: "text", text: " " });
    nodes.push(...overlayText(ln.text, ln.start, refs, linkRanges, ctx));
  });
  return nodes;
}

export function buildProseTree(
  source: string,
  refs: FieldRefs | ProseRef[] | null | undefined,
  ctx: ProseHrefCtx,
  opts: MarkdownLiteOptions = {},
): ProseTree {
  const field = asFieldRefs(refs);
  const linkRanges = markdownLinkRanges(source);
  const sourced = parseSourcedBlocks(source, opts);
  const blocks: ProseTreeBlock[] = sourced.map((b) => {
    if (b.kind === "code") return b;
    if (b.kind === "heading") {
      return {
        kind: "heading",
        level: b.level,
        nodes: overlayText(b.line.text, b.line.start, field.refs, linkRanges, ctx),
      };
    }
    if (b.kind === "list") {
      return {
        kind: "list",
        items: b.items.map((it) => overlayText(it.text, it.start, field.refs, linkRanges, ctx)),
      };
    }
    return { kind: "paragraph", nodes: joinLineNodes(b.lines, field.refs, linkRanges, ctx) };
  });
  return { blocks, truncated: field.truncated };
}

/**
 * Overlay refs onto a substring already extracted as a markdown run (the
 * Document tab's non-card prose). `base` is the UTF-16 offset of `text`
 * inside the original field. When `base` is unknown, pass `-1` and matching
 * falls back to `ref.text` occurrence (used only when the caller cannot
 * recover offsets).
 */
export function overlayPlain(
  text: string,
  refs: FieldRefs | ProseRef[] | null | undefined,
  ctx: ProseHrefCtx,
  base = 0,
  linkRanges?: [number, number][],
): OverlayNode[] {
  const field = asFieldRefs(refs);
  if (base < 0) {
    // Sequential occurrence fallback — last resort for a caller that lost
    // offsets. Prefer `base >= 0`.
    const nodes: OverlayNode[] = [];
    let cursor = 0;
    const hits = overlayable(field.refs).filter((r) => text.includes(r.text));
    const used = new Set<number>();
    for (const ref of hits) {
      const at = text.indexOf(ref.text, cursor);
      if (at < 0 || used.has(ref.span.start)) continue;
      used.add(ref.span.start);
      if (at > cursor) nodes.push({ t: "text", text: text.slice(cursor, at) });
      nodes.push(refNode(ref, ref.text, ctx, false));
      cursor = at + ref.text.length;
    }
    if (cursor < text.length) nodes.push({ t: "text", text: text.slice(cursor) });
    return nodes.length ? nodes : [{ t: "text", text }];
  }
  const ranges = linkRanges ?? markdownLinkRanges(text);
  // Shift link ranges if they were computed on the full source.
  return overlayText(text, base, field.refs, ranges, ctx);
}


