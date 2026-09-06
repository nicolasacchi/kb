import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Icon } from "../icons";
import { useAnchors } from "../../hooks/useAnchors";
import { artifactHref } from "../../lib/artifactHref";
import { withKb } from "../../lib/navItems";
import { getLastGalleryUrl } from "../../lib/lastGalleryUrl";
import { flowLabel, truncateLabel, type FlowEntry } from "../../lib/flowStack";
import AddToListButton from "../lists/AddToListButton";
import ShareModal from "../ShareModal";
import FolderCrumbLinks from "../FolderCrumbLinks";

// v0.10 P1 — Detail-route ContextBar.
//
// Replaces the QueryRibbon row on /a/:kb/* with a per-artifact chrome
// strip: back-to-gallery chip → folder/file breadcrumb (+ optional ⌂
// index badge) → optional #N of total counter → prev/next buttons →
// action cluster (anchor / copy link / ↗ open in tab).
//
// Lives inside Detail's own DOM (Detail mounts it above the iframe);
// QueryRibbon stays null on this route so the chrome shell's auto
// grid row collapses.
//
// Prev/next + #N counter are hooks the caller provides — Detail wires
// them to the current `?q=` selection in P3 / S2; until then the
// counter is hidden and the nav buttons fall through to anchor-driven
// j/k handlers (no-op until rebound).
export default function ContextBar({
  kb,
  id,
  title,
  folder,
  filename,
  isIndex,
  sourceRelative,
  position,
  total,
  onPrev,
  onNext,
  reviewActive = false,
  annotateMode = false,
  onToggleAnnotate,
  commentCount = 0,
  inspectorOpen = false,
  onToggleInspector,
  onFullscreen,
  bareUrl,
  folio = false,
  onToggleFolio,
  splitMode,
  onSplit,
  flow,
}: {
  kb: string;
  id: string;
  title: string;
  folder?: string | null;
  filename: string;
  isIndex: boolean;
  sourceRelative: string;
  /// 1-based position in the current selection. Undefined = no
  /// selection context (P3 ships the wiring).
  position?: number;
  total?: number;
  onPrev?: () => void;
  onNext?: () => void;
  /// v0.12 X1 finish — comments-aware actions folded in from the
  /// retired FloatingPill. When `reviewActive` is true the chrome
  /// renders pencil (annotate) + panel toggles; commentCount drives
  /// the badge.
  reviewActive?: boolean;
  annotateMode?: boolean;
  onToggleAnnotate?: () => void;
  commentCount?: number;
  /// Mobile-only (v0.23) — the SINGLE reader-tools entry on a phone. The
  /// inspector rail is the desktop default right column but CSS-collapsed on a
  /// phone; this toggle raises the SAME rail (inspector sub-tabs + comments +
  /// versions) as one bottom sheet, so every panel switches from within it —
  /// there is no longer a separate comments/versions button in the header.
  /// commentCount badges it so the reader sees pending review at a glance.
  inspectorOpen?: boolean;
  onToggleInspector?: () => void;
  /// U1 — icon-only fullscreen (immersive read) trigger. Detail owns the
  /// `immersive` state; this is the canonical desktop control (replaces the
  /// old ↗ open-in-tab primary slot). The raw cross-origin tab returns as a
  /// demoted link in the inspector "About" sub-panel (I1).
  onFullscreen?: () => void;
  /// v0.22 — the BARE artifact origin (`<id>.artifacts.<suffix>/…`), opened in
  /// a new tab next to fullscreen: no SPA chrome, just the scrubbed artifact.
  /// `rel=noreferrer` makes the daemon's referrer-gated bounce skip so it
  /// renders bare. Absent ⇒ the button is hidden.
  bareUrl?: string;
  /// W2.7 — folio reading density (`?view=folio`): centered measure + a
  /// running header/colophon around the canvas (see FolioChrome.tsx). Same
  /// "one home per action" slot as fullscreen (invariant #30); no keybind —
  /// `f` is reserved for the Wave-2 hint-mode toggle elsewhere.
  folio?: boolean;
  onToggleFolio?: () => void;
  /// W3.P-b — the two-pane compare split (`?pane2=`, see lib/paneUrl.ts).
  /// ContextBar IS per-pane chrome (unlike the inspector rail, which stays a
  /// route-level singleton — invariant #30), so each pane renders its own
  /// verb: `"open"` on the primary ("open beside"), `"close"` on the second
  /// pane ("close this pane"). Absent (`onSplit` undefined) ⇒ no button —
  /// which is how mobile and a no-sibling artifact opt out.
  splitMode?: "open" | "close";
  onSplit?: () => void;
  /// link-flow — the READING FLOW return chip (`lib/flowStack.ts`). Rendered
  /// only for the PRIMARY pane and only while the stack is non-empty: "↩
  /// <title you came from>", plus a depth badge and (desktop) a hover
  /// popover listing the whole descent. It lives HERE, beside the other
  /// per-artifact verbs, rather than as a 7th inspector sub-tab (#30 — the
  /// rail is a route-level singleton and its count stays 6).
  /// `entries` is oldest-first; the TOP (where a plain back goes) is last.
  flow?: {
    entries: readonly FlowEntry[];
    onBack: () => void;
    /// Jump `depth` rows down the stack (0 = the top — same as `onBack`).
    onJump: (depth: number) => void;
  };
}) {
  const navigate = useNavigate();
  const { isAnchored, toggle } = useAnchors();
  const anchored = isAnchored(kb, id);
  const [shareOpen, setShareOpen] = useState(false);
  // Desktop hover popover over the flow chip. Plain component state — an
  // ephemeral hover surface is not a place (#23/#35: no URL param), and the
  // popover is CSS-hidden on a phone, where the chip is tap-to-go-back.
  const [flowOpen, setFlowOpen] = useState(false);
  // U1 — transient copy-link feedback: the clipboard write used to succeed
  // (or fail) silently. "ok" swaps the icon to a check + label to "copied";
  // "fail" surfaces the denied-clipboard case instead of mystifying the user.
  const [copyState, setCopyState] = useState<"idle" | "ok" | "fail">("idle");

  const copyLink = async () => {
    try {
      const here = window.location.origin + artifactHref(kb, sourceRelative);
      await navigator.clipboard.writeText(here);
      setCopyState("ok");
    } catch {
      // Clipboard API may be denied in iframes / non-https — surface it.
      setCopyState("fail");
    }
  };

  useEffect(() => {
    if (copyState === "idle") return;
    const t = setTimeout(() => setCopyState("idle"), 1500);
    return () => clearTimeout(t);
  }, [copyState]);

  return (
    <div className="kb-ctxbar" role="navigation" aria-label="artifact context">
      <button
        type="button"
        className="kb-ctxbar__back"
        onClick={() => {
          // X1 — return to the exact filtered gallery the reader came from
          // (restoring filters + the saved scroll slot) when it was for this
          // artifact's kb; else fall back to the bare recent grid for this kb.
          const last = getLastGalleryUrl();
          navigate(last && last.kb === kb ? last.url : withKb("/", kb));
        }}
        title="back to recent (Esc)"
      >
        ← <span className="kb-ctxbar__lbl">back to recent</span>
      </button>
      {flow && flow.entries.length > 0 && (
        <FlowChip
          entries={flow.entries}
          onBack={flow.onBack}
          onJump={flow.onJump}
          open={flowOpen}
          onOpenChange={setFlowOpen}
        />
      )}
      <span className="kb-ctxbar__crumb">
        {folder && (
          <>
            {/* v0.22 — each folder segment deep-links to the folder-filtered
                gallery (descendant-inclusive), matching the inspector passport.
                Wrapped in one span so the crumb stays a single flex child (the
                mobile rule hides this first-child span + the sep). */}
            <span className="kb-ctxbar__crumb-folder">
              <FolderCrumbLinks
                kb={kb}
                folder={folder}
                linkClass="kb-ctxbar__crumb-link"
              />
            </span>
            <span className="kb-ctxbar__sep">/</span>
          </>
        )}
        <b title={title}>{filename}</b>
        {isIndex && (
          <span className="kb-ctxbar__badge">⌂ index</span>
        )}
      </span>
      <span className="kb-ctxbar__nav">
        {position !== undefined && total !== undefined && (
          <span className="kb-ctxbar__pos">
            #{String(position).padStart(3, "0")} of {total.toLocaleString()}
          </span>
        )}
        {onPrev && (
          <button
            type="button"
            className="kb-ctxbar__nav-btn"
            onClick={onPrev}
            title="previous artifact (j)"
            aria-label="previous artifact"
          >
            <Icon.ArrowLeft aria-hidden="true" /> <span className="kb-ctxbar__key">j</span>
          </button>
        )}
        {onNext && (
          <button
            type="button"
            className="kb-ctxbar__nav-btn"
            onClick={onNext}
            title="next artifact (k)"
            aria-label="next artifact"
          >
            <Icon.Arrow aria-hidden="true" /> <span className="kb-ctxbar__key">k</span>
          </button>
        )}
      </span>
      <span className="kb-ctxbar__actions">
        {/* v0.23 — the SINGLE mobile reader-tools entry. Raises the merged rail
            (inspector sub-tabs + comments + versions) as ONE bottom sheet;
            everything switches from inside it, so the header carries no
            separate comments/versions toggle. Badged with the open-comment
            count — the one time-sensitive signal a reader acts on. Desktop
            hides this (`--mobile`); the docked rail is the entry there. */}
        {onToggleInspector && (
          <button
            type="button"
            data-kb-act="inspect"
            className={`kb-ctxbar__act kb-ctxbar__act--mobile ${inspectorOpen ? "is-on" : ""}`}
            onClick={onToggleInspector}
            title={inspectorOpen ? "hide reader tools" : "reader tools"}
            aria-pressed={inspectorOpen}
            aria-expanded={inspectorOpen}
            aria-haspopup="dialog"
            aria-controls="kb-reader-sheet"
            aria-label={`reader tools${
              commentCount > 0
                ? `, ${commentCount} open comment${commentCount === 1 ? "" : "s"}`
                : ""
            }`}
          >
            <Icon.Doc />
            {commentCount > 0 ? (
              <>
                {" "}
                <span className="kb-ctxbar__cnt">{commentCount}</span>
              </>
            ) : (
              <span className="kb-ctxbar__lbl"> reader tools</span>
            )}
          </button>
        )}
        {reviewActive && onToggleAnnotate && (
          <button
            type="button"
            data-kb-act="annotate"
            className={`kb-ctxbar__act ${annotateMode ? "is-on" : ""}`}
            onClick={onToggleAnnotate}
            title={annotateMode ? "stop annotating" : "annotate (✎)"}
            aria-pressed={annotateMode}
          >
            <Icon.Pen />
            <span className="kb-ctxbar__lbl">
              {annotateMode ? " annotating" : " annotate"}
            </span>
          </button>
        )}
        {/* v0.23 — the phone-only comments + versions toggles are GONE: both
            now live inside the reader-tools sheet's rail (reached via the single
            "inspect" button above). On desktop these panels were always driven
            by the docked rail icons (dock-comments / dock-versions). */}
        <button
          type="button"
          data-kb-act="anchor"
          className={`kb-ctxbar__act ${anchored ? "is-on" : ""}`}
          onClick={() => toggle(kb, id)}
          title={anchored ? "unanchor (a)" : "anchor (a)"}
          aria-pressed={anchored}
        >
          <Icon.Anchor />
          <span className="kb-ctxbar__lbl">
            {anchored ? " anchored" : " anchor"}
          </span>
        </button>
        <AddToListButton kb={kb} artifactId={id} />
        <button
          type="button"
          data-kb-act="copy-link"
          className={`kb-ctxbar__act${copyState === "ok" ? " is-ok" : ""}${
            copyState === "fail" ? " is-fail" : ""
          }`}
          onClick={copyLink}
          title="copy permalink"
        >
          {copyState === "ok" ? <Icon.Check /> : <Icon.Copy />}{" "}
          <span className="kb-ctxbar__lbl" aria-live="polite">
            {copyState === "ok"
              ? "copied"
              : copyState === "fail"
                ? "copy failed"
                : "copy link"}
          </span>
        </button>
        <button
          type="button"
          data-kb-act="share"
          className="kb-ctxbar__act"
          onClick={() => setShareOpen(true)}
          title="publish to a gated/public static URL"
        >
          <Icon.Share /> <span className="kb-ctxbar__lbl">share</span>
        </button>
        {onSplit && (
          <button
            type="button"
            data-kb-act="split"
            className={`kb-ctxbar__act kb-ctxbar__act--icon${
              splitMode === "close" ? " kb-ctxbar__act--split-close" : ""
            }`}
            onClick={onSplit}
            title={
              splitMode === "close"
                ? "close this pane (w q)"
                : "open beside — two-pane compare (w v)"
            }
            aria-label={
              splitMode === "close" ? "close this pane" : "open beside"
            }
          >
            {splitMode === "close" ? <Icon.X /> : <Icon.Table />}
          </button>
        )}
        {onToggleFolio && (
          <button
            type="button"
            data-kb-act="folio"
            className={`kb-ctxbar__act kb-ctxbar__act--icon ${folio ? "is-on" : ""}`}
            onClick={onToggleFolio}
            title="folio · published reading"
            aria-label="folio reading mode"
            aria-pressed={folio}
          >
            <Icon.BookOpen />
          </button>
        )}
        <button
          type="button"
          data-kb-act="fullscreen"
          className="kb-ctxbar__act kb-ctxbar__act--primary kb-ctxbar__act--icon"
          onClick={onFullscreen}
          title="read full screen (o)"
          aria-label="read full screen"
        >
          <Icon.Expand />
        </button>
        {bareUrl && (
          <a
            data-kb-act="bare"
            className="kb-ctxbar__act kb-ctxbar__act--icon"
            href={bareUrl}
            target="_blank"
            rel="noopener noreferrer"
            title="open the bare artifact in a new tab (b)"
            aria-label="open the bare artifact in a new tab"
          >
            <Icon.External />
          </a>
        )}
      </span>
      {shareOpen && (
        <ShareModal
          kb={kb}
          target={sourceRelative}
          folder={folder}
          allowPage
          onClose={() => setShareOpen(false)}
        />
      )}
    </div>
  );
}

// link-flow — the return chip + its desktop popover.
//
// ONE affordance, two depths: a click on the chip returns to the artifact
// you came from (identical to the `u` keybind — both call the route's single
// `goBack`), and hovering it (desktop) lists the whole descent so a reader
// three links deep can jump straight back to the top of it. Rows are shown
// MOST RECENT FIRST, and `onJump(depth)` takes that same display index, so
// "row 0" and "the chip" mean the same thing by construction.
function FlowChip({
  entries,
  onBack,
  onJump,
  open,
  onOpenChange,
}: {
  entries: readonly FlowEntry[];
  onBack: () => void;
  onJump: (depth: number) => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const rows = [...entries].reverse();
  const top = rows[0];
  const depth = rows.length;
  return (
    <span
      className="kb-ctxbar__flow"
      onMouseEnter={() => onOpenChange(true)}
      onMouseLeave={() => onOpenChange(false)}
    >
      <button
        type="button"
        data-kb-act="flow-back"
        className="kb-ctxbar__back kb-ctxbar__flow-btn"
        onClick={onBack}
        title={`back to ${flowLabel(top)} (u)`}
        aria-label={`back to ${flowLabel(top)}`}
      >
        ↩{" "}
        <span className="kb-ctxbar__flow-lbl">
          {truncateLabel(flowLabel(top))}
        </span>
        {depth > 1 && <span className="kb-ctxbar__flow-depth">{depth}</span>}
      </button>
      {open && (
        <div className="kb-ctxbar__flow-pop" role="menu" aria-label="reading flow">
          {rows.map((e, i) => (
            <button
              key={`${e.kb}/${e.sourceRelative}/${e.ts}`}
              type="button"
              role="menuitem"
              className="kb-ctxbar__flow-row"
              data-kb-act={`flow-jump-${i}`}
              onClick={() => {
                onOpenChange(false);
                onJump(i);
              }}
              title={`${e.kb} · ${e.sourceRelative}`}
            >
              <span className="kb-ctxbar__flow-row-title">{flowLabel(e)}</span>
              {e.sec && <span className="kb-ctxbar__flow-row-sec">§ {e.sec}</span>}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}
