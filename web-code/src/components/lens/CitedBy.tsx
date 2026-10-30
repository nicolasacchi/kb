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

import { useEffect, useRef, useState } from "react";
import { useDocRefs } from "../../hooks/useDocRefs";
import { CITEDBY_OPEN_EVENT, citedByLabel, citedByNote, citedByRowLabel } from "../../lib/citedBy";
import { safeHref } from "../../lib/safeHref";
import { Icon } from "../iconSet";

export interface CitedByProps {
  repo: string;
  path: string;
}

// A row link is only ever an http(s) URL (the wire value is vetted, as the
// TopBar chip's is): anything else renders as inert text.
function rowHref(c: { doc_section_href?: string | null; doc_public_href?: string | null }): string | null {
  return safeHref(c.doc_section_href ?? c.doc_public_href);
}

export default function CitedBy({ repo, path }: CitedByProps) {
  const [open, setOpen] = useState(false);
  const { data } = useDocRefs(repo, path);
  const rootRef = useRef<HTMLDivElement | null>(null);

  // OL-SIBL1 - the TopBar crossing chip's open-strip mode asks this strip to
  // expand and scroll into view (a CustomEvent; no key binding, no other
  // listener). Only a mounted strip hears it.
  useEffect(() => {
    const onOpen = () => {
      setOpen(true);
      rootRef.current?.scrollIntoView?.({ block: "nearest" });
    };
    window.addEventListener(CITEDBY_OPEN_EVENT, onOpen);
    return () => window.removeEventListener(CITEDBY_OPEN_EVENT, onOpen);
  }, []);

  // `null` before the fetch resolves or on error. OL-SIBL3: once it HAS
  // resolved, zero claims renders one muted line saying which kind of absence
  // it is (nothing cites the file / kb could not be asked / only pinned docs
  // were looked at) instead of silence, which could not tell them apart. That
  // line deliberately carries no `data-kbc-citedby` (it is not the strip).
  if (!data) return null;
  const note = citedByNote(data.claims.length, data.live_lookup);
  if (data.claims.length === 0) {
    return note ? (
      <div className="kbc-inspector__citedby-note" role="note" data-kbc-citedby-absence={note.kind}>
        {note.text}
      </div>
    ) : null;
  }

  const label = citedByLabel(data.claims, data.live);

  return (
    <div ref={rootRef} className="kbc-inspector__citedby" data-kbc-citedby data-kbc-citedby-rotted={!data.live || undefined}>
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
      {note && (
        <div className="kbc-inspector__citedby-note" role="note" data-kbc-citedby-note={note.kind}>
          {note.text}
        </div>
      )}
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
              {rowHref(c) ? (
                <a
                  className="kbc-inspector__citedby-title"
                  href={rowHref(c) ?? undefined}
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
                {c.source === "kb-live" && (
                  <span
                    className="kbc-inspector__citedby-source"
                    data-kbc-citedby-source="kb-live"
                    title="Read live from kb for this exact path; not checked against a pinned checkout"
                  >
                    unpinned - live from kb
                  </span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
