import { useRef, useState } from "react";
import type { CSSProperties, TouchEvent as RTouchEvent } from "react";
import { useNavigate } from "react-router-dom";
import type { InboxItem } from "../api/inbox";
import { artifactHref } from "../lib/artifactHref";
import { useConfirm } from "./ConfirmProvider";
import { toast } from "../lib/toast";
import { Icon } from "./icons";
import {
  clampSwipeDelta,
  resolveSwipeAction,
  swipeHintStrength,
} from "./inboxSwipe";

// W1.mobile — inbox triage: per-item actions on every comment row (resolve /
// reply), a per-artifact archive (the existing exclusion, one per card
// header, not per comment), and two mobile-only affordances layered on top
// of the SAME three actions: a thumb-zone action rail (large touch targets,
// appears while a row is "active") and swipe-to-reveal on the row itself.
//
// The per-row buttons below are the e2e contract (`data-kb-act="inbox-
// resolve"`/`"inbox-reply"`/`"inbox-archive"`) — always rendered, desktop
// AND mobile. Swipe and the rail are enhancements that end up calling the
// exact same handlers; neither is required to drive an action in a test.
// The rail's own buttons carry `-rail` suffixed data-kb-act names so a
// selector never has to disambiguate two live matches for the same target.

export type InboxTarget = {
  kb: string;
  artifactId: string;
  commentId: string;
  sourceRelative: string | null;
  title: string;
};

/// Build the reply deep-link: the reader with the comments panel pre-opened
/// AND the specific comment named. `artifactHref`'s `panel` option is owned
/// by `lib/artifactHref.ts` (out of this phase's file ownership); the
/// `comment` query param is this phase's own addition to the same URL, so
/// it's appended here rather than growing `ArtifactHrefOpts`.
export function inboxReplyHref(
  kb: string,
  sourceRelative: string,
  commentId: string,
): string {
  const base = artifactHref(kb, sourceRelative, { panel: "comments" });
  const sep = base.includes("?") ? "&" : "?";
  return `${base}${sep}comment=${encodeURIComponent(commentId)}`;
}

// eye-off — the same "hide this from the index" glyph Card.tsx's exclude
// action uses, reused verbatim so archive reads as the same verb everywhere.
function ArchiveGlyph() {
  return (
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
  );
}

function archiveConfirmCopy(title: string) {
  return {
    title: "Archive from the index?",
    body: `Archive “${title}” out of search and the gallery? Comments and reading history survive — re-include it any time from Settings → Excluded.`,
    confirmLabel: "Archive",
  };
}

/// Per-artifact "archive artifact" button — lives in the card header, NOT
/// per comment (a card may have several open comments; archiving is a
/// per-file decision). Reuses `excludeArtifact` via the `onArchive` prop
/// (threaded from `useInbox().archiveArtifact`), confirm-guarded (#32).
export function ArchiveArtifactButton({
  kb,
  artifactId,
  sourceRelative,
  title,
  onArchive,
}: {
  kb: string;
  artifactId: string;
  sourceRelative: string | null;
  title: string;
  onArchive: (
    kb: string,
    artifactId: string,
    sourceRelative: string,
  ) => Promise<void>;
}) {
  const confirm = useConfirm();
  if (!sourceRelative) return null; // no path to exclude — dead artifact row
  return (
    <button
      type="button"
      className="inbox-card__archive"
      data-kb-act="inbox-archive"
      aria-label="archive artifact"
      title="archive artifact — comments + reading history survive; re-include from Settings → Excluded"
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        void (async () => {
          const ok = await confirm(archiveConfirmCopy(title));
          if (!ok) return;
          try {
            await onArchive(kb, artifactId, sourceRelative);
            toast.ok("archived — reversible in Settings → Excluded");
          } catch (err) {
            toast.err(
              `archive failed: ${err instanceof Error ? err.message : String(err)}`,
            );
          }
        })();
      }}
    >
      <ArchiveGlyph />
    </button>
  );
}

export function InboxCommentRow({
  kb,
  artifactId,
  sourceRelative,
  item,
  active,
  swipeEnabled,
  onActivate,
  onResolve,
}: {
  kb: string;
  artifactId: string;
  sourceRelative: string | null;
  item: InboxItem;
  active: boolean;
  /// Gates BOTH tap-to-activate and the swipe gesture — mobile-only
  /// (`useIsMobile()` at the route), so desktop keeps today's exact DOM
  /// (buttons render there too, just never gain the touch/click wiring).
  swipeEnabled: boolean;
  onActivate: (commentId: string | null) => void;
  onResolve: (kb: string, artifactId: string, commentId: string) => Promise<void>;
}) {
  const navigate = useNavigate();
  const [dragX, setDragX] = useState(0);
  const [dragging, setDragging] = useState(false);
  const touchRef = useRef<{ x: number; y: number; locked: boolean } | null>(null);

  function doResolve() {
    void onResolve(kb, artifactId, item.comment_id).catch(() =>
      toast.err("Couldn't resolve comment"),
    );
  }

  function doReply() {
    if (!sourceRelative) return;
    onActivate(null);
    navigate(inboxReplyHref(kb, sourceRelative, item.comment_id));
  }

  function resetDrag() {
    touchRef.current = null;
    setDragging(false);
    setDragX(0);
  }

  function onTouchStart(e: RTouchEvent<HTMLLIElement>) {
    const t = e.touches[0];
    touchRef.current = { x: t.clientX, y: t.clientY, locked: false };
    setDragging(true);
  }

  function onTouchMove(e: RTouchEvent<HTMLLIElement>) {
    const start = touchRef.current;
    if (!start) return;
    const t = e.touches[0];
    const dx = t.clientX - start.x;
    const dy = t.clientY - start.y;
    if (!start.locked) {
      // Don't commit to an axis until the drag is unambiguous — a mostly
      // vertical drag is a page scroll, never hijacked into a swipe.
      if (Math.abs(dx) < 10 && Math.abs(dy) < 10) return;
      if (Math.abs(dy) > Math.abs(dx)) {
        resetDrag();
        return;
      }
      start.locked = true;
    }
    setDragX(clampSwipeDelta(dx));
  }

  function onTouchEnd() {
    const action = resolveSwipeAction(dragX);
    resetDrag();
    if (action === "resolve") doResolve();
    else if (action === "reply") doReply();
  }

  const resolveStrength = dragX > 0 ? swipeHintStrength(dragX) : 0;
  const replyStrength = dragX < 0 ? swipeHintStrength(dragX) : 0;
  const bodyStyle: CSSProperties | undefined = swipeEnabled
    ? {
        transform: dragX ? `translateX(${dragX}px)` : undefined,
        transition: dragging ? "none" : undefined,
      }
    : undefined;

  return (
    <li
      className={`inbox-comment${active ? " inbox-comment--active" : ""}`}
      onClick={swipeEnabled ? () => onActivate(active ? null : item.comment_id) : undefined}
      onTouchStart={swipeEnabled ? onTouchStart : undefined}
      onTouchMove={swipeEnabled ? onTouchMove : undefined}
      onTouchEnd={swipeEnabled ? onTouchEnd : undefined}
      onTouchCancel={swipeEnabled ? resetDrag : undefined}
    >
      {swipeEnabled && (
        <>
          <span
            className="inbox-comment__hint inbox-comment__hint--resolve"
            aria-hidden="true"
            style={{ opacity: resolveStrength }}
          >
            <Icon.Check /> resolve
          </span>
          <span
            className="inbox-comment__hint inbox-comment__hint--reply"
            aria-hidden="true"
            style={{ opacity: replyStrength }}
          >
            reply <Icon.Comment />
          </span>
        </>
      )}
      <div className="inbox-comment__body" style={bodyStyle}>
        <span className={`inbox-comment__author inbox-comment__author--${item.author}`}>
          {item.author}
        </span>
        <span className="inbox-comment__excerpt">{item.excerpt}</span>
        <span className="inbox-comment__meta">
          {item.stale && (
            <span className="inbox-comment__stale" title="anchor is stale">
              ⚠
            </span>
          )}
          {item.reply_count > 0 && (
            <span className="inbox-comment__replies">
              {item.reply_count} repl{item.reply_count === 1 ? "y" : "ies"}
            </span>
          )}
          <span className="inbox-comment__anchor">{item.anchor}</span>
        </span>
        <span className="inbox-comment__actions">
          <button
            type="button"
            className="inbox-comment__act inbox-comment__act--resolve"
            data-kb-act="inbox-resolve"
            aria-label="resolve comment"
            title="resolve"
            onClick={(e) => {
              e.stopPropagation();
              doResolve();
            }}
          >
            <Icon.Check />
          </button>
          <button
            type="button"
            className="inbox-comment__act inbox-comment__act--reply"
            data-kb-act="inbox-reply"
            aria-label="reply to comment"
            title={sourceRelative ? "reply" : "artifact no longer indexed"}
            disabled={!sourceRelative}
            onClick={(e) => {
              e.stopPropagation();
              doReply();
            }}
          >
            <Icon.Comment />
          </button>
        </span>
      </div>
    </li>
  );
}

/// Mobile thumb-zone rail — a fixed bottom bar with large (≥44px) copies of
/// the same three actions, shown while `target` is set (a row tapped
/// active). Mounted only via `useIsMobile()` at the route, so desktop DOM
/// never gains this element.
export function InboxActionRail({
  target,
  onResolve,
  onArchive,
  onDismiss,
}: {
  target: InboxTarget | null;
  onResolve: (kb: string, artifactId: string, commentId: string) => Promise<void>;
  onArchive: (
    kb: string,
    artifactId: string,
    sourceRelative: string,
  ) => Promise<void>;
  onDismiss: () => void;
}) {
  const navigate = useNavigate();
  const confirm = useConfirm();
  if (!target) return null;
  const { kb, artifactId, commentId, sourceRelative, title } = target;

  return (
    <div className="inbox-rail" data-testid="inbox-thumb-rail" role="toolbar" aria-label="comment actions">
      <button
        type="button"
        className="inbox-rail__btn inbox-rail__btn--resolve"
        data-kb-act="inbox-resolve-rail"
        aria-label="resolve comment"
        onClick={() => {
          onDismiss();
          void onResolve(kb, artifactId, commentId).catch(() =>
            toast.err("Couldn't resolve comment"),
          );
        }}
      >
        <Icon.Check />
        <span>resolve</span>
      </button>
      <button
        type="button"
        className="inbox-rail__btn inbox-rail__btn--reply"
        data-kb-act="inbox-reply-rail"
        aria-label="reply to comment"
        disabled={!sourceRelative}
        onClick={() => {
          if (!sourceRelative) return;
          onDismiss();
          navigate(inboxReplyHref(kb, sourceRelative, commentId));
        }}
      >
        <Icon.Comment />
        <span>reply</span>
      </button>
      <button
        type="button"
        className="inbox-rail__btn inbox-rail__btn--archive"
        data-kb-act="inbox-archive-rail"
        aria-label="archive artifact"
        disabled={!sourceRelative}
        onClick={() => {
          if (!sourceRelative) return;
          void (async () => {
            const ok = await confirm(archiveConfirmCopy(title));
            if (!ok) return;
            onDismiss();
            try {
              await onArchive(kb, artifactId, sourceRelative);
              toast.ok("archived — reversible in Settings → Excluded");
            } catch (err) {
              toast.err(
                `archive failed: ${err instanceof Error ? err.message : String(err)}`,
              );
            }
          })();
        }}
      >
        <ArchiveGlyph />
        <span>archive</span>
      </button>
    </div>
  );
}
