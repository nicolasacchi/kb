import { useEffect, useMemo, useRef } from "react";
import { useCreateAnnotation, usePatchAnnotation, useAnnotations } from "../../hooks/useAnnotations";
import { useCommentKeywords, useCommentsFile } from "../../hooks/useComments";
import {
  claimAnnotationBody,
  deriveBridgeRows,
  freshnessCaption,
  isBridgeable,
  type BridgeRow,
} from "../../lib/comments";
import type { CommentOut } from "../../api/types";
import { toast } from "../../lib/toast";
import SafeMarkdown from "../SafeMarkdown";
import "../../styles/comments.css";

export interface CommentsPanelProps {
  repo: string;
  path: string;
  /// The line a gutter marker click/hover, or `comments.open-card`, last
  /// targeted — the block covering it renders `data-kbc-comment-active`
  /// and is scrolled into view. `null` leaves the list as-is.
  activeLine: number | null;
  onGotoLine: (line: number, lineEnd?: number) => void;
}

const KIND_LABELS: Record<string, string> = {
  doc: "Doc",
  annotation: "Annotation",
  directive: "Directive",
  section: "Section",
  licence: "Licence",
  generated: "Generated",
  commented_code: "Commented-out code",
  prose: "Prose",
};

function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

const STATE_LABELS: Record<string, string> = {
  fresh: "fresh",
  drifted: "drifted",
  unknown: "unknown",
  aged: "aged",
  unreasoned: "unreasoned",
};

function StateChip({ comment }: { comment: CommentOut }) {
  if (comment.state.state === "none") return null;
  const label = STATE_LABELS[comment.state.state] ?? comment.state.state;
  return (
    <span
      className={`kbc-comment-state kbc-comment-state--${comment.state.state}`}
      title={freshnessCaption(comment.state)}
      data-kbc-comment-state={comment.state.state}
    >
      {label}
    </span>
  );
}

function BridgeBadge({
  row,
  onTrack,
  onGotoLine,
  tracking,
}: {
  row: BridgeRow;
  onTrack: (c: CommentOut) => void;
  onGotoLine: (line: number) => void;
  tracking: boolean;
}) {
  if (row.state === "open" && row.comment) {
    return (
      <button
        type="button"
        className="kbc-comment-bridge kbc-comment-bridge--open"
        data-kbc-bridge="open"
        disabled={tracking}
        onClick={() => onTrack(row.comment as CommentOut)}
        title="Create an annotation (intent: claim) tracking this comment"
      >
        {tracking ? "Tracking…" : "Track as annotation"}
      </button>
    );
  }
  if (row.state === "tracked" && row.annotation) {
    return (
      <button
        type="button"
        className="kbc-comment-bridge kbc-comment-bridge--tracked"
        data-kbc-bridge="tracked"
        onClick={() => onGotoLine(row.annotation!.line)}
      >
        tracked
      </button>
    );
  }
  if (row.state === "resolved") {
    return (
      <span className="kbc-comment-bridge kbc-comment-bridge--resolved" data-kbc-bridge="resolved">
        {row.comment ? "resolved" : "resolved (TODO text remains)"}
      </span>
    );
  }
  // "gone" — an orphan: the annotation is still open, its source comment is
  // no longer at that live line. Surfaced, never hidden (the carry-forward
  // ladder's own "an uncertain match is an honest orphan" posture).
  return (
    <button
      type="button"
      className="kbc-comment-bridge kbc-comment-bridge--gone"
      data-kbc-bridge="gone"
      onClick={() => row.annotation && onGotoLine(row.annotation.line)}
      title="This annotation's source comment is gone — the annotation itself is still open"
    >
      orphan
    </button>
  );
}

/// V72-J2 (D8) — the rail's Comments tab: every comments/1 block in the
/// open file, grouped by kind, each carrying its own state chip (with the
/// freshness caption as its `title`) and — for a bridgeable `annotation`
/// row — the claim → annotation bridge's badge/action. A gutter marker
/// click/hover sets `activeLine`, which this panel scrolls to and marks;
/// it renders the WHOLE file's comments regardless of the gutter's own
/// display mode (the gutter mode is a reading filter over the buffer, not
/// a second index) — `all`/`quiet`/`doc-only` narrow what's drawn in the
/// margin, never what's readable here.
export default function CommentsPanel({ repo, path, activeLine, onGotoLine }: CommentsPanelProps) {
  const file = useCommentsFile(repo, path);
  const keywords = useCommentKeywords();
  const annotations = useAnnotations(repo, path);
  const create = useCreateAnnotation(repo, path);
  const patch = usePatchAnnotation(repo, path);
  const activeRef = useRef<HTMLLIElement | null>(null);

  const todoFamily = keywords.data?.todo_family ?? [];
  const comments = file.data?.comments ?? [];
  const claimAnnotations = useMemo(
    () => (annotations.data?.annotations ?? []).filter((a) => a.intent === "claim"),
    [annotations.data],
  );
  const bridgeRows = useMemo(
    () => deriveBridgeRows(comments, todoFamily, claimAnnotations),
    [comments, todoFamily, claimAnnotations],
  );
  const bridgeByLine = useMemo(() => {
    const map = new Map<number, BridgeRow>();
    for (const row of bridgeRows) {
      if (row.comment) map.set(row.comment.line_start, row);
    }
    return map;
  }, [bridgeRows]);
  // `gone` rows carry NO comment (that's what makes them orphans) — they
  // can never attach to a row in the `comments.map(...)` list below, so
  // they get their OWN section instead. Surfaced, never hidden: the
  // carry-forward ladder's "an uncertain match is an honest orphan" rule.
  const orphanRows = useMemo(() => bridgeRows.filter((r) => r.state === "gone"), [bridgeRows]);

  useEffect(() => {
    if (activeLine !== null) activeRef.current?.scrollIntoView({ block: "center" });
  }, [activeLine]);

  async function handleTrack(c: CommentOut) {
    try {
      await create.mutateAsync({
        repo,
        path,
        line: c.line_start,
        body: claimAnnotationBody(c),
        intent: "claim",
      });
    } catch (e) {
      toast.err(e instanceof Error ? e.message : "failed to create the tracking annotation");
    }
  }

  if (file.isLoading) return <div className="kbc-inspector__hint">Loading…</div>;
  if (file.error) {
    return <div className="kbc-inspector__hint">{(file.error as Error).message}</div>;
  }
  if (comments.length === 0 && orphanRows.length === 0) {
    return <div className="kbc-inspector__hint">No comments indexed for this file.</div>;
  }

  return (
    <div className="kbc-comments-panel" data-kbc-comments-panel>
      {(file.data?.notes ?? []).map((n) => (
        <p key={n} className="kbc-comments-panel__note" data-kbc-comments-note>
          {n}
        </p>
      ))}
      {orphanRows.length > 0 && (
        <ul className="kbc-comments-panel__list" data-kbc-comments-orphans>
          {orphanRows.map((row) => (
            <li key={row.annotation!.id} className="kbc-comments-panel__row">
              <div className="kbc-comments-panel__kind" data-kbc-comment-kind="orphan">
                Orphaned claim
              </div>
              <div className="kbc-comments-panel__text">
                <SafeMarkdown text={row.annotation!.body} fences />
              </div>
              <BridgeBadge
                row={row}
                onTrack={handleTrack}
                onGotoLine={onGotoLine}
                tracking={create.isPending}
              />
              <button
                type="button"
                className="kbc-comments-panel__resolve"
                onClick={() => patch.mutate({ id: row.annotation!.id, input: { resolved: true } })}
                data-kbc-comment-resolve-orphan
              >
                Resolve orphan
              </button>
            </li>
          ))}
        </ul>
      )}
      <ul className="kbc-comments-panel__list">
        {comments.map((c) => {
          const isActive =
            activeLine !== null && activeLine >= c.line_start && activeLine <= Math.max(c.line_end, c.line_start);
          const bridgeable = isBridgeable(c, todoFamily);
          const row = bridgeable ? bridgeByLine.get(c.line_start) : undefined;
          return (
            <li
              key={`${c.path}:${c.line_start}:${c.kind}`}
              ref={isActive ? activeRef : undefined}
              className={"kbc-comments-panel__row" + (isActive ? " is-active" : "")}
              data-kbc-comment-row={c.line_start}
              {...(isActive ? { "data-kbc-comment-active": "1" } : {})}
            >
              <button
                type="button"
                className="kbc-comments-panel__jump"
                onClick={() => onGotoLine(c.line_start, c.line_end > c.line_start ? c.line_end : undefined)}
                data-kbc-comments-jump
              >
                <span className="kbc-comments-panel__kind" data-kbc-comment-kind={c.kind}>
                  {kindLabel(c.kind)}
                </span>
                {c.keyword && <span className="kbc-comments-panel__keyword">{c.keyword}</span>}
                <span className="kbc-comments-panel__line">:{c.line_start}</span>
              </button>
              <StateChip comment={c} />
              {c.symbol && (
                <button
                  type="button"
                  className="kbc-comments-panel__symbol"
                  onClick={() => onGotoLine(c.symbol!.line_start)}
                  title="Jump to the documented symbol"
                  data-kbc-comment-symbol
                >
                  → {c.symbol.name}
                </button>
              )}
              <div className="kbc-comments-panel__text">
                <SafeMarkdown text={c.text} fences />
              </div>
              {/* `row` here is never `state: "gone"` — a `gone` row carries
                  no `comment` by construction, so it can never be looked up
                  by THIS comment's own line; those render in the dedicated
                  orphans section above instead. */}
              {row && (
                <BridgeBadge
                  row={row}
                  onTrack={handleTrack}
                  onGotoLine={onGotoLine}
                  tracking={create.isPending}
                />
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
