// V76-C1 — one read-only snippet, painted with `highlight/1` spans.
// Never a spinner: while the batch is in flight the text renders plain;
// `tier: none` keeps it plain and adds a "no grammar" caption.

import { useMemo } from "react";
import { useHighlight } from "../hooks/useHighlight";
import { paintSpans } from "../lib/paintSpans";
import { splitContentLines, type PaintedSegment } from "../lib/diffHighlight";

export interface HighlightedSnippetProps {
  text: string;
  lang?: string | null;
  path?: string;
  className?: string;
  /** Fallback language when the fence info string is empty. */
  fallbackLang?: string | null;
}

function Line({ segs }: { segs: PaintedSegment[] }) {
  if (segs.length === 1 && !segs[0].cls) return <>{segs[0].text}</>;
  return (
    <>
      {segs.map((s, i) =>
        s.cls ? (
          <span key={i} className={s.cls} data-kbc-hl>
            {s.text}
          </span>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

export default function HighlightedSnippet({
  text,
  lang,
  path,
  className,
  fallbackLang,
}: HighlightedSnippetProps) {
  const resolved = lang || fallbackLang || null;
  const items = useMemo(
    () => (text ? [{ id: "s", lang: resolved, text, path }] : []),
    [resolved, text, path],
  );
  const { byId } = useHighlight(items);
  const result = byId.get("s");
  const painted = useMemo(() => {
    if (!result || result.tier === "none" || result.spans.length === 0) {
      return splitContentLines(text).map((t) => [{ text: t } as PaintedSegment]);
    }
    return paintSpans(text, result.spans);
  }, [result, text]);
  const none = result?.tier === "none";
  return (
    <pre
      className={className}
      data-kbc-hl-snippet
      data-kbc-hl-tier={result?.tier ?? "pending"}
      data-kbc-hl-lang={result?.lang ?? resolved ?? undefined}
    >
      <code>
        {painted.map((segs, i) => (
          <span key={i}>
            <Line segs={segs} />
            {i < painted.length - 1 ? "\n" : null}
          </span>
        ))}
      </code>
      {none && (
        <span className="kbc-hl-no-grammar" data-kbc-hl-no-grammar>
          {result?.honesty.reason ?? "no grammar"}
        </span>
      )}
    </pre>
  );
}
