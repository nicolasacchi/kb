import { useMemo } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useAnchors } from "../hooks/useAnchors";
import { useStaleAnchors, type StaleAnchor } from "../hooks/useStaleAnchors";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { artifactHref } from "../lib/artifactHref";
import { relativeAge } from "../lib/time";
import type { CorkboardEntry } from "../api/client";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";

// v0.10 K3 — /anchors view. Replaces the dedicated /stale-anchors
// route (which now permanent-redirects here with ?filter=stale).
//
// Two tabs:
//   - All   → the corkboard (pinned artifacts the user wants to keep
//             handy, persisted via /api/anchors).
//   - Stale → the legacy stale-comment-anchors dashboard (sessions +
//             cold-load from .anchors-stale.json sidecars).
//
// Filter selection lives in `?filter=all|stale` so the link is shareable
// and the back button hops cleanly between tabs.
type Filter = "all" | "stale";

export default function AnchorsRoute() {
  const [params, setParams] = useSearchParams();
  const filter: Filter = params.get("filter") === "stale" ? "stale" : "all";
  useDocumentTitle(filter === "stale" ? "Stale anchors" : "Anchors");

  return (
    <div className="anchors-view">
      <header className="anchors-view__head">
        <h1 className="anchors-view__title">Anchors</h1>
        <nav className="anchors-view__tabs" role="tablist" aria-label="anchor filter">
          <button
            type="button"
            role="tab"
            aria-selected={filter === "all"}
            className={filter === "all" ? "is-on" : ""}
            onClick={() => {
              const next = new URLSearchParams(params);
              next.delete("filter");
              setParams(next, { replace: true });
            }}
          >
            All
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={filter === "stale"}
            className={`anchors-view__tab-stale ${filter === "stale" ? "is-on" : ""}`}
            onClick={() => {
              const next = new URLSearchParams(params);
              next.set("filter", "stale");
              setParams(next, { replace: true });
            }}
          >
            Stale
          </button>
        </nav>
      </header>

      {filter === "all" ? <AllPanel /> : <StalePanel />}
    </div>
  );
}

// --- All panel — the corkboard ---------------------------------------------

function AllPanel() {
  const { anchors, loading, error, unpin } = useAnchors();
  if (loading) return <div className="anchors-view__empty">loading…</div>;
  if (error) return <div className="anchors-view__empty">recall failed: {error}</div>;
  if (anchors.length === 0) {
    return (
      <EmptyState
        icon={<Icon.Anchor />}
        title="no anchored artifacts yet"
        hint="Click ⚓ on a card or in a preview's inspector to pin an artifact here for fast return."
      />
    );
  }
  return (
    <ul className="anchors-view__list">
      {anchors.map((a) => (
        <AnchorRow key={`${a.kb}:${a.artifact_id}`} entry={a} onUnpin={() => unpin(a.kb, a.artifact_id)} />
      ))}
    </ul>
  );
}

function AnchorRow({
  entry,
  onUnpin,
}: {
  entry: CorkboardEntry;
  onUnpin: () => void;
}) {
  const orphan = !entry.source_relative;
  const inner = (
    <>
      <span className="anchors-view__kb">{entry.kb}</span>
      <span className="anchors-view__sep">·</span>
      <span className="anchors-view__title-text">
        {entry.title ?? entry.artifact_id}
      </span>
      {entry.folder !== undefined && entry.folder.length > 0 && (
        <span className="anchors-view__folder">{entry.folder}</span>
      )}
      <span className="anchors-view__age">{relativeAge(entry.created_at)}</span>
    </>
  );
  return (
    <li className={`anchors-view__row ${orphan ? "is-orphan" : ""}`}>
      {orphan ? (
        // Artifact left lance — the SPA can't deep-link; show the row
        // greyed out as a tombstone so the user can clean it up.
        <span className="anchors-view__row-link">{inner}</span>
      ) : (
        <Link
          to={artifactHref(entry.kb, entry.source_relative!)}
          className="anchors-view__row-link"
        >
          {inner}
        </Link>
      )}
      <button
        type="button"
        className="anchors-view__row-x"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          onUnpin();
        }}
        title="unpin"
        aria-label={`unpin ${entry.title ?? entry.artifact_id}`}
      >
        <Icon.X />
      </button>
    </li>
  );
}

// --- Stale panel — legacy stale-comment-anchors dashboard ------------------

function StalePanel() {
  const stale = useStaleAnchors();
  const grouped = useMemo(() => groupByArtifact(stale), [stale]);
  if (stale.length === 0) {
    return (
      <EmptyState
        title="no stale anchors this session"
        hint="This populates live as the indexer emits comment.anchor_stale events — usually right after a reindex moved or renamed a commented section."
      />
    );
  }
  return (
    <div className="anchors-view__stale">
      <p className="anchors-view__stale-count">
        {stale.length} stale comment anchor{stale.length === 1 ? "" : "s"} across{" "}
        {grouped.length} artifact{grouped.length === 1 ? "" : "s"}
      </p>
      <ol className="anchors-view__stale-list">
        {grouped.map(({ kb, artifactId, sourceRelative, anchors }) => {
          const inner = (
            <>
              <span className="anchors-view__kb">{kb}</span>
              <span className="anchors-view__sep">·</span>
              <span className="anchors-view__title-text">
                {sourceRelative ?? artifactId}
              </span>
              <span className="anchors-view__badge anchors-view__badge--warn">
                {anchors.length} stale
              </span>
            </>
          );
          return (
            <li key={`${kb}:${artifactId}`} className="anchors-view__row">
              {sourceRelative ? (
                <Link
                  to={artifactHref(kb, sourceRelative)}
                  className="anchors-view__row-link"
                >
                  {inner}
                </Link>
              ) : (
                <span className="anchors-view__row-link is-orphan">{inner}</span>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

type StaleGroup = {
  kb: string;
  artifactId: string;
  sourceRelative?: string;
  anchors: StaleAnchor[];
};

function groupByArtifact(stale: StaleAnchor[]): StaleGroup[] {
  const map = new Map<string, StaleGroup>();
  for (const a of stale) {
    const key = `${a.kb}:${a.artifactId}`;
    const cur = map.get(key);
    if (cur) {
      cur.anchors.push(a);
      if (!cur.sourceRelative && a.sourceRelative)
        cur.sourceRelative = a.sourceRelative;
    } else
      map.set(key, {
        kb: a.kb,
        artifactId: a.artifactId,
        sourceRelative: a.sourceRelative,
        anchors: [a],
      });
  }
  return Array.from(map.values());
}
