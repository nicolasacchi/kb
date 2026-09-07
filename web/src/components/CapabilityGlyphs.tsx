import type { DocSummary } from "../api/client";
import { Icon } from "./icons";

import type { JSX } from "react";

type Glyph = { I: (p: { className?: string }) => JSX.Element; n: number | null };

// Derive the (up to 4) capability glyphs to render for an artifact.
// Each glyph is a kind (svg / code / table / interactive / longread)
// and an optional count (rendered as a small badge when > 1).
//
// The backend exposes raw counts (svg_count, table_count,
// code_block_count) and boolean indicators (has_canvas, has_form,
// has_animation, has_details). "Interactive" rolls these together;
// "longread" derives from word_count via the parser's longread flag.
export function glyphsFor(doc: DocSummary): Glyph[] {
  const items: Glyph[] = [];
  if (doc.svg_count && doc.svg_count > 0) {
    items.push({ I: Icon.Spark, n: doc.svg_count });
  }
  if (doc.code_block_count && doc.code_block_count > 0) {
    items.push({ I: Icon.Code, n: doc.code_block_count });
  }
  if (doc.table_count && doc.table_count > 0) {
    items.push({ I: Icon.Table, n: doc.table_count });
  }
  const interactive =
    !!doc.has_canvas ||
    !!doc.has_form ||
    !!doc.has_animation ||
    !!doc.has_details ||
    !!doc.has_drag;
  if (interactive) {
    items.push({ I: Icon.Spark, n: null });
  }
  if (doc.longread) {
    items.push({ I: Icon.BookOpen, n: null });
  }
  return items.slice(0, 4);
}

// Slim glyph strip — renders up to 4 capability badges with optional
// count chips. Used inside the hybrid card's prev-hybrid block.
export default function CapabilityGlyphs({ doc }: { doc: DocSummary }) {
  const glyphs = glyphsFor(doc);
  if (glyphs.length === 0) return null;
  return (
    <div className="prev-hyb-glyphs">
      {glyphs.map((g, i) => (
        <span key={i} className="prev-hyb-glyph">
          <g.I />
          {g.n && g.n > 1 && <span className="prev-hyb-n">{g.n}</span>}
        </span>
      ))}
    </div>
  );
}
