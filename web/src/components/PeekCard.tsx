import { useEffect, useMemo } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchDocByPath, type DocSummary } from "../api/client";
import {
  artifactKbLadder,
  docIdQueryKey,
  fetchDocByIdLadder,
} from "../api/artifactLookup";
import { useKbs } from "../hooks/useKbs";
import { artifactHref } from "../lib/artifactHref";
import type { PaneLoc } from "../lib/paneUrl";
import { relativeAge } from "../lib/time";
import ReadingChip from "./ReadingChip";

/// A plain object rather than a live `DOMRect` — captured once at hover
/// time (`getBoundingClientRect()`), in PARENT-viewport coordinates
/// (matches `position: fixed`). A rect relayed from inside the artifact
/// iframe is iframe-viewport-relative, so the pane adds the iframe's own
/// box offset before handing it here.
export type PeekAnchorRect = {
  top: number;
  left: number;
  bottom: number;
  right: number;
};

/// What the hovered link resolved to — the two artifact-shaped cases of
/// `lib/artifactLinks.ts`'s union (`same-doc` / `external` never open a
/// card). An `artifact-id` target with a null `kb` came off an artifact
/// SUBDOMAIN url, which carries no kb: the ladder below tries the hovering
/// pane's kb first, then the daemon's other corpora.
export type PeekTarget =
  | { kind: "artifact"; kb: string; sourceRelative: string; sec?: string }
  | { kind: "artifact-id"; kb: string | null; id: string; sec?: string };

const CARD_WIDTH = 320;
const MARGIN = 8;

// W2.6b → link-flow — the ONE preview card, with two triggers:
//
//   1. Alt-hover on an SPA-native `a[href^="/a/"]` link. `HotkeyRoot`'s
//      app-level delegate listener owns that trigger (unchanged behaviour).
//   2. Plain hover (≈350ms dwell) on a link INSIDE an artifact iframe,
//      relayed by the daemon runtime's `kb:link-hover` message and driven
//      by `components/reader/ArtifactPane.tsx`.
//
// Both triggers mount THIS component — one home per action (#30). The card
// is interactive (`pointer-events: auto`): the mouse may cross from the
// link onto it (the trigger owner holds a short grace timer and cancels the
// hide on `onHoverIn`), and it carries the two actions a preview wants —
// Open, and Open beside (the reader's `?pane2=` split, desktop only, and
// only when the caller passes `onSplit`).
//
// Data comes from the CANONICAL doc cache keys (#23): the path form reuses
// `["doc", kb, sourceRelative]` — the very entry `Card.tsx`'s hover-prefetch
// and `detail.tsx`'s own doc query populate, so a peek is usually instant
// and never adds an endpoint — and the id form rides `["doc", kb, "by-id",
// id]`, still under the `["doc", kb]` prefix the SSE bridge invalidates.
// Both are `retry: false` and SILENT on failure: a hover that lands on
// something unindexed must never raise a toast.
export default function PeekCard({
  target,
  rect,
  fallbackKb,
  onClose,
  onSplit,
  onHoverIn,
  onHoverOut,
}: {
  target: PeekTarget;
  rect: PeekAnchorRect;
  /// The kb to try FIRST when the target is id-shaped with no kb (the
  /// hovering pane's own corpus). Ignored for path-shaped targets.
  fallbackKb?: string | null;
  /// Dismiss (Escape, an action taken, the trigger's own hide path).
  onClose: () => void;
  /// "Open beside" — omitted ⇒ the button is hidden (mobile, no reader, or
  /// a reader that can't split right now).
  onSplit?: (loc: PaneLoc) => void;
  /// The pointer entered the card: the trigger owner cancels its hide timer.
  onHoverIn?: () => void;
  /// The pointer left the card: the trigger owner re-arms the hide.
  onHoverOut?: () => void;
}) {
  const navigate = useNavigate();
  const kbsQuery = useKbs();
  const kbIds = useMemo(
    () => (kbsQuery.data ?? []).map((k) => k.name),
    [kbsQuery.data],
  );

  const isPath = target.kind === "artifact";
  const pathQuery = useQuery({
    queryKey: [
      "doc",
      isPath ? (target as { kb: string }).kb : null,
      isPath ? (target as { sourceRelative: string }).sourceRelative : null,
    ] as const,
    enabled: isPath,
    retry: false,
    queryFn: ({ signal }) =>
      fetchDocByPath(
        (target as { kb: string }).kb,
        (target as { sourceRelative: string }).sourceRelative,
        signal,
      ),
  });

  // The id ladder (`api/artifactLookup.ts` — shared with the click path, so
  // a click straight after a hover reads the same cache entry): an artifact
  // subdomain names an id but no kb, so try the hovering pane's corpus
  // first and then the rest, in the `["kbs"]` query's stable order.
  const idTarget =
    target.kind === "artifact-id"
      ? (target as { kb: string | null; id: string })
      : null;
  const ladder = useMemo(
    () => artifactKbLadder(idTarget?.kb ?? null, fallbackKb ?? null, kbIds),
    [idTarget?.kb, fallbackKb, kbIds],
  );
  const idQuery = useQuery({
    queryKey: docIdQueryKey(
      idTarget ? (idTarget.kb ?? fallbackKb ?? null) : null,
      idTarget?.id ?? "",
    ),
    enabled: !!idTarget && ladder.length > 0,
    retry: false,
    queryFn: ({ signal }) => fetchDocByIdLadder(ladder, idTarget!.id, signal),
  });

  const doc: DocSummary | null = isPath
    ? (pathQuery.data ?? null)
    : (idQuery.data?.doc ?? null);
  const docKb = isPath
    ? (target as { kb: string }).kb
    : (idQuery.data?.kb ?? null);
  const failed = isPath ? pathQuery.isError : idQuery.isError;

  // Escape dismisses. (A hover card has no focus, so this is a window-level
  // listener — the same shape every other transient overlay uses. Harmless
  // if the trigger owner also closes on Escape: both mean "hide".)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const vw = typeof window !== "undefined" ? window.innerWidth : CARD_WIDTH + MARGIN * 2;
  const vh = typeof window !== "undefined" ? window.innerHeight : 0;
  const left = Math.min(Math.max(MARGIN, rect.left), Math.max(MARGIN, vw - CARD_WIDTH - MARGIN));
  const spaceBelow = vh - rect.bottom;
  const openUpward = spaceBelow < 220 && rect.top > 220;
  const style: React.CSSProperties = openUpward
    ? { left, width: CARD_WIDTH, bottom: Math.max(MARGIN, vh - rect.top + MARGIN) }
    : { left, width: CARD_WIDTH, top: Math.max(MARGIN, rect.bottom + MARGIN) };

  const sec = target.sec;
  const open = () => {
    if (!doc || !docKb) return;
    navigate(artifactHref(docKb, doc.source_relative, sec ? { sec } : undefined));
    onClose();
  };
  const openBeside = () => {
    if (!doc || !docKb || !onSplit) return;
    onSplit(
      sec
        ? { kb: docKb, sourceRelative: doc.source_relative, sec }
        : { kb: docKb, sourceRelative: doc.source_relative },
    );
    onClose();
  };

  const card = (
    <div
      className="kb-peek"
      style={style}
      role="dialog"
      aria-label="artifact preview"
      onMouseEnter={onHoverIn}
      onMouseLeave={onHoverOut}
    >
      {!doc ? (
        <div className="kb-peek__loading">
          {failed ? "not indexed here" : "…"}
        </div>
      ) : (
        <>
          <div className="kb-peek__title">{doc.title || "(untitled)"}</div>
          {(doc.summary || sec) && (
            <p className="kb-peek__summary">
              {sec && <span className="kb-peek__sec">§ {sec}</span>}
              {doc.summary}
            </p>
          )}
          <div className="kb-peek__meta">
            {doc.kb_category && (
              <span className="kb-peek__cat">{doc.kb_category}</span>
            )}
            {doc.folder && <span className="kb-peek__folder">{doc.folder}</span>}
            {doc.mtime_unix != null && (
              <span className="kb-peek__age">{relativeAge(doc.mtime_unix)}</span>
            )}
            {typeof doc.word_count === "number" && doc.word_count > 0 && (
              <span className="kb-peek__words">{doc.word_count.toLocaleString()}w</span>
            )}
            {/* KNOWN GAP (unchanged from W2.6b): `GET .../docs/by-path/{path}`
                never decorates `read_state`/`read_pct` server-side
                (`docs.rs`'s `single_doc_response`), so this chip renders only
                when the cache entry was warmed by a LIST response. Omitted —
                never faked — when the field is absent. */}
            {doc.read_state && (
              <ReadingChip
                pct={doc.read_pct ?? 0}
                isDone={doc.read_state === "read"}
                compact
              />
            )}
          </div>
          {doc.tags && doc.tags.length > 0 && (
            <div className="kb-peek__tags">
              {doc.tags.slice(0, 4).map((t) => (
                <span className="kb-peek__tag" key={t}>
                  {t}
                </span>
              ))}
            </div>
          )}
          <div className="kb-peek__acts">
            <button
              type="button"
              data-kb-act="peek-open"
              className="kb-peek__act"
              onClick={open}
            >
              Open
            </button>
            {onSplit && (
              <button
                type="button"
                data-kb-act="peek-split"
                className="kb-peek__act"
                onClick={openBeside}
                title="open beside — two-pane compare (w v)"
              >
                Open beside
              </button>
            )}
          </div>
        </>
      )}
    </div>
  );

  // Portal like the other floating cards, so the card can never be clipped
  // by a pane's `overflow` or trapped under the reader's stacking context.
  if (typeof document === "undefined") return card;
  return createPortal(card, document.body);
}
