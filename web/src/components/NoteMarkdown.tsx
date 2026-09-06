import { useMemo } from "react";
import { useNavigate } from "react-router-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import type { Components } from "react-markdown";
import { parseCallout } from "../lib/callout";
import { makeResolver, rehypeWikilinks } from "../lib/wikilink";
import type { ResolvedLink } from "../api/notes";

// N-track — render a note's Markdown body with INTERACTIVE GFM task
// checkboxes. Toggling a box calls `onToggle(taskIndex, nextChecked)`;
// omit `onToggle` for a read-only render (disabled boxes, like CommentBody).
//
// SECURITY: same posture as CommentBody — NO `rehype-raw`, NO custom
// `urlTransform`. Both rehype plugins here are transform-only AST passes
// (one stamps a data attribute; the other relabels a blockquote element +
// inserts a hast TEXT node title — React-escaped). Neither injects raw HTML,
// so there is no raw-HTML React sink.
//
// The load-bearing invariant: `taskIndex` is the DOCUMENT-ORDER ordinal of
// GFM task items, which is exactly what the server's `POST …/toggle {index}`
// addresses (`kb_core::notes::scan_tasks` walks the same GFM document). The
// rehype pass below stamps that ordinal deterministically (a pre-render tree
// walk, StrictMode-safe — it resets the counter each pass), rather than a
// fragile render-time counter.

// hast node shapes we touch. Kept local + loose so we don't pull a hast
// type dependency for a three-field walk.
type HastNode = {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

function rehypeTaskIndex() {
  return (tree: HastNode) => {
    let i = 0;
    const walk = (node: HastNode) => {
      if (
        node.type === "element" &&
        node.tagName === "input" &&
        node.properties &&
        node.properties.type === "checkbox"
      ) {
        node.properties.dataTaskIndex = i++;
      }
      if (node.children) for (const c of node.children) walk(c);
    };
    walk(tree);
  };
}

// Obsidian-style callouts: a blockquote whose first line is `[!type] title`
// becomes <div class="kb-callout kb-callout--type"> with a title row — the
// same structure markdown::rewrite_callouts emits server-side, so the native
// note view and the served-HTML iframe render callouts identically. Body
// markdown is preserved (we only strip the `[!type] title` marker + its soft
// break). A task inside a callout keeps its document position, so the
// rehypeTaskIndex ordinals are unaffected. The header parse + default-title
// rule lives in `lib/callout.parseCallout` (pinned to the server's semantics
// by `callout.test.ts`); this pass only mutates the hast tree.

function rehypeCallouts() {
  return (tree: HastNode) => {
    const walk = (node: HastNode) => {
      if (node.children) for (const c of node.children) walk(c);
      if (node.type !== "element" || node.tagName !== "blockquote" || !node.children)
        return;
      const para = node.children.find(
        (c) => c.type === "element" && c.tagName === "p",
      );
      const first = para?.children?.[0];
      if (!para || !first || first.type !== "text" || typeof first.value !== "string")
        return;
      const co = parseCallout(first.value);
      if (!co) return;
      const { type, title } = co;
      // Drop the marker text node, then a soft-break <br> if it followed.
      para.children!.shift();
      if (
        para.children![0]?.type === "element" &&
        para.children![0]?.tagName === "br"
      ) {
        para.children!.shift();
      }
      // Title-only line (body lives in later paragraphs) → drop the now-empty <p>.
      if (para.children!.length === 0) {
        node.children = node.children.filter((c) => c !== para);
      }
      const titleEl: HastNode = {
        type: "element",
        tagName: "div",
        properties: { className: ["kb-callout__title"] },
        children: [{ type: "text", value: title }],
      };
      node.tagName = "div";
      node.properties = {
        ...(node.properties ?? {}),
        className: ["kb-callout", `kb-callout--${type}`],
      };
      node.children = [titleEl, ...node.children];
    };
    walk(tree);
  };
}

type Props = {
  body: string;
  /// Omit for a read-only render. When present, checkboxes are enabled and
  /// fire `onToggle(taskIndex, nextChecked)`.
  onToggle?: (taskIndex: number, nextChecked: boolean) => void;
  className?: string;
  /// Resolved `[[wikilinks]]` for this body (from `NoteDetail.links`). When
  /// present, `[[target]]` renders as a clickable internal link; when absent
  /// (e.g. a live edit preview) wikilinks render as muted pending spans.
  links?: ResolvedLink[];
};

export default function NoteMarkdown({ body, onToggle, className, links }: Props) {
  const navigate = useNavigate();
  const resolve = useMemo(() => makeResolver(links), [links]);
  const components: Components = {
    a({ href, children, ...rest }) {
      const node = (rest as { node?: HastNode }).node;
      // Wikilinks navigate WITHIN the SPA (no new tab); modified clicks fall
      // through to the browser. Ordinary markdown links keep opening in a tab.
      if (node?.properties?.dataWikilink && href) {
        return (
          <a
            href={href}
            className="kb-wikilink"
            onClick={(e) => {
              if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.button !== 0) return;
              e.preventDefault();
              navigate(href);
            }}
          >
            {children}
          </a>
        );
      }
      return (
        <a href={href} target="_blank" rel="noopener noreferrer">
          {children}
        </a>
      );
    },
    input(props) {
      // react-markdown@9 passes the hast `node` to every component override.
      const node = (props as { node?: HastNode }).node;
      const isCheckbox = node?.properties?.type === "checkbox";
      if (!isCheckbox) {
        const { node: _drop, ...rest } = props as Record<string, unknown> & {
          node?: HastNode;
        };
        return <input {...rest} />;
      }
      const idx = Number(node?.properties?.dataTaskIndex ?? -1);
      const checked = node?.properties?.checked === true;
      const interactive = !!onToggle && idx >= 0;
      return (
        <input
          type="checkbox"
          checked={checked}
          disabled={!interactive}
          readOnly={!interactive}
          aria-label={`task ${idx + 1}`}
          data-task-index={idx}
          className="kb-note-md__box"
          onChange={(e) => onToggle?.(idx, e.currentTarget.checked)}
        />
      );
    },
  };
  return (
    <div className={`kb-note-md ${className ?? ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        // `rehypeWikilinks` is a unified plugin (attacher) that TAKES the
        // resolver and RETURNS the transformer, so it must be passed in the
        // `[plugin, ...options]` form. Passing `rehypeWikilinks(resolve)` (the
        // pre-applied transformer) directly made unified treat that transformer
        // AS the attacher and invoke it at freeze with no tree → `walk(undefined)`
        // → "Cannot read properties of undefined (reading 'children')", crashing
        // every native note render (wikilink.test.ts calls the transformer
        // directly, so it never caught this integration bug).
        rehypePlugins={[rehypeCallouts, rehypeTaskIndex, [rehypeWikilinks, resolve]]}
        components={components}
      >
        {body}
      </ReactMarkdown>
    </div>
  );
}
