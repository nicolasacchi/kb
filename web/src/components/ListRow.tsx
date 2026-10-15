import { memo, type CSSProperties } from "react";
import { Link } from "react-router-dom";
import type { DocSummary } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import { artifactDownloadUrl, triggerDownload } from "../lib/download";
import { Icon } from "@kb/ui/icons";
import { relativeAge } from "../lib/derive";
import { defaultColumns, gridTemplate, type ListColumnId } from "../lib/listColumns";
import CapabilityGlyphs, { glyphsFor } from "./CapabilityGlyphs";

// Single compact row in the list view (the Index). The visible columns come
// from lib/listColumns.ts (OL L5); the row is a fixed 32px so the virtualizer's
// estimate holds. Numeric columns use tabular numbers and right-align.
//
// P1 — memo'd: the `doc` ref is stable across no-op SSE refetches (react-query
// structural sharing), so visible rows skip re-render on reindex bursts. Only
// `doc`/`kb` props (no progress/session maps reach this row), so default shallow
// compare is correct.
const DEFAULT_COLUMNS = defaultColumns();

function ListRow({
  doc,
  kb,
  columns = DEFAULT_COLUMNS,
  refs = 0,
}: {
  doc: DocSummary;
  kb: string;
  columns?: readonly ListColumnId[];
  /// Code-ref count for the Refs column (0 renders as an empty cell).
  refs?: number;
}) {
  const has = (id: ListColumnId) => columns.includes(id);
  // Prefer the server-supplied relative folder (v0.8 G1); fall back to
  // the legacy path-parent derivation for older daemon rows.
  const folder = doc.folder ?? parentDir(doc.path);
  return (
    // The row is a grid <div> with a stretched overlay <Link> as its click
    // target, so the download <button> is a sibling of the link rather than
    // nested inside it (a <button> inside an <a> is invalid HTML + breaks
    // keyboard/SR behaviour). The absolute link doesn't occupy a grid track;
    // the button is raised above it via z-index (see app.css).
    <div
      className="list-row"
      style={{ "--list-cols": gridTemplate(columns) } as CSSProperties}
    >
      <Link
        to={artifactHref(kb, doc.source_relative)}
        className="list-row__link"
        aria-label={`Open ${doc.title || doc.id}`}
      />
      <span className="list-row__title">{doc.title || "(untitled)"}</span>
      {has("folder") && (
        <span className="list-row__folder" title={folder}>
          {folder}
        </span>
      )}
      {has("category") && (
        <span className="list-row__cat">
          {doc.kb_category ?? <span className="list-row__cat--empty">—</span>}
        </span>
      )}
      {has("words") && (
        <span className="list-row__num list-row__words">
          {doc.word_count ? doc.word_count.toLocaleString() : ""}
        </span>
      )}
      {has("links") && (
        <span className="list-row__num list-row__links">
          {doc.backlinks ? doc.backlinks : ""}
        </span>
      )}
      {has("refs") && (
        <span className="list-row__num list-row__refs">{refs > 0 ? refs : ""}</span>
      )}
      {has("kept") && (
        <span className="list-row__num list-row__kept">
          {doc.session_residue ? doc.session_residue : ""}
        </span>
      )}
      {has("caps") && (
        <span className="list-row__caps">
          {glyphsFor(doc).length > 0 ? <CapabilityGlyphs doc={doc} /> : null}
        </span>
      )}
      {has("read") && (
        <span className="list-row__num list-row__read">
          {readLabel(doc)}
        </span>
      )}
      {has("age") && (
        <span className="list-row__num list-row__age">
          {relativeAge(doc.mtime_unix ?? doc.indexed_at_unix)}
        </span>
      )}
      <button
        type="button"
        className="list-row__download"
        aria-label={`Download ${doc.title || doc.id}`}
        title="download (raw source)"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          triggerDownload(artifactDownloadUrl(kb, doc.id));
        }}
      >
        <Icon.Download />
      </button>
    </div>
  );
}

function readLabel(doc: DocSummary): string {
  if (doc.read_state === "read") return "100%";
  if (typeof doc.read_pct === "number" && doc.read_pct > 0) {
    return `${Math.round(doc.read_pct)}%`;
  }
  return "";
}

function parentDir(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  if (i < 0) return "";
  const dir = p.slice(0, i);
  // Show only the leaf folder for compactness.
  const j = Math.max(dir.lastIndexOf("/"), dir.lastIndexOf("\\"));
  return j >= 0 ? dir.slice(j + 1) : dir;
}

export default memo(ListRow);
