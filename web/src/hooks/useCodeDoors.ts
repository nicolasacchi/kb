// OL IDEA6 - the doors for one document, composed from queries the Links tab
// already runs (same keys: ["codeRefs",...], ["doclensScorecard",...],
// ["doclens",...]). No new request shape, no refresh of its own - a second
// observer of the doclens exception in queryClient.ts (#23). With no valid
// `code_url` for the kb nothing is enabled, so no request is issued.

import { useMemo } from "react";
import { useCodeRefs } from "./useCodeRefs";
import { useCodeUrlForKb } from "./useCodeUrlForKb";
import { useDocLens, useDocLensScorecard } from "./useDocLens";
import {
  buildCodeDoors,
  doorRepo,
  httpCodeUrl,
  type CodeDoor,
} from "../lib/codeDoors";

export function useCodeDoors(args: {
  kb: string | undefined;
  docId: string | null | undefined;
  focused: boolean;
  repoParam: string | null | undefined;
  enabled: boolean;
}): CodeDoor[] {
  const { kb, docId, focused, repoParam, enabled } = args;
  const codeUrl = httpCodeUrl(useCodeUrlForKb(kb));
  const active = enabled && !!codeUrl && !!kb && !!docId;
  const refsQuery = useCodeRefs(kb ?? null, docId ?? null, active);
  const refs = refsQuery.data?.refs;
  const hasRefs = active && !!refs && refs.length > 0;
  const scorecard = useDocLensScorecard(codeUrl, kb ?? "", docId ?? "", hasRefs);
  const repo = doorRepo({
    focused,
    repoParam,
    pinnedRepo: scorecard.data?.pinned_repo,
  });
  const lensQuery = useDocLens(codeUrl, kb ?? "", docId ?? "", hasRefs ? repo : null);
  const lens = lensQuery.data;
  const lensError =
    repo && lensQuery.isError
      ? lensQuery.error
      : scorecard.isError
        ? scorecard.error
        : null;
  const lensLoading = repo ? lensQuery.isLoading : scorecard.isLoading;
  return useMemo(
    () =>
      hasRefs && refs
        ? buildCodeDoors(refs, { codeUrl, repo, lens, lensError, lensLoading })
        : [],
    [hasRefs, refs, codeUrl, repo, lens, lensError, lensLoading],
  );
}
