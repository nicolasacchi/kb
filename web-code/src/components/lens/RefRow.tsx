// DCB W2.B — one tiered reference row (Decision 3's tiered UX). Every href
// here is built from the RESOLUTION overlay (`r.reader`/`r.resolved_path`/
// `r.search`/`r.symbol_hits`), NEVER from `r.path_hint`/`r.line_hint` — the
// doc's own unverified citation. Mirrors kb's own `web/src/components/
// CodeRefsSection.tsx` (`RefRow`, W1.D.R) tier-for-tier and badge-for-badge
// since both SPAs render the SAME `codelens/1` wire — deliberate parity,
// not a coincidence.

import type { CodeLensRef } from "../../api/types";
import { readerUrl } from "../../lib/breadcrumbs";
import { refTier } from "../../lib/refTier";
import { WHEN_WRITTEN_BADGE_LABEL, whenWrittenBadge } from "../../lib/whenWritten";
import { Icon } from "../icons";

export { refTier } from "../../lib/refTier";
export type { RefTier } from "../../lib/refTier";

/// `kind ∈ {path_line, path_range, path_list}` carry a line hint and get the
/// visually-primary line-state badge; every other kind has nothing to
/// confirm. R18 — a git-verified rev-remap that ALSO moved the line is
/// still the strongest evidence class, rendered honestly (never a bare
/// "confirmed" that silently masks the move).
function LineBadge({ r }: { r: CodeLensRef }) {
  const hasLineKind = r.kind === "path_line" || r.kind === "path_range" || r.kind === "path_list";
  if (!hasLineKind) return null;
  switch (r.line_state) {
    case "confirmed": {
      const moved = r.line_evidence === "rev_remap" && r.line_hint_delta != null && r.line_hint_delta !== 0;
      if (moved) {
        const n = r.line_hint_delta as number;
        return (
          <span className="kbc-lens__badge kbc-lens__badge--confirmed" data-kbc-lens-line="confirmed-moved">
            <Icon.Check width={12} height={12} aria-hidden />
            <span className="kbc-sr-only">✓</span> moved {n > 0 ? `+${n}` : n} · git-verified
          </span>
        );
      }
      return (
        <span className="kbc-lens__badge kbc-lens__badge--confirmed" data-kbc-lens-line="confirmed">
          <Icon.Check width={12} height={12} aria-hidden />
          <span className="kbc-sr-only">✓</span> confirmed
        </span>
      );
    }
    case "drifted":
      return (
        <span className="kbc-lens__badge kbc-lens__badge--drifted" data-kbc-lens-line="drifted">
          ≈ now :{r.resolved_line ?? "?"}
        </span>
      );
    case "unverifiable":
      return (
        <span className="kbc-lens__badge kbc-lens__badge--unverifiable" data-kbc-lens-line="unverifiable">
          ? unverifiable
        </span>
      );
    case "absent":
    default:
      return null;
  }
}

/// `symbol_state` renders only on `symbol_method`/`symbol_const` kinds,
/// using the SAME visual weight as `LineBadge` — the label text is
/// `symbol_state`'s OWN literal value, never a mapping back to
/// `/api/resolve`'s `exact`/`likely`/`candidate` vocabulary (amendment 6).
function SymbolBadge({ r }: { r: CodeLensRef }) {
  if (r.kind !== "symbol_method" && r.kind !== "symbol_const") return null;
  return (
    <span
      className={`kbc-lens__badge kbc-lens__badge--symbol-${r.symbol_state}`}
      data-kbc-lens-symbol={r.symbol_state}
    >
      {r.symbol_state}
    </span>
  );
}

/// CT-F2 — a compact SECONDARY badge, additive to `LineBadge`/`SymbolBadge`
/// above it: it never replaces the current-tree verdict, only notes when
/// the era-resolved ("when written") one carries a DIFFERENT meaning.
/// `null` when `when_written` is absent (no `?at=declared`, or the doc had
/// no usable `kb-code-rev`) or when the two verdicts agree — nothing extra
/// worth a footnote.
function WhenWrittenBadge({ r }: { r: CodeLensRef }) {
  const badge = whenWrittenBadge(r);
  if (!badge) return null;
  return (
    <span
      className={`kbc-lens__badge kbc-lens__badge--era kbc-lens__badge--era-${badge}`}
      data-kbc-lens-era={badge}
      title="era-resolved: correctness at the doc's declared kb-code-rev vs. now"
    >
      {WHEN_WRITTEN_BADGE_LABEL[badge]}
    </span>
  );
}

function searchHref(q: string, repo: string): string {
  return `/search?q=${encodeURIComponent(q)}&repo=${encodeURIComponent(repo)}`;
}

export interface RefRowProps {
  r: CodeLensRef;
  repo: string;
  selected: boolean;
  onSelect: () => void;
}

export default function RefRow({ r, repo, selected, onSelect }: RefRowProps) {
  const tier = refTier(r);
  const rowClass = `kbc-lens__row kbc-lens__row--${tier}${selected ? " is-selected" : ""}`;
  const declaredBadge = r.declared && (
    <Icon.Pin className="kbc-lens__declared" aria-hidden data-kbc-lens-declared />
  );

  if (tier === "issue") {
    if (!r.issue) {
      return (
        <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
          <span className="kbc-lens__row-raw">{r.raw}</span>
          {r.note && <span className="kbc-lens__row-note">{r.note}</span>}
        </div>
      );
    }
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        <a className="kbc-lens__row-link" href={r.issue.href} target="_blank" rel="noreferrer">
          {r.raw} <Icon.External width={12} height={12} aria-hidden />
        </a>
      </div>
    );
  }

  if (tier === "external") {
    // DCB-W2.B.R fix 6 — a gem/vendor path, fully inert: no line/symbol
    // badge (nothing was ever checked against the working tree), just the
    // raw citation plus a tag naming what it is — parity with kb's own
    // `is-external` row (`web/src/components/CodeRefsSection.tsx`).
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <span className="kbc-lens__row-raw">{r.raw}</span>
        <span className="kbc-lens__row-external-tag" data-kbc-lens-external-tag>
          external
        </span>
      </div>
    );
  }

  if (tier === "unique") {
    // `r.reader` is non-null whenever `path_state === "present"` (frozen
    // field rule) — the guard here is belt-and-suspenders against a
    // producer regression, never a case the tier check alone lets through.
    const href = r.reader ? readerUrl(repo, r.reader.path, undefined, r.reader.line ?? undefined) : "";
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <LineBadge r={r} />
        <WhenWrittenBadge r={r} />
        <a
          className="kbc-lens__row-link"
          href={href || undefined}
          onClick={(e) => {
            e.preventDefault();
            onSelect();
          }}
        >
          {r.raw}
        </a>
        {href && (
          <a
            className="kbc-lens__row-external"
            href={href}
            target="_blank"
            rel="noreferrer"
            title="open full reader"
            aria-label="open full reader"
            data-kbc-lens-external
          >
            <Icon.External width={12} height={12} />
          </a>
        )}
      </div>
    );
  }

  if (tier === "ambiguous-inline") {
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <span className="kbc-lens__row-raw">{r.raw}</span>
        <ul className="kbc-lens__candidates">
          {r.candidates.map((c) => (
            <li key={c}>
              <a href={readerUrl(repo, c)} target="_blank" rel="noreferrer">
                {c} <Icon.External width={12} height={12} aria-hidden />
              </a>
            </li>
          ))}
        </ul>
      </div>
    );
  }

  if (tier === "ambiguous-search") {
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <span className="kbc-lens__row-raw">{r.raw}</span>
        {r.search && (
          <a
            className="kbc-lens__row-search"
            href={searchHref(r.search.q, r.search.repo)}
            target="_blank"
            rel="noreferrer"
          >
            {r.candidate_count} candidates <Icon.External width={12} height={12} aria-hidden />
          </a>
        )}
      </div>
    );
  }

  if (tier === "symbol-unique") {
    const hit = r.symbol_hits[0];
    const href = readerUrl(repo, hit.path, undefined, hit.line_start);
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <SymbolBadge r={r} />
        <a
          className="kbc-lens__row-link"
          href={href}
          onClick={(e) => {
            e.preventDefault();
            onSelect();
          }}
        >
          {r.raw}
        </a>
        <a
          className="kbc-lens__row-external"
          href={href}
          target="_blank"
          rel="noreferrer"
          title="open full reader"
          aria-label="open full reader"
          data-kbc-lens-external
        >
          <Icon.External width={12} height={12} />
        </a>
      </div>
    );
  }

  if (tier === "symbol-ambiguous") {
    return (
      <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
        {declaredBadge}
        <SymbolBadge r={r} />
        <span className="kbc-lens__row-raw">{r.raw}</span>
        <ul className="kbc-lens__candidates">
          {r.symbol_hits.map((hit, i) => (
            <li key={`${hit.path}:${hit.line_start}:${i}`}>
              <a href={readerUrl(repo, hit.path, undefined, hit.line_start)} target="_blank" rel="noreferrer">
                {hit.path}:{hit.line_start} <Icon.External width={12} height={12} aria-hidden />
              </a>
            </li>
          ))}
        </ul>
        {/* symbol_hits is capped at MAX_SYMBOL_HITS (5) — the count stays
            exact even when the list doesn't (same overflow shape
            candidate_count/candidates already uses). */}
        {r.symbol_hit_count > r.symbol_hits.length && (
          <span className="kbc-lens__row-note">{r.symbol_hit_count} matches</span>
        )}
      </div>
    );
  }

  // absent | symbol-none — inert row, no href.
  return (
    <div className={rowClass} data-kbc-lens-ref-ordinal={r.ordinal}>
      {declaredBadge}
      <span className="kbc-lens__row-raw">{r.raw}</span>
      {r.search && (
        <a
          className="kbc-lens__row-search"
          href={searchHref(r.search.q, r.search.repo)}
          target="_blank"
          rel="noreferrer"
        >
          search <Icon.External width={12} height={12} aria-hidden />
        </a>
      )}
      {r.note && <span className="kbc-lens__row-note">{r.note}</span>}
    </div>
  );
}
