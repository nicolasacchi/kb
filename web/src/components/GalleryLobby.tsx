import { censusBump } from "../lib/census";
import VitalSigns from "./VitalSigns";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchFacets, fetchTags, type DocSummary } from "../api/client";
import Card from "./Card";
import { Icon } from "./icons";
import { galleryUrl } from "../lib/galleryUrl";
import { tagColor } from "../lib/derive";
import type { Progress } from "../hooks/useReadingProgress";
import {
  bumpLobbySeenExpanded,
  categorySegments,
  decideLobbyCollapsed,
  pickHighlights,
  readLobbyCollapseState,
  setLobbyOverride,
  sumWords,
  topTagChips,
} from "../lib/lobby";

// W1.gallery — the corpus lobby: the generous, unfiltered home for a kb.
// Renders above the sessions-strip/resurface-strip on the empty-query,
// unfiltered grid/list (same gate the two strips already use — see
// gallery.tsx). Calm-computing contract: no goals, no streaks, no percent-
// complete anywhere in here — the segmented bar and word/artifact counts are
// a plain census, not a target.

type Props = {
  kb: string;
  /// Server-side post-filter total (envelope `total`, NOT `rows.length`) —
  /// the accurate artifact count even mid-pagination.
  total: number;
  /// The currently-loaded page(s) of docs. Word count + highlights are
  /// derived from this, not the full corpus — see the sumWords/pickHighlights
  /// doc comments for the (documented) undercounting caveat on paginated kbs.
  rows: DocSummary[];
  progress?: Map<string, Progress>;
};

export default function GalleryLobby({ kb, total, rows, progress }: Props) {
  // SH.D.3 — visit-gated auto-collapse (lib/lobby.ts's decideLobbyCollapsed):
  // computed ONCE per mount from cross-session localStorage evidence, then
  // `toggle()` below owns everything for the rest of this mount's lifetime.
  // `initialCollapsed` is captured in a ref (not re-derived) so the one-time
  // "seen expanded" impression bump below reads exactly what this mount
  // started as, even after later manual toggles change `collapsed`.
  const initialCollapsedRef = useRef<boolean | null>(null);
  if (initialCollapsedRef.current === null) {
    initialCollapsedRef.current = decideLobbyCollapsed(readLobbyCollapseState());
  }
  const [collapsed, setCollapsed] = useState(initialCollapsedRef.current);
  const seenBumpedRef = useRef(false);
  useEffect(() => {
    if (seenBumpedRef.current) return;
    seenBumpedRef.current = true;
    // Only a genuine "shown expanded on load" counts as an impression — a
    // mid-session manual toggle isn't a fresh visit, so it doesn't inflate
    // the seen-count the auto-collapse threshold reads.
    if (!initialCollapsedRef.current) bumpLobbySeenExpanded();
  }, []);
  const toggle = () => {
    setCollapsed((c) => {
      const next = !c;
      // A manual toggle always wins over the seen-count heuristic, in
      // either direction, until reversed by another manual toggle.
      setLobbyOverride(next ? "closed" : "open");
      return next;
    });
  };

  // Same cache keys Sidebar/useFacetCounts use (["facets", kb] / ["tags",
  // kb]) — react-query dedupes by key, so this doesn't add a second network
  // round trip when either has already fetched them.
  const facetsQ = useQuery({
    queryKey: ["facets", kb],
    queryFn: ({ signal }) => fetchFacets(kb, signal),
    staleTime: Infinity,
  });
  const tagsQ = useQuery({
    queryKey: ["tags", kb],
    queryFn: ({ signal }) => fetchTags(kb, signal),
    staleTime: Infinity,
  });

  const categories = facetsQ.data?.categories ?? [];
  const segments = useMemo(() => categorySegments(categories), [categories]);
  const words = useMemo(() => sumWords(rows), [rows]);
  const highlights = useMemo(() => pickHighlights(rows, 3), [rows]);
  const tagChips = useMemo(() => topTagChips(tagsQ.data ?? [], 4), [tagsQ.data]);

  return (
    <section
      className={`gallery-lobby${collapsed ? " gallery-lobby--collapsed" : ""}`}
      aria-label="corpus lobby"
    >
      <button
        type="button"
        className="gallery-lobby__census"
        onClick={toggle}
        aria-expanded={!collapsed}
        title={collapsed ? "expand the corpus lobby" : "collapse the corpus lobby"}
      >
        <Icon.Chevron
          className={`gallery-lobby__chev${collapsed ? "" : " gallery-lobby__chev--open"}`}
        />
        <span className="gallery-lobby__census-line mono">
          {total.toLocaleString()} artifact{total === 1 ? "" : "s"}
          {" · "}
          {words.toLocaleString()} word{words === 1 ? "" : "s"}
          {" · "}
          {categories.length} categor{categories.length === 1 ? "y" : "ies"}
        </span>
      </button>

      {!collapsed && (
        <div className="gallery-lobby__body">
          {/* SH.D.1 — a single category carries zero information as a
              100%-width bar (it just repaints the whole rule one color), and
              read as a broken/oversized rule in the design audit. With only
              one category, skip the bar entirely and show one compact chip;
              from two categories up, the slim stacked bar earns its place
              and the
              legend renders as an inline row of chips after it (wraps at
              the container edge — .gallery-lobby__legend already flex-wraps).
              Colors come from the same deterministic tagColor() hash the
              tag pills / atlas already use, so a category and a tag sharing
              a name always read as the same color. */}
          {segments.length === 1 && (
            <div className="gallery-lobby__cat-solo">
              <Link
                to={galleryUrl(kb, { category: segments[0].value })}
                className="gallery-lobby__legend-item gallery-lobby__legend-item--solo"
                onClick={() => censusBump("lobby.explore")}
              >
                <span
                  className="gallery-lobby__legend-dot"
                  aria-hidden
                  style={{ background: tagColor(segments[0].value) }}
                />
                {segments[0].value}
                <span className="gallery-lobby__legend-ct">
                  {segments[0].count}
                </span>
              </Link>
            </div>
          )}
          {segments.length > 1 && (
            <div className="gallery-lobby__bar-wrap">
              <div
                className="gallery-lobby__bar"
                role="img"
                aria-label={`categories by size: ${segments
                  .map((s) => `${s.value} (${s.count})`)
                  .join(", ")}`}
              >
                {segments.map((s) => (
                  <Link
                    key={s.value}
                    to={galleryUrl(kb, { category: s.value })}
                    className="gallery-lobby__seg"
                    style={{ width: `${s.pct * 100}%`, background: tagColor(s.value) }}
                    title={`${s.value} · ${s.count}`}
                    onClick={() => censusBump("lobby.explore")}
                  />
                ))}
              </div>
              <div className="gallery-lobby__legend">
                {segments.slice(0, 6).map((s) => (
                  <Link
                    key={s.value}
                    to={galleryUrl(kb, { category: s.value })}
                    className="gallery-lobby__legend-item"
                    onClick={() => censusBump("lobby.explore")}
                  >
                    <span
                      className="gallery-lobby__legend-dot"
                      aria-hidden
                      style={{ background: tagColor(s.value) }}
                    />
                    {s.value}
                    <span className="gallery-lobby__legend-ct">{s.count}</span>
                  </Link>
                ))}
              </div>
            </div>
          )}

          {highlights.length > 0 && (
            <div className="gallery-lobby__highlights grid">
              {highlights.map((d) => (
                <Card key={d.id} doc={d} kb={kb} progress={progress?.get(d.id)} />
              ))}
            </div>
          )}

          {tagChips.length > 0 && (
            <div className="gallery-lobby__examples">
              <span className="gallery-lobby__examples-label">try</span>
              {tagChips.map((t) => (
                <Link
                  key={t.name}
                  to={`/search?kb=${encodeURIComponent(kb)}&q=${encodeURIComponent(`tag:${t.name}`)}`}
                  className="gallery-lobby__example-chip"
                >
                  {t.name}
                </Link>
              ))}
            </div>
          )}
          {/* W1.9 — the same vital-signs rollup Settings shows, mounted in
              the lobby so the numbers live where browsing starts. */}
          <VitalSigns kb={kb} />
        </div>
      )}
    </section>
  );
}
