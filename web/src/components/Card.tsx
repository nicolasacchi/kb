import { memo, useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { fetchDocByPath, type DocSummary } from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import {
  isIndexPage,
  isNew,
  relativeAge,
  tagColor,
  tagsFor,
} from "../lib/derive";
import { cardMetaParts } from "../lib/cardMeta";
import { navigateWithTitleTransition } from "../lib/viewTransition";
import { Icon } from "@kb/ui/icons";
import { EyeIcon, EyeOffIcon, useCardActions } from "./cardActions";
import CardActionsSheet from "./CardActionsSheet";
import TagPill from "./TagPill";
import SessionCardBody from "./SessionCard";
import Illumination from "./Illumination";
import type { SessionRow } from "../api/sessions";
import type { Progress } from "../hooks/useReadingProgress";

// v0.14 S8 (sessions.tsx) precedent — long enough that a fast cursor sweep
// across the grid doesn't fire N parallel prefetches, short enough that a
// deliberate hover-then-click feels instant.
const PREFETCH_DELAY_MS = 300;

// OL L4 — a card says one thing loudly. Anatomy (top → bottom):
//   ┌──────────────────────────┐
//   │  ┃ Serif title (2 lines) │  accent stripe + hover actions top-right
//   │  ┃ one-line excerpt      │
//   │  ┃ #tag #tag             │  at most two tags
//   │  ┃ folder · 3d · 5 min   │  ONE quiet meta line
//   │▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱│  2px reading-progress bar on the bottom edge
//   └──────────────────────────┘
// Capability glyphs, the word tape, link/ref/kept counts and the MD/pages
// badges left the face; they live in the Index view.
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
  const actions = useCardActions(kb, doc);
  const [moreOpen, setMoreOpen] = useState(false);
  const moreRef = useRef<HTMLButtonElement>(null);
  // Close, then hand focus back to More on the next frame: covers iOS Safari
  // (taps do not focus buttons, so the Sheet's opener capture can be <body>)
  // and a cancelled ConfirmProvider, which does not restore focus itself.
  const restoreFocus = () => {
    requestAnimationFrame(() => moreRef.current?.focus());
  };
  const closeMore = () => {
    setMoreOpen(false);
    restoreFocus();
  };
  // Exclude: close without scheduling a focus move; the confirm dialog owns
  // focus while it is up and restoreFocus runs only after it resolves.
  const dismissMore = () => setMoreOpen(false);
  const queryClient = useQueryClient();
  const isSession = doc.kb_category === "memory-session" && !!session;
  const tags = tagsFor(doc);
  const accentTag = tags[0];
  const accent = accentTag ? tagColor(accentTag) : "var(--accent)";
  const age = relativeAge(doc.indexed_at_unix ?? doc.mtime_unix);
  const indexPage = isIndexPage(doc);
  const meta = cardMetaParts(doc.folder, doc.word_count);

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
          e.preventDefault();
          e.stopPropagation();
          actions.peek();
        }}
      >
        <EyeIcon />
      </button>
      <button
        type="button"
        className="kb-card__action kb-card__download"
        aria-label={`Download ${doc.title || doc.id}`}
        title="download (raw source)"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          actions.download();
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
          e.preventDefault();
          e.stopPropagation();
          void actions.exclude();
        }}
      >
        <EyeOffIcon />
      </button>
      <button
        type="button"
        className="kb-card__action kb-card__more"
        data-kb-act="more"
        aria-label={`More actions for ${doc.title || doc.id}`}
        aria-haspopup="dialog"
        aria-expanded={moreOpen}
        title="more actions"
        ref={moreRef}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setMoreOpen(true);
        }}
      >
        <Icon.More />
      </button>
      {moreOpen && (
        <CardActionsSheet doc={doc} actions={actions} close={closeMore} dismiss={dismissMore} restoreFocus={restoreFocus} />
      )}

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
        <h3 className="kb-card__title" ref={titleRef}>
          {doc.title || "(untitled)"}
        </h3>
        {doc.summary && <p className="kb-card__excerpt">{doc.summary}</p>}

        {tags.length > 0 && (
          <div className="kb-card__tags">
            {tags.slice(0, 2).map((t) => (
              <TagPill key={t} tag={t} accent={t === accentTag} />
            ))}
          </div>
        )}

        <div className="kb-card__meta">
          {meta.folder && (
            <span className="kb-card__folder" title={meta.folder}>
              {meta.folder}
            </span>
          )}
          {age && <span className="kb-card__age">{age}</span>}
          {meta.read && <span className="kb-card__read">{meta.read}</span>}
        </div>
          </>
        )}
      </Link>
      {!isSession && progress && (progress.isDone || progress.pct > 0) && (
        <span
          className={`kb-card__progress${progress.isDone ? " kb-card__progress--done" : ""}`}
          role="img"
          title={progress.isDone ? "fully read" : `${progress.pct}% read`}
          aria-label={progress.isDone ? "fully read" : `${progress.pct}% read`}
        >
          <i style={{ width: `${progress.isDone ? 100 : progress.pct}%` }} />
        </span>
      )}
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
