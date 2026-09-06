import { useMemo } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import type { Components } from "react-markdown";
import { ATTACHMENT_SCHEME, attachmentServeUrl } from "../lib/attachmentUrl";
import { rehypeCitations } from "../lib/linkifyCitations";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";
import { Icon } from "./icons";
import Sketch from "./slate/Sketch";

// Renders a kb-comments/1 body as GitHub-flavored Markdown.
//
// SECURITY: rendered ONLY in the parent SPA (never inside the artifact
// iframe). No `rehype-raw` — raw HTML in a body is escaped, never parsed —
// so there is no raw-HTML React sink and no script-injection path. Do NOT
// add rehype-raw. `rehypeCitations` (CT-B5) is a transform-only AST pass
// (same posture as `NoteMarkdown.tsx`'s callout/task-index/wikilink
// passes) — it only wraps EXISTING text in `<a>` elements built from our
// own `codeSearchUrl`, never injects raw HTML, so this doesn't reopen that
// door.
//
// The ONE deliberate deviation from "no custom urlTransform" (Y-track): we
// pass a urlTransform that whitelists ONLY the `attachment:` scheme and
// delegates every other URL to react-markdown's `defaultUrlTransform`, so
// `javascript:`/`data:`/unknown schemes are still stripped exactly as
// before. An `attachment:<aid>` URL never reaches the DOM literally — the
// `img`/`a` overrides below rewrite it to a same-origin daemon serve URL
// (raster → inline `<img>`, file → download chip). When `kb`/`id` are
// absent (e.g. a Preview with no context), the ref renders as an inert
// placeholder rather than a broken link.

function urlTransform(url: string): string {
  if (url.startsWith(ATTACHMENT_SCHEME)) return url;
  return defaultUrlTransform(url);
}

type Props = {
  body: string;
  className?: string;
  /// Needed to resolve inline `attachment:` refs to a serve URL. Absent in
  /// contexts with no artifact (e.g. a bare Preview) → refs render inert.
  kb?: string;
  id?: string;
  /// Open an inline image in the lightbox.
  onOpenImage?: (url: string, alt: string) => void;
  /// SL4 — OPT-IN ```mermaid rendering (design §10 "Drawings"). Pass a slate
  /// post's `seq` and a ```mermaid fence mounts the sandboxed <Sketch>
  /// frame instead of a code block; EVERY other fence renders exactly as it
  /// does today. Absent (every pre-SL4 call site: comments, replies, note
  /// previews, the editor's Preview pane) ⇒ byte-identical output — a
  /// comment body's ```mermaid fence stays a code block, because a drawing
  /// is a slate affordance and widening it to comments is not this phase's
  /// call to make.
  sketchSeq?: number;
};

export default function CommentBody({
  body,
  className = "cp__row-body",
  kb,
  id,
  onOpenImage,
  sketchSeq,
}: Props) {
  const codeUrl = useCodeUrlForKb(kb);
  const components: Components = useMemo(
    () => ({
      // SL4 — the ```mermaid interception. Gated on `sketchSeq` so the
      // override is literally absent for every other surface; inside the
      // gate it matches ONLY `language-mermaid` and returns the untouched
      // element for every other fence. The frame is sandboxed
      // ("allow-scripts", no allow-same-origin) and mermaid ships in its own
      // Vite entry — this component never gains a raw-HTML sink, so the
      // no-rehype-raw posture above is intact.
      ...(sketchSeq === undefined
        ? {}
        : {
            code(props: { className?: string; children?: unknown }) {
              const { className, children } = props;
              if (className?.split(/\s+/).includes("language-mermaid")) {
                const src = String(children ?? "").replace(/\n$/, "");
                return <Sketch source={src} seq={sketchSeq} />;
              }
              const { node: _n, ...rest } = props as Record<string, unknown> & {
                node?: unknown;
              };
              return <code {...(rest as Record<string, unknown>)} />;
            },
          }),
      a({ href, children }) {
        if (href && href.startsWith(ATTACHMENT_SCHEME)) {
          const aid = href.slice(ATTACHMENT_SCHEME.length);
          if (!kb || !id || !aid) {
            return <span className="cp__att-missing">⎘ attachment</span>;
          }
          return (
            <a
              className="cp__att-chip cp__att-chip-inline"
              href={attachmentServeUrl(kb, id, aid)}
              target="_blank"
              rel="noopener noreferrer"
              download
            >
              <span aria-hidden="true"><Icon.Paperclip /></span> {children}
            </a>
          );
        }
        return (
          <a href={href} target="_blank" rel="noopener noreferrer">
            {children}
          </a>
        );
      },
      img(props) {
        const { src, alt } = props as { src?: string; alt?: string };
        if (src && src.startsWith(ATTACHMENT_SCHEME)) {
          const aid = src.slice(ATTACHMENT_SCHEME.length);
          if (!kb || !id || !aid) {
            return (
              <span className="cp__att-missing">
                <Icon.ImageFrame aria-hidden="true" /> {alt || "image"}
              </span>
            );
          }
          const url = attachmentServeUrl(kb, id, aid);
          return (
            <img
              className="cp__att-embed"
              src={url}
              alt={alt || "attachment"}
              loading="lazy"
              onClick={() => onOpenImage?.(url, alt || "")}
            />
          );
        }
        // A non-attachment image: render plainly (drop the hast `node` prop,
        // as NoteMarkdown does, so it isn't forwarded to the DOM <img>).
        const { node: _node, ...rest } = props as Record<string, unknown> & {
          node?: unknown;
        };
        return <img {...(rest as Record<string, unknown>)} />;
      },
    }),
    [kb, id, onOpenImage, sketchSeq],
  );

  return (
    <div className={`${className} cp__md`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        rehypePlugins={[[rehypeCitations, codeUrl]]}
        urlTransform={urlTransform}
        components={components}
      >
        {body}
      </ReactMarkdown>
    </div>
  );
}
