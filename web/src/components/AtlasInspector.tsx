import { useMemo } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  fetchSimilar,
  type SimilarOut,
  type DocSummary,
} from "../api/client";
import { useAnchors } from "../hooks/useAnchors";
import { useInspectorCollapsed } from "../hooks/useInspectorCollapsed";
import { artifactHref } from "../lib/artifactHref";
import { layoutStress, type StressPoint } from "../lib/atlasSelection";
import { relativeAge, tagColor, tagsFor } from "../lib/derive";
import CapabilityGlyphs, { glyphsFor } from "./CapabilityGlyphs";
import TagPill from "./TagPill";
import { Icon } from "./icons";

// v0.11 A1 — Atlas inspector rail.
//
// 320px right column the AtlasView shows when a node is selected.
// Mirrors PreviewInspector's shape but compressed for the constellation
// context: title + excerpt → words/backlinks/age/caps stat block →
// tag chips → true-neighbors rail (W2.3b) → sessions touching this dot →
// action row (preview / open / ⚓).
//
// W2.3b ships the "Nearest neighbors" section the v0.11 A1 comment above
// deferred ("neighbors land in a follow-up once cosine-similarity is in
// the edge payload") — see `TrueNeighborsSection` below; it degrades to
// the same honest "not available yet" copy when the endpoint is absent or
// this kb has no embeddings, so an un-shipped/un-recomputed daemon never
// looks broken.
/// U2 — one entry per session whose touches set includes the
/// currently-selected dot. The atlas view derives this from its
/// already-loaded `sessionPolylines` so the inspector doesn't pay
/// for an extra round-trip.
export type InspectorSessionTouch = {
  sessionId: string;
  color: string;
  preview?: string;
};

// W2.3b — how many true (embedding-space) neighbors to ask for / render,
// and the k used for the layout-stress comparison (of THIS many true
// neighbors, how many land outside the same count's 2-D nearest points).
const NEIGHBOR_LIMIT = 8;

/// W3.F-c — this dot's row in the operator-vs-machine comparison, as the
/// SERVER computed and ordered it (distance desc, id asc). `null` when the
/// onion skin is off, or when this artifact was never hand-placed — the
/// section then says so rather than showing a zero that would read as
/// "we agree perfectly".
export type InspectorFieldRow = {
  distance: number;
  rank: number;
  total: number;
};

export default function AtlasInspector({
  kb,
  doc,
  onClear,
  touchedBySessions,
  atlasPoints,
  onSelectNeighbor,
  fieldRow,
}: {
  kb: string;
  doc: DocSummary | null;
  onClear: () => void;
  touchedBySessions?: InspectorSessionTouch[];
  /// W2.3b — every dot's 2-D logical coords, for the layout-stress badge's
  /// client-side 2-D KNN. Passed down rather than re-derived here so the
  /// single source of truth stays AtlasView's `points` memo.
  atlasPoints: StressPoint[];
  /// W2.3b — clicking a neighbor row re-centers + selects that dot
  /// (AtlasView's `selectAndCenter`, `panToCluster`-shaped).
  onSelectNeighbor: (id: string) => void;
  /// W3.F-c — the operator-field displacement for THIS dot, or null.
  fieldRow?: InspectorFieldRow | null;
}) {
  const { isAnchored, toggle } = useAnchors();
  const { collapsed, toggle: toggleCollapsed } = useInspectorCollapsed("atlas");

  if (collapsed) {
    return (
      <aside className="kb-atlas-insp kb-atlas-insp--collapsed" aria-label="atlas inspector (collapsed)">
        <button
          type="button"
          className="kb-atlas-insp__expand"
          onClick={toggleCollapsed}
          title="expand inspector"
          aria-label="expand inspector"
          aria-expanded="false"
        >
          ‹
        </button>
      </aside>
    );
  }

  if (!doc) {
    return (
      <aside className="kb-atlas-insp kb-atlas-insp--empty" aria-label="atlas inspector">
        <header className="kb-atlas-insp__head">
          <span className="kb-atlas-insp__lab">selected</span>
          <button
            type="button"
            className="kb-atlas-insp__close"
            onClick={toggleCollapsed}
            title="collapse inspector"
            aria-label="collapse inspector"
            aria-expanded="true"
          >
            ›
          </button>
        </header>
        <div className="kb-atlas-insp__body">
          <p className="kb-atlas-insp__hint">
            Click a dot in the constellation to inspect it. Cmd/⌘+click
            opens in a new tab.
          </p>
        </div>
      </aside>
    );
  }

  const tags = tagsFor(doc);
  const accentTag = tags[0];
  const accent = accentTag ? tagColor(accentTag) : "var(--accent)";
  const age = relativeAge(doc.indexed_at_unix ?? doc.mtime_unix);
  const glyphs = glyphsFor(doc);
  const anchored = isAnchored(kb, doc.id);

  return (
    <aside
      className="kb-atlas-insp"
      style={{ "--insp-accent": accent } as React.CSSProperties}
      aria-label="atlas inspector"
    >
      <header className="kb-atlas-insp__head">
        <span className="kb-atlas-insp__live" aria-hidden />
        <span className="kb-atlas-insp__lab">selected</span>
        <button
          type="button"
          className="kb-atlas-insp__close"
          onClick={onClear}
          title="clear selection"
          aria-label="clear selection"
        >
          <Icon.X />
        </button>
        <button
          type="button"
          className="kb-atlas-insp__close"
          onClick={toggleCollapsed}
          title="collapse inspector"
          aria-label="collapse inspector"
          aria-expanded="true"
        >
          ›
        </button>
      </header>
      <div className="kb-atlas-insp__body">
        {doc.folder && (
          <div className="kb-atlas-insp__crumb" title={doc.folder}>
            {doc.folder}
          </div>
        )}
        <h2 className="kb-atlas-insp__title">{doc.title || "(untitled)"}</h2>
        {doc.summary && (
          <p className="kb-atlas-insp__excerpt">
            {doc.summary.length > 240
              ? doc.summary.slice(0, 240) + "…"
              : doc.summary}
          </p>
        )}

        {doc.word_count != null && (
          <Stat label="words">{doc.word_count.toLocaleString()}</Stat>
        )}
        {doc.backlinks != null && (
          <Stat label="backlinks">{doc.backlinks}↵</Stat>
        )}
        {age && <Stat label="age">{age}</Stat>}
        {glyphs.length > 0 && (
          <Stat label="caps">
            <CapabilityGlyphs doc={doc} />
          </Stat>
        )}

        {tags.length > 0 && (
          <>
            <h4>Tags</h4>
            <div className="kb-atlas-insp__tags">
              {tags.map((t) => (
                <TagPill key={t} tag={t} accent={t === accentTag} />
              ))}
            </div>
          </>
        )}

        {/* W3.F-c — where YOU put this one, versus where the model did.
            The number is the daemon's (Procrustes-aligned, so rotation /
            scale / offset are already out of it); the rail only reads it.
            Rendered only while the onion skin is on, so the inspector
            doesn't grow a section for a feature that isn't in play. */}
        {fieldRow && (
          <>
            <h4>Your field</h4>
            <div className="kb-atlas-insp__stat">
              <span>displacement</span>
              <b>{fieldRow.distance.toFixed(3)}</b>
            </div>
            <p
              className="kb-atlas-insp__hint"
              title="Ranked by the daemon (largest disagreement first, ties by id) — the SPA never re-sorts or re-fits this."
            >
              #{fieldRow.rank} of {fieldRow.total} hand-placed artifacts, by
              how far your placement sits from the machine&rsquo;s — relative
              disagreement, after the two fields are fitted into one frame.
            </p>
          </>
        )}

        <TrueNeighborsSection
          kb={kb}
          docId={doc.id}
          atlasPoints={atlasPoints}
          onSelectNeighbor={onSelectNeighbor}
        />

        {touchedBySessions && touchedBySessions.length > 0 && (
          <>
            <h4>Sessions</h4>
            <ul className="kb-atlas-insp__sessions">
              {touchedBySessions.map((s) => (
                <li key={s.sessionId}>
                  <Link
                    to={`/sessions?focus=${encodeURIComponent(s.sessionId)}`}
                    className="kb-atlas-insp__session-link"
                  >
                    <span
                      className="kb-atlas-insp__session-swatch"
                      style={
                        { background: s.color } as React.CSSProperties
                      }
                      aria-hidden
                    />
                    <span className="kb-atlas-insp__session-label">
                      {s.preview ?? s.sessionId.slice(0, 12)}
                    </span>
                  </Link>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
      <div className="kb-atlas-insp__actions">
        <Link
          to={artifactHref(kb, doc.source_relative)}
          className="kb-atlas-insp__btn kb-atlas-insp__btn--primary"
          title="preview (p)"
        >
          <svg
            width="11"
            height="11"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden
          >
            <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
          preview
        </Link>
        <a
          className="kb-atlas-insp__btn"
          href={artifactHref(kb, doc.source_relative)}
          target="_blank"
          rel="noopener noreferrer"
          title="open in tab (o)"
        >
          ↗ open
        </a>
        <button
          type="button"
          className={`kb-atlas-insp__btn kb-atlas-insp__btn--anchor ${anchored ? "is-on" : ""}`}
          onClick={() => toggle(kb, doc.id)}
          title={anchored ? "unanchor (a)" : "anchor (a)"}
          aria-pressed={anchored}
        >
          <Icon.Anchor />
        </button>
      </div>
    </aside>
  );
}

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="kb-atlas-insp__stat">
      <span>{label}</span>
      <b>{children}</b>
    </div>
  );
}

// W2.3b — true (embedding-space) neighbors for the selected dot, plus the
// layout-stress badge (of these true neighbors, how many land far away in
// the 2-D projection — Distill's "the map lies" lesson made concrete per
// selection). Plain useQuery, key `["atlasSimilar", kb, id]` (documented in
// queryClient.ts, NOT bridged — see that comment for why), `staleTime:
// Infinity` inherited from the client default.
//
// Honest fallbacks: a 404/error (endpoint not live on this daemon — see
// client.ts's WIRE CAVEAT) or an empty `neighbors` array (kb has no
// embeddings yet) both render the same "not available yet" copy the v0.11
// A1 comment used to carry as a code-only note; nothing crashes or looks
// broken either way.
function TrueNeighborsSection({
  kb,
  docId,
  atlasPoints,
  onSelectNeighbor,
}: {
  kb: string;
  docId: string;
  atlasPoints: StressPoint[];
  onSelectNeighbor: (id: string) => void;
}) {
  const q = useQuery({
    queryKey: ["atlasSimilar", kb, docId] as const,
    queryFn: ({ signal }) => fetchSimilar(kb, docId, NEIGHBOR_LIMIT, signal),
    staleTime: Infinity,
  });
  const neighbors: SimilarOut[] = q.data?.neighbors ?? [];
  const stress = useMemo(
    () => layoutStress(docId, neighbors.map((n) => n.id), atlasPoints),
    [docId, neighbors, atlasPoints],
  );

  return (
    <>
      <h4>Nearest neighbors</h4>
      {q.isPending ? (
        <p className="kb-atlas-insp__hint">loading…</p>
      ) : neighbors.length === 0 ? (
        <p className="kb-atlas-insp__hint">
          {q.isError
            ? "Nearest neighbors ride the kb's embedding space; this daemon doesn't have the endpoint live yet."
            : "No embedding neighbors yet — recompute the atlas once this kb has vectors."}
        </p>
      ) : (
        <>
          <ul className="kb-atlas-insp__neighbors">
            {neighbors.map((n) => {
              const pct = Math.max(0, Math.min(1, n.cosine)) * 100;
              return (
                <li key={n.id}>
                  <button
                    type="button"
                    className="kb-atlas-insp__neighbor"
                    onClick={() => onSelectNeighbor(n.id)}
                    title={`select ${n.title || n.id}`}
                  >
                    <span className="kb-atlas-insp__neighbor-title">
                      {n.title || n.id}
                    </span>
                    <span className="kb-atlas-insp__neighbor-bar" aria-hidden>
                      <span
                        className="kb-atlas-insp__neighbor-fill"
                        style={{ width: `${pct}%` }}
                      />
                    </span>
                    <span className="kb-atlas-insp__neighbor-val">
                      {n.cosine.toFixed(3)}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
          {stress.total > 0 && (
            <p
              className="kb-atlas-insp__stress"
              title="Distill's lesson (distill.pub on t-SNE/UMAP): a 2-D projection can never preserve every high-dimensional distance — some true neighbors will always land far apart on the map. That's a property of the projection, not a defect in this one."
            >
              {stress.farCount} of {stress.total} high-D neighbors are far in 2-D
            </p>
          )}
        </>
      )}
    </>
  );
}
