import { memo, useEffect, useRef } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { excludeArtifact, fetchDocByPath, type DocSummary } from "../api/client";
import { useConfirm } from "./ConfirmProvider";
import { toast } from "../lib/toast";
import { artifactHref } from "../lib/artifactHref";
import { artifactDownloadUrl, triggerDownload } from "../lib/download";
import {
  isIndexPage,
  isNew,
  relativeAge,
  tagColor,
  tagsFor,
} from "../lib/derive";
import { navigateWithTitleTransition } from "../lib/viewTransition";
import CapabilityGlyphs, { glyphsFor } from "./CapabilityGlyphs";
import { Icon } from "./icons";
import TagPill from "./TagPill";
import ReadingChip from "./ReadingChip";
import SessionCardBody from "./SessionCard";
import Illumination from "./Illumination";
import type { SessionRow } from "../api/sessions";
import type { Progress } from "../hooks/useReadingProgress";
import { useCodeRefCounts } from "../hooks/useCodeRefs";
import { galleryRefCount } from "../lib/codeRefCounts";

// v0.14 S8 (sessions.tsx) precedent — long enough that a fast cursor sweep
// across the grid doesn't fire N parallel prefetches, short enough that a
// deliberate hover-then-click feels instant.
const PREFETCH_DELAY_MS = 300;

// v0.10 G1 — refined-cartographic card.
//
// Anatomy (top → bottom):
//   ┌──────────────────────────┐
//   │ ✦┃ folder/slug   [⤓][⌃]  │  illumination + accent stripe + hover actions
//   │  │                    ● │  read-state dot (top-RIGHT — see below)
//   │  │ Serif title (h3)     │
//   │  │ Mono excerpt (1.5l)  │
//   │  │ #tag #tag            │
//   │  ┕ glyph⁵ ▰▰▰▰▱ · 4↵ 1d │  caps + word-tape + age in footer
//   └──────────────────────────┘
//
// The 3px accent stripe pinned to the left lives in ::before; the
// soft top-left radial glow in ::after. Word-tape fills proportional
// to min(1, words/4000).
//
// W2.12 — the `<Illumination>` ornament (✦ above) is pinned top-LEFT,
// z-index 0 (gallery.css `.kb-illum`), deliberately NOT top-right: the
// read-state corner marks (unseen/updated dots, gallery.css
// `.kb-card__readmark`) and the hover-revealed peek/download/exclude
// actions (chrome.css `.kb-card__action`, z-index 3) already own that
// corner. Top-left has its own occupants — the "new" dot (chrome.css
// `.kb-card__new`, z-index 2) and, on index pages only, the "⌂ index"
// badge (`.kb-card__badge`, z-index 2) — but the illumination sits BELOW
// both (z-index 0, under the folder/title's z-index 1 and the badges'
// z-index 2), so it never visually competes: it reads as a faint textured
// backdrop that an opaque dot/badge/title simply paints over when present,
// and shows through as quiet ornament when they're absent.
type CardProps = {
  doc: DocSummary;
  kb: string;
  progress?: Progress;
  /// Sessions-gallery join (invariant #27: SessionRow.artifact_id === doc.id).
  /// Present only when this doc is a captured session transcript AND its row
  /// has paged in; the card then renders the session-shaped body instead of
  /// the generic title/excerpt/word-tape.
  session?: SessionRow;
};

function Card({ doc, kb, progress, session }: CardProps) {
  const navigate = useNavigate();
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const isSession = doc.kb_category === "memory-session" && !!session;
  const tags = tagsFor(doc);
  const accentTag = tags[0];
  const accent = accentTag ? tagColor(accentTag) : "var(--accent)";
  const age = relativeAge(doc.indexed_at_unix ?? doc.mtime_unix);
  const glyphs = glyphsFor(doc);
  const pages = doc.pages ?? null;
  const indexPage = isIndexPage(doc);
  const words = doc.word_count ?? 0;
  const wordPct = Math.min(1, words / 4000);

  // CT-E3 tier (a) — the "N refs" chip. One shared, hard-capped headers-only
  // walk of the coderef/1 feed per kb (useCodeRefCounts dedupes across every
  // mounted card); `galleryRefCount` returns 0 for EVERY doc when the walk
  // came back incomplete, so a too-big corpus degrades to no chips at all
  // rather than a partial set that reads as "this doc cites nothing".
  const refCountsQuery = useCodeRefCounts(kb);
  const codeRefN = isSession ? 0 : galleryRefCount(refCountsQuery.data, doc.id);

  // CT-F4 tier — the "N kept" session-residue chip. No extra query: the
  // docs-list route already decorates the visible page (absent when zero, so
  // an old daemon or a doc with no birth session simply renders nothing).
  // Session cards carry their own body/chrome, so they skip it like the
  // coderef chip above.
  const residueN = isSession ? 0 : (doc.session_residue ?? 0);

  // Antilibrary read-state corner marks (W1.A wire fields). "unseen" and
  // "updated" are mutually exclusive by construction: a never-opened doc has
  // no last_opened_unix to compare mtime against.
  const rs = doc;
  // Never-opened on the wire: an OLD daemon omits read_state entirely; a
  // W1.A daemon decorates absent-from-rollup rows as "unread" with NO
  // last_opened_unix (an opened-but-unread row always carries one).
  const neverOpened =
    !isSession &&
    (rs.read_state == null ||
      (rs.read_state === "unread" && rs.last_opened_unix == null));
  const updatedSinceRead =
    !isSession &&
    !neverOpened &&
    rs.last_opened_unix != null &&
    doc.mtime_unix != null &&
    doc.mtime_unix > rs.last_opened_unix;

  // Progressive gallery→reader view transition (lib/viewTransition.ts) —
  // the clicked card's title is the one DOM node that gets the shared
  // transition name, applied right before navigating.
  const titleRef = useRef<HTMLHeadingElement>(null);

  // Hover/focus prefetch — warms the exact TanStack key detail.tsx's doc
  // query uses (["doc", kb, source_relative]) via the same fetchDocByPath
  // fetcher, so a real click lands on a populated cache. Per-card timer +
  // once-only guard (each Card is its own component instance, keyed by
  // doc.id in the parent grid — no cross-card Set needed).
  const prefetchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const prefetched = useRef(false);
  const startPrefetch = () => {
    if (prefetched.current || prefetchTimer.current) return;
    prefetchTimer.current = setTimeout(() => {
      prefetchTimer.current = null;
      prefetched.current = true;
      void queryClient
        .prefetchQuery({
          queryKey: ["doc", kb, doc.source_relative] as const,
          queryFn: ({ signal }) => fetchDocByPath(kb, doc.source_relative, signal),
        })
        .catch(() => {});
    }, PREFETCH_DELAY_MS);
  };
  const cancelPrefetch = () => {
    if (prefetchTimer.current) {
      clearTimeout(prefetchTimer.current);
      prefetchTimer.current = null;
    }
  };
  useEffect(() => cancelPrefetch, []);
  // Markdown source indicator — derived from the path (no lance `kind`
  // column; P3 deferred). `.md`/`.markdown` artifacts render to HTML at
  // serve time but the gallery still flags their source format. The badge is
  // static/in-flow (see chrome.css) so it stacks cleanly under the multi-file
  // pills and above the folder slug.
  const isMd = /\.(md|markdown)$/i.test(doc.source_relative);

  const accentStyle = { "--card-accent": accent } as React.CSSProperties;

  return (
    // The card is an <article> so the action <button>s can be siblings of the
    // navigation <Link> rather than nested inside it (a <button> inside an <a>
    // is invalid HTML + breaks keyboard/SR behaviour). The Link wraps the
    // (non-interactive) content and flex-fills the card, so the whole body
    // stays clickable; the absolutely-positioned buttons sit above it.
    <article
      className={`kb-card ${indexPage ? "kb-card--index" : ""}${
        isSession && session?.substance === "trivial" ? " kb-card--husk" : ""
      }`}
      style={accentStyle}
      onMouseEnter={startPrefetch}
      onMouseLeave={cancelPrefetch}
      onFocus={startPrefetch}
      onBlur={cancelPrefetch}
    >
      {!isSession && <Illumination seed={doc.id} />}
      {isNew(doc) && <span className="kb-card__new" aria-label="new" />}
      {neverOpened && (
        <span
          className="kb-card__readmark kb-card__readmark--unseen"
          title="research reserve — not opened yet"
          aria-label="not opened yet — part of your research reserve"
        />
      )}
      {updatedSinceRead && (
        <span
          className="kb-card__readmark kb-card__readmark--updated"
          title="changed since you last opened it"
          aria-label="changed since you last opened it"
        />
      )}
      <button
        type="button"
        className="kb-card__action kb-card__peek"
        aria-label={`Preview ${doc.title || doc.id}`}
        title="preview (p)"
        onClick={(e) => {
          // Now that this button is a sibling of the card link (not nested
          // inside it), the click no longer bubbles to the anchor — wire the
          // navigation explicitly so the affordance still opens the artifact.
          e.preventDefault();
          e.stopPropagation();
          navigate(artifactHref(kb, doc.source_relative));
        }}
      >
        <svg
          width="13"
          height="13"
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
      </button>
      <button
        type="button"
        className="kb-card__action kb-card__download"
        aria-label={`Download ${doc.title || doc.id}`}
        title="download (raw source)"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          triggerDownload(artifactDownloadUrl(kb, doc.id));
        }}
      >
        <Icon.Download />
      </button>
      <button
        type="button"
        className="kb-card__action kb-card__exclude"
        aria-label={`Exclude ${doc.title || doc.id} from the index`}
        title="exclude from index (comments + history survive; re-include from Settings → Excluded)"
        onClick={(e) => {
          // Sibling of the card link (like peek/download) so the click never
          // navigates; the confirm is the invariant-#32 destructive prompt and
          // the card's disappearance rides SSE (artifact.removed → docsGate).
          e.preventDefault();
          e.stopPropagation();
          void (async () => {
            const ok = await confirm({
              title: "Exclude from index?",
              body: `Exclude “${doc.title || doc.source_relative}” from search and the gallery? The file stays on disk and its comments + reading history survive — re-include it any time from Settings → Excluded.`,
              confirmLabel: "Exclude",
            });
            if (!ok) return;
            excludeArtifact(kb, doc.source_relative)
              .then(() => toast.ok("excluded from index"))
              .catch((err) =>
                toast.err(
                  `exclude failed: ${err instanceof Error ? err.message : String(err)}`,
                ),
              );
          })();
        }}
      >
        {/* eye-off — the peek eye's counterpart: "hide this from the index" */}
        <svg
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden
        >
          <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
          <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
          <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
          <line x1="1" y1="1" x2="23" y2="23" />
        </svg>
      </button>

      <Link
        to={artifactHref(kb, doc.source_relative)}
        className={`kb-card__link${isSession ? " kb-card__link--session" : ""}`}
        aria-label={
          isSession
            ? `Open session: ${session.display_name}`
            : `Open ${doc.title || doc.id}`
        }
        onClick={(e) => {
          // Session cards render no h3.kb-card__title (SessionCardBody has
          // its own layout) — nothing to name, so let the plain Link
          // navigate. Modifier/middle clicks must keep opening in a new
          // tab/window, so only a genuine plain left click is intercepted
          // (mirrors NoteMarkdown.tsx's link-click guard).
          if (
            isSession ||
            e.defaultPrevented ||
            e.button !== 0 ||
            e.metaKey ||
            e.ctrlKey ||
            e.shiftKey ||
            e.altKey
          ) {
            return;
          }
          e.preventDefault();
          navigateWithTitleTransition(titleRef.current, () =>
            navigate(artifactHref(kb, doc.source_relative)),
          );
        }}
      >
        {isSession ? (
          <SessionCardBody session={session} />
        ) : (
          <>
        {indexPage && (
          <span className="kb-card__badge" title="index / landing page">
            ⌂ index
          </span>
        )}
        {pages && pages.length > 1 && (
          <span
            className="kb-card__badge kb-card__badge--pages"
            title={`${pages.length}-file artifact`}
          >
            {pages.length} files
          </span>
        )}
        {isMd && (
          <span
            className="kb-card__badge kb-card__badge--markdown"
            title="markdown source — rendered to HTML on open"
          >
            MD
          </span>
        )}

        {doc.folder && (
          <div className="kb-card__folder" title={doc.folder}>
            {doc.folder}
          </div>
        )}
        <h3 className="kb-card__title" ref={titleRef}>
          {doc.title || "(untitled)"}
        </h3>
        {doc.summary && <p className="kb-card__excerpt">{doc.summary}</p>}

        {tags.length > 0 && (
          <div className="kb-card__tags">
            {tags.slice(0, 3).map((t) => (
              <TagPill key={t} tag={t} accent={t === accentTag} />
            ))}
          </div>
        )}

        <div className="kb-card__foot">
          {glyphs.length > 0 ? <CapabilityGlyphs doc={doc} /> : <span />}
          <span className="kb-card__tape" aria-hidden>
            <i style={{ width: `${wordPct * 100}%` }} />
          </span>
          <span className="kb-card__words">
            {words ? `${words.toLocaleString()}w` : ""}
          </span>
          {doc.backlinks ? (
            <span className="kb-card__links">· {doc.backlinks}↵</span>
          ) : null}
          {codeRefN > 0 && (
            <span
              className="kb-card__coderefs"
              title={`cites ${codeRefN} code ref${codeRefN === 1 ? "" : "s"} — open the reader's Code section for freshness`}
            >
              · {codeRefN} ref{codeRefN === 1 ? "" : "s"}
            </span>
          )}
          {residueN > 0 && (
            <span
              className="kb-card__residue"
              title={`${residueN} memor${residueN === 1 ? "y" : "ies"} kept from this artifact's birth session — sort the gallery with ?sort=residue`}
            >
              · {residueN} kept
            </span>
          )}
          {progress && (
            <ReadingChip pct={progress.pct} isDone={progress.isDone} />
          )}
          {age && <span className="kb-card__age">· {age}</span>}
        </div>
          </>
        )}
      </Link>
    </article>
  );
}

// Custom comparator. `doc`, `kb`, and `progress` are referentially
// stable from the memoized parents when content is unchanged, so `===`
// is correct for them.
function cardPropsEqual(prev: CardProps, next: CardProps): boolean {
  return (
    prev.doc === next.doc &&
    prev.kb === next.kb &&
    prev.progress === next.progress &&
    prev.session === next.session
  );
}

export default memo(Card, cardPropsEqual);
