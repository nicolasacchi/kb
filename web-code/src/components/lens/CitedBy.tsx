// DCB W3.B — the reverse "cited by" index, rendered as an always-visible
// slot in the reader's `InspectorRail` (root CLAUDE.md invariant #30's
// "passport" precedent: an always-visible strip above the tab rail, not
// itself a tab — see `16-w3-reverse-index.md` §2's ratified placement). A
// SIBLING `GET /api/doc-refs` fetch, never a `LensesOut` field and never
// folded into `WhyPanel` (that panel is scoped to ONE clicked line; this is
// file-scoped and has nothing to do with a gutter click).
//
// Deliberately NO trust badge on a claim — not because the wire lacks the
// data (`DocRefClaim.line_state` IS present, per-line confirmed/moved/
// unverifiable/…; this v1 UI simply never renders it here), but because a
// per-line badge belongs to `WhyPanel`'s ONE-line scope, not this
// file-scoped strip. The fields genuinely persisted server-side but unread
// by any v1 consumer (`DocRefClaim`'s own doc, `api/types.ts`) are
// `head_sha`/`dirty`/`doc_hash`/`kind` — dropped from the wire type
// entirely, not merely unrendered. Each claim renders as: the citing doc's
// title (linked via
// `doc_public_href`, new tab — an external link to KB, not to this file, so
// it stays live even when the cited path has rotted) + its `group_label`
// (when present) + `raw_hint` as secondary text.

import { useState } from "react";
import { useDocRefs } from "../../hooks/useDocRefs";
import { citedByLabel, citedByRowLabel } from "../../lib/citedBy";
import { Icon } from "../icons";

export interface CitedByProps {
  repo: string;
  path: string;
}

export default function CitedBy({ repo, path }: CitedByProps) {
  const [open, setOpen] = useState(false);
  const { data } = useDocRefs(repo, path);

  // `null` before the fetch resolves, on error, or when the repo/path pair
  // has zero claims — an absent citation is the overwhelmingly common case
  // and must not compete visually with the 6 real tabs
  // (`InspectorRailProps.citedBy`'s own doc).
  if (!data || data.claims.length === 0) return null;

  const label = citedByLabel(data.claims, data.live);

  return (
    <div className="kbc-inspector__citedby" data-kbc-citedby data-kbc-citedby-rotted={!data.live || undefined}>
      <button
        type="button"
        className="kbc-inspector__citedby-toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-controls="kbc-citedby-list"
        data-kbc-citedby-toggle
      >
        <span
          className={"kbc-inspector__citedby-label" + (data.live ? "" : " kbc-inspector__citedby-label--rotted")}
          data-kbc-citedby-label
        >
          {label}
        </span>
        <Icon.ChevDown />
      </button>
      {open && (
        <ul className="kbc-inspector__citedby-list" id="kbc-citedby-list" data-kbc-citedby-list>
          {data.claims.map((c, i) => (
            // Keyed on the server-ordered index, not `doc_id`+`raw_hint`:
            // the wire deliberately omits `ordinal` (M1, W3.B.R review), so
            // a same-doc/same-path claim at two different ordinals can
            // collide on that pair, and a `scope=all`-style response could
            // in principle span more than one `kb`. Server order is
            // deterministic (`ORDER BY kb, doc_id, ordinal` — `store.rs`'s
            // `doc_refs_for_path`), so the array index is a stable, unique
            // key for this render.
            <li key={`${c.kb}:${c.doc_id}:${i}`} className="kbc-inspector__citedby-row" data-kbc-citedby-row>
              {c.doc_public_href ? (
                <a
                  className="kbc-inspector__citedby-title"
                  href={c.doc_public_href}
                  target="_blank"
                  rel="noreferrer"
                  data-kbc-citedby-link
                >
                  {citedByRowLabel(c)}
                </a>
              ) : (
                <span className="kbc-inspector__citedby-title" data-kbc-citedby-title>
                  {citedByRowLabel(c)}
                </span>
              )}
              <span className="kbc-inspector__citedby-meta">
                {c.group_label && (
                  <span className="kbc-inspector__citedby-group" data-kbc-citedby-group>
                    {c.group_label}
                  </span>
                )}
                <span className="kbc-inspector__citedby-hint" data-kbc-citedby-hint>
                  {c.raw_hint}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
