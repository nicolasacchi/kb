// V76-C1 — the safe Markdown path with a fence renderer that calls
// `useHighlight`. Same parser as ReportPanel / DocMarkdown (`markdownLite`);
// fences opt in and paint through HighlightedSnippet.

import { useMemo } from "react";
import { parseMarkdownLite, type InlineRun, type MarkdownBlock } from "../lib/markdownLite";
import HighlightedSnippet from "./HighlightedSnippet";

export interface SafeMarkdownProps {
  text: string;
  fences?: boolean;
  headings?: boolean;
  /** Language used when a fence has no info string. */
  fallbackLang?: string | null;
  className?: string;
}

function InlineRuns({ runs }: { runs: InlineRun[] }) {
  return (
    <>
      {runs.map((r, i) => {
        if (r.kind === "bold") return <strong key={i}>{r.text}</strong>;
        if (r.kind === "code") return <code key={i}>{r.text}</code>;
        return <span key={i}>{r.text}</span>;
      })}
    </>
  );
}

export function FenceBlock({
  text,
  lang,
  fallbackLang,
}: {
  text: string;
  lang: string | null;
  fallbackLang?: string | null;
}) {
  return (
    <HighlightedSnippet
      text={text}
      lang={lang}
      fallbackLang={fallbackLang}
      className="kbc-doc__fence"
    />
  );
}

export default function SafeMarkdown({
  text,
  fences = true,
  headings = false,
  fallbackLang,
  className,
}: SafeMarkdownProps) {
  const blocks: MarkdownBlock[] = useMemo(
    () => parseMarkdownLite(text, { fences, headings }),
    [text, fences, headings],
  );
  return (
    <div className={className} data-kbc-safe-md>
      {blocks.map((b, i) => {
        if (b.kind === "code") {
          return <FenceBlock key={i} text={b.text} lang={b.lang} fallbackLang={fallbackLang} />;
        }
        if (b.kind === "list") {
          return (
            <ul key={i}>
              {b.items.map((item, j) => (
                <li key={j}>
                  <InlineRuns runs={item} />
                </li>
              ))}
            </ul>
          );
        }
        if (b.kind === "heading") {
          const level = Math.min(6, b.level + (headings ? 2 : 0));
          const Tag = (`h${Math.max(3, level)}` as "h3" | "h4" | "h5" | "h6");
          return (
            <Tag key={i} className="kbc-doc__heading">
              <InlineRuns runs={b.runs} />
            </Tag>
          );
        }
        return (
          <p key={i}>
            <InlineRuns runs={b.runs} />
          </p>
        );
      })}
    </div>
  );
}
