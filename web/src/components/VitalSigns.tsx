import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { fetchDocsPage, fetchKbStats, fetchZeroHitQueries } from "../api/client";
import { galleryUrl } from "../lib/galleryUrl";
import { useInbox } from "../hooks/useInbox";

/*
 * W1.pulse — the vital-signs strip (standing rule 3: evidence over
 * appetite). One quiet, clickable mono-font line of daemon-truth numbers —
 * what's here, what's been left unopened, what's still open, what's
 * failed to answer — each number a link to where you'd go to act on it.
 * No invented metrics: median-time-to-first-read is not on any endpoint
 * (see /tmp/w1-recon-server.md §8), so it isn't here.
 *
 * This is background data, not a dashboard demanding attention: a number
 * that fails to load renders "–" silently (no toast, no retry banner) —
 * see the calm-computing contract (pull-only, density-not-goals).
 */

const ZERO_HIT_LIMIT = 100;

function fmt(n: number | undefined): string {
  return n == null ? "–" : String(n);
}

export type VitalSignsProps = { kb: string | null };

export default function VitalSigns({ kb }: VitalSignsProps) {
  // staleTime: 0 (not the queryClient default of Infinity) — this is
  // background data with no SSE bridge wiring, so a fresh mount is the
  // only refresh signal; cheap enough (one small GET each) to just refetch.
  const stats = useQuery({
    queryKey: ["stats", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchKbStats(kb as string, signal),
    staleTime: 0,
  });
  const neverOpened = useQuery({
    queryKey: ["docs", kb, "vitals-never-opened"] as const,
    enabled: !!kb,
    queryFn: ({ signal }) =>
      fetchDocsPage(kb as string, {
        read: ["never-opened"],
        limit: 1,
        offset: 0,
        signal,
      }),
    staleTime: 0,
  });
  const inbox = useInbox();
  const zeroHit = useQuery({
    queryKey: ["zeroHitQueries", kb, ZERO_HIT_LIMIT] as const,
    enabled: !!kb,
    queryFn: ({ signal }) =>
      fetchZeroHitQueries(kb as string, ZERO_HIT_LIMIT, signal),
    staleTime: 0,
  });

  if (!kb) return null;

  const docCount = stats.data?.doc_count;
  const neverOpenedTotal = neverOpened.data?.total;
  // useInbox's totalOpen defaults to 0 while loading (shared-cache
  // contract) — that's fine, a transient 0-then-real-count isn't a
  // "failed to load"; only a genuine fetch error renders "–".
  const openThreads = inbox.error ? undefined : inbox.totalOpen;
  const zeroHitCount = zeroHit.data?.length;

  return (
    <div className="kb-vitals" role="status" data-testid="vitals-strip">
      <Link
        to={galleryUrl(kb)}
        className="kb-vitals__n"
        title="Artifacts indexed in this kb"
      >
        {fmt(docCount)} artifacts
      </Link>
      {" · "}
      <Link
        to={galleryUrl(kb, { read: ["never-opened"] })}
        className="kb-vitals__n"
        title="Artifacts in this kb never opened in the reader"
      >
        {fmt(neverOpenedTotal)} never opened
      </Link>
      {" · "}
      <Link
        to="/inbox"
        className="kb-vitals__n"
        title="Open comment threads across every kb on this daemon"
      >
        {fmt(openThreads)} open threads
      </Link>
      {" · "}
      <Link
        to="/settings#census"
        className="kb-vitals__n"
        title="Queries in this kb that returned zero hits since the daemon started (resets on restart) — see the census panel"
      >
        {fmt(zeroHitCount)} zero-hit queries
      </Link>{" "}
      <span className="kb-vitals__hint">(since daemon start)</span>
    </div>
  );
}
