import { Link } from "react-router-dom";
import type { DocSummary } from "../../api/client";
import { galleryUrl } from "../../lib/galleryUrl";
import { toast } from "../../lib/toast";
import FolderCrumbLinks from "../FolderCrumbLinks";

// OL R2 — the rail's "what is this" block: two lines (folder · category ·
// mtime, then the id with copy) and a Details disclosure holding the rest
// (file, created, indexed, metrics). Folder/category/mtime stay gallery
// deep-links built via `galleryUrl` (#35); the id copies to the clipboard.

// ±7 days, in seconds, centred on the doc's mtime — the window the mtime link
// opens in the gallery.
const MTIME_WINDOW_SECS = 7 * 24 * 60 * 60;

export function formatUnix(unix: number): string {
  const d = new Date(unix * 1000);
  return d.toISOString().slice(0, 16).replace("T", " ");
}

export function shortFilename(doc: Pick<DocSummary, "source_relative">): string {
  const fn = doc.source_relative.split("/").pop() ?? doc.source_relative;
  if (fn.length <= 28) return fn;
  return fn.slice(0, 25) + "…";
}

export function IdCopy({ id }: { id: string }) {
  return (
    <button
      type="button"
      className="kb-pinsp__id-copy"
      title="copy full artifact id"
      aria-label="copy full artifact id"
      onClick={() => {
        navigator.clipboard
          ?.writeText(id)
          .then(() => toast.ok("id copied"))
          .catch(() => toast.err("couldn't copy id"));
      }}
    >
      {id.slice(0, 12)}
    </button>
  );
}

function KV({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="kb-pinsp__kv">
      <span>{label}</span>
      <b>{children}</b>
    </div>
  );
}

const has = (n: number | null | undefined): n is number =>
  n !== null && n !== undefined;

export default function Passport({ kb, doc }: { kb: string; doc: DocSummary }) {
  return (
    <div className="kb-pinsp__passport" data-testid="rail-passport">
      <div className="kb-pinsp__passport-line">
        <FolderCrumbLinks
          kb={kb}
          folder={doc.folder}
          linkClass="kb-pinsp__kv-link"
        />
        {doc.kb_category && (
          <>
            <span className="kb-pinsp__sep">·</span>
            <Link
              className="kb-pinsp__kv-link"
              to={galleryUrl(kb, { category: doc.kb_category })}
              title={`all ${doc.kb_category} artifacts`}
            >
              {doc.kb_category}
            </Link>
          </>
        )}
        {doc.mtime_unix && (
          <>
            <span className="kb-pinsp__sep">·</span>
            <Link
              className="kb-pinsp__kv-link"
              to={galleryUrl(kb, {
                from: doc.mtime_unix - MTIME_WINDOW_SECS,
                to: doc.mtime_unix + MTIME_WINDOW_SECS,
                sort: "recent",
                dir: "desc",
              })}
              title="files modified around this time (±7d)"
            >
              {formatUnix(doc.mtime_unix)}
            </Link>
          </>
        )}
      </div>
      <div className="kb-pinsp__passport-line">
        <span className="kb-pinsp__passport-lbl">id</span>
        <IdCopy id={doc.id} />
      </div>
      <details className="kb-pinsp__details">
        <summary>Details</summary>
        <KV label="file">{shortFilename(doc)}</KV>
        {doc.created_unix && (
          <KV label="created">{formatUnix(doc.created_unix)}</KV>
        )}
        {doc.indexed_at_unix && (
          <KV label="indexed">{formatUnix(doc.indexed_at_unix)}</KV>
        )}
        {has(doc.word_count) && (
          <KV label="words">{doc.word_count.toLocaleString()}</KV>
        )}
        {has(doc.svg_count) && doc.svg_count > 0 && (
          <KV label="svgs">{doc.svg_count}</KV>
        )}
        {has(doc.table_count) && doc.table_count > 0 && (
          <KV label="tables">{doc.table_count}</KV>
        )}
        {has(doc.code_block_count) && doc.code_block_count > 0 && (
          <KV label="code blocks">{doc.code_block_count}</KV>
        )}
      </details>
    </div>
  );
}
