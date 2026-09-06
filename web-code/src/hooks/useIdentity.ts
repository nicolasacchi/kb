import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchIdentity } from "../api/client";
import { setKbSessionBase } from "../lib/searchLanes";

/// One-shot boot fetch of `GET /api/identity`, mounted once at the app root
/// (`app.tsx`) purely for its side effect: feeding `kb_public_url`
/// (`IdentityOut`'s doc) to `lib/searchLanes.ts`'s `setKbSessionBase`, so
/// the "open session in kb" link (`WhyPanel.tsx`) points at the RIGHT kb
/// daemon for this deployment - the hardcoded local-dev default otherwise
/// silently 404s on a hosted install where kb-code's own federation `url`
/// is a container hostname (see `KbDaemonSection::public_url`'s doc,
/// crates/kb-code-server/src/config.rs). `useQuery`'s `onSuccess` was
/// dropped in TanStack Query v5, hence the `useEffect` on `data` here
/// rather than a callback option. `staleTime: Infinity` matches the
/// query-client's repo-wide default (`api/queryClient.ts`) - restated
/// explicitly because this hook's one-shot-at-boot contract depends on it:
/// the daemon's public URL is fixed for the process lifetime, so there is
/// never a reason to refetch.
export function useIdentity() {
  const query = useQuery({
    queryKey: ["identity"],
    queryFn: fetchIdentity,
    staleTime: Infinity,
  });

  useEffect(() => {
    // `kb_public_url` is optional on the wire (an older daemon omits it) —
    // missing/empty keeps `searchLanes`' local-dev default rather than
    // crashing the whole app at boot on `undefined.replace`.
    if (query.data?.kb_public_url) setKbSessionBase(query.data.kb_public_url);
  }, [query.data]);

  return query;
}
