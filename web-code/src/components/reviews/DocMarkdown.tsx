// The `kbc-review/1` document body, rendered (V73-K2b).
//
// One renderer over `lib/reviewDoc.ts`'s `DocBlock` stream, which is
// `lib/markdownLite.ts` (the SPA's only Markdown path — see its own header
// for why there is no dependency) plus one extra inline run kind: a REF.
//
// Safety is structural, as everywhere else in this SPA: nothing here builds
// an HTML string and there is no `dangerouslySetInnerHTML` in `web-code/` at
// all. Every run becomes a React text child, which React escapes.
import type { FieldRefs } from "../../api/types";
import type { DocBlock, DocRun } from "../../lib/reviewDoc";
import { ProseRun } from "../prose/ProseBlock";
import { FenceBlock } from "../SafeMarkdown";
import RefCard from "./RefCard";

export interface DocMarkdownProps {
  blocks: DocBlock[];
  repo: string;
  reviewId: number;
  /** Ref bodies whose cards are folded (`?cards=folded` folds them all). */
  foldedRefs: ReadonlySet<string>;
  cardsFolded: boolean;
  focusedRef: string | null;
  onToggleFold: (ref: string) => void;
  /** V76-B3 — overlay kbc-prose/1 refs on non-card runs. */
  proseRefs?: FieldRefs | null;
}

function Runs({
  runs,
  repo,
  reviewId,
  foldedRefs,
  cardsFolded,
  focusedRef,
  onToggleFold,
  proseRefs,
}: { runs: DocRun[] } & Omit<DocMarkdownProps, "blocks">) {
  return (
    <>
      {runs.map((r, i) => {
        if (r.kind === "ref") {
          const ref = r.span.kind === "card" ? r.span.card.ref : r.span.body;
          return (
            <RefCard
              key={i}
              span={r.span}
              repo={repo}
              reviewId={reviewId}
              folded={cardsFolded || foldedRefs.has(ref)}
              focused={focusedRef === ref}
              onToggleFold={onToggleFold}
            />
          );
        }
        if (r.kind === "bold") {
          return (
            <b key={i}>
              <ProseRun text={r.text} refs={proseRefs} repo={repo} reviewId={reviewId} base={-1} />
            </b>
          );
        }
        if (r.kind === "code") {
          return (
            <code key={i}>
              <ProseRun text={r.text} refs={proseRefs} repo={repo} reviewId={reviewId} base={-1} />
            </code>
          );
        }
        return (
          <span key={i}>
            <ProseRun text={r.text} refs={proseRefs} repo={repo} reviewId={reviewId} base={-1} />
          </span>
        );
      })}
    </>
  );
}

export default function DocMarkdown(props: DocMarkdownProps) {
  const { blocks, ...rest } = props;
  return (
    <div className="kbc-doc__body" data-kbc-doc-body>
      {blocks.map((b, i) => {
        if (b.kind === "code") {
          return (
            <span key={i} data-kbc-doc-fence={b.lang ?? undefined}>
              <FenceBlock text={b.text} lang={b.lang} />
            </span>
          );
        }
        if (b.kind === "list") {
          return (
            <ul key={i}>
              {b.items.map((item, j) => (
                <li key={j}>
                  <Runs runs={item} {...rest} />
                </li>
              ))}
            </ul>
          );
        }
        if (b.kind === "heading") {
          // The document's own `#` levels are shifted DOWN one: the panel
          // header already owns the page's `<h2>`, so an authored `#` must
          // not compete with it. Clamped at `<h6>`.
          const level = Math.min(6, b.level + 2);
          const Tag = `h${level}` as "h3" | "h4" | "h5" | "h6";
          return (
            <Tag key={i} className="kbc-doc__heading">
              <Runs runs={b.runs} {...rest} />
            </Tag>
          );
        }
        return (
          <p key={i}>
            <Runs runs={b.runs} {...rest} />
          </p>
        );
      })}
    </div>
  );
}
