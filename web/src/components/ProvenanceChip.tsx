import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchDoc, type RecallHit } from "../api/client";
import { docIdQueryKey } from "../api/artifactLookup";
import { artifactHref } from "../lib/artifactHref";

/// CT-A1 (U3 parse-back) — "Lifted from <artifact> §<section>" sub-line on a
/// highlight-born memory row (`author`/`source_kb`/`source_artifact`/
/// `source_anchor` — the `MemoryProvenance` metas parsed back). Renders
/// nothing when the memory isn't highlight-born, or while/if the origin
/// doc lookup hasn't resolved — this is a BEST-EFFORT deep link, never a
/// blocking placeholder.
///
/// Resolving (source_kb, source_artifact) → title/source_relative reuses the
/// existing id-lookup lane (`fetchDoc` + `docIdQueryKey`, invariant #20's
/// Linkflow infra) rather than inventing a second one; the query is cached
/// under the SSE-invalidated `["doc", kb]` prefix (invariant #23,
/// staleTime: Infinity).
///
/// HARD FENCE: no anchor re-resolution/staleness machinery. `source_anchor`
/// is used ONLY to read a `section` anchor's `id` for the existing `?sec=`
/// deep-link param — any other anchor shape (or none at all) links the
/// artifact plainly.
export default function ProvenanceChip({ h }: { h: RecallHit }) {
  const sourceKb = h.source_kb;
  const sourceArtifact = h.source_artifact;
  const enabled = !!sourceKb && !!sourceArtifact;
  const origin = useQuery({
    queryKey: enabled
      ? docIdQueryKey(sourceKb as string, sourceArtifact as string)
      : docIdQueryKey(null, ""),
    queryFn: ({ signal }) =>
      fetchDoc(sourceKb as string, sourceArtifact as string, signal),
    enabled,
    staleTime: Infinity,
  });

  if (!enabled || !origin.data) return null;

  const sec = h.source_anchor?.kind === "section" ? h.source_anchor.id : undefined;
  const href = artifactHref(
    sourceKb as string,
    origin.data.source_relative,
    sec ? { sec } : undefined,
  );
  const label = origin.data.title || (sourceArtifact as string);

  return (
    <Link className="kb-mem__provchip" to={href} title={`lifted from ${label}`}>
      Lifted from <code>{label}</code>
      {sec && <> §{sec}</>}
    </Link>
  );
}
