// kbc-prose/1 (V76-B3) — THE prose renderer.
//
// Every prose surface in this SPA (finding cards, report summary/lede,
// comments, claims, timeline events, the Document tab's non-card prose)
// renders through this component. Markdown structure is `lib/markdownLite`
// (the SPA's only Markdown path). Refs come from the wire — this file
// never parses the closed grammar.
//
// Fences: V76-C1's `useHighlight` hook is not on this base, so a fence is
// a plain `<pre>` with a TODO naming C1. Do not invent a second highlighter.
import type { MouseEvent } from "react";
import { Link } from "react-router-dom";
import type { MarkdownLiteOptions } from "../../lib/markdownLite";
import {
  buildProseTree,
  overlayPlain,
  type FieldRefs,
  type OverlayNode,
  type ProseRef,
  type ProseTreeBlock,
} from "../../lib/proseRefs";
import "../../styles/prose.css";

export interface ProseBlockProps {
  text: string;
  refs?: FieldRefs | ProseRef[] | null;
  repo: string;
  reviewId?: number;
  className?: string;
  /** Default: report-summary subset (no headings/fences). Documents opt in. */
  markdown?: MarkdownLiteOptions;
  /** Skip block wrappers — title lines, compact rows. */
  inline?: boolean;
  /** Paint refs without nested `<a>` (a parent row is already a link). */
  nolink?: boolean;
}

function focusFinding(slug: string): boolean {
  const sel = `[data-kbc-finding="${cssAttr(slug)}"], [data-kbc-finding-row="${cssAttr(slug)}"]`;
  const el = document.querySelector<HTMLElement>(sel);
  if (!el) return false;
  document.querySelectorAll("[data-kbc-prose-focused]").forEach((n) => n.removeAttribute("data-kbc-prose-focused"));
  el.setAttribute("data-kbc-prose-focused", "");
  el.classList.add("kbc-finding--prose-focus");
  el.scrollIntoView({ block: "nearest" });
  return true;
}

function cssAttr(s: string): string {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function onFindingClick(e: MouseEvent, href: string | null | undefined, slug?: string) {
  if (!slug) return;
  if (focusFinding(slug)) {
    e.preventDefault();
  }
  void href;
}

export function ProseNodes({ nodes, nolink }: { nodes: OverlayNode[]; nolink?: boolean }) {
  return (
    <>
      {nodes.map((n, i) => {
        if (n.t === "bold") return <b key={i}>{n.text}</b>;
        if (n.t === "code") return <code key={i}>{n.text}</code>;
        if (n.t !== "ref") return <span key={i}>{n.text}</span>;

        const inner = n.wrap === "code" ? <code>{n.text}</code> : n.wrap === "bold" ? <b>{n.text}</b> : n.text;
        const peek = n.peek ? (
          <span className="kbc-prose-peek" data-kbc-prose-peek={n.kind}>
            {n.peek}
          </span>
        ) : null;

        if (!n.href || nolink) {
          return (
            <span
              key={i}
              className={n.className}
              title={n.title ?? n.peek}
              data-kbc-prose-ref={n.kind}
              data-kbc-prose-trust={n.trust}
              data-kbc-prose-orphan={n.trust === "orphan" ? "" : undefined}
              data-kbc-prose-skipped={n.skippedLink ? "markdown-link" : undefined}
            >
              {inner}
              {peek}
            </span>
          );
        }

        const slug = n.kind === "finding" ? n.slug : undefined;
        return (
          <Link
            key={i}
            to={n.href}
            className={n.className}
            title={n.title ?? n.peek}
            data-kbc-prose-ref={n.kind}
            data-kbc-prose-trust={n.trust}
            data-kbc-prose-href={n.href}
            onClick={(e) => onFindingClick(e, n.href, slug)}
          >
            {inner}
            {peek}
          </Link>
        );
      })}
    </>
  );
}

function BlockView({ block, nolink }: { block: ProseTreeBlock; nolink?: boolean }) {
  if (block.kind === "code") {
    // TODO(V76-C1): paint this fence via useHighlight / highlight::extract_highlights
    // (kbc-theme/1 `.kbc-hl-*`, never a second `.tok-*` highlighter). C1 is
    // not on this base — a plain <pre> is the honest degrade.
    return (
      <pre className="kbc-prose__fence" data-kbc-prose-fence={block.lang ?? undefined}>
        <code>{block.text}</code>
      </pre>
    );
  }
  if (block.kind === "list") {
    return (
      <ul>
        {block.items.map((item, j) => (
          <li key={j}>
            <ProseNodes nodes={item} nolink={nolink} />
          </li>
        ))}
      </ul>
    );
  }
  if (block.kind === "heading") {
    const level = Math.min(6, Math.max(1, block.level));
    const Tag = `h${level}` as "h1" | "h2" | "h3" | "h4" | "h5" | "h6";
    return (
      <Tag>
        <ProseNodes nodes={block.nodes} nolink={nolink} />
      </Tag>
    );
  }
  return (
    <p>
      <ProseNodes nodes={block.nodes} nolink={nolink} />
    </p>
  );
}

export default function ProseBlock({
  text,
  refs,
  repo,
  reviewId,
  className,
  markdown,
  inline,
  nolink,
}: ProseBlockProps) {
  if (!text) return null;
  const tree = buildProseTree(text, refs, { repo, reviewId }, markdown);
  if (inline) {
    const nodes = tree.blocks.flatMap((b) => {
      if (b.kind === "paragraph" || b.kind === "heading") return b.nodes;
      if (b.kind === "list") return b.items.flat();
      return [{ t: "text" as const, text: b.text }];
    });
    return (
      <span className={["kbc-prose", "kbc-prose--inline", className].filter(Boolean).join(" ")} data-kbc-prose>
        <ProseNodes nodes={nodes} nolink={nolink} />
        {tree.truncated && (
          <span className="kbc-prose__truncated" data-kbc-prose-truncated>
            refs truncated
          </span>
        )}
      </span>
    );
  }
  return (
    <div className={["kbc-prose", className].filter(Boolean).join(" ")} data-kbc-prose>
      {tree.blocks.map((b, i) => (
        <BlockView key={i} block={b} nolink={nolink} />
      ))}
      {tree.truncated && (
        <p className="kbc-prose__truncated" data-kbc-prose-truncated>
          refs truncated at 64 — split the prose
        </p>
      )}
    </div>
  );
}

/** Overlay a single already-parsed markdown run (Document tab non-card prose). */
export function ProseRun({
  text,
  refs,
  repo,
  reviewId,
  base = 0,
}: {
  text: string;
  refs?: FieldRefs | ProseRef[] | null;
  repo: string;
  reviewId?: number;
  base?: number;
}) {
  const nodes = overlayPlain(text, refs, { repo, reviewId }, base);
  return <ProseNodes nodes={nodes} />;
}
