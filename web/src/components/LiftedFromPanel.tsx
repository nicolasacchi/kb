import { Link } from "react-router-dom";
import { type MemoryFromRow } from "../api/client";
import { useMemoriesFrom } from "../hooks/useMemoriesFrom";

/// CT-A1 (U3 parse-back) — "Memories highlighted from here" inspector
/// section: the EXACT reverse of `MemoryProvenance` (every memory that names
/// THIS artifact as its `kb-source-artifact`), rendered ABOVE
/// `RelatedMemoriesPanel`'s similarity-based list — an exact provenance
/// link, not a recall guess. Renders nothing while loading, on error, or
/// when the row set is empty (this is a bonus fact, not a load-bearing
/// panel that needs its own empty state).
///
/// The response deliberately carries no `source_relative` for the memory
/// itself (kb_core::routes::memory::MemoryFromRow — id/kb/title/summary/
/// author/anchor/created only), so each row links to `/memory?kb=<kb>` (the
/// same honest "open the memory view scoped to that corpus" link
/// `RelatedMemoriesPanel`'s own empty state already uses) rather than
/// guessing a per-memory permalink this endpoint doesn't resolve.
export default function LiftedFromPanel({
  kb,
  artifactId,
}: {
  kb: string;
  artifactId: string;
}) {
  const { rows, loading, error } = useMemoriesFrom(kb, artifactId);
  if (loading || error || rows.length === 0) return null;

  return (
    <>
      <h4>Memories highlighted from here</h4>
      <div className="kb-pinsp__memlist">
        {rows.map((r) => (
          <LiftedFromRow key={`${r.kb}:${r.id}`} r={r} />
        ))}
      </div>
    </>
  );
}

function LiftedFromRow({ r }: { r: MemoryFromRow }) {
  return (
    <div className="kb-pinsp__memrow">
      <Link
        className="kb-pinsp__memtitle"
        to={`/memory?kb=${encodeURIComponent(r.kb)}`}
        title={r.title}
      >
        {r.title}
      </Link>
      {r.summary && (
        <span className="kb-pinsp__memsummary" title={r.summary}>
          {r.summary}
        </span>
      )}
      <div className="kb-pinsp__memmeta">
        <span className="kb-pinsp__memkb" title={`home kb: ${r.kb}`}>
          {r.kb}
        </span>
        {r.author && <span title="highlighted by">{r.author}</span>}
        {r.anchor?.kind === "section" && <span title="section anchor">§{r.anchor.id}</span>}
      </div>
    </div>
  );
}
