// OL-SIBL2 - data for the Header's Code chip on the reader route (/a/:kb/*).
//
// Deliberately NO store, breaker or idle ladder: the chip enables its
// scorecard/lens queries exactly where the Links tab enables them
// (`hasCodeRefs && codeUrl`, the same query keys), so TanStack dedupes them
// into the Links tab's own requests. The one failed request per artifact open
// an unreachable kb-code costs is the Links tab's, not an extra. The queries
// are the #23 fourth exception (api/doclens.ts), unchanged.
//
// The target follows the pane's LIVE `kb:section` (the runtime's postMessage,
// exact-origin checked like TocSpy/HotkeyRoot), falling back to `?sec=`.
//
// Known scopes (disclosed, not hidden):
// - PANE 1 ONLY. In `?pane2=` split mode the route's `/a/:kb/*` is pane 1;
//   the listener's exact-origin check admits only that artifact, so pane 2's
//   sections never move the chip.
// - The hide rule reads the inspector's collapsed flag and sub-tab, both
//   persisted. The reader's comments/versions `panelMode` is route-local
//   state in detail.tsx and not readable here: with the rail open on a
//   Code-bearing tab but showing the comments/versions panel the chip stays
//   hidden (and its `inspect` button, when shown on a collapsed rail, opens
//   the Links tab, which the panel then covers until the user closes it).
//   Lifting panelMode into a shared store would be a second home for route
//   state (#23); accepted instead.
// - `rel` mirrors detail.tsx's `params["*"]` (react-router already decodes
//   it), so the shared ["doc", kb, rel] key matches byte for byte.

import { useEffect, useMemo, useState } from "react";
import { matchPath, useLocation, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchDocByPath } from "../api/client";
import { isOriginOfArtifact } from "../lib/artifactHost";
import { httpCodeUrl, siblingChipState, type SiblingChipState } from "../lib/siblingChip";
import { useArtifactHostSuffix } from "./useArtifactHost";
import { useCodeRefs } from "./useCodeRefs";
import { useDocLens, useDocLensScorecard } from "./useDocLens";
import { useInspectorCollapsed } from "./useInspectorCollapsed";
import { useInspectorTab } from "./useInspectorTab";
import { useIsMobile } from "./useIsMobile";
import { useKbs } from "./useKbs";

export type SiblingChipModel = SiblingChipState & {
  /// Opens the inspector on its Links tab (the `inspect` target).
  openLinks: () => void;
};

export function useSiblingChip(): SiblingChipModel | null {
  const loc = useLocation();
  const m = matchPath({ path: "/a/:kb/*" }, loc.pathname);
  const kb = m?.params.kb ?? null;
  const rel = m?.params["*"] ?? "";
  const onReader = !!kb && rel !== "";
  const [searchParams] = useSearchParams();
  const hostSuffix = useArtifactHostSuffix();
  const isMobile = useIsMobile();

  const docQuery = useQuery({
    queryKey: ["doc", kb, rel] as const,
    enabled: onReader,
    queryFn: ({ signal }) => fetchDocByPath(kb as string, rel, signal),
  });
  const docId = docQuery.data?.id ?? null;

  const { data: kbs } = useKbs();
  const codeUrl = httpCodeUrl(kbs?.find((k) => k.name === kb)?.code_url);

  const codeRefsQuery = useCodeRefs(kb, docId, onReader);
  const refs = codeRefsQuery.data?.refs ?? [];
  const hasCodeRefs = codeRefsQuery.data !== undefined && refs.length > 0;
  const urlRepo = searchParams.get("repo");
  const scorecardQuery = useDocLensScorecard(codeUrl, kb ?? "", docId ?? "", onReader && !!docId && hasCodeRefs);
  // The lens repo is `?repo=`, else the doc's pin (as ProvenanceThread does):
  // the Links tab seeds `?repo=` from the pin, so this lands on the same
  // query key, but a collapsed rail (Links tab unmounted) must still verify.
  const repoParam = urlRepo || scorecardQuery.data?.pinned_repo || null;
  const lensQuery = useDocLens(
    codeUrl,
    kb ?? "",
    docId ?? "",
    onReader && docId && hasCodeRefs ? repoParam : null,
  );

  // Live section + heading order from the artifact runtime.
  const [liveSec, setLiveSec] = useState<string | null>(null);
  const [tocIds, setTocIds] = useState<string[]>([]);
  useEffect(() => {
    setLiveSec(null);
    setTocIds([]);
    if (!onReader || !docId || !kb) return;
    function onMessage(e: MessageEvent) {
      if (!isOriginOfArtifact(e.origin, docId, kb, hostSuffix)) return;
      const data = e.data as { kind?: string; id?: string; toc?: { id: string }[] } | null;
      if (!data || typeof data !== "object") return;
      if (data.kind === "kb:section" && typeof data.id === "string") {
        setLiveSec(data.id);
      } else if (data.kind === "kb:toc" && Array.isArray(data.toc)) {
        setTocIds(data.toc.map((t) => t.id).filter((id) => typeof id === "string"));
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [onReader, docId, kb, hostSuffix]);

  // One home: while the rail is open on a tab that stacks the Code section,
  // the section is the surface and the chip stays out of the way.
  const { collapsed, setCollapsed } = useInspectorCollapsed("detail");
  const { tab, setTab } = useInspectorTab("detail");
  const railShowsCode = !collapsed && (tab === "all" || tab === "links");

  const crossErr = scorecardQuery.isError
    ? scorecardQuery.error
    : repoParam && lensQuery.isError
      ? lensQuery.error
      : null;

  const state = useMemo(
    () =>
      siblingChipState({
        codeUrl,
        neverScanned: codeRefsQuery.data?.never_scanned === true,
        refCount: codeRefsQuery.data?.ref_count ?? refs.length,
        extractedAt: codeRefsQuery.data?.extracted_at ?? null,
        truncated: codeRefsQuery.data?.truncated === true,
        refs,
        groups: codeRefsQuery.data?.groups ?? [],
        sec: liveSec ?? searchParams.get("sec"),
        tocIds,
        repoParam,
        scorecard: scorecardQuery.data,
        lens: lensQuery.data,
        crossErr,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [codeUrl, codeRefsQuery.data, liveSec, tocIds, repoParam, searchParams, scorecardQuery.data, lensQuery.data, crossErr],
  );

  if (!onReader || isMobile || railShowsCode || state.kind === "none") return null;
  return {
    ...state,
    openLinks: () => {
      setCollapsed(false);
      setTab("links");
    },
  };
}
