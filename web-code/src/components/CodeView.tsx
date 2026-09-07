import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { Compartment, EditorSelection, EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import type { CommentOut, Span, Symbol } from "../api/types";
import { makeByteToUtf16Mapper, spansToDecorationRanges } from "../lib/decorations";
import { ageOverlayExtension, setAgeOverlayLines } from "../editor/ageOverlay";
import { conflictOverlayExtension, setConflictOverlayActive } from "../editor/conflictOverlay";
import { highlightField, highlightRangesFacet } from "../editor/highlightField";
import { createLineGutter, type LineGutterHandlers, type LineMarkerSpec } from "../editor/lineGutter";
import { linkifyExtension, type LinkifyCallbacks } from "../editor/linkify";
import { occurrenceHighlightExtension } from "../editor/occurrenceHighlight";
import { paramHintsExtension, paramHintsRefreshEffect } from "../editor/paramHints";
import {
  applyLensDecorations,
  createLensDecorationsExtension,
  type LensClickHandlers,
} from "../editor/lensDecorations";
import type { LensDeclaration } from "../api/types";
import { setStoryOverlayLines, storyOverlayExtension, type StoryLineRange } from "../editor/storyOverlay";
import {
  applyInlinePeek,
  inlinePeekExtension,
  restoreInlinePeekScroll,
  type InlinePeekHandlers,
  type InlinePeekRender,
} from "../editor/inlinePeek";
import { hoverTooltipExtension } from "../editor/hoverTooltip";
import { useCommands } from "../commands/CommandRoot";
import { vimReader, vimStatus, type VimReaderCallbacks } from "../editor/vimReader";
import type { AgeLineInfo } from "../lib/ageHeatmap";
import type { BlameDotInfo } from "../lib/blameGutter";
import type { CommentGutterMark } from "../lib/comments";
import type { DiagnosticGutterMark } from "../lib/diagnostics";
import { scanLinkTokens } from "../lib/linkifyScan";
import StickyContext from "./StickyContext";
import "../styles/reader.css";
import "../styles/provenance.css";
import "../styles/history.css";

// CodeMirror 6, READ-ONLY. Renders a file's content with server-derived
// syntax-highlight decorations (`highlightField.ts`) and line numbers, plus
// (W4.4/W4.6/PRR-U9/V72-J2) FOUR optional gutters built on the shared
// `editor/lineGutter.ts` factory: a blame-provenance dot gutter, an
// annotations gutter, a diagnostics gutter, and (V72-J2, kbc-theme/1's Lane
// Budget "gutter slot four") a comments/1 gutter. All four gutters are
// ALWAYS present in the extension set
// (so their `StateField`s exist on every `EditorState`), but render nothing
// when their marker map is empty — `blameDots`/`annotationMarkers` being
// `null`/`undefined` (provenance mode off / annotations still loading)
// simply means "no markers to show," never a structural difference in the
// editor's extensions (which would force a full view recreation just to
// toggle a feature).
//
// A3 (kb-code v2): the buffer is now the reader's PRIMARY focus surface —
// `vimReader` (visible reading cursor + modal keymap, `editor/vimReader.ts`)
// is always in the extension set, the parent can imperatively `focus()` the
// buffer via the forwarded `CodeViewHandle`, and position flows BOTH ways:
// `gotoSel` (a line RANGE + nonce, so re-selecting the same range still
// re-scrolls) drives the selection in, `onSelectionLines` reports it back
// out (Reader debounces that into the `?line=` URL param via
// `lib/cursorUrlSync.ts`).
//
// The view is recreated (not incrementally patched) on every `content`/
// `blobHash` change — a file/ref switch is a whole-new-document event, not
// an edit, so there is no "diff the old doc against the new doc" concern
// CM6's controlled-editor idiom (`web/src/editor/useCodeMirror.ts`) exists
// to solve; this is simpler than that hook on purpose. Gutter marker maps
// are pushed onto the (possibly freshly recreated) view via a SEPARATE
// effect, since blame/annotation data arrives from its own async query,
// independent of the file load.

/// A selection target driven from OUTSIDE the editor (URL `?line=` param,
/// outline click, annotation jump). `nonce` makes every drive distinct so
/// clicking the same outline symbol twice still re-centers the view.
export interface GotoSel {
  start: number;
  end: number;
  nonce: number;
}

export interface CodeViewHandle {
  /// Move keyboard focus into the buffer (the vim keymap only sees keys
  /// while the content element is focused).
  focus(): void;
  /// V70-A6 — push (or clear, with `null`) the inline peek stack rendered
  /// under the caret line. Returns the buffer's scroll offset AT THE MOMENT
  /// OF THE CALL, which the host stores on the peek state so `Esc` can put
  /// it back exactly (§P7).
  applyInlinePeek(render: InlinePeekRender | null): number;
  /// Restore a scroll offset captured by `applyInlinePeek`.
  restoreScroll(top: number): void;
  /// Viewport (client) coordinates of the caret's current head — B1's peek
  /// panel anchors near the cursor at the moment `gd`/`gr`/`K` fires.
  /// `null` when the view isn't mounted yet, or `coordsAtPos` itself
  /// returns `null` (the position isn't currently rendered — same contract
  /// CM6 uses; can't happen for the cursor's OWN position in practice,
  /// since a rendered selection implies its endpoints are in the viewport,
  /// but the type is honest about the possibility rather than asserting).
  cursorCoords(): { top: number; bottom: number; left: number } | null;
}

export interface CodeViewProps {
  content: string;
  spans: Span[] | null;
  /// Cache-busting/identity key — changes whenever the file's bytes
  /// change (blob hash), even if `path` is the same (e.g. switching refs
  /// on the same file). Forces a fresh `EditorState`.
  blobHash: string;
  /// Externally-driven selection (see `GotoSel`). A single line is
  /// `start === end`; a range selects from `start`'s first char to `end`'s
  /// line end, cursor anchored at the start.
  gotoSel?: GotoSel | null;

  // --- A3 — the vim reading buffer ------------------------------------
  /// Action callbacks for the vim keymap (`a`, `Y`, `gd`, `gr`, `K`, …).
  /// Undefined callbacks leave their keys inert — see `editor/vimReader.ts`.
  vim?: VimReaderCallbacks;
  /// Fires on every selection change with the 1-based first/last line of
  /// the main selection (collapsed cursor ⇒ `start === end`).
  onSelectionLines?: (sel: { start: number; end: number }) => void;
  /// Fires when the vim mode/pending-keys status changes (for the status
  /// chip) — deduplicated here so React state churn stays proportional to
  /// actual mode changes, not keystrokes.
  onVimStatus?: (s: { mode: string; pending: string }) => void;

  // --- W4.4 — the blame gutter's disclosure ladder --------------------
  /// `null`/`undefined` = gutter renders no dots (provenance mode off, or
  /// blame hasn't loaded yet).
  blameDots?: Map<number, BlameDotInfo> | null;
  onBlameHover?: (line: number, rect: DOMRect) => void;
  onBlameUnhover?: (line: number) => void;
  onBlameClick?: (line: number) => void;

  // --- Wave C — the age heatmap overlay (3rd Provenance state) ---------
  /// `null`/`undefined` = no line tinting (overlay off, `dots` mode
  /// active instead, or blame hasn't loaded yet).
  ageLines?: Map<number, AgeLineInfo> | null;

  // --- Phase C7 — the story player's changed-line tint ------------------
  /// `null`/`undefined` = no tint (not in story mode, or the current
  /// step's diff hasn't loaded/fetched cleanly yet). Every non-empty value
  /// is treated as a fresh STEP (drives the CSS flash — see
  /// `editor/storyOverlay.ts`'s doc), so callers should only pass a new
  /// array identity when the step actually changed.
  storyLines?: StoryLineRange[] | null;

  // --- Phase G2 — the repo-state banner's conflict-marker line tint -----
  /// `true` only while THIS pane's open file is named in `GET
  /// /api/repo-state`'s own `conflicted` list (`Reader.tsx` derives it from
  /// that fetch) — every conflict-marker line (`<<<<<<< `/`=======`/
  /// `>>>>>>> `) then gets a background tint (`editor/conflictOverlay.ts`).
  conflictActive?: boolean;

  // --- W4.6 — the annotations gutter -----------------------------------
  annotationMarkers?: Map<number, LineMarkerSpec> | null;
  onAnnotationClick?: (line: number) => void;

  // --- PRR-U9 — the diagnostics gutter (design-addendum-2.md §D) --------
  /// `null`/`undefined` = gutter renders no marks (no provider covers this
  /// file's language, or diagnostics haven't loaded yet) — reuses
  /// `editor/lineGutter.ts`'s SAME factory as the blame/annotation gutters
  /// above, called a third time with its own `StateField`/`StateEffect`
  /// identity (see that module's doc for why each call needs its own).
  diagnosticMarkers?: Map<number, DiagnosticGutterMark> | null;

  // --- V72-J2 (D8) — the comments/1 gutter (kbc-theme/1 Lane Budget's
  // "gutter slot four") -------------------------------------------------
  /// `null`/`undefined` = gutter renders no marks (comments/1 hasn't loaded
  /// yet). Already filtered to the active `CommentGutterMode` by the host
  /// (`Reader.tsx`) — this component renders whatever it's handed, same
  /// "the projection is computed once outside, this renders it honestly"
  /// split the annotations/diagnostics gutters above already use.
  commentMarkers?: Map<number, CommentGutterMark> | null;
  onCommentHover?: (line: number, rect: DOMRect) => void;
  onCommentUnhover?: (line: number) => void;
  onCommentClick?: (line: number) => void;

  // --- W4.5 — live-mirror auto-refresh heuristic inputs ----------------
  /// Fires whenever the viewer's "dirty" state changes (scrolled away from
  /// the top, or carries a non-collapsed selection) — `useLiveMirror`'s
  /// `viewerDirtyRef` is fed from this.
  onViewerDirtyChange?: (dirty: boolean) => void;
  /// Fires on every selection change with the CURSOR's 1-based line — the
  /// annotation `a` keybinding's "current line" (`Reader.tsx`).
  onCursorLineChange?: (line: number) => void;

  // --- B1 — linkify (paths/URLs/session-ids inside comments+strings) ----
  /// Undefined leaves every token inert (no click handler fires) — same
  /// "missing callback = no-op, never a crash" contract `vim`'s own
  /// per-action callbacks use.
  linkify?: LinkifyCallbacks;

  // --- V3.N2 — sticky context lines ------------------------------------
  /// Current file symbols (same list OutlineRail uses). Empty/omitted →
  /// no sticky stack.
  symbols?: Symbol[];
  /// Preference toggle (default true). When false, StickyContext stays off.
  stickyContextEnabled?: boolean;
  /// Click a sticky row → scroll to that symbol's start line.
  onStickyJump?: (line: number) => void;
  /// Fires when the first visible line of the viewport changes (1-based).
  onFirstVisibleLineChange?: (line: number) => void;

  // --- V3.1-H3a — param-name inlay hints ---------------------------------
  /// Repo + path for resolve; omitted disables the extension's fetches.
  paramHintsRepo?: string | null;
  paramHintsPath?: string | null;
  /// Preference toggle (default true).
  paramHintsEnabled?: boolean;

  // --- V3.1-H3b — Code Vision lens chips ---------------------------------
  /// Declarations from `/api/lenses` (host-fetched via TanStack Query).
  lensDeclarations?: LensDeclaration[] | null;
  /// Preference toggle (default true).
  codeLensesEnabled?: boolean;
  onLensUsages?: (decl: LensDeclaration) => void;
  onLensImpls?: (decl: LensDeclaration) => void;
  onLensAuthor?: (decl: LensDeclaration) => void;

  // --- SH.C3 — reading-mode prefs (line wrap + font size) ----------------
  /// Persisted preference (default false). Threaded through a CM6
  /// `Compartment` (`wrapCompartment` below) so a toggle reconfigures the
  /// LIVE view via `dispatch` — same "no full remount" contract the
  /// param-hints/lens prefs already use, just via `Compartment` instead of
  /// a ref-read plugin, since `EditorView.lineWrapping` is a static
  /// extension (no per-keystroke facet to re-read from a ref).
  wrap?: boolean;
  /// Persisted CM6 buffer font-size in px (`lib/prefs.ts` clamps to
  /// `[READER_FONT_SIZE_MIN, READER_FONT_SIZE_MAX]`; default 13 here matches
  /// the pre-SH.C3 hardcoded value for callers that don't pass it).
  fontSize?: number;

  // --- V70-A6 — inline peek + the identifier hover tooltip ---------------
  /// Handlers for the inline-peek block widget (`editor/inlinePeek.ts`).
  /// Omitted leaves the widget inert — the same "missing callback = no-op"
  /// contract `vim` and `linkify` use.
  inlinePeek?: InlinePeekHandlers;
  /// `GET /api/hover` needs a repo + path to ask about; both `null` disables
  /// the tooltip entirely (no fetch is ever issued). Separate from
  /// `paramHintsRepo`/`paramHintsPath` on purpose — the two features are
  /// independently toggleable and reading one pref through the other's prop
  /// is exactly how a coupling nobody intended gets made.
  hoverRepo?: string | null;
  hoverPath?: string | null;
  hoverRef?: string;
  /// V72-J2 (D8) — the CURRENT file's already-fetched comments/1 rows, for
  /// the hover tooltip's freshness-caption/YARD-mismatch enrichment
  /// (`editor/hoverTooltip.ts`'s `findDocCommentForSymbol`). `null`/
  /// `undefined` renders the pre-V72-J2 tooltip byte-identical — same "an
  /// absent enrichment input changes nothing" contract every other optional
  /// prop here already keeps.
  docComments?: CommentOut[] | null;
}

const BLAME_DOT_SOLID: LineMarkerSpec["className"] = "kbc-blame-dot kbc-blame-dot--solid";
const BLAME_DOT_OUTLINE: LineMarkerSpec["className"] = "kbc-blame-dot kbc-blame-dot--outline";
const ANNOTATION_PLUS: LineMarkerSpec = { className: "kbc-annot-plus", title: "Add annotation" };

function blameMarkersFrom(dots: Map<number, BlameDotInfo> | null | undefined): Map<number, LineMarkerSpec> {
  const out = new Map<number, LineMarkerSpec>();
  if (!dots) return out;
  for (const [line, info] of dots) {
    out.set(line, { className: info.solid ? BLAME_DOT_SOLID : BLAME_DOT_OUTLINE, title: info.label });
  }
  return out;
}

/// PRR-U9 — translate `lib/diagnostics.ts`'s pure `DiagnosticGutterMark`
/// map into the gutter factory's `LineMarkerSpec` shape (`styles/
/// provenance.css` defines one `.kbc-diag-dot--<severity>` rule per
/// `DiagnosticSeverityLabel`), same split `blameMarkersFrom` above draws
/// between pure derivation (lib) and CM6 marker shape (this component).
function diagMarkersFrom(
  marks: Map<number, DiagnosticGutterMark> | null | undefined,
): Map<number, LineMarkerSpec> {
  const out = new Map<number, LineMarkerSpec>();
  if (!marks) return out;
  for (const [line, mark] of marks) {
    out.set(line, { className: `kbc-diag-dot kbc-diag-dot--${mark.severity}`, title: mark.title });
  }
  return out;
}

/// V72-J2 — `lib/comments.ts`'s pure `CommentGutterMark` (kind/state/title)
/// into the gutter factory's `LineMarkerSpec` shape (`styles/comments.css`
/// defines one `.kbc-comment-dot--<kind>` rule per `CommentKind` PLUS one
/// `.kbc-comment-dot--state-<state>` modifier per non-`"none"` state — the
/// state renders as a border/underline STYLE, never hue-only, kbc-theme/1's
/// Lane Budget "trust is a line style" rule applied to comments/1's own
/// state vocabulary). Same split `blameMarkersFrom`/`diagMarkersFrom` draw
/// between pure derivation (`lib/comments.ts`) and CM6 marker shape (here).
function commentMarkersFrom(
  marks: Map<number, CommentGutterMark> | null | undefined,
): Map<number, LineMarkerSpec> {
  const out = new Map<number, LineMarkerSpec>();
  if (!marks) return out;
  for (const [line, mark] of marks) {
    const stateClass = mark.state !== "none" ? ` kbc-comment-dot--state-${mark.state}` : "";
    out.set(line, {
      className: `kbc-comment-dot kbc-comment-dot--${mark.kind}${stateClass}`,
      title: mark.title,
    });
  }
  return out;
}

/// The listener-facing subset of props, read through a ref at call time so
/// the extensions (created once per `blobHash` view) never capture a stale
/// render's closures — same pattern as the gutter `handlersRef`s.
interface ListenerCallbacks {
  onViewerDirtyChange?: (dirty: boolean) => void;
  onCursorLineChange?: (line: number) => void;
  onSelectionLines?: (sel: { start: number; end: number }) => void;
  onVimStatus?: (s: { mode: string; pending: string }) => void;
  onFirstVisibleLineChange?: (line: number) => void;
}

function firstVisibleLineOf(view: EditorView): number {
  const from = view.viewport.from;
  return view.state.doc.lineAt(from).number;
}

/// SH.C3 — the reader's base theme, parameterized on the persisted font
/// size. Re-built (not patched) on every font-size change and pushed
/// through `fontSizeCompartment.reconfigure` — cheaper than it sounds,
/// `EditorView.theme` just builds a StyleModule.
function readerThemeExtension(px: number) {
  return EditorView.theme({
    "&": { height: "100%", fontSize: `${px}px` },
    ".cm-scroller": { fontFamily: "var(--font-mono, monospace)", overflow: "auto" },
  });
}

const CodeView = forwardRef<CodeViewHandle, CodeViewProps>(function CodeView(
  {
    content,
    spans,
    blobHash,
    gotoSel,
    vim,
    onSelectionLines,
    onVimStatus,
    blameDots,
    onBlameHover,
    onBlameUnhover,
    onBlameClick,
    ageLines,
    storyLines,
    annotationMarkers,
    onAnnotationClick,
    diagnosticMarkers,
    commentMarkers,
    onCommentHover,
    onCommentUnhover,
    onCommentClick,
    onViewerDirtyChange,
    onCursorLineChange,
    linkify,
    conflictActive,
    symbols,
    stickyContextEnabled = true,
    onStickyJump,
    onFirstVisibleLineChange,
    paramHintsRepo = null,
    paramHintsPath = null,
    paramHintsEnabled = true,
    lensDeclarations = null,
    codeLensesEnabled = true,
    onLensUsages,
    onLensImpls,
    onLensAuthor,
    wrap = false,
    fontSize = 13,
    inlinePeek,
    hoverRepo = null,
    hoverPath = null,
    hoverRef,
    docComments = null,
  }: CodeViewProps,
  ref,
) {
  const parentRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [firstVisibleLine, setFirstVisibleLine] = useState(1);
  const firstVisRef = useRef(1);

  useImperativeHandle(ref, () => ({
    focus() {
      viewRef.current?.focus();
    },
    applyInlinePeek(render: InlinePeekRender | null) {
      const view = viewRef.current;
      if (!view) return 0;
      return applyInlinePeek(view, render);
    },
    restoreScroll(top: number) {
      const view = viewRef.current;
      if (view) restoreInlinePeekScroll(view, top);
    },
    cursorCoords() {
      const view = viewRef.current;
      if (!view) return null;
      const rect = view.coordsAtPos(view.state.selection.main.head);
      if (!rect) return null;
      return { top: rect.top, bottom: rect.bottom, left: rect.left };
    },
  }));

  // Byte→UTF16 span mapping is pure CPU work over `content`+`spans` — memoized
  // so a `gotoSel`-only re-render doesn't redo it.
  const ranges = useMemo(() => {
    if (!spans || spans.length === 0) return [];
    const mapper = makeByteToUtf16Mapper(content);
    return spansToDecorationRanges(content, spans, mapper);
  }, [content, spans]);

  // B1 — clickable comment/string tokens. Pure CPU work over `content`+
  // `spans`, same memoization rationale as `ranges` above.
  const linkTokens = useMemo(() => scanLinkTokens(content, spans ?? []), [content, spans]);
  const linkifyHandlersRef = useRef<LinkifyCallbacks>({});
  linkifyHandlersRef.current = {
    onOpenUrl: linkify?.onOpenUrl,
    onOpenPath: linkify?.onOpenPath,
    onOpenSession: linkify?.onOpenSession,
  };

  // Handler refs — read at call-time by the gutter extensions (see
  // `editor/lineGutter.ts`'s doc for why a ref, not a closure captured at
  // extension-creation time).
  const blameHandlersRef = useRef<LineGutterHandlers>({});
  blameHandlersRef.current = {
    onHover: onBlameHover,
    onUnhover: onBlameUnhover,
    onClick: onBlameClick ? (line) => onBlameClick(line) : undefined,
  };
  const annotHandlersRef = useRef<LineGutterHandlers>({});
  annotHandlersRef.current = {
    onClick: onAnnotationClick ? (line) => onAnnotationClick(line) : undefined,
  };
  // PRR-U9 — no click/hover affordance of its own (a diagnostic row is read
  // from the inspector's Diagnostics card, not clicked from the gutter);
  // the empty, never-reassigned handlers object is still ref-shaped so
  // `createLineGutter`'s call contract stays identical across all three
  // gutters.
  const diagHandlersRef = useRef<LineGutterHandlers>({});
  // V72-J2 — the comments/1 gutter's own hover/click affordances (the small
  // card on hover, the rail's Comments tab on click).
  const commentHandlersRef = useRef<LineGutterHandlers>({});
  commentHandlersRef.current = {
    onHover: onCommentHover,
    onUnhover: onCommentUnhover,
    onClick: onCommentClick ? (line) => onCommentClick(line) : undefined,
  };

  const listenerCbRef = useRef<ListenerCallbacks>({});
  listenerCbRef.current = {
    onViewerDirtyChange,
    onCursorLineChange,
    onSelectionLines,
    onVimStatus,
    onFirstVisibleLineChange,
  };

  // The vim callbacks prop, re-read at dispatch time through the same
  // ref idiom; `stableVimCb` keeps ONE object identity for the extension's
  // whole life so view recreation never re-binds the keymap.
  const vimRef = useRef<VimReaderCallbacks | undefined>(vim);
  vimRef.current = vim;
  // V71-K4 — the chord shield's source. This ONE member of the callbacks
  // object below is deliberately not routed through the `vim` prop: "is a
  // central chord in flight" is a property of the dispatcher, not of
  // whichever route happens to render a buffer, and every CodeView needs
  // the same answer. Wiring it here means the shield exists wherever a
  // buffer does (`Reader`, `ReaderLegacy`, anything later) instead of once
  // per host that remembers to pass it. Outside a `CommandRoot` the bus is
  // the null-object, whose `chordWillConsume` is `() => false`.
  const commandBus = useCommands();
  const busRef = useRef(commandBus);
  busRef.current = commandBus;
  const stableVimCb = useRef<VimReaderCallbacks>({
    onAnnotate: (s) => vimRef.current?.onAnnotate?.(s),
    onPermalink: (s) => vimRef.current?.onPermalink?.(s),
    onGotoDef: (p) => vimRef.current?.onGotoDef?.(p),
    onFindRefs: (p) => vimRef.current?.onFindRefs?.(p),
    onHover: (p) => vimRef.current?.onHover?.(p),
    onHistoryStep: (d) => vimRef.current?.onHistoryStep?.(d),
    onCycleFile: (d) => vimRef.current?.onCycleFile?.(d),
    onPaneFocus: (d) => vimRef.current?.onPaneFocus?.(d),
    onSplitSelf: () => vimRef.current?.onSplitSelf?.(),
    onClosePane: () => vimRef.current?.onClosePane?.(),
    onShowHelp: () => vimRef.current?.onShowHelp?.(),
    onRecordJump: (info) => vimRef.current?.onRecordJump?.(info),
    onJumpBack: () => vimRef.current?.onJumpBack?.(),
    onJumpForward: () => vimRef.current?.onJumpForward?.(),
    // V70-A6 — `u`, the Ramp's Back rung (§P7); see `vimReader.ts`'s doc on
    // why the bare key needs its own arm inside the buffer at all.
    onNavBack: () => vimRef.current?.onNavBack?.(),
    onRecentLocations: () => vimRef.current?.onRecentLocations?.(),
    onStructurePopup: () => vimRef.current?.onStructurePopup?.(),
    onToggleBookmark: () => vimRef.current?.onToggleBookmark?.(),
    onMnemonicPopup: () => vimRef.current?.onMnemonicPopup?.(),
    onLineHistory: (s) => vimRef.current?.onLineHistory?.(s),
    onHierarchyCallers: (p) => vimRef.current?.onHierarchyCallers?.(p),
    onHierarchyCallees: (p) => vimRef.current?.onHierarchyCallees?.(p),
    onHierarchyTypes: (p) => vimRef.current?.onHierarchyTypes?.(p),
    onImpact: (p) => vimRef.current?.onImpact?.(p),
    onEgoGraph: (p) => vimRef.current?.onEgoGraph?.(p),
    // V70-A4 — the Desk's region chords. This object is an explicit
    // whitelist, not a spread: a callback missing from it is silently
    // dead (`dispatchCommand` sees `undefined` and returns false), so
    // every new `VimReaderCallbacks` member has to be added here too.
    onResizeMode: () => vimRef.current?.onResizeMode?.(),
    onZoomRegion: () => vimRef.current?.onZoomRegion?.(),
    // V70-A6 — `p` pins a provisional pane (§P7's commitment ladder).
    onPinPane: () => vimRef.current?.onPinPane?.(),
    // V71-K4 — the chord shield (see `busRef` above and `vimReader.ts`'s
    // own doc on this member).
    chordWillConsume: (e) => busRef.current.chordWillConsume(e),
  }).current;

  // Param-hints options re-read at plugin time via refs so a pref toggle
  // doesn't force an EditorView recreation.
  const paramHintsOptsRef = useRef({
    repo: paramHintsRepo,
    path: paramHintsPath,
    enabled: paramHintsEnabled,
  });
  paramHintsOptsRef.current = {
    repo: paramHintsRepo,
    path: paramHintsPath,
    enabled: paramHintsEnabled,
  };
  const stableParamHints = useRef(
    paramHintsExtension({
      getPath: () => paramHintsOptsRef.current.path ?? null,
      getRepo: () => paramHintsOptsRef.current.repo ?? null,
      isEnabled: () => paramHintsOptsRef.current.enabled !== false,
    }),
  ).current;

  // Code Vision lenses — host supplies declarations; chips call host handlers.
  const lensHandlersRef = useRef<LensClickHandlers>({});
  lensHandlersRef.current = {
    onUsages: onLensUsages,
    onImpls: onLensImpls,
    onAuthor: onLensAuthor,
  };
  const stableLenses = useRef(
    createLensDecorationsExtension(() => lensHandlersRef.current),
  ).current;

  // V70-A6 — the inline peek's handlers, read at call time through the same
  // ref idiom (`editor/lineGutter.ts`'s doc) so a fresh closure every render
  // never forces a view recreation.
  const inlinePeekHandlersRef = useRef<InlinePeekHandlers>({});
  inlinePeekHandlersRef.current = inlinePeek ?? {};
  const stableInlinePeek = useRef(inlinePeekExtension(inlinePeekHandlersRef)).current;

  // V70-A6 — the identifier hover tooltip. Options are read through a ref for
  // the same reason; the extension itself is built ONCE.
  const hoverOptsRef = useRef({ repo: hoverRepo, path: hoverPath, ref: hoverRef, docComments });
  hoverOptsRef.current = { repo: hoverRepo, path: hoverPath, ref: hoverRef, docComments };
  const stableHover = useRef(
    hoverTooltipExtension({
      getRepo: () => hoverOptsRef.current.repo,
      getPath: () => hoverOptsRef.current.path,
      getRef: () => hoverOptsRef.current.ref,
      // V72-J2 (D8) — comments/1's doc-hover enrichment; `undefined`/`null`
      // (comments/1 not wired for this caller) renders byte-identical.
      getDocComments: () => hoverOptsRef.current.docComments,
      // Ctrl/Cmd-click IS `gd` — the pointer affordance for the registry's
      // existing `reader.goto-definition` row, not a second definition of it.
      onGotoDefinition: (pos) => vimRef.current?.onGotoDef?.(pos),
    }),
  ).current;

  // Pref toggle → ask the plugin to recompute (clear or re-fetch).
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: paramHintsRefreshEffect.of(null) });
  }, [paramHintsEnabled, paramHintsPath, paramHintsRepo]);

  // Push lens decorations whenever the host payload changes.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    applyLensDecorations(view, {
      declarations: lensDeclarations ?? [],
      enabled: codeLensesEnabled !== false,
    });
  }, [lensDeclarations, codeLensesEnabled, blobHash]);

  function reportFirstVisible(view: EditorView) {
    const line = firstVisibleLineOf(view);
    if (line === firstVisRef.current) return;
    firstVisRef.current = line;
    setFirstVisibleLine(line);
    listenerCbRef.current.onFirstVisibleLineChange?.(line);
  }

  // Created ONCE for this component's lifetime (a fresh `StateField`/
  // `StateEffect` pair per `createLineGutter` call — see that module's
  // doc); reused across every `blobHash`-triggered view recreation.
  const blameGutterHandle = useRef(createLineGutter("kbc-blame-gutter", blameHandlersRef)).current;
  const annotGutterHandle = useRef(
    createLineGutter("kbc-annot-gutter", annotHandlersRef, ANNOTATION_PLUS),
  ).current;
  // PRR-U9 — a THIRD `createLineGutter` call: the reader's gutter slot
  // mechanism generalizes cleanly to a third lane, no new CM6 state-machine
  // plumbing needed (`editor/lineGutter.ts`'s doc: each call gets its own
  // `StateField`/`StateEffect` pair by construction).
  const diagGutterHandle = useRef(createLineGutter("kbc-diag-gutter", diagHandlersRef)).current;
  // V72-J2 — a FOURTH `createLineGutter` call (kbc-theme/1 Lane Budget's
  // "gutter slot four"): the comments/1 gutter. Same generalization PRR-U9's
  // own comment above already notes for the third.
  const commentGutterHandle = useRef(
    createLineGutter("kbc-comment-gutter", commentHandlersRef),
  ).current;

  // SH.C3 — one Compartment each for line-wrap and font-size, created ONCE
  // for the component's lifetime (same rationale as the gutter handles
  // above) and reused across every `blobHash`-triggered view recreation.
  const wrapCompartment = useRef(new Compartment()).current;
  const fontSizeCompartment = useRef(new Compartment()).current;

  const dirtyRef = useRef(false);
  const vimStatusRef = useRef<{ mode: string; pending: string } | null>(null);

  useEffect(() => {
    const parent = parentRef.current;
    if (!parent) return;
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: content,
        extensions: [
          EditorView.editable.of(false),
          EditorState.readOnly.of(true),
          lineNumbers(),
          annotGutterHandle.extension,
          blameGutterHandle.extension,
          diagGutterHandle.extension,
          commentGutterHandle.extension,
          ageOverlayExtension,
          storyOverlayExtension,
          conflictOverlayExtension,
          vimReader(stableVimCb),
          // V3.N1 — identifier occurrence tint (debounced; idle = no deco).
          // Composes alongside `highlightField` (server spans) and CM6's
          // own search matches via separate decoration providers.
          occurrenceHighlightExtension,
          // V3.1-H3a — param-name inlays at literal call-site args.
          stableParamHints,
          // V3.1-H3b — Code Vision lens chips (block widgets above decls).
          stableLenses,
          // V70-A6 — the inline peek block widget + the hover tooltip.
          stableInlinePeek,
          stableHover,
          highlightRangesFacet.of(ranges),
          highlightField,
          linkifyExtension(linkTokens, linkifyHandlersRef),
          EditorView.updateListener.of((update) => {
            const cbs = listenerCbRef.current;
            // Vim status transactions are effect-only (no selection), so
            // this check runs BEFORE the selectionSet early-return.
            if (cbs.onVimStatus) {
              const s = vimStatus(update.state);
              const prev = vimStatusRef.current;
              if (!prev || prev.mode !== s.mode || prev.pending !== s.pending) {
                vimStatusRef.current = s;
                cbs.onVimStatus(s);
              }
            }
            // Viewport geometry changes (scroll / doc size) → first-visible line.
            if (update.geometryChanged || update.viewportChanged || update.docChanged) {
              reportFirstVisible(update.view);
            }
            if (!update.selectionSet) return;
            const sel = update.state.selection.main;
            const hasSelection = sel.from !== sel.to;
            const scrolled = update.view.scrollDOM.scrollTop > 4;
            const dirty = hasSelection || scrolled;
            if (dirty !== dirtyRef.current) {
              dirtyRef.current = dirty;
              cbs.onViewerDirtyChange?.(dirty);
            }
            cbs.onCursorLineChange?.(update.state.doc.lineAt(sel.head).number);
            cbs.onSelectionLines?.({
              start: update.state.doc.lineAt(sel.from).number,
              end: update.state.doc.lineAt(sel.to).number,
            });
          }),
          // SH.C3 — line-wrap + font-size, each in its own Compartment so a
          // pref toggle reconfigures the LIVE view (see the two effects
          // below) instead of forcing this whole `new EditorView` to re-run.
          wrapCompartment.of(wrap ? EditorView.lineWrapping : []),
          fontSizeCompartment.of(readerThemeExtension(fontSize)),
        ],
      }),
    });
    viewRef.current = view;

    const scroller = view.scrollDOM;
    const onScroll = () => {
      const scrolled = scroller.scrollTop > 4;
      const sel = view.state.selection.main;
      const dirty = scrolled || sel.from !== sel.to;
      if (dirty !== dirtyRef.current) {
        dirtyRef.current = dirty;
        listenerCbRef.current.onViewerDirtyChange?.(dirty);
      }
      reportFirstVisible(view);
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    // Seed sticky line for a freshly-created view.
    firstVisRef.current = 0;
    reportFirstVisible(view);

    // Re-push whatever marker maps are ALREADY current (the props that
    // triggered this very effect via `blobHash`) — the marker-sync effect
    // below only fires on marker-prop changes, which won't happen again if
    // they were already set before this file/ref switch.
    blameGutterHandle.setMarkers(view, blameMarkersFrom(blameDots));
    annotGutterHandle.setMarkers(view, annotationMarkers ?? new Map());
    diagGutterHandle.setMarkers(view, diagMarkersFrom(diagnosticMarkers));
    commentGutterHandle.setMarkers(view, commentMarkersFrom(commentMarkers));
    setAgeOverlayLines(view, ageLines ?? new Map());
    setStoryOverlayLines(view, storyLines ?? []);
    setConflictOverlayActive(view, conflictActive ?? false);
    dirtyRef.current = false;

    return () => {
      scroller.removeEventListener("scroll", onScroll);
      view.destroy();
      viewRef.current = null;
    };
    // `ranges`/`linkTokens` are both derived from `content`/`spans` (already
    // in the dep list transitively via their own memos above) — re-keying
    // on `blobHash` is what actually matters (it's the one field guaranteed
    // to change whenever the underlying bytes do, including a same-path ref
    // switch).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [blobHash]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view || !gotoSel || gotoSel.start < 1) return;
    const lineCount = view.state.doc.lines;
    const startLine = view.state.doc.line(Math.min(gotoSel.start, lineCount));
    const endLine = view.state.doc.line(Math.min(Math.max(gotoSel.end, gotoSel.start), lineCount));
    const selection =
      startLine.number === endLine.number
        ? EditorSelection.cursor(startLine.from)
        : EditorSelection.range(startLine.from, endLine.to);
    view.dispatch({
      selection,
      effects: EditorView.scrollIntoView(startLine.from, { y: "center" }),
    });
  }, [gotoSel, blobHash]);

  // Marker-map sync — independent of `blobHash` (blame/annotation data
  // arrives from its own async query, on its own schedule).
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    blameGutterHandle.setMarkers(view, blameMarkersFrom(blameDots));
  }, [blameDots, blameGutterHandle]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    annotGutterHandle.setMarkers(view, annotationMarkers ?? new Map());
  }, [annotationMarkers, annotGutterHandle]);

  // PRR-U9 — independent of `blobHash` (diagnostics arrive from their own
  // async query, on their own schedule), same rationale as the two marker
  // syncs above.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    diagGutterHandle.setMarkers(view, diagMarkersFrom(diagnosticMarkers));
  }, [diagnosticMarkers, diagGutterHandle]);

  // V72-J2 — independent of `blobHash` for the same reason the marker syncs
  // above are: `GET /api/comments/file` resolves on its own schedule, and a
  // gutter-mode toggle re-filters the SAME already-fetched rows without a
  // new fetch (`Reader.tsx` recomputes `commentMarkers` from its own
  // `CommentGutterMode` state, which is what actually changes this prop's
  // identity on a mode cycle).
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    commentGutterHandle.setMarkers(view, commentMarkersFrom(commentMarkers));
  }, [commentMarkers, commentGutterHandle]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    setAgeOverlayLines(view, ageLines ?? new Map());
  }, [ageLines]);

  // Independent of `blobHash` for the SAME reason the marker-map syncs
  // above are: the story player's diff fetch (whose ranges this prop
  // carries) resolves on its own schedule, separate from the file-content
  // fetch that drives a view recreation. Every non-empty `storyLines`
  // identity change is treated as a fresh step by `setStoryOverlayLines`
  // (drives the CSS flash) — see `editor/storyOverlay.ts`'s doc.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    setStoryOverlayLines(view, storyLines ?? []);
  }, [storyLines]);

  // Phase G2 — independent of `blobHash` for the same reason as the marker/
  // overlay syncs above: `GET /api/repo-state` resolves on its own schedule,
  // separate from the file-content fetch that drives a view recreation (a
  // merge can start or resolve while the file is already open, with no
  // path/ref change at all).
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    setConflictOverlayActive(view, conflictActive ?? false);
  }, [conflictActive]);

  // SH.C3 — reconfigure the wrap/font-size compartments IN PLACE on a pref
  // change. Independent of `blobHash` for the same reason as the syncs
  // above: a reading-mode toggle mid-file must not remount the view (that
  // would blow away scroll position and the live selection).
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: wrapCompartment.reconfigure(wrap ? EditorView.lineWrapping : []) });
  }, [wrap, wrapCompartment]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: fontSizeCompartment.reconfigure(readerThemeExtension(fontSize)) });
  }, [fontSize, fontSizeCompartment]);

  // V70-A4 — the Desk makes this box resizable, so CM6 has to be TOLD.
  // Marijn's guidance (https://discuss.codemirror.net/t/resizing-
  // codemirror-6/3265, cited in docs/research/kb-code-v7-evidence/
  // research/panel-layout-system.md §1.11): drive re-measurement from a
  // `ResizeObserver` on the container rather than polling, and put every
  // DOM read inside `requestMeasure`'s `read` phase so repeated calls
  // during a drag are batched into the next measure cycle instead of
  // forcing a synchronous reflow per frame.
  //
  // The other half of that guidance is CSS, and it lives in
  // `styles/desk.css`: every wrapper between the sized Panel and
  // `.cm-scroller` carries an explicit height, or the scroller's
  // computed height goes wrong.
  useEffect(() => {
    const host = parentRef.current;
    if (!host || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      const view = viewRef.current;
      if (!view) return;
      view.requestMeasure({
        read: () => null,
        write: () => {},
      });
    });
    ro.observe(host);
    return () => ro.disconnect();
  }, []);

  return (
    <div className="kbc-codeview-wrap">
      <StickyContext
        symbols={symbols ?? []}
        firstVisibleLine={firstVisibleLine}
        enabled={stickyContextEnabled}
        onJump={(line) => onStickyJump?.(line)}
      />
      <div className="kbc-codeview" ref={parentRef} />
    </div>
  );
});

export default CodeView;
