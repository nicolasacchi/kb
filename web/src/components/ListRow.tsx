import { memo } from "react";
import { Link } from "react-router-dom";
import type { DocSummary } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import { artifactDownloadUrl, triggerDownload } from "../lib/download";
import { Icon } from "./icons";

// Single compact row in the list view. Columns: id-chip · title · folder
// · file · category · open-affordance. Designed for ~28px row height
// (matches --row-h token from tokens.css).
//
// P1 — memo'd: the `doc` ref is stable across no-op SSE refetches (react-query
// structural sharing), so visible rows skip re-render on reindex bursts. Only
// `doc`/`kb` props (no progress/session maps reach this row), so default shallow
// compare is correct.
function ListRow({
  doc,
  kb,
}: {
  doc: DocSummary;
  kb: string;
}) {
  // Prefer the server-supplied relative folder (v0.8 G1); fall back to
  // the legacy path-parent derivation for older daemon rows.
  const folder = doc.folder ?? parentDir(doc.path);
  const file = basename(doc.path);
  return (
    // The row is a grid <div> with a stretched overlay <Link> as its click
    // target, so the download <button> is a sibling of the link rather than
    // nested inside it (a <button> inside an <a> is invalid HTML + breaks
    // keyboard/SR behaviour). The absolute link doesn't occupy a grid track;
    // the button is raised above it via z-index (see app.css).
    <div className="list-row">
      <Link
        to={artifactHref(kb, doc.source_relative)}
        className="list-row__link"
        aria-label={`Open ${doc.title || doc.id}`}
      />
      <span className="list-row__id" title={doc.id}>
        {doc.id.slice(0, 10)}
      </span>
      <span className="list-row__title">{doc.title || "(untitled)"}</span>
      <span className="list-row__folder" title={folder}>
        {folder}
      </span>
      <span className="list-row__file" title={file}>
        {file}
      </span>
      <span className="list-row__cat">
        {doc.kb_category ?? <span className="list-row__cat--empty">—</span>}
      </span>
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
      <span className="list-row__open" aria-hidden="true">
        ↗
      </span>
    </div>
  );
}

function basename(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) : p;
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
