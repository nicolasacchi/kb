import { useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useKbs } from "../hooks/useKbs";
import { useReviewNotes } from "../hooks/useReviewNotes";
import { anchorLabel } from "../lib/commentFmt";
import { buildCiteUrl } from "../lib/quote";
import { relativeAge } from "../lib/time";
import EmptyState from "../components/EmptyState";
import Loading from "../components/Loading";
import { Icon } from "../components/icons";
import type { ReviewNoteRow, ReviewNoteStatus } from "../api/reviewNotes";
import "../styles/reviewNotes.css";

// v0.40 TN — /review-notes. Every PRIVATE comment across every corpus,
// grouped by artifact, filterable by kb / tag (repeatable, ANDed) / body
// substring / status. NOT /notes (app.tsx's kb Markdown note artifacts —
// a different entity, `kb_core::notes`); the two names both end in "notes"
// and must not be treated as siblings in either direction.
//
// These rows exist here and NOWHERE ELSE for a reader who is not the
// operator: every agent-facing surface filters private comments out with no
// opt-in (the flag is the guarantee), so this page is the operator's own
// view of what the agent deliberately cannot see.
//
// Grammar (read off the URL, written with setSearchParams — the
// /slates?topic= precedent; no URL-helper module, unlike
// lib/galleryUrl.ts which earns one by having six filter axes):
//   ?kb=<name>&tag=<slug>&tag=<slug>&q=<substring>&status=open|resolved|all
// `status` defaults to `all` (a note browser wants resolved notes), `kb`
// absent means every kb, no sort param, no paging (the server caps and
// says so with `truncated`).

const STATUSES: ReviewNoteStatus[] = ["open", "resolved", "all"];

type Group = {
  kb: string;
  artifactId: string;
  title: string;
  sourceRelative: string | null;
  latest: number;
  items: ReviewNoteRow[];
};

// Same shape as routes/inbox.tsx's groupByArtifact: first-seen order, then a
// defensive re-sort (the server already sends updated_at DESC).
function groupByArtifact(rows: ReviewNoteRow[]): Group[] {
  const map = new Map<string, Group>();
  for (const r of rows) {
    const key = `${r.kb} ${r.artifact_id}`;
    let g = map.get(key);
    if (!g) {
      g = {
        kb: r.kb,
        artifactId: r.artifact_id,
        title: r.artifact_title,
        sourceRelative: r.source_relative ?? null,
        latest: r.updated_at,
        items: [],
      };
      map.set(key, g);
    }
    g.items.push(r);
    if (r.updated_at > g.latest) g.latest = r.updated_at;
  }
  const groups = Array.from(map.values());
  for (const g of groups) g.items.sort((a, b) => b.updated_at - a.updated_at);
  groups.sort((a, b) => b.latest - a.latest);
  return groups;
}

export default function ReviewNotesRoute() {
  useDocumentTitle("Review notes");
  const [params, setParams] = useSearchParams();
  const { data: kbs = [] } = useKbs();

  const kb = params.get("kb") ?? "";
  // `tag` is repeatable: getAll, ANDed server-side. Order is preserved as
  // clicked so the URL reads the way the chip row does.
  const tags = params.getAll("tag").filter((t) => t !== "");
  const q = params.get("q") ?? "";
  const rawStatus = params.get("status");
  // An unrecognised `status` falls back to the default instead of being
  // forwarded — the server 400s it, and a stale bookmark shouldn't blank
  // the page.
  const status: ReviewNoteStatus =
    STATUSES.find((s) => s === rawStatus) ?? "all";

  const { notes, tags: facets, total, truncated, tagsTruncated, loading, error } =
    useReviewNotes({ kb: kb || undefined, tags, q, status });

  const groups = useMemo(() => groupByArtifact(notes), [notes]);

  // One writer for the whole filter bar: the URL is the single source of
  // truth, so a chip click, the kb select, the status tabs and the search
  // box can't drift out of sync with each other (and the back button works
  // for free). Values equal to the default are dropped, so the address bar
  // stays clean.
  function write(patch: {
    kb?: string;
    tags?: string[];
    q?: string;
    status?: ReviewNoteStatus;
  }) {
    const next = new URLSearchParams();
    const nKb = patch.kb ?? kb;
    const nTags = patch.tags ?? tags;
    const nQ = patch.q ?? q;
    const nStatus = patch.status ?? status;
    if (nKb) next.set("kb", nKb);
    for (const t of nTags) next.append("tag", t);
    if (nQ.trim()) next.set("q", nQ.trim());
    if (nStatus !== "all") next.set("status", nStatus);
    setParams(next);
  }

  const toggleTag = (name: string) =>
    write({
      tags: tags.includes(name)
        ? tags.filter((t) => t !== name)
        : [...tags, name],
    });

  const filtered =
    kb !== "" || tags.length > 0 || q.trim() !== "" || status !== "all";

  return (
    <div className="rnotes">
      <header className="rnotes__head">
        <h1 className="rnotes__title">
          Review notes
          {total > 0 && <span className="rnotes__count">{total}</span>}
        </h1>
        <p className="rnotes__sub">
          Comments marked private — for you only. Agents never see these,
          in any export, prompt or count.
        </p>
      </header>

      <div className="rnotes__filters">
        <NotesSearchBox value={q} onChange={(next) => write({ q: next })} />
        <div className="rnotes__kb">
          <select
            className="rnotes__kb-select"
            value={kb}
            aria-label="corpus"
            onChange={(e) => write({ kb: e.target.value })}
          >
            <option value="">every corpus</option>
            {kbs.map((k) => (
              <option key={k.name} value={k.name}>
                {k.name}
              </option>
            ))}
          </select>
        </div>
        <div className="rnotes__status" role="tablist" aria-label="note status filter">
          {STATUSES.map((s) => (
            <button
              key={s}
              type="button"
              role="tab"
              aria-selected={status === s}
              className={`rnotes__status-btn ${status === s ? "is-active" : ""}`}
              onClick={() => write({ status: s })}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      {facets.length > 0 && (
        <div className="rnotes__facets" aria-label="filter by tag">
          {facets.map((t) => {
            const on = tags.includes(t.name);
            return (
              <button
                key={t.name}
                type="button"
                className={`rnotes__chip ${on ? "is-active" : ""}`}
                aria-pressed={on}
                title={
                  on
                    ? `carrying “${t.name}” — click to drop the filter`
                    : `${t.count} note${t.count === 1 ? "" : "s"} carry “${t.name}”`
                }
                onClick={() => toggleTag(t.name)}
              >
                {/* Same deterministic chip colour as the gallery sidebar
                    and the search rail (hsl(seed % 360, 64%, 64%)) — the
                    tag slug's FNV-1a seed, so a slug always looks the
                    same. A different NAMESPACE from an artifact's own
                    kb-tags, but the same colour rule on purpose. */}
                <span
                  className="rnotes__chip-dot"
                  style={{ background: `hsl(${t.color_seed % 360}, 64%, 64%)` }}
                />
                <span className="rnotes__chip-name">{t.name}</span>
                <span className="rnotes__chip-count">{t.count}</span>
              </button>
            );
          })}
          {tagsTruncated && (
            <span className="rnotes__facets-trunc" title="the server capped the tag list">
              more tags exist
            </span>
          )}
        </div>
      )}

      {filtered && (
        <div className="rnotes__filter-note">
          filtered
          <button type="button" className="rnotes__clear" onClick={() => setParams(new URLSearchParams())}>
            clear all
          </button>
        </div>
      )}

      {error && (
        <div className="rnotes__error" role="alert">
          Couldn’t load your notes: {error}
        </div>
      )}

      {loading && <Loading label="loading notes…" />}

      {!error && !loading && notes.length === 0 && (
        <EmptyState
          icon={<Icon.Comment />}
          title={filtered ? "No notes match these filters" : "No private notes yet"}
          hint={
            filtered
              ? "Tags are ANDed — a note must carry every one you picked."
              : "Mark a comment private from the reader’s comments panel to keep it to yourself."
          }
        />
      )}

      {notes.length > 0 && (
        <ul className="rnotes__list">
          {groups.map((g) => (
            <NoteGroup key={`${g.kb} ${g.artifactId}`} group={g} />
          ))}
        </ul>
      )}

      {truncated && (
        <p className="rnotes__trunc">
          Showing the first {notes.length} notes — narrow the filters to see
          the rest.
        </p>
      )}
    </div>
  );
}

function NoteGroup({ group }: { group: Group }) {
  const title = group.title || group.artifactId;
  return (
    <li className="rnotes-card">
      <div className="rnotes-card__head">
        <span className="rnotes-card__kb" title={`corpus: ${group.kb}`}>
          {group.kb}
        </span>
        <span className="rnotes-card__title">{title}</span>
        <span className="rnotes-card__age">{relativeAge(group.latest)}</span>
      </div>
      <ul className="rnotes-card__notes">
        {group.items.map((r) => (
          <NoteRow key={`${r.kb} ${r.comment_id}`} row={r} />
        ))}
      </ul>
    </li>
  );
}

function NoteRow({ row }: { row: ReviewNoteRow }) {
  // THE cite grammar, verbatim (lib/quote.ts): `?panel=comments&comment=<id>`
  // plus `?sec=` for a section anchor, which is what makes detail.tsx open
  // the panel and activate this exact row. Never hand-roll that query here —
  // invariant #30's one-home-per-action rule. The URL is absolute because
  // buildCiteUrl's contract is a shareable permalink (the panel's "cite"
  // action copies the very same string), so the row renders a plain <a>
  // rather than a router <Link>.
  //
  // A row whose artifact is no longer indexed (no `source_relative`) has no
  // permalink at all — same as the inbox's dead-title case. `private` is
  // always true on this route; the row still renders the lock so a future
  // non-private row can't be mistaken for a note.
  const href = row.source_relative
    ? buildCiteUrl({
        kb: row.kb,
        sourceRelative: row.source_relative,
        title: row.artifact_title || row.artifact_id,
        anchor: row.anchor,
        commentId: row.comment_id,
        body: row.body,
      })
    : null;

  const meta = (
    <span className="rnotes-note__meta">
      {row.private && (
        <span className="rnotes-note__lock" title="private — never shown to an agent">
          🔒
        </span>
      )}
      <span className={`rnotes-note__status rnotes-note__status--${row.status}`}>
        {row.status}
      </span>
      <span className="rnotes-note__author">{row.author}</span>
      {row.user && <span className="rnotes-note__user">{row.user}</span>}
      {row.stale && (
        <span
          className="rnotes-note__stale"
          title="this note's anchor doesn't bind to the current artifact text"
        >
          stale
        </span>
      )}
      {row.reply_count > 0 && (
        <span className="rnotes-note__replies">
          {row.reply_count} repl{row.reply_count === 1 ? "y" : "ies"}
        </span>
      )}
      <span className="rnotes-note__anchor">{anchorLabel({ anchor: row.anchor })}</span>
      <span className="rnotes-note__age">{relativeAge(row.updated_at)}</span>
    </span>
  );

  return (
    <li className={`rnotes-note rnotes-note--${row.status}`}>
      {href ? (
        <a
          className="rnotes-note__link"
          href={href}
          title="open this note in the reader"
        >
          <span className="rnotes-note__body">{row.body}</span>
          {meta}
        </a>
      ) : (
        <span className="rnotes-note__link rnotes-note__link--dead" title="artifact no longer indexed">
          <span className="rnotes-note__body">{row.body}</span>
          {meta}
        </span>
      )}
      {row.tags.length > 0 && (
        <span className="rnotes-note__tags">
          {row.tags.map((t) => (
            <span key={t} className="rnotes-note__tag">
              {t}
            </span>
          ))}
        </span>
      )}
    </li>
  );
}

// Controlled input that buffers keystrokes and flushes to the URL on a short
// debounce, so the address bar (and the query key) don't thrash per
// keystroke — the same shape routes/search.tsx's SearchInput uses.
function NotesSearchBox({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  const onChangeRef = useRef(onChange);
  const lastWrittenRef = useRef(value);
  onChangeRef.current = onChange;

  // Adopt external URL changes (back button, a pasted link) while ignoring
  // the echo of our own debounced write.
  useEffect(() => {
    if (value !== lastWrittenRef.current) {
      lastWrittenRef.current = value;
      setDraft(value);
    }
  }, [value]);

  useEffect(() => {
    if (draft === value) return;
    const t = setTimeout(() => {
      lastWrittenRef.current = draft;
      onChangeRef.current(draft);
    }, 110);
    return () => clearTimeout(t);
  }, [draft, value]);

  return (
    <div className="rnotes__search">
      <Icon.Search aria-hidden="true" />
      <input
        type="search"
        className="rnotes__search-input"
        placeholder="search note bodies…"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        aria-label="search note bodies"
      />
    </div>
  );
}
