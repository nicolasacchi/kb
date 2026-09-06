// DCB W1.D — same-origin `coderef/1` fetch. Fits the SSE bridge exactly
// like `["edges", kb]` (useEdges.ts): NOT a #23 exception — the docsGate
// invalidates `["codeRefs", kb]` on artifact.indexed/removed (there is no
// dedicated code_refs.* event, amendment 2 — record_code_refs never bumps
// the generation).

import { useQuery } from "@tanstack/react-query";
import { fetchCodeRefs, fetchCodeRefsFeedPage } from "../api/client";
import { collectCodeRefCounts } from "../lib/codeRefCounts";

export function useCodeRefs(
  kb: string | null,
  docId: string | null,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["codeRefs", kb ?? "", docId ?? ""],
    enabled: enabled && !!kb && !!docId,
    queryFn: ({ signal }) => fetchCodeRefs(kb!, docId!, signal),
    staleTime: Infinity,
  });
}

/// CT-E3 tier (a) — the per-kb doc_id → ref_count join behind the gallery's
/// "N refs" card chip. One hard-capped headers-only walk of the coderef/1
/// feed per kb (lib/codeRefCounts.ts owns the walk + the all-or-nothing
/// honesty gate); TanStack dedupes the observers, so N mounted cards share
/// the one fetch. The `"::feed"` sub-key can never collide with a per-doc
/// key above (artifact ids are 12-hex; `is_safe_id` rejects `:`), so this
/// deliberately rides the SAME `["codeRefs", kb]` prefix the docsGate
/// already invalidates on artifact.indexed/removed — like `useCodeRefs`,
/// NOT a #23 exception.
export function useCodeRefCounts(kb: string | null) {
  return useQuery({
    queryKey: ["codeRefs", kb ?? "", "::feed"],
    enabled: !!kb,
    queryFn: ({ signal }) =>
      collectCodeRefCounts((cursor) => fetchCodeRefsFeedPage(kb!, cursor, signal)),
    staleTime: Infinity,
  });
}
