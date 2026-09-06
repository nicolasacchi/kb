import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ClipboardEvent as RClipboardEvent,
  DragEvent as RDragEvent,
  RefObject,
} from "react";
import type {
  Anchor,
  Choice,
  Comment,
  Reply,
  ReviewFile,
  Verdict,
  VerdictState,
} from "../api/client";
import { toast } from "../lib/toast";
import { Icon } from "./icons";
import CommentBody from "./CommentBody";
import CommentModal from "./CommentModal";
import MarkdownEditor, { type MarkdownEditorHandle } from "./LazyMarkdownEditor";
import AttachmentStrip from "./AttachmentStrip";
import AttachmentLightbox from "./AttachmentLightbox";
import ComposerAttachBar, {
  type ComposerAttachBarHandle,
} from "./ComposerAttachBar";
import {
  useComposerAttachments,
  type ComposerAttachments,
} from "../hooks/useComposerAttachments";
import { CANNED, QuickButtons, threadAwaitsResponse } from "./QuickButtons";
import { useConfirm } from "./ConfirmProvider";
import UserChip from "./UserChip";
import { relTime, anchorLabel } from "../lib/commentFmt";
import { canEditComment } from "../lib/canEditComment";
import { fetchArtifactHtml, importReview } from "../api/client";
import { embedReviewIntoHtml, extractReviewFromHtml } from "../lib/reviewEmbed";
import { buildCiteMarkdown } from "../lib/quote";
import { stableAnchorKey, useDraft } from "../lib/drafts";
import { useIdentity } from "../hooks/useArtifactHost";
import { useIsMobile } from "../hooks/useIsMobile";
import { useReview } from "../hooks/useReview";

// Right-side 360px panel — reads a kb-comments/1 ReviewFile + drives the
// R7 fine-grained mutation actions from useReview. Each action posts a
// small delta to its dedicated endpoint; the daemon owns the mutation and
// the server assigns comment/reply ids (the panel never mints them).
//
// File-scope comments are added via the prominent button at the top.
// Block / chapter / section / selection comments come from the iframe
// annotator (B1) — this panel just renders + manages them. Y-track adds
// attachments: each composer can stage files (📎 + drag/drop + paste),
// dropping an `attachment:` ref into the draft; the strip + inline embeds
// render under each comment/reply.

type Filter = "open" | "resolved" | "all";

export type CommentsPanelProps = {
  kb: string;
  artifactId: string;
  /// W1.reader — `doc.source_relative`, needed to build a shareable citation
  /// permalink for a comment (lib/quote.ts) via the same path-based
  /// permalink every other artifact deep-link uses (Track U). The panel is
  /// otherwise doc-agnostic (everything else keys on `artifactId`).
  sourceRelative: string;
  file: ReviewFile;
  loading: boolean;
  error: string | null;
  staleCommentIds: Set<string>;
  onAddComment: (
    anchor: Anchor,
    body: string,
    opts?: { author?: "you" | "claude"; choices?: Choice[]; attachmentIds?: string[] },
  ) => Promise<Comment>;
  onAddReply: (
    commentId: string,
    author: "you" | "claude",
    body: string,
    opts?: { attachmentIds?: string[] },
  ) => Promise<Reply>;
  onResolveComment: (commentId: string) => Promise<void>;
  onUnresolveComment: (commentId: string) => Promise<void>;
  onEditComment: (commentId: string, body: string) => Promise<void>;
  onDeleteComment: (commentId: string) => Promise<void>;
  /// Y-track — detach an attachment from a posted comment / reply.
  onDetachAttachment: (commentId: string, aid: string) => void;
  onDetachReplyAttachment: (
    commentId: string,
    replyId: string,
    aid: string,
  ) => void;
  /// Tell the iframe annotator to scroll/flash a marker. Omitted on the
  /// native note view (no iframe) → the per-row "jump" button is hidden.
  onRequestFlash?: (commentId: string) => void;
  /// Hover feedback — glow the comment's in-page highlight (null = off).
  /// Omitted (no-op) on the native note view.
  onHoverComment?: (commentId: string | null) => void;
  /// Comment whose row should be marked active + scrolled into view —
  /// set when the user clicks that comment's in-page marker.
  activeCommentId: string | null;
  /// Anchor the user clicked inside the artifact (cm:compose). When set,
  /// the panel shows an inline composer (the shared tabbed editor) for a
  /// new comment on that anchor, in place of the file-scope box.
  composeAnchor?: Anchor | null;
  onCloseCompose?: () => void;
  /// Iframe annotate-mode toggle. Omitted on the native note view (no
  /// iframe annotator) → the header's ✎ annotate button is hidden.
  annotateMode?: boolean;
  onToggleAnnotate?: () => void;
  /// W2.13 — errata-slip display toggle: renders OPEN comments as a
  /// numbered correction sheet (`ErrataSheet`) pinned over the reader's top
  /// margin, a sibling of the iframe owned by detail.tsx. This panel only
  /// hosts the toggle button (one home per action, #30) — the sheet itself
  /// is a separate component detail.tsx mounts. Omitted on the native note
  /// view (no iframe to pin the sheet over) → the header's toggle is hidden,
  /// same convention as `onToggleAnnotate`/`onRequestFlash` above.
  errataMode?: boolean;
  onToggleErrata?: () => void;
  /// R4 — "comment on selection", the PULL half of the selection→comment
  /// path. Asks the iframe annotator what is selected right now
  /// (`cm:selection-query`) and opens this panel's composer on the answer.
  ///
  /// Why a second way in when SelectionActions already has a comment button:
  /// that button rides the PUSH relay, which only stays on screen for as
  /// long as the engine keeps the selection alive. Gecko collapses a
  /// selection BEFORE the `click` that caused it (a W3C-documented ordering
  /// divergence from Blink, not a bug), so on Firefox for Android the bottom
  /// bar could unmount under the user's own finger and the tap landed on
  /// nothing. Pulling depends on no relayed event at all: the user selects,
  /// opens the sheet, taps here, and annotate.ts answers from the live
  /// selection or its cached anchor.
  ///
  /// Why the button lives INSIDE this panel rather than in the ContextBar:
  /// invariant #30 — on mobile the reader chrome collapses to the single
  /// `inspect` entry and ONE sheet hosts inspector/comments/versions. A
  /// second ContextBar icon would reopen exactly the "two homes for one
  /// action" the v0.23 rework closed; the composer's own panel is where a
  /// compose verb belongs.
  ///
  /// Omitted on the native-note path (no annotator to query) → hidden, the
  /// same convention as `onRequestFlash` / `onToggleAnnotate` above.
  onCommentOnSelection?: () => void;
};

/// Paste / drag-drop wiring for a composer container: stage dropped/pasted
/// files + insert each token at the editor's cursor. Shared by every composer.
function dndProps(
  att: ComposerAttachments,
  editorRef: RefObject<MarkdownEditorHandle | null>,
) {
  const onToken = (t: string) => editorRef.current?.insertAtCursor(t);
  return {
    onPaste: (e: RClipboardEvent) => {
      const files = Array.from(e.clipboardData?.files ?? []);
      if (files.length) {
        e.preventDefault();
        att.upload(files, onToken);
      }
    },
    onDragOver: (e: RDragEvent) => e.preventDefault(),
    onDrop: (e: RDragEvent) => {
      e.preventDefault();
      const files = Array.from(e.dataTransfer?.files ?? []);
      if (files.length) att.upload(files, onToken);
    },
  };
}

export default function CommentsPanel({
  kb,
  artifactId,
  sourceRelative,
  file,
  loading,
  error,
  staleCommentIds,
  onAddComment,
  onAddReply,
  onResolveComment,
  onUnresolveComment,
  onEditComment,
  onDeleteComment,
  onDetachAttachment,
  onDetachReplyAttachment,
  onRequestFlash,
  onHoverComment,
  activeCommentId,
  composeAnchor,
  onCloseCompose,
  annotateMode,
  onToggleAnnotate,
  errataMode,
  onToggleErrata,
  onCommentOnSelection,
}: CommentsPanelProps) {
  // R4 — the pull affordance is MOBILE-ONLY. On desktop the rect-anchored
  // SelectionActions floater is reliable (a mouse selection survives the
  // click that follows it on every engine), and #30's "one home per action"
  // says don't grow a second door next to a working one.
  const isMobile = useIsMobile();
  const [filter, setFilter] = useState<Filter>("open");
  // A-SPA — durable drafts (lib/drafts.ts): each composer's text is
  // hydrated from / persisted to localStorage per (kb, artifactId, slot),
  // debounced, so a reload / crashed tab / accidental nav doesn't lose an
  // in-progress comment. `draft`/`setDraft`/`composeDraft`/`setComposeDraft`
  // keep their old names — every read site below is unchanged.
  const fileDraft = useDraft(kb, artifactId, "file");
  const draft = fileDraft.text;
  const setDraft = fileDraft.setText;
  const composeSlot = composeAnchor
    ? `compose:${stableAnchorKey(composeAnchor)}`
    : null;
  const composeDraftStore = useDraft(kb, artifactId, composeSlot);
  const composeDraft = composeDraftStore.text;
  const setComposeDraft = composeDraftStore.setText;
  const [exportOpen, setExportOpen] = useState(false);
  const importInputRef = useRef<HTMLInputElement | null>(null);
  const [importErr, setImportErr] = useState<string | null>(null);
  const [modalId, setModalId] = useState<string | null>(null); // expanded view
  const [lightbox, setLightbox] = useState<{ url: string; alt: string } | null>(
    null,
  );
  const openImage = (url: string, alt: string) => setLightbox({ url, alt });
  const confirm = useConfirm();

  // W2.15a — the verdict mutation. Threading it as a prop from the
  // parent's own useReview(kb, artifactId) call (routes/detail.tsx) is out
  // of this phase's file ownership; calling the hook again here is safe —
  // both instances share the same TanStack Query cache entry
  // (["review", kb, artifactId]), so this is a second subscriber, not a
  // second fetch (staleTime: Infinity, the entry is already populated by
  // the parent's call). `file`/`loading`/`error`/the other mutations still
  // come from the parent as props, unchanged.
  const { setVerdict } = useReview(kb, artifactId);
  function handleSetVerdict(v: { state: VerdictState; note?: string } | null) {
    void setVerdict(v).catch(() => toast.err("Couldn't save verdict"));
  }

  // Portable round-trip — read an HTML file that carries an embedded
  // `kb-review-state` block (produced by "export with comments" or
  // `kb comments export --embed`) and POST it to the import endpoint. The
  // daemon emits `comments.updated`, so the SSE bridge refreshes this panel.
  async function onImportFile(f: File) {
    setImportErr(null);
    try {
      const incoming = extractReviewFromHtml(await f.text());
      if (!incoming) {
        setImportErr("No embedded comments found in that file.");
        return;
      }
      const existing = file.comments.length;
      let force = false;
      if (existing > 0) {
        const ok = await confirm({
          title: "Replace existing comments?",
          body: `This artifact already has ${existing} comment(s). Importing replaces them with ${incoming.comments.length} comment(s) from the file.`,
          confirmLabel: "Replace",
        });
        if (!ok) return;
        force = true;
      }
      await importReview(kb, artifactId, incoming, force);
    } catch (e) {
      setImportErr(e instanceof Error ? e.message : "import failed");
    }
  }

  // Per-composer attachment staging + editor refs (for cursor insertion).
  const fileScopeEditor = useRef<MarkdownEditorHandle | null>(null);
  const composeEditor = useRef<MarkdownEditorHandle | null>(null);
  const fileScopeBar = useRef<ComposerAttachBarHandle | null>(null);
  const composeBar = useRef<ComposerAttachBarHandle | null>(null);
  const fileScopeAtt = useComposerAttachments(kb, artifactId);
  const composeAtt = useComposerAttachments(kb, artifactId);
  const attach = useMemo(() => ({ kb, id: artifactId }), [kb, artifactId]);

  const counts = useMemo(() => countByStatus(file.comments), [file.comments]);
  const grouped = useMemo(
    () => groupByFile(filterComments(file.comments, filter)),
    [file.comments, filter],
  );

  // A fresh artifact click brings a new anchor → drop any staged
  // attachments (they don't persist the way text drafts do; abandoning a
  // compose already leaves them to the server's GC, invariant #18). The
  // TEXT itself no longer needs an explicit reset here: `useDraft` above
  // re-hydrates per `composeSlot`, which already reads "" for an anchor
  // that's never been composed on — and, load-bearing for the durability
  // story, re-reads whatever was persisted for an anchor that HAS (the
  // outgoing anchor's own debounced write already landed in storage
  // before this fires, so switching back to it later restores the text
  // even though this component's in-memory state moved on).
  useEffect(() => {
    composeAtt.reset();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [composeAnchor]);

  // Add a new comment on `anchor` via the fine-grained endpoint. Shared
  // by the file-scope box and the routed-anchor composer.
  function addComment(anchor: Anchor, body: string, attachmentIds?: string[]) {
    const text = body.trim();
    if (!text && !(attachmentIds && attachmentIds.length > 0)) return;
    void onAddComment(anchor, text, { attachmentIds }).catch(() =>
      toast.err("Couldn't add comment"),
    );
  }

  function addFileScope() {
    if (!draft.trim() && fileScopeAtt.attachmentIds.length === 0) return;
    addComment({ kind: "file" }, draft, fileScopeAtt.attachmentIds);
    fileDraft.clear();
    fileScopeAtt.reset();
  }

  function addRoutedComment() {
    if (!composeAnchor) return;
    if (!composeDraft.trim() && composeAtt.attachmentIds.length === 0) return;
    addComment(composeAnchor, composeDraft, composeAtt.attachmentIds);
    composeDraftStore.clear();
    composeAtt.reset();
    onCloseCompose?.();
  }

  // Discard the routed composer, confirming first if there's unsaved text.
  async function discardCompose() {
    if (
      composeDraft.trim() &&
      !(await confirm({
        title: "Discard this comment?",
        body: "Your unsaved comment draft will be lost.",
        confirmLabel: "Discard",
      }))
    )
      return;
    composeDraftStore.clear();
    composeAtt.reset();
    onCloseCompose?.();
  }

  function toggleResolved(commentId: string, status: "open" | "resolved") {
    const fn = status === "open" ? onResolveComment : onUnresolveComment;
    void fn(commentId).catch(() =>
      toast.err(
        status === "open"
          ? "Couldn't resolve comment"
          : "Couldn't reopen comment",
      ),
    );
  }

  async function removeComment(commentId: string) {
    // Deleting a comment thread drops the whole thread (replies included) and
    // can't be undone — confirm before the irreversible mutation.
    const ok = await confirm({
      title: "Delete this comment?",
      body: "The comment and all its replies will be permanently removed.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    void onDeleteComment(commentId).catch(() =>
      toast.err("Couldn't delete comment"),
    );
  }

  function addReply(
    commentId: string,
    author: "you" | "claude",
    body: string,
    attachmentIds?: string[],
  ) {
    const text = body.trim();
    if (!text && !(attachmentIds && attachmentIds.length > 0)) return;
    void onAddReply(commentId, author, text, { attachmentIds }).catch(() =>
      toast.err("Couldn't add reply"),
    );
  }

  // R3 — a quick-response button click: append a "you" reply to the
  // comment (and resolve it when the choice says so).
  function respond(commentId: string, body: string, resolve: boolean) {
    const text = body.trim();
    if (!text) return;
    void (async () => {
      await onAddReply(commentId, "you", text);
      if (resolve) await onResolveComment(commentId);
    })().catch(() => toast.err("Couldn't post response"));
  }

  // Edit a comment's body in place (own comments only — gated in the UI).
  function editCommentBody(commentId: string, body: string) {
    const text = body.trim();
    if (!text) return;
    void onEditComment(commentId, text).catch(() =>
      toast.err("Couldn't save edit"),
    );
  }

  const modalComment = modalId
    ? file.comments.find((c) => c.id === modalId) ?? null
    : null;
  // W1.reader — same title fallback as buildClaudePrompt/buildMarkdown below.
  const docTitle = file.artifact.title || file.artifact.id;

  return (
    <aside className="comments-panel" aria-label="comments">
      <header className="cp__head">
        <div
          className="cp__filter"
          role="tablist"
          aria-label="comment status filter"
        >
          {(["open", "resolved", "all"] as Filter[]).map((f) => (
            <button
              key={f}
              role="tab"
              aria-selected={filter === f}
              className={`cp__filter-btn ${filter === f ? "is-active" : ""}`}
              onClick={() => setFilter(f)}
            >
              {f} ({f === "all" ? file.comments.length : counts[f]})
            </button>
          ))}
        </div>
        <div className="cp__head-actions">
          {onToggleAnnotate && (
            <button
              className={`cp__annot ${annotateMode ? "is-active" : ""}`}
              aria-pressed={annotateMode}
              onClick={onToggleAnnotate}
              title="toggle annotate mode"
            >
              ✎ {annotateMode ? "on" : "annotate"}
            </button>
          )}
          {onToggleErrata && (
            <button
              data-kb-act="errata-toggle"
              className={`cp__errata ${errataMode ? "is-active" : ""}`}
              aria-pressed={errataMode}
              onClick={onToggleErrata}
              title="show open comments as a numbered errata slip over the reader"
            >
              ¶ errata
            </button>
          )}
          {/* v0.22 — the redundant ✕ is gone: the rail's comments icon now
              toggles the panel (click again to close). */}
        </div>
        <VerdictStrip
          verdict={file.verdict}
          openCount={counts.open}
          onSet={handleSetVerdict}
          onShowOpen={() => setFilter("open")}
        />
      </header>

      {error && (
        <div className="cp__banner cp__banner--err" role="alert">
          {error}
        </div>
      )}
      {loading && <div className="cp__hint">loading…</div>}

      {/* R4 — the pull-based "comment on selection" entry. Mobile-only (see
          the `onCommentOnSelection` prop doc for why it lives here and not in
          the ContextBar), and hidden while a composer is already open: a pull
          REPLACES `composeAnchor`, so offering it above a half-typed draft
          would silently re-target that draft at different text. Sits directly
          above the composer slot it fills, so the cause and its effect are
          adjacent. */}
      {isMobile && onCommentOnSelection && !composeAnchor && (
        <button
          type="button"
          data-kb-act="comment-selection-pull"
          className="cp__sel-pull"
          onClick={onCommentOnSelection}
          title="comment on the text you selected in the artifact"
        >
          <Icon.Comment aria-hidden="true" /> comment on selection
        </button>
      )}

      {composeAnchor ? (
        <section
          className="cp__compose"
          aria-label="new comment"
          {...dndProps(composeAtt, composeEditor)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey) {
              e.preventDefault();
              e.stopPropagation();
              discardCompose();
            }
          }}
        >
          <div className="cp__compose-label">
            commenting on: {anchorLabel({ anchor: composeAnchor })}
          </div>
          <MarkdownEditor
            ref={composeEditor}
            value={composeDraft}
            onChange={setComposeDraft}
            placeholder="add a comment… (markdown supported)"
            ariaLabel="new comment body"
            textareaClassName="cp__file-scope-input"
            autoFocus
            attachment={attach}
            onSubmit={addRoutedComment}
            onAttach={() => composeBar.current?.open()}
            renderPreview={(v) => (
              <CommentBody body={v} kb={kb} id={artifactId} />
            )}
          />
          <ComposerAttachBar
            ref={composeBar}
            att={composeAtt}
            compact
            onToken={(t) => composeEditor.current?.insertAtCursor(t)}
          />
          <div className="cp__compose-actions">
            <button className="cp__compose-cancel" onClick={discardCompose}>
              cancel
            </button>
            <button
              className="cp__compose-add"
              onClick={addRoutedComment}
              disabled={
                (!composeDraft.trim() &&
                  composeAtt.attachmentIds.length === 0) ||
                composeAtt.uploading
              }
            >
              add comment
            </button>
          </div>
        </section>
      ) : (
        <section
          className="cp__file-scope"
          {...dndProps(fileScopeAtt, fileScopeEditor)}
        >
          <MarkdownEditor
            ref={fileScopeEditor}
            value={draft}
            onChange={setDraft}
            placeholder="comment on the whole artifact…"
            ariaLabel="file-scope comment"
            textareaClassName="cp__file-scope-input"
            attachment={attach}
            onSubmit={addFileScope}
            onAttach={() => fileScopeBar.current?.open()}
            renderPreview={(v) => (
              <CommentBody body={v} kb={kb} id={artifactId} />
            )}
          />
          <ComposerAttachBar
            ref={fileScopeBar}
            att={fileScopeAtt}
            compact
            onToken={(t) => fileScopeEditor.current?.insertAtCursor(t)}
          />
          <button
            className="cp__file-scope-btn"
            onClick={addFileScope}
            disabled={
              (!draft.trim() && fileScopeAtt.attachmentIds.length === 0) ||
              fileScopeAtt.uploading
            }
          >
            add file-scope
          </button>
        </section>
      )}

      <div className="cp__list">
        {grouped.length === 0 && (
          <div className="cp__empty">
            no {filter === "all" ? "" : filter} comments
          </div>
        )}
        {grouped.map(({ file: gf, label, items }) => (
          <section key={gf} className="cp__group" aria-label={label}>
            <h3 className="cp__group-h">{label}</h3>
            {items.map((c) => (
              <CommentRow
                key={c.id}
                kb={kb}
                artifactId={artifactId}
                sourceRelative={sourceRelative}
                docTitle={docTitle}
                comment={c}
                stale={staleCommentIds.has(c.id)}
                active={c.id === activeCommentId}
                onJump={onRequestFlash ? () => onRequestFlash(c.id) : undefined}
                onHover={(on) => onHoverComment?.(on ? c.id : null)}
                onToggleResolved={() => toggleResolved(c.id, c.status)}
                onDelete={() => removeComment(c.id)}
                onReply={(author, body, attachmentIds) =>
                  addReply(c.id, author, body, attachmentIds)
                }
                onChoose={(body, resolve) => respond(c.id, body, resolve)}
                onExpand={() => setModalId(c.id)}
                onOpenImage={openImage}
                onDetachAttachment={(aid) => onDetachAttachment(c.id, aid)}
                onDetachReplyAttachment={(rid, aid) =>
                  onDetachReplyAttachment(c.id, rid, aid)
                }
              />
            ))}
          </section>
        ))}
      </div>

      <footer className="cp__foot">
        <button className="cp__export-btn" onClick={() => setExportOpen(true)}>
          ⤓ export…
        </button>
        <button
          className="cp__export-btn"
          onClick={() => importInputRef.current?.click()}
          title="import comments from an exported HTML file"
        >
          ⤒ import…
        </button>
        <input
          ref={importInputRef}
          type="file"
          accept=".html,text/html"
          hidden
          data-testid="comments-import-input"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void onImportFile(f);
            e.target.value = "";
          }}
        />
      </footer>
      {importErr && (
        <div className="cp__import-err" role="alert">
          {importErr}
        </div>
      )}

      {exportOpen && (
        <ExportModal
          file={file}
          kb={kb}
          onClose={() => setExportOpen(false)}
        />
      )}

      {modalComment && (
        <CommentModal
          kb={kb}
          artifactId={artifactId}
          comment={modalComment}
          onClose={() => setModalId(null)}
          onEditBody={(body) => editCommentBody(modalComment.id, body)}
          onReply={(author, body) => addReply(modalComment.id, author, body)}
          onChoose={(body, resolve) => respond(modalComment.id, body, resolve)}
          onOpenImage={openImage}
        />
      )}

      {lightbox && (
        <AttachmentLightbox
          url={lightbox.url}
          alt={lightbox.alt}
          onClose={() => setLightbox(null)}
        />
      )}
    </aside>
  );
}

// --- verdict strip (W2.15a) --------------------------------------------------
//
// Three-state review-pass verdict, distinct from any individual comment's
// open/resolved status: comment (neutral note) | approve | request changes.
// Clicking a state opens a note popover (pre-filled when re-clicking the
// already-set state); confirming calls `onSet`. The ✕ (shown once a
// verdict exists) clears it (`onSet(null)`). When the verdict is
// "request changes" and comments are still open, a must-fix line links
// into the existing "open" filter tab (D5 — no new filtering, reuse it).

const VERDICT_STATES: { key: VerdictState; label: string; act: string }[] = [
  { key: "comment", label: "comment", act: "comment" },
  { key: "approve", label: "approve", act: "approve" },
  { key: "request_changes", label: "request changes", act: "request-changes" },
];

function VerdictStrip({
  verdict,
  openCount,
  onSet,
  onShowOpen,
}: {
  verdict: Verdict | undefined;
  openCount: number;
  onSet: (v: { state: VerdictState; note?: string } | null) => void;
  onShowOpen: () => void;
}) {
  const [pending, setPending] = useState<VerdictState | null>(null);
  const [noteDraft, setNoteDraft] = useState("");

  function choose(state: VerdictState) {
    setNoteDraft(verdict?.state === state ? (verdict.note ?? "") : "");
    setPending(state);
  }
  function confirmPending() {
    if (!pending) return;
    onSet({ state: pending, note: noteDraft.trim() || undefined });
    setPending(null);
  }

  return (
    <div className="cp__verdict">
      <div className="cp__verdict-seg" role="group" aria-label="review verdict">
        {VERDICT_STATES.map((s) => (
          <button
            key={s.key}
            type="button"
            className={`cp__verdict-btn ${verdict?.state === s.key ? "is-active" : ""}`}
            data-kb-act={`verdict-${s.act}`}
            aria-pressed={verdict?.state === s.key}
            onClick={() => choose(s.key)}
          >
            {s.label}
          </button>
        ))}
        {verdict && (
          <button
            type="button"
            className="cp__verdict-clear"
            data-kb-act="verdict-clear"
            title="clear verdict"
            aria-label="clear verdict"
            onClick={() => onSet(null)}
          >
            <Icon.X />
          </button>
        )}
      </div>

      {verdict && (
        <div className="cp__verdict-meta">
          {verdict.state === "request_changes" ? "changes requested" : verdict.state}
          {" · "}
          {relTime(verdict.at)}
          {verdict.note && <span className="cp__verdict-note"> — {verdict.note}</span>}
        </div>
      )}

      {verdict?.state === "request_changes" && openCount > 0 && (
        <button type="button" className="cp__verdict-mustfix" onClick={onShowOpen}>
          {openCount} open comment{openCount === 1 ? "" : "s"} block approval
        </button>
      )}

      {pending && (
        <div className="cp__verdict-popover" role="dialog" aria-label="verdict note">
          <textarea
            className="cp__verdict-popover-input"
            placeholder="optional note…"
            value={noteDraft}
            onChange={(e) => setNoteDraft(e.target.value)}
            autoFocus
          />
          <div className="cp__verdict-popover-actions">
            <button
              type="button"
              className="cp__verdict-popover-cancel"
              onClick={() => setPending(null)}
            >
              cancel
            </button>
            <button
              type="button"
              className="cp__verdict-popover-confirm"
              onClick={confirmPending}
            >
              set {pending === "request_changes" ? "request changes" : pending}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// --- helpers ---------------------------------------------------------------

function countByStatus(cs: Comment[]): { open: number; resolved: number } {
  let open = 0;
  let resolved = 0;
  for (const c of cs) {
    if (c.status === "open") open++;
    else resolved++;
  }
  return { open, resolved };
}

// Exported (not just used here) — W2.13's ErrataSheet numbers OPEN comments
// in the SAME order this panel would render them under the "open" filter
// (grouped by file, original append order within/across groups), so the
// sheet and the panel never disagree about which comment is "#3".
export function filterComments(cs: Comment[], f: Filter): Comment[] {
  if (f === "all") return cs;
  return cs.filter((c) => c.status === f);
}

export function groupByFile(
  cs: Comment[],
): { file: string; label: string; items: Comment[] }[] {
  const m = new Map<string, { label: string; items: Comment[] }>();
  for (const c of cs) {
    const e = m.get(c.file);
    if (e) e.items.push(c);
    else m.set(c.file, { label: c.fileLabel || c.file, items: [c] });
  }
  return Array.from(m.entries()).map(([file, v]) => ({
    file,
    label: v.label,
    items: v.items,
  }));
}

// --- comment row + reply composer ------------------------------------------

function CommentRow({
  kb,
  artifactId,
  sourceRelative,
  docTitle,
  comment,
  stale,
  active,
  onJump,
  onHover,
  onToggleResolved,
  onDelete,
  onReply,
  onChoose,
  onExpand,
  onOpenImage,
  onDetachAttachment,
  onDetachReplyAttachment,
}: {
  kb: string;
  artifactId: string;
  sourceRelative: string;
  docTitle: string;
  comment: Comment;
  stale: boolean;
  active: boolean;
  onJump?: () => void;
  onHover: (on: boolean) => void;
  onToggleResolved: () => void;
  onDelete: () => void;
  onReply: (
    author: "you" | "claude",
    body: string,
    attachmentIds?: string[],
  ) => void;
  onChoose: (body: string, resolve: boolean) => void;
  onExpand: () => void;
  onOpenImage: (url: string, alt: string) => void;
  onDetachAttachment: (aid: string) => void;
  onDetachReplyAttachment: (replyId: string, aid: string) => void;
}) {
  const [replyOpen, setReplyOpen] = useState(false);
  const [replyAuthor, setReplyAuthor] = useState<"you" | "claude">("you");
  // A-SPA — shares its slot (`reply:<commentId>`) with CommentModal's own
  // reply composer for the SAME comment: one logical "reply draft", not
  // two, so opening the modal on a comment you'd half-typed a reply to
  // here (or vice versa) picks the text back up.
  const replyDraft = useDraft(kb, artifactId, `reply:${comment.id}`);
  const replyBody = replyDraft.text;
  const setReplyBody = replyDraft.setText;
  const rowRef = useRef<HTMLDivElement | null>(null);
  const replyEditor = useRef<MarkdownEditorHandle | null>(null);
  const replyBar = useRef<ComposerAttachBarHandle | null>(null);
  const replyAtt = useComposerAttachments(kb, artifactId);
  const identity = useIdentity();
  const me = identity?.user;
  const operator = identity?.operator;
  // v0.34 W — detach only own content (user-stamped ownership, not role).
  const canDetach = canEditComment(comment, me, operator);

  const canSendReply =
    (replyBody.trim().length > 0 || replyAtt.attachmentIds.length > 0) &&
    !replyAtt.uploading;
  const sendReply = () => {
    if (!canSendReply) return;
    onReply(replyAuthor, replyBody, replyAtt.attachmentIds);
    replyDraft.clear();
    replyAtt.reset();
    setReplyOpen(false);
  };

  // Scroll into view when this row becomes active (page marker clicked).
  useEffect(() => {
    if (active) rowRef.current?.scrollIntoView({ block: "nearest" });
  }, [active]);

  // W1.reader — quiet "cite" action: a markdown blockquote of the comment
  // body plus an attribution link back to it (lib/quote.ts owns the URL +
  // markdown grammar; this just wires the clipboard + toast, same
  // fire-and-forget style as the export copy buttons below).
  function copyCite() {
    const md = buildCiteMarkdown({
      kb,
      sourceRelative,
      title: docTitle,
      anchor: comment.anchor,
      commentId: comment.id,
      body: comment.body,
    });
    void navigator.clipboard
      .writeText(md)
      .then(() => toast.ok("citation copied"))
      .catch(() => toast.err("copy failed"));
  }

  const cls = [
    "cp__row",
    comment.status === "resolved" ? "cp__row--resolved" : "",
    stale ? "cp__row--stale" : "",
    active ? "cp__row--active" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      ref={rowRef}
      className={cls}
      data-comment-id={comment.id}
      onMouseEnter={() => onHover(true)}
      onMouseLeave={() => onHover(false)}
    >
      <div className="cp__row-meta">
        <span className="cp__row-author">{comment.author}</span>
        <UserChip user={comment.user} me={me} className="cp__row-user" />
        <span className="cp__row-time">{relTime(comment.createdAt)}</span>
        {comment.editedAt && <span className="cp__row-edited">edited</span>}
        <span className="cp__row-anchor">{anchorLabel(comment)}</span>
        {stale && (
          <span
            className="cp__row-stale"
            title="this comment's anchor doesn't bind to the current artifact text"
          >
            stale
          </span>
        )}
      </div>
      <CommentBody
        body={comment.body}
        kb={kb}
        id={artifactId}
        onOpenImage={onOpenImage}
      />
      <AttachmentStrip
        kb={kb}
        id={artifactId}
        attachments={comment.attachments}
        onOpenImage={onOpenImage}
        onDetach={
          canDetach ? (aid) => onDetachAttachment(aid) : undefined
        }
      />
      {comment.status === "open" &&
        comment.author === "claude" &&
        !!comment.choices?.length && (
          <QuickButtons items={comment.choices} onChoose={onChoose} />
        )}
      {comment.replies.length > 0 && (
        <div className="cp__row-replies">
          {comment.replies.map((r) => (
            <div key={r.id} className={`cp__reply cp__reply--${r.author}`}>
              <div className="cp__row-meta">
                <span className="cp__row-author">{r.author}</span>
                <UserChip user={r.user} me={me} className="cp__row-user" />
                <span className="cp__row-time">{relTime(r.createdAt)}</span>
              </div>
              <CommentBody
                body={r.body}
                kb={kb}
                id={artifactId}
                onOpenImage={onOpenImage}
              />
              <AttachmentStrip
                kb={kb}
                id={artifactId}
                attachments={r.attachments}
                onOpenImage={onOpenImage}
                onDetach={
                  canEditComment(r, me, operator)
                    ? (aid) => onDetachReplyAttachment(r.id, aid)
                    : undefined
                }
              />
              {comment.status === "open" &&
                r.author === "claude" &&
                !!r.choices?.length && (
                  <QuickButtons items={r.choices} onChoose={onChoose} />
                )}
            </div>
          ))}
        </div>
      )}
      {comment.status === "open" && threadAwaitsResponse(comment) && (
        <QuickButtons items={CANNED} onChoose={onChoose} />
      )}
      <div className="cp__row-actions">
        <button onClick={onExpand} title="expand — read / edit / quote">
          ⤢ expand
        </button>
        <button
          data-kb-act="cite-comment"
          onClick={copyCite}
          title="copy a markdown citation — quote + a deep link back to this comment"
        >
          ❝ cite
        </button>
        {onJump && (
          <button onClick={onJump} title="jump to anchor in iframe">
            ↗ jump
          </button>
        )}
        <button onClick={() => setReplyOpen((o) => !o)}>
          {replyOpen ? "× reply" : "↩ reply"}
        </button>
        <button onClick={onToggleResolved}>
          {comment.status === "open" ? "✓ resolve" : "↺ reopen"}
        </button>
        <button
          className="cp__row-action-danger"
          onClick={onDelete}
          aria-label="delete comment"
        >
          ⌫ delete
        </button>
      </div>
      {replyOpen && (
        <div
          className="cp__reply-composer"
          {...dndProps(replyAtt, replyEditor)}
        >
          <div
            className="cp__author-toggle"
            role="radiogroup"
            aria-label="reply author"
          >
            {(["you", "claude"] as const).map((a) => (
              <button
                key={a}
                role="radio"
                aria-checked={replyAuthor === a}
                className={`cp__author-btn ${replyAuthor === a ? "is-active" : ""}`}
                onClick={() => setReplyAuthor(a)}
              >
                {a}
              </button>
            ))}
          </div>
          <MarkdownEditor
            ref={replyEditor}
            value={replyBody}
            onChange={setReplyBody}
            placeholder="reply…"
            ariaLabel="reply body"
            textareaClassName="cp__reply-input"
            attachment={{ kb, id: artifactId }}
            onSubmit={sendReply}
            onAttach={() => replyBar.current?.open()}
            renderPreview={(v) => (
              <CommentBody body={v} kb={kb} id={artifactId} />
            )}
          />
          <ComposerAttachBar
            ref={replyBar}
            att={replyAtt}
            compact
            onToken={(t) => replyEditor.current?.insertAtCursor(t)}
          />
          <button
            disabled={!canSendReply}
            onClick={sendReply}
            className="cp__reply-send"
          >
            send
          </button>
        </div>
      )}
    </div>
  );
}

// --- export modal ----------------------------------------------------------

function ExportModal({
  file,
  kb,
  onClose,
}: {
  file: ReviewFile;
  kb: string;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  // Open via .show() on first render so DOM nodes mount.
  if (ref.current && !ref.current.open) {
    ref.current.show();
  }

  const claudePrompt = useMemo(() => buildClaudePrompt(file, kb), [file, kb]);
  const json = useMemo(() => JSON.stringify(file, null, 2), [file]);
  const md = useMemo(() => buildMarkdown(file), [file]);

  return (
    <dialog
      ref={(el) => {
        ref.current = el;
        if (el && !el.open) el.show();
      }}
      className="cp__export"
      aria-label="export comments"
    >
      <header className="cp__export-h">
        <h3>export</h3>
        <button onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      <ExportSection title="Claude prompt" body={claudePrompt} ext="md" />
      <ExportSection title="kb JSON" body={json} ext="json" />
      <ExportSection title="Markdown" body={md} ext="md" />
      <PortableHtmlSection file={file} kb={kb} />
    </dialog>
  );
}

/// Portable round-trip export: a standalone copy of the artifact HTML with the
/// review state baked into an inert `kb-review-state` block (the SPA side of
/// `kb comments export --embed`). Re-import it later with the panel's
/// "import…" action. Fetches the artifact bytes on click, embeds, downloads.
function PortableHtmlSection({ file, kb }: { file: ReviewFile; kb: string }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  async function download() {
    setBusy(true);
    setErr(null);
    try {
      const html = await fetchArtifactHtml(kb, file.artifact.id);
      const out = embedReviewIntoHtml(html, file);
      const slug =
        (file.artifact.title || file.artifact.id)
          .toLowerCase()
          .replace(/[^\w.-]+/g, "-")
          .replace(/^-+|-+$/g, "") || file.artifact.id;
      downloadHtml(`${slug}.review.html`, out);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "export failed");
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="cp__export-sec">
      <h4>Portable HTML (artifact + comments)</h4>
      <p className="cp__export-note">
        A standalone copy of the artifact with its {file.comments.length}{" "}
        comment(s) embedded — share it, or re-import later with “import…”.
      </p>
      <div className="cp__export-actions">
        <button disabled={busy} onClick={download}>
          {busy ? "preparing…" : "download .html"}
        </button>
      </div>
      {err && (
        <div className="cp__import-err" role="alert">
          {err}
        </div>
      )}
    </section>
  );
}

function ExportSection({
  title,
  body,
  ext,
}: {
  title: string;
  body: string;
  ext: string;
}) {
  return (
    <section className="cp__export-sec">
      <h4>{title}</h4>
      <pre className="cp__export-pre">{body}</pre>
      <div className="cp__export-actions">
        <button
          onClick={() =>
            navigator.clipboard
              .writeText(body)
              .then(() => toast.ok("copied"))
              .catch(() => toast.err("copy failed"))
          }
        >
          copy
        </button>
        <button
          onClick={() => downloadText(`${title.replace(/\s+/g, "-")}.${ext}`, body)}
        >
          download
        </button>
      </div>
    </section>
  );
}

function downloadText(filename: string, body: string) {
  downloadBlob(filename, body, "text/plain");
}

function downloadHtml(filename: string, body: string) {
  downloadBlob(filename, body, "text/html");
}

function downloadBlob(filename: string, body: string, type: string) {
  const blob = new Blob([body], { type });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

function buildClaudePrompt(file: ReviewFile, kb: string): string {
  const lines: string[] = [];
  lines.push(`# Review: ${file.artifact.title || file.artifact.id}`);
  lines.push("");
  lines.push(
    `The kb-comments/1 file lives at \`<kb-state>/kb/${kb}/.review/${file.artifact.id}.json\`.`,
  );
  lines.push("");
  lines.push("Open comments to address:");
  lines.push("");
  for (const c of file.comments.filter((x) => x.status === "open")) {
    lines.push(`- **${anchorLabel(c)}** (${c.author}, ${relTime(c.createdAt)}):`);
    lines.push(`  ${c.body}`);
    if (c.replies.length > 0) {
      for (const r of c.replies) {
        lines.push(`  - ↳ ${r.author}: ${r.body}`);
      }
    }
  }
  lines.push("");
  lines.push(
    'When done, append a reply with `"author": "claude"` and (optionally) flip `"status": "resolved"`.',
  );
  return lines.join("\n");
}

function buildMarkdown(file: ReviewFile): string {
  const lines: string[] = [];
  lines.push(`# ${file.artifact.title || file.artifact.id}`);
  lines.push("");
  for (const c of file.comments) {
    const state = c.status === "resolved" ? " (resolved)" : "";
    lines.push(`## ${anchorLabel(c)}${state}`);
    lines.push(`*${c.author}, ${relTime(c.createdAt)}*`);
    lines.push("");
    lines.push(c.body);
    if (c.replies.length > 0) {
      lines.push("");
      for (const r of c.replies) {
        lines.push(`- **${r.author}** (${relTime(r.createdAt)}): ${r.body}`);
      }
    }
    lines.push("");
  }
  return lines.join("\n");
}
