import { useMemo } from "react";
import { Link } from "react-router-dom";
import type { DocSummary } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import { isIndexPage, relativeAge, tagColor, tagsFor } from "../lib/derive";
import { useAnchors } from "../hooks/useAnchors";
import CapabilityGlyphs, { glyphsFor } from "./CapabilityGlyphs";
import TagPill from "./TagPill";
import { Icon } from "./icons";

// v0.10 G2 — pinned/index card. Replaces IndexHero. Two-column layout:
// left = identity (badge + folder + serif title + excerpt + tags),
// right = stat list (words / backlinks / age / caps) + 3-button action
// row (preview / open / ⚓ anchor). Wires the ⚓ action to the K3
// corkboard toggle so the user can pin/unpin the kb's primary
// landing page in one click.
//
// Picks the same hero IndexHero did (root-level `index.html` wins; else
// the most recently indexed `isIndexPage`). Returns null when no
// landing page exists.
export default function PinnedIndexCard({
  docs,
  kb,
}: {
  docs: DocSummary[];
  kb: string;
}) {
  const hero = useMemo(() => pickHero(docs), [docs]);
  const { isAnchored, toggle } = useAnchors();
  if (!hero) return null;

  const tags = tagsFor(hero);
  const accentTag = tags[0];
  const accent = accentTag ? tagColor(accentTag) : "var(--accent)";
  const age = relativeAge(hero.indexed_at_unix ?? hero.mtime_unix);
  const glyphs = glyphsFor(hero);
  const anchored = isAnchored(kb, hero.id);
  const style = { "--card-accent": accent } as React.CSSProperties;

  return (
    <div className="kb-pin" style={style}>
      <Link
        to={artifactHref(kb, hero.source_relative)}
        className="kb-pin__left"
        aria-label={`kb landing: ${hero.title || hero.id}`}
      >
        <span className="kb-pin__badge">⌂ index</span>
        {hero.folder && (
          <div className="kb-pin__folder" title={hero.folder}>
            {hero.folder}
          </div>
        )}
        <h2 className="kb-pin__title">{hero.title || "(untitled)"}</h2>
        {hero.summary && <p className="kb-pin__excerpt">{hero.summary}</p>}
        {tags.length > 0 && (
          <div className="kb-pin__tags">
            {tags.slice(0, 6).map((t) => (
              <TagPill key={t} tag={t} accent={t === accentTag} />
            ))}
          </div>
        )}
      </Link>
      <div className="kb-pin__right">
        {hero.word_count ? (
          <div className="kb-pin__stat">
            <span>words</span>
            <b>{hero.word_count.toLocaleString()}</b>
          </div>
        ) : null}
        {hero.backlinks ? (
          <div className="kb-pin__stat">
            <span>backlinks</span>
            <b>{hero.backlinks}↵</b>
          </div>
        ) : null}
        {age && (
          <div className="kb-pin__stat">
            <span>age</span>
            <b>{age}</b>
          </div>
        )}
        {glyphs.length > 0 && (
          <div className="kb-pin__stat kb-pin__stat--caps">
            <span>caps</span>
            <CapabilityGlyphs doc={hero} />
          </div>
        )}
        <div className="kb-pin__actions">
          <Link
            to={artifactHref(kb, hero.source_relative)}
            className="kb-pin__btn kb-pin__btn--primary"
            title="preview (p)"
          >
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z" />
              <circle cx="12" cy="12" r="3" />
            </svg>
            preview
          </Link>
          <a
            className="kb-pin__btn"
            href={artifactHref(kb, hero.source_relative)}
            target="_blank"
            rel="noopener noreferrer"
            title="open in tab (o)"
          >
            ↗ open
          </a>
          <button
            type="button"
            className={`kb-pin__btn kb-pin__btn--anchor ${anchored ? "is-on" : ""}`}
            onClick={() => toggle(kb, hero.id)}
            title={anchored ? "unanchor" : "anchor (a)"}
            aria-pressed={anchored}
          >
            <Icon.Anchor />
          </button>
        </div>
      </div>
    </div>
  );
}

function pickHero(docs: DocSummary[]): DocSummary | null {
  let rootCandidate: DocSummary | null = null;
  let fallback: DocSummary | null = null;
  for (const d of docs) {
    if (!isIndexPage(d)) continue;
    const rel = d.path.includes("/") ? d.path : `/${d.path}`;
    const segments = rel.split("/").filter(Boolean);
    if (segments.length === 1 && segments[0].toLowerCase() === "index.html") {
      if (rootCandidate == null || newerThan(d, rootCandidate)) {
        rootCandidate = d;
      }
      continue;
    }
    if (fallback == null || newerThan(d, fallback)) {
      fallback = d;
    }
  }
  return rootCandidate ?? fallback;
}

function newerThan(a: DocSummary, b: DocSummary): boolean {
  const ta = a.indexed_at_unix ?? a.mtime_unix ?? 0;
  const tb = b.indexed_at_unix ?? b.mtime_unix ?? 0;
  return ta > tb;
}
