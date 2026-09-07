import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  fetchDefs,
  fetchFile,
  fetchHierarchyCallees,
  fetchHierarchyCallers,
  fetchHierarchyTypes,
  fetchImpactAnalysis,
  fetchResolve,
  fetchResolveSymbol,
  fetchSet,
  fetchUsages2,
  fetchActions,
  fetchWhyLine,
  fetchXrefs,
} from "../api/client";
import Breadcrumbs from "../components/Breadcrumbs";
import CodeView, { type CodeViewHandle, type GotoSel } from "../components/CodeView";
import DiffView from "../components/DiffView";
import EmptyState from "../components/EmptyState";
import FileTree, { type FileTreeHandle } from "../components/FileTree";
import { Icon } from "../components/icons";
import InspectorRail, { type InspectorRailHandle } from "../components/InspectorRail";
import KeyboardHelp from "../components/KeyboardHelp";
import { useCommandHandlers, useCommandScope, useCommands } from "../commands/CommandRoot";
import { GdCoachMark, useGdCoachMark } from "../commands/learn";
import LineHistoryPopup from "../components/LineHistoryPopup";
import RecentLocations from "../components/RecentLocations";
import StructurePopup from "../components/StructurePopup";
import MnemonicPopup from "../components/bookmarks/MnemonicPopup";
import { FileChangedToast, HeadMovedBanner } from "../components/LiveMirrorBanners";
import HierarchyPanel from "../components/hierarchy/HierarchyPanel";
import ImpactPanel from "../components/impact/ImpactPanel";
import EgoGraph from "../components/graph/EgoGraph";
import EntityRail from "../components/entity/EntityRail";
import DossierRail from "../components/entity/DossierRail";
import DossierView from "../components/entity/DossierView";
import PeekPanel, { type PeekAnchor } from "../components/peek/PeekPanel";
import UsagesDock from "../components/usages/UsagesDock";
import ActionMenu, { ActionPill } from "../components/actions/ActionMenu";
import AddToBoardDialog from "../components/boards/AddToBoardDialog";
import RepoStateBanner from "../components/RepoStateBanner";
import AddToSetMenu from "../components/sets/AddToSetMenu";
import BlameChip from "../components/provenance/BlameChip";
import CommentGutterCard from "../components/comments/CommentGutterCard";
import CommentsPanel from "../components/comments/CommentsPanel";
import DiagnosticsCard from "../components/provenance/DiagnosticsCard";
import FrameworkCard from "../components/provenance/FrameworkCard";
import StoryTimeline from "../components/provenance/StoryTimeline";
import WhyPanel from "../components/provenance/WhyPanel";
import HistoryPanel from "../components/history/HistoryPanel";
import CitedBy from "../components/lens/CitedBy";
import RefPicker from "../components/RefPicker";
import StoryPlayer from "../components/story/StoryPlayer";
import WorkingSetStrip, { type ActiveWorkspaceChip } from "../components/WorkingSetStrip";
import SaveWorkspaceDialog from "../components/workspaces/SaveWorkspaceDialog";
import WorkspaceNotesPanel from "../components/workspaces/WorkspaceNotesPanel";
import Desk, { type DeskRailSlotCtx } from "../desk/Desk";
import { effectiveCollapsed, useDesk } from "../desk/useDesk";
import { placementFor } from "../desk/placement";
import {
  cycleMemberSort,
  DEFAULT_MEMBER_SORT,
  DEFAULT_USAGES_PER_KIND,
  DOSSIER_SECTIONS,
  stepSection,
  type DossierSectionId,
  type MemberSort,
} from "../lib/dossier";
import { useDossier } from "../hooks/useDossier";
import { drawerSetId, drawerTabOrder, type DrawerRow } from "../desk/drawerSets";
import type { RailTab } from "../desk/deskState";
import {
  NO_CHIPS,
  applyChips,
  groupRows,
  stepCursor,
  usagesTitle,
  walkOrder,
  type GroupAxis,
  type UsageChips,
} from "../lib/usages2";
import { pillRows, resolveOp } from "../lib/actionOps";
import type { ActionRow, ActionsOut, ActionTarget, Usages2Out, UsageRow2 } from "../api/types";
import type { LinkifyCallbacks } from "../editor/linkify";
import type { LineMarkerSpec } from "../editor/lineGutter";
import { copyToClipboard, type LineSel, type VimReaderCallbacks, type WordPos } from "../editor/vimReader";
import { useActiveWorkspace } from "../hooks/useActiveWorkspace";
import { useAnnotations, useCreateAnnotation } from "../hooks/useAnnotations";
import { useBlame } from "../hooks/useBlame";
import { useCommentKeywords, useCommentsFile } from "../hooks/useComments";
import { useBlameAttributions } from "../hooks/useBlameAttributions";
import {
  useBookmarks,
  useCreateBookmark,
  useDeleteBookmark,
} from "../hooks/useBookmarks";
import { useDiagnostics } from "../hooks/useDiagnostics";
import { useFile } from "../hooks/useFile";
import { useFileHistory } from "../hooks/useFileHistory";
import { useIsMobile } from "../hooks/useIsMobile";
import { useRailSubject } from "../hooks/useRailSubject";
import { useLiveMirror } from "../hooks/useLiveMirror";
import { useRepos } from "../hooks/useRepos";
import { useRepoState } from "../hooks/useRepoState";
import { usePatchSet, useSet } from "../hooks/useSets";
import { useWorkingSet } from "../hooks/useWorkingSet";
import { buildAgeLineBuckets, type AgeLineInfo } from "../lib/ageHeatmap";
import { annotationGutterTitle, annotationsByLine, unresolvedCount } from "../lib/annotations";
import { buildLineDots, regionCoveringLine, type BlameDotInfo } from "../lib/blameGutter";
import {
  ACTIONABLE_STATES,
  claimAnnotationBody,
  commentAtLine,
  commentGutterMarkers,
  filterCommentsForMode,
  isBridgeable,
  nextCommentLine,
  nextGutterMode,
  type CommentGutterMode,
} from "../lib/comments";
import { diagnosticGutterMarks, type DiagnosticGutterMark } from "../lib/diagnostics";
import {
  codeUrl,
  commitUrl,
  entityUrl,
  formatLineParam,
  parseEntParam,
  parseLineParam,
  parsePane2,
  permalinkFor,
  storyUrl,
  type LineSel as PaneLineSel,
  type PaneLoc,
  type TrailVia,
} from "../lib/codeUrl";
import { createCursorUrlSync, createPane2CursorUrlSync, type CursorUrlSync } from "../lib/cursorUrlSync";
import { currentHistoryIndex, historyStepTarget } from "../lib/historyStep";
import { workspacesUrl } from "../lib/setsUrl";
import { isWorkingSetDirty } from "../lib/workspaceDirty";
import { buildWorkspaceSnapshot, parseWorkspaceSnapshot } from "../lib/workspaceSnapshot";
import { initialLadderState, ladderReducer } from "../lib/ladderState";
import { fileChangedPaneLabel } from "../lib/liveMirror";
import { nextKeyboardRegion, type KeyboardRegion } from "../lib/keyboardRegion";
import {
  defRowsFrom,
  initialPeekState,
  peekReducer,
  resolveCandidateToRow,
  singleExactMatch,
  type HoverCard,
  type HoverProvenance,
  type PeekRow,
} from "../lib/peekState";
import {
  ancestorKeys,
  buildCalleesTree,
  buildCallersTree,
  buildTypesTree,
  calleesToNodes,
  callersToNodes,
  collectAncestorLocs,
  hierarchyReducer,
  HIERARCHY_DEPTH_CAP,
  initialHierarchyState,
  isTypeIshKind,
  typeEdgesToNodes,
  updateNode,
  type HierarchyMode,
  type HierarchyNode,
} from "../lib/hierarchyState";
import {
  impactReducer,
  initialImpactState,
  isImpactNavigable,
  type ImpactFlatRow,
} from "../lib/impactState";
import {
  layoutEgoGraph,
  type EgoLayoutInput,
  type EgoLaidOutNode,
  type EgoLayoutResult,
} from "../lib/egoGraph";
import { getRecentFiles, goBack, goForward, recordJump } from "../lib/navHistory";
import PaneHistoryNav from "../components/nav/PaneHistoryNav";
import { useRamp, type RampRung, type RampTarget } from "../nav/ramp";
import { emptyLocation, encode as encodeLocation } from "../nav/location";
import {
  closedInlinePeek,
  inlinePeekReducer,
  isOpen as inlinePeekIsOpen,
  topFrame as inlinePeekTop,
} from "../lib/inlinePeek";
import type { InlinePeekRender } from "../editor/inlinePeek";
import {
  isProvisional,
  markProvisional,
  noteInteraction,
  paneModifiers,
  pin as pinPane,
  NO_PROVISIONAL,
} from "../lib/provisionalPane";
import { loadProvisionalPanes } from "../lib/prefs";
import {
  loadCodeLenses,
  loadCommentGutterMode,
  loadParamHints,
  loadReaderFontSize,
  loadStickyContext,
  loadWrap,
  READER_FONT_SIZE_MAX,
  READER_FONT_SIZE_MIN,
  saveCodeLenses,
  saveCommentGutterMode,
  saveParamHints,
  saveReaderFontSize,
  saveStickyContext,
  saveWrap,
} from "../lib/prefs";
import { sessionUrl } from "../lib/searchLanes";
import { toast } from "../lib/toast";
import type { AttributionOut, EntryKind, LensDeclaration, Span } from "../api/types";
import { useLenses } from "../hooks/useLenses";

const DIFF_SENTINEL = "~diff";
/// Phase C7 — like `DIFF_SENTINEL` above, a FILE-scoped sentinel trailing an
/// arbitrary path (`lib/codeUrl.ts`'s `storyUrl`), so it rides Reader's own
/// splat parsing rather than getting a dedicated `<Route>` the way the
/// repo-scoped `~commit`/`~compare`/`~branches` sentinels below do.
const STORY_SENTINEL = "~story";
/// Phase C-SPA — the repo-scoped, non-file sentinels registered as their
/// OWN routes in `app.tsx` (`~commit/:sha`, `~compare`, `~branches` — see
/// `lib/codeUrl.ts`'s header comment for why these don't ride Reader's
/// splat the way `~diff` does). React Router always prefers those static
/// routes for a well-formed URL, so this list only matters DEFENSIVELY: a
/// malformed URL missing a required param (e.g. bare `~commit`, no `:sha`)
/// falls through to this catch-all, and must render an honest hint rather
/// than treat the sentinel as a literal file/dir path.
const HISTORY_SENTINELS = [
  "~commit",
  "~compare",
  "~branches",
  "~range-diff",
  "~prs",
  "~sets",
  "~todos",
  "~hotspots",
  "~reviews",
];

function isHistorySentinel(splat: string): boolean {
  return HISTORY_SENTINELS.some((s) => splat === s || splat.startsWith(`${s}/`));
}

/// The reader's Provenance overlay is a 3-state control (Wave C) —
/// `off` (nothing fetched), `dots` (the original W4.4 gutter-dot disclosure
/// ladder), `age` (the new line-background heatmap, `lib/ageHeatmap.ts`).
type ProvenanceMode = "off" | "dots" | "age";

function isEditableTarget(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
}

/// True when the event originated inside the CM6 buffer — the vim keymap
/// (`editor/vimReader.ts`) owns EVERY key there; the window-level handler
/// below must never double-handle (`j` would move BOTH the code cursor and
/// the tree's row cursor). Matches EITHER pane's buffer (`.kbc-codeview`
/// appears once per open pane).
function isInsideBuffer(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && typeof t.closest === "function" && t.closest(".kbc-codeview") !== null;
}

/// SH.C3 — the "start here" panel shown alongside the empty "select a file"
/// state (a repo landing with a populated tree but no open buffer used to
/// be a near-total void below the EmptyState card). Both cards read data
/// the app ALREADY tracks client-side — no new fetch, no new endpoint:
/// `getRecentFiles()` is the same `lib/navHistory.ts` ring the `g.`
/// `RecentLocations` popup reads, just filtered to this repo; the keyboard
/// card is purely static (a condensed pointer into the full `?` cheatsheet,
/// not a duplicate of it).
function ReaderStartCards({ repo, onOpen }: { repo: string; onOpen: (path: string) => void }) {
  const recent = useMemo(
    () => getRecentFiles().filter((f) => f.repo === repo).slice(0, 6),
    [repo],
  );
  return (
    <div className="kbc-reader-start__cards">
      {recent.length > 0 && (
        <section className="kbc-reader-start__card" aria-label="Continue where you left off">
          <h2>Continue where you left off</h2>
          <ul>
            {recent.map((f) => (
              <li key={f.path}>
                <button
                  type="button"
                  className="kbc-reader-start__row"
                  onClick={() => onOpen(f.path)}
                  data-kbc-start-recent={f.path}
                >
                  {f.path}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <section className="kbc-reader-start__card" aria-label="Keyboard essentials">
        <h2>Keyboard essentials</h2>
        <dl>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>/</kbd>
            </dt>
            <dd>filter the tree</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>⌘K</kbd>
            </dt>
            <dd>search everywhere</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>g.</kbd>
            </dt>
            <dd>recent locations</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>?</kbd>
            </dt>
            <dd>full cheatsheet</dd>
          </div>
        </dl>
      </section>
    </div>
  );
}

/// V70-A6 — one small strip per pane: the history arrows (`pane.back` /
/// `pane.forward`, count-badged, hover-previewed) and the provisional chip.
///
/// §P7 caps a pane at TWO modifier chips in one row (frame · follow · trail);
/// `lib/provisionalPane.ts`'s `paneModifiers` owns that cap and its fixed
/// order, so the slot is reserved before the units that fill it (frame is
/// D14, follow is the linked-pane unit) rather than discovered when the third
/// chip arrives.
function PaneChrome({
  pane,
  provisional,
  onBack,
  onForward,
  onPin,
}: {
  pane: 1 | 2;
  provisional: boolean;
  onBack(): void;
  onForward(): void;
  onPin(): void;
}) {
  const mods = paneModifiers(provisional ? ["provisional"] : []);
  return (
    <div
      className={"kbc-panechrome" + (provisional ? " kbc-panechrome--provisional" : "")}
      data-kbc-panechrome={pane}
    >
      <PaneHistoryNav pane={pane} onBack={onBack} onForward={onForward} />
      <span className="kbc-panechrome__mods">
        {mods.map((m) =>
          m === "provisional" ? (
            <button
              key={m}
              type="button"
              className="kbc-panechrome__chip"
              data-cmd="pane.pin"
              data-kbc-pane-provisional
              title="this pane was promoted from a peek — pin it to keep it"
              onClick={onPin}
            >
              provisional — <kbd>p</kbd> to pin
            </button>
          ) : null,
        )}
      </span>
    </div>
  );
}

/// The reader's home surface (W4.2, extended by W4.4/W4.5/W4.6, re-centered
/// by A3, split by Wave E): top breadcrumbs + ref picker + a "Provenance"
/// toggle, left file tree, center CM6 read-only viewer(s) (or the diff
/// view), right inspector rail (outline / provenance / annotations). Live-
/// mirror affordances (W4.5) float above the body: a dismissible file-
/// changed toast per open pane and a HEAD-moved banner.
///
/// **Focus model (A3, extended by Wave E)**: a BUFFER is the primary focus
/// surface — but now there can be TWO (`pane1`/`pane2`), and `focusedPane`
/// (1 | 2, plain UI state — not itself part of the shareable URL, same
/// footing as `treeVisible`) tracks which one is live. `focusedPane` is kept
/// in sync with REAL DOM focus via each pane's own `onFocus` (React's
/// synthetic focus event bubbles from the CM6 content div through the
/// wrapper), so both a mouse click into a pane and a programmatic
/// `Ctrl-w h/l` `.focus()` call converge on the same source of truth — no
/// separate bookkeeping needed in the pane-focus handler itself. Every
/// interactive vim action (`gd`/`gr`/`K`/`a`/`Y`/`[c ]c`/`[f ]f`/`Ctrl-w
/// v`/`Ctrl-w q`) is bound PER PANE (`vimCallbacksForPane`), parameterized
/// by which pane it came from — since only the DOM-focused pane's own CM6
/// keymap instance can ever dispatch a keypress, "which pane fired this"
/// and "the focused pane" are always the same value by construction.
///
/// **Shared inspector state follows focus**: blame/annotations/history/peek
/// (the right rail's content) are each a SINGLE hook instance, keyed to
/// `focusedPath`/`focusedRef` (whichever pane currently has focus) rather
/// than duplicated per pane — switching focus re-points the whole rail at
/// the newly-focused pane's file. Each CodeView only receives non-null
/// `blameDots`/`ageLines`/`annotationMarkers` while ITS pane is the focused
/// one (`focusedPane === 1|2` gates); the unfocused pane still renders its
/// own highlights + its own file's content, but no gutter markers, and its
/// own cursor→URL sync is simply never fed a selection while unfocused (see
/// `cursorSyncRef1`/`cursorSyncRef2` below) — so only the active view's
/// position is ever bookmarked in the address bar.
///
/// **Position flows through the URL (A3, extended by Wave E)**: pane1's
/// `?line=` is unchanged; pane2's own position lives entirely inside the
/// `?pane2=<path@ref:line>` grammar (`lib/codeUrl.ts`, A2-reserved, Wave E's
/// first consumer) — `parsePane2`/`formatPane2` are the ONLY parser/
/// builder for it. `pane2Loc` is derived PURELY from the URL (a `useMemo`
/// over the raw query param) — there is no separate "is a split open" piece
/// of React state to drift out of sync with it.
export default function Reader() {
  const { repo = "" } = useParams<{ repo: string }>();
  const splat = useParams()["*"] ?? "";
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  // V70-A6 — `u` inside the CM6 buffer forwards to the SAME registered
  // `nav.back` handler every other surface uses (`app.tsx`), rather than
  // duplicating its trail-origin-or-plain-Back decision here.
  const commandBus = useCommands();

  const diffMode = splat === DIFF_SENTINEL || splat.endsWith(`/${DIFF_SENTINEL}`);
  const storyMode = !diffMode && (splat === STORY_SENTINEL || splat.endsWith(`/${STORY_SENTINEL}`));
  const historySentinelMode = !diffMode && !storyMode && isHistorySentinel(splat);
  const path = diffMode
    ? splat.slice(0, -DIFF_SENTINEL.length).replace(/\/$/, "")
    : storyMode
      ? splat.slice(0, -STORY_SENTINEL.length).replace(/\/$/, "")
      : historySentinelMode
        ? ""
        : splat;
  const gitRef = searchParams.get("ref") ?? undefined;
  const fromRef = searchParams.get("from") ?? undefined;
  const toRef = searchParams.get("to") ?? undefined;
  const atRef = searchParams.get("at") ?? undefined;
  const lineParam = searchParams.get("line");
  const pane2Param = searchParams.get("pane2");
  // T1 (design-ui.md §5/§9.2) — `?sym=` deep link, resolved on load below.
  const symParam = searchParams.get("sym");
  // V72-G1.2 (§P9/D6) — `?ent=` puts this SAME route into the shell's
  // `dossier` center mode. Deliberately NOT a new route: D1's rule is that a
  // new surface lands in an existing REGION, and the dossier's dock, rail,
  // drawer and stripes are the ones the reader already has. `parseEntParam`
  // (lib/codeUrl.ts) is the one parser — a blank `ent=` is not an address.
  const entParam = parseEntParam(searchParams.get("ent"));
  const dossierMode = entParam !== null;
  // Dossier VIEW state. None of it belongs in the URL: `?ent=` names the
  // PLACE, and the sort/inherited/usages cut are refinements of how that one
  // place is read — the Location Contract's own axis (`samePlace` keys on
  // `ent`, and a refinement is a replace at most). `inherited` is the one that
  // re-fetches, because it changes what the SERVER sends, not what this side
  // shows.
  const [dossierInherited, setDossierInherited] = useState(false);
  const [dossierSort, setDossierSort] = useState<MemberSort>(DEFAULT_MEMBER_SORT);
  const [dossierUsagesPerKind, setDossierUsagesPerKind] = useState(DEFAULT_USAGES_PER_KIND);
  const [dossierSection, setDossierSection] = useState<DossierSectionId | null>(null);
  const dossier = useDossier(repo, entParam, dossierInherited, dossierUsagesPerKind);

  // F5 — mobile shell (≤860px). `isMobile` is read FIRST so the lazy state
  // initializers below can close over its already-current value (both
  // `useState` calls run within the same initial render pass) — no flash of
  // an open tree/sheet before a resize-driven correction lands.
  const isMobile = useIsMobile();
  // V70-A4 — the Desk owns every piece of shell geometry (region sizes,
  // collapse, the preset, the rail tab, the drawer's result-set ring).
  // Persisted per repo, read synchronously before first paint; see
  // `desk/deskState.ts` for why it is not `autoSaveId`.
  const desk = useDesk(repo);
  // The MOBILE dock's open state only. On desktop the dock's visibility
  // IS `desk.state.regions.dock.collapsed` — one home for one fact — and
  // this flag is inert there (see `toggleDock` below).
  const [treeVisible, setTreeVisible] = useState(false);
  // F5 — the mobile-only "reader tools" bottom sheet (`.kbc-reader__outline`
  // promoted via CSS, mirrors kb's own `.kb-pinsp` sheet promotion, root
  // CLAUDE.md invariant #30). Always false on desktop in practice (nothing
  // ever sets it there); gated on `isMobile` at every render site rather than
  // reset on breakpoint change, since a `!isMobile` guard already keeps the
  // CSS + `asSheet` prop inert regardless of this flag's value.
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [gotoSel1, setGotoSel1] = useState<GotoSel | null>(null);
  const [gotoSel2, setGotoSel2] = useState<GotoSel | null>(null);
  const [helpOpen, setHelpOpen] = useState(false);
  /// D24 — the one-time `gd` coach-mark for the commitment Ramp.
  const coach = useGdCoachMark();
  /// V3.N1 — Recent Locations popup (`g.`).
  const [recentLocsOpen, setRecentLocsOpen] = useState(false);
  // V3.N2 — structure / bookmarks popups + sticky pref + cursor line for crumbs.
  const [structureOpen, setStructureOpen] = useState(false);
  const [mnemonicOpen, setMnemonicOpen] = useState(false);
  /// V3.R2 / R12 — `gh` history-for-selection popup target.
  const [lineHistory, setLineHistory] = useState<{
    path: string;
    line: number;
    lineEnd?: number;
  } | null>(null);
  const [stickyEnabled, setStickyEnabled] = useState(() => loadStickyContext());
  const [paramHintsEnabled, setParamHintsEnabled] = useState(() => loadParamHints());
  const [codeLensesEnabled, setCodeLensesEnabled] = useState(() => loadCodeLenses());
  // SH.C3 — reading-mode prefs (line wrap + CM6 font size), same
  // useState-lazy-init + prefs.ts round-trip pattern as the three above.
  const [wrapEnabled, setWrapEnabled] = useState(() => loadWrap());
  const [readerFontSize, setReaderFontSize] = useState(() => loadReaderFontSize());
  const [cursorLineUi, setCursorLineUi] = useState(1);
  const [copiedTick, setCopiedTick] = useState(0);
  /// Suppress the next file-open `recordJump` when the navigation was
  /// driven by Ctrl-o / Ctrl-i (the jump list already moved its pointer;
  /// re-recording would clobber the forward half).
  ///
  /// V70-A6 fixed the recon's R1 leak: this was set UNCONDITIONALLY by
  /// `navigateToNavLocation`, but the consuming effect only runs on
  /// `[repo, activeFile]` change — so a `Ctrl-o` landing on a different LINE
  /// of the file already open never re-fired it, the flag stayed `true`, and
  /// it silently swallowed the NEXT genuine file-open record. It is now set
  /// only when the target is actually a different file (the guard both
  /// sibling call sites already had — `handleJumpBookmark` and the
  /// RecentLocations pick — which is what showed the hazard was known).
  const skipNavRecordRef = useRef(false);
  /// V70-A6 — the typed edge the NEXT file-open record should carry. Set by
  /// whichever gesture caused the navigation and consumed once, so the ring
  /// records WHY a jump happened, not just where it landed (recon G7).
  const pendingViaRef = useRef<TrailVia | undefined>(undefined);
  const [vimStat, setVimStat] = useState<{ mode: string; pending: string }>({ mode: "normal", pending: "" });
  const nonceRef = useRef(0);
  const treeRef = useRef<FileTreeHandle | null>(null);
  const inspectorRef = useRef<InspectorRailHandle | null>(null);
  const codeViewRef1 = useRef<CodeViewHandle | null>(null);
  const codeViewRef2 = useRef<CodeViewHandle | null>(null);
  // Phase E4 — the "+ Set" capture affordance's own idea of "the current
  // selection": each pane's latest `onSelectionLines` report, kept purely so
  // a click on "+ Set" can grab it at that moment (`AddToSetMenu`'s
  // `getSelection` prop) — mirrors `cursorLineRef1`/`cursorLineRef2` below
  // (a ref, not state, since nothing here needs to RE-RENDER on every
  // selection change).
  const lastSelRef1 = useRef<{ start: number; end: number } | null>(null);
  const lastSelRef2 = useRef<{ start: number; end: number } | null>(null);

  const isFile = path !== "" && !path.endsWith("/");
  const activeFile = isFile && !diffMode && !storyMode ? path : undefined;
  const file = useFile(repo, activeFile, gitRef);
  const lenses1 = useLenses(
    repo,
    activeFile,
    file.data?.blob_hash,
    codeLensesEnabled && !!activeFile,
  );
  const repos = useRepos();
  const repoRoot = repos.data?.repos.find((r) => r.name === repo)?.path;

  // Phase G2 — the repo-state banner + the CM6 conflict-marker line tint.
  // `RepoStateBanner` fetches its OWN copy of `GET /api/repo-state` (the
  // SAME `["repo-state", repo]` query key — React Query dedupes to one
  // network call), so this component only needs the `conflicted` set to
  // decide whether EITHER open pane's file gets the tint.
  const repoState = useRepoState(repo);
  const conflictedPaths = useMemo(() => new Set(repoState.data?.conflicted ?? []), [repoState.data]);

  // --- Wave E — pane2 is derived PURELY from the URL, gated the same way
  // `activeFile` is (a file, not a diff) — a split has no meaning over the
  // diff view, story mode, or a directory listing.
  const canSplit = isFile && !diffMode && !storyMode;
  const pane2Loc = useMemo<PaneLoc | null>(() => (canSplit ? parsePane2(pane2Param) : null), [canSplit, pane2Param]);
  const pane2File = useFile(repo, pane2Loc?.path, pane2Loc?.ref);
  const lenses2 = useLenses(
    repo,
    pane2Loc?.path,
    pane2File.data?.blob_hash,
    codeLensesEnabled && !!pane2Loc?.path,
  );

  const [focusedPane, setFocusedPane] = useState<1 | 2>(1);
  // A split that closes (or was never opened) while pane2 was focused has
  // nowhere for that focus to point — fall back to pane1.
  useEffect(() => {
    if (!pane2Loc && focusedPane === 2) setFocusedPane(1);
  }, [pane2Loc, focusedPane]);
  // A fresh pane's vim state always starts at "normal" — without this, a
  // status chip left over from the PREVIOUS pane's visual-mode selection
  // would misleadingly linger after focus moves.
  useEffect(() => {
    setVimStat({ mode: "normal", pending: "" });
  }, [focusedPane]);

  const focusedPath = focusedPane === 1 ? activeFile : pane2Loc?.path;
  const focusedRef = focusedPane === 1 ? gitRef : pane2Loc?.ref;

  // V70-A4 — mirror the reader's focused pane into the desk so the
  // placement table and the resize submode read ONE value. The reader
  // stays the source of truth (its pane state is URL-derived); the desk
  // only follows.
  const deskDispatch = desk.dispatch;
  useEffect(() => {
    deskDispatch({ type: "focusPane", pane: focusedPane });
  }, [deskDispatch, focusedPane]);
  useEffect(() => {
    deskDispatch({ type: "setPaneCount", count: pane2Loc ? 2 : 1 });
  }, [deskDispatch, pane2Loc]);

  const workingSet = useWorkingSet(repo);
  // "touch on every file open," regardless of HOW it was opened (tree
  // click, Shift+Enter, a hard-navigated permalink, `gd`, a breadcrumb…) —
  // keying this off the resolved path itself (not each individual call
  // site) is what makes that guarantee total rather than best-effort.
  useEffect(() => {
    if (activeFile) workingSet.touch(activeFile);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFile]);
  useEffect(() => {
    if (pane2Loc?.path) workingSet.touch(pane2Loc.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pane2Loc?.path]);

  // V3.N1 — record a jump on every file open (path change). Skipped when
  // the navigation came from Ctrl-o / Ctrl-i (`skipNavRecordRef`).
  //
  // V70-A6: PANE 1's recorder. Pane 2 has its own, right below — the ring is
  // per pane now (recon G6: opening a file into pane 2 used to be invisible
  // to `Ctrl-o` and to `g.` entirely). Both carry the typed `via` the gesture
  // set and a real snippet, so `g.` renders code rather than a bare path list
  // (recon R2).
  useEffect(() => {
    if (!activeFile || !repo) return;
    if (skipNavRecordRef.current) {
      skipNavRecordRef.current = false;
      pendingViaRef.current = undefined;
      return;
    }
    const line = parseLineParam(lineParam)?.start ?? 1;
    const via = pendingViaRef.current;
    pendingViaRef.current = undefined;
    recordJump({ repo, path: activeFile, line, snippet: "", pane: 1, via });
    // Only re-fire on path/repo change — not every line-param tick.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, activeFile]);

  useEffect(() => {
    const p2 = pane2Loc?.path;
    if (!p2 || !repo) return;
    const via = pendingViaRef.current;
    pendingViaRef.current = undefined;
    recordJump({
      repo,
      path: p2,
      line: typeof pane2Loc?.line === "number" ? pane2Loc.line : (pane2Loc?.line?.start ?? 1),
      snippet: "",
      pane: 2,
      via,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, pane2Loc?.path]);

  /// The (repo-relative) path/ref/CodeView-ref this pane is currently
  /// showing — the one place every per-pane handler below resolves "which
  /// pane am I acting on" from, so pane1/pane2 stay symmetric rather than
  /// one being the "real" implementation and the other a bolt-on copy.
  function paneRepoPath(pane: 1 | 2) {
    return pane === 1
      ? { path: activeFile, ref: gitRef, viewRef: codeViewRef1 }
      : { path: pane2Loc?.path, ref: pane2Loc?.ref, viewRef: codeViewRef2 };
  }

  /// Phase E4 — the FOCUSED pane's last-reported selection lines, or `null`
  /// (no selection — a plain collapsed cursor). `AddToSetMenu`'s
  /// `getSelection` prop; read once at click time, never re-rendered on.
  function currentPaneSelection(): { start: number; end: number } | null {
    return focusedPane === 1 ? lastSelRef1.current : lastSelRef2.current;
  }

  /// Build a reader URL, defaulting EITHER slot to its current value unless
  /// explicitly overridden — every Wave E navigation (splits open/close,
  /// per-pane history stepping, tree/working-set opens, peek/linkify
  /// jumps) goes through this ONE function, so "leave whatever pane I
  /// didn't touch exactly as it is" is enforced here rather than re-derived
  /// at each call site. `pane2: null` clears it; `pane2: undefined` (or
  /// omitted) keeps whatever's currently open.
  function buildReaderUrl(next: {
    pane1?: { path: string; ref?: string; line?: PaneLineSel };
    pane2?: PaneLoc | null;
  }): string {
    const currentPane1Line = parseLineParam(lineParam) ?? undefined;
    const pane1 = next.pane1 ?? { path: activeFile ?? "", ref: gitRef, line: currentPane1Line };
    const pane2 = next.pane2 === undefined ? pane2Loc ?? undefined : (next.pane2 ?? undefined);
    return codeUrl({ repo, path: pane1.path, ref: pane1.ref, line: pane1.line, pane2 });
  }

  /// A resolved candidate/peek-row landing: same-repo lands in `pane`
  /// (preserving whatever the OTHER pane is showing); a different repo
  /// always opens as a fresh single-pane view there — a split is scoped to
  /// one repo (`PaneLoc` carries no `repo` field, see `lib/codeUrl.ts`), so
  /// crossing repos closes it regardless of which pane asked.
  function navigateToCandidate(pane: 1 | 2, candRepo: string, candPath: string, line: number) {
    // V3.N1 — same-file landings need an explicit record (the file-open
    // effect only fires on path change). Cross-file opens are recorded by
    // that effect once `activeFile` updates.
    const panePath = pane === 1 ? activeFile : pane2Loc?.path;
    if (candRepo === repo && candPath === panePath) {
      recordJump({ repo: candRepo, path: candPath, line, snippet: "" });
    }
    if (candRepo !== repo) {
      navigate(codeUrl({ repo: candRepo, path: candPath, line }));
      return;
    }
    if (pane === 1) navigate(buildReaderUrl({ pane1: { path: candPath, line } }));
    else navigate(buildReaderUrl({ pane2: { path: candPath, line } }));
  }

  /// Open `targetPath` (same repo) into the focused pane, or explicitly
  /// into pane2 (`Shift+Enter`/Shift+click/middle-click) — the shared entry
  /// point for the file tree and the working-set strip. Inherits pane1's
  /// current `?ref=` either way (mirrors the pre-Wave-E tree click, which
  /// already preserved `gitRef` across a plain file-to-file navigation).
  /// Jump recording is owned by the file-open effect on `activeFile`.
  function openPath(targetPath: string, target: "focused" | "pane2") {
    // V70-A4 — an explicit `"pane2"` ask always wins; a plain open goes
    // through the placement rule table (`desk/placement.ts`), which is
    // where "file open → main.focused UNLESS that pane is pinned" is
    // written down once for every caller.
    const home = placementFor("file-open", {
      pinned: desk.state.panes.pinned,
      focused: focusedPane,
      paneCount: pane2Loc ? 2 : 1,
    });
    const otherPane: 1 | 2 = focusedPane === 1 ? 2 : 1;
    // `main.focused` is the pre-V70-A4 behaviour byte for byte (open into
    // whichever pane has focus); `main.other` is the ONE thing the table
    // adds — a pinned pane refuses to be replaced and the open lands
    // beside it instead.
    const toPane: 1 | 2 =
      target === "pane2" ? 2 : home.region === "main" && home.pane === "other" ? otherPane : focusedPane;
    if (toPane === 2) {
      navigate(buildReaderUrl({ pane2: { path: targetPath, ref: gitRef } }));
      setFocusedPane(2);
    } else {
      navigate(buildReaderUrl({ pane1: { path: targetPath, ref: gitRef } }));
      setFocusedPane(1);
    }
  }

  /// Snapshot a pane's current location for jump-list / recent.
  function currentJumpLocation(pane: 1 | 2 = focusedPane): {
    repo: string;
    path: string;
    line: number;
    snippet: string;
  } {
    const path = pane === 1 ? activeFile : pane2Loc?.path;
    const line = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    return { repo, path: path ?? "", line: line || 1, snippet: "" };
  }

  function navigateToNavLocation(target: { repo: string; path: string; line: number }) {
    // R1 fix (see `skipNavRecordRef`'s own doc): only suppress the record
    // when the file-open effect will actually FIRE, i.e. when the path really
    // changes. Setting it for a same-file line jump left the flag armed and
    // ate the next genuine record.
    const currentPath = focusedPane === 2 && pane2Loc ? pane2Loc.path : activeFile;
    skipNavRecordRef.current = target.repo !== repo || target.path !== currentPath;
    if (target.repo !== repo) {
      navigate(codeUrl({ repo: target.repo, path: target.path, line: target.line }));
      return;
    }
    if (focusedPane === 2 && pane2Loc) {
      navigate(buildReaderUrl({ pane2: { path: target.path, ref: pane2Loc.ref, line: target.line } }));
    } else {
      navigate(buildReaderUrl({ pane1: { path: target.path, ref: gitRef, line: target.line } }));
    }
  }

  /// `Ctrl-o` / the pane-header back arrow. `pane` defaults to the focused
  /// one; the arrows pass their OWN pane, which is what makes them per-pane
  /// (Pane Relief's rule: the arrows act on the pane you clicked, not the
  /// active one).
  function handleJumpBack(pane: 1 | 2 = focusedPane) {
    const cur = currentJumpLocation(pane);
    if (!cur.path) return;
    const target = goBack({ ...cur, pane });
    if (target) navigateToNavLocation(target);
  }

  function handleJumpForward(pane: 1 | 2 = focusedPane) {
    const target = goForward(pane);
    if (target) navigateToNavLocation(target);
  }

  // --- V70-A6 — the inline peek (§P7's "inline expansion") ---------------
  //
  // `gd` on a SINGLE candidate no longer navigates away: it opens the
  // destination's source UNDER the caret line, so a call chain reads in one
  // column. State is the pure reducer in `lib/inlinePeek.ts`; the widget is
  // `editor/inlinePeek.ts`; the fetched spans live BESIDE the state (a big
  // array the reducer has no business copying on every action).
  const [inpeek, dispatchInpeek] = useReducer(inlinePeekReducer, closedInlinePeek);
  const [inpeekSpans, setInpeekSpans] = useState<Record<string, Span[] | undefined>>({});
  const inpeekPaneRef = useRef<1 | 2>(1);
  const inpeekReqRef = useRef(0);

  /// Fetch a peeked file's bytes + highlight spans. Stale-guarded by the same
  /// monotonic-request idiom the peek/hierarchy fetches use.
  async function loadInlinePeekFile(path: string, ref?: string) {
    const reqId = ++inpeekReqRef.current;
    try {
      const f = await fetchFile(repo, path, ref);
      if (reqId !== inpeekReqRef.current) return;
      if (f.encoding === "base64") {
        dispatchInpeek({ type: "SET_ERROR", path, message: "binary file — no preview" });
        return;
      }
      setInpeekSpans((prev) => ({ ...prev, [path]: f.highlights ?? [] }));
      dispatchInpeek({ type: "SET_CONTENT", path, content: f.content });
    } catch (e) {
      if (reqId !== inpeekReqRef.current) return;
      dispatchInpeek({ type: "SET_ERROR", path, message: e instanceof Error ? e.message : String(e) });
    }
  }

  function openInlinePeek(
    pane: 1 | 2,
    frame: { repo: string; path: string; line: number; title: string; trust?: string },
  ) {
    inpeekPaneRef.current = pane;
    const { viewRef } = paneRepoPath(pane);
    // `applyInlinePeek(null)` returns the buffer's CURRENT scroll offset
    // without changing anything — the value `Esc` restores.
    const scrollTop = viewRef.current?.applyInlinePeek(null) ?? 0;
    const hostLine = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    dispatchInpeek({ type: "OPEN", hostLine: hostLine || 1, scrollTop, frame });
    void loadInlinePeekFile(frame.path);
  }

  function nestInlinePeek(frame: { repo: string; path: string; line: number; title: string; trust?: string }) {
    dispatchInpeek({ type: "PUSH", frame });
    void loadInlinePeekFile(frame.path);
  }

  function closeInlinePeek() {
    const pane = inpeekPaneRef.current;
    const { viewRef } = paneRepoPath(pane);
    const savedTop = inpeek.savedScrollTop;
    dispatchInpeek({ type: "CLOSE" });
    // §P7: "Esc closes and restores the exact prior scroll."
    viewRef.current?.restoreScroll(savedTop);
    viewRef.current?.focus();
  }

  /// `Enter` inside a peek — the ONE gesture that creates a provisional pane
  /// (`lib/provisionalPane.ts`). Everything else opens a pinned one.
  function promoteInlinePeek() {
    const frame = inlinePeekTop(inpeek);
    if (!frame) return;
    dispatchInpeek({ type: "CLOSE" });
    // A promotion always lands in pane 2 — pane 1 is where the reader IS, and
    // replacing it would be the opposite of "look at this without losing
    // where you are".
    navigate(buildReaderUrl({ pane2: { path: frame.path, line: frame.line } }));
    setFocusedPane(2);
    setProvisional(markProvisional(2, provisionalEnabled));
  }

  // Push the rendered stack into whichever pane owns it, every time either
  // the state or the fetched spans change.
  useEffect(() => {
    const pane = inpeekPaneRef.current;
    const { viewRef } = paneRepoPath(pane);
    const render: InlinePeekRender | null = inlinePeekIsOpen(inpeek)
      ? { state: inpeek, spansByPath: inpeekSpans }
      : null;
    viewRef.current?.applyInlinePeek(render);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [inpeek, inpeekSpans]);

  // --- V70-A6 — provisional panes (OFF by default, `lib/prefs.ts`) --------
  const [provisionalEnabled] = useState(() => loadProvisionalPanes());
  const [provisional, setProvisional] = useState(NO_PROVISIONAL);
  // A pane that closes cannot stay provisional.
  useEffect(() => {
    if (!pane2Loc && provisional.pane === 2) setProvisional(NO_PROVISIONAL);
  }, [pane2Loc, provisional.pane]);
  // Auto-pin on the SECOND interaction with a provisional pane. "Interaction"
  // is a cursor move inside it — reading is one, doing something is the next.
  function noteProvisionalInteraction(pane: 1 | 2) {
    setProvisional((st) => noteInteraction(st, pane));
  }

  // --- V70-A6 — the Ramp (§P7), shared with every result surface ---------
  //
  // `useRamp` is the ONE handler (`nav/ramp.ts`); the reader supplies the
  // pane a plain `Enter` opens into and its own peek, and every rung —
  // `K`/`Enter`/`Shift-Enter`/`Ctrl-Enter`/`o`/`O` — resolves identically
  // here and in the search rows, the tree and the drawer.
  const ramp = useRamp({
    focusedPane,
    onPeek: (t) =>
      openInlinePeek(focusedPane, {
        repo: t.repo,
        path: t.path,
        line: t.line ?? 1,
        title: t.subject ?? `${t.path.split("/").pop() ?? t.path}:${t.line ?? 1}`,
        ...(t.trust ? { trust: t.trust } : {}),
      }),
  });

  /// A peek ROW as a Ramp target. `via` is the peek's own mode — a `gd` row
  /// is a `definition_of` edge and a `gr` row is a `usage_of` one, which is
  /// exactly the typed provenance the trail records.
  function peekRowTarget(row: PeekRow): RampTarget {
    return {
      repo: row.repo || repo,
      path: row.path,
      line: row.line,
      via: peek.mode === "refs" ? "usage_of" : "definition_of",
      subject: peek.word || undefined,
      ...(row.trustClass ? { trust: row.trustClass } : {}),
      ...(row.symbolKind ? { kind: row.symbolKind } : {}),
      ...(row.container !== undefined ? { container: row.container } : {}),
      ...(row.text ? { snippet: row.text } : {}),
    };
  }

  function handlePeekRamp(rung: RampRung, row: PeekRow) {
    // The peek popup is a modal overlay; every rung that goes somewhere
    // closes it, and `peek` (the zero rung) replaces it with an inline one.
    if (rung !== "tab" && rung !== "window") dispatchPeek({ type: "CLOSE" });
    if (rung === "here") {
      handlePeekActivate(row);
      return;
    }
    pendingViaRef.current = peek.mode === "refs" ? "usage_of" : "definition_of";
    ramp.activate(rung, peekRowTarget(row));
  }

  /// Run a Ramp rung against the file tree's focused row. A directory has
  /// nowhere else to be opened, so it is an honest no-op rather than a
  /// swallowed key.
  function rampFocusedTreeRow(rung: RampRung) {
    const row = treeRef.current?.focusedFile();
    if (!row) return;
    ramp.activate(rung, { repo, path: row.path, via: "tree" });
  }

  /// The inline peek's own key/click handlers, handed to BOTH panes' CodeView
  /// (the widget only ever exists in one at a time — `inpeekPaneRef`).
  const inlinePeekHandlers = {
    onClose: () => closeInlinePeek(),
    onContext: (delta: 1 | -1) => dispatchInpeek({ type: "CONTEXT", delta }),
    onPromote: () => promoteInlinePeek(),
    onNest: (line: number) => {
      const frame = inlinePeekTop(inpeek);
      if (!frame) return;
      nestInlinePeek({
        repo: frame.repo,
        path: frame.path,
        line,
        title: `${frame.path.split("/").pop() ?? frame.path}:${line}`,
      });
    },
  };

  // --- T1 — `?sym=` deep link resolution (design-ui.md §5/§9.2) -----------
  // `GET /api/resolve-symbol` is scoped to ONE repo (no cross-repo fan-out —
  // `symbol_addr.rs`'s own doc), so a `found` result always lands in PANE1
  // of THIS repo, never a cross-repo `navigate` the way `gd`'s candidates
  // sometimes do. `found: true` re-centers on the resolved path/line
  // (dropping any `?ref=` — resolve-symbol, like `/api/resolve`, always
  // answers against the CURRENT working tree, never a historical ref);
  // `found: false` (or a network failure) toasts the reason and just STRIPS
  // `sym=` from the URL, leaving pane1 exactly where it already is — the
  // URL's own `path`/`line` was always the honest fallback anchor, never a
  // dead end. `symResolvedRef` guards against React 18 dev-mode's
  // double-invoke re-firing the same resolve twice; by construction the
  // effect never loops (both branches remove `sym=` from the next URL).
  const symResolvedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!symParam || !repo) return;
    if (symResolvedRef.current === symParam) return;
    symResolvedRef.current = symParam;
    let cancelled = false;
    function stripSym() {
      const next = new URLSearchParams(window.location.search);
      next.delete("sym");
      const qs = next.toString();
      navigate({ search: qs ? `?${qs}` : "" }, { replace: true });
    }
    (async () => {
      try {
        const result = await fetchResolveSymbol(repo, symParam);
        if (cancelled) return;
        if (result.found && result.path) {
          navigate(
            buildReaderUrl({ pane1: { path: result.path, ref: undefined, line: result.line } }),
            { replace: true },
          );
        } else {
          toast.warn(`sym=${symParam} didn't resolve${result.reason ? `: ${result.reason}` : ""} — staying put`);
          stripSym();
        }
      } catch (e) {
        if (cancelled) return;
        toast.err(`sym= resolve failed: ${e instanceof Error ? e.message : String(e)}`);
        stripSym();
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [symParam, repo]);

  // --- A3 — URL ?line= → pane1's buffer selection -------------------------
  // The `line` param is the source of truth for externally-driven position
  // (search-lane landings, permalinks, in-app links). Writes that ORIGINATED
  // from the cursor itself (cursorUrlSync below) are skipped via
  // `lastSyncedLineRef`, or the URL echo would yank the cursor to the
  // range's start half a second after every move.
  const lastSyncedLineRef = useRef<string | null>(null);
  useEffect(() => {
    if (lineParam === null || lineParam === lastSyncedLineRef.current) return;
    const range = parseLineParam(lineParam);
    if (!range) return;
    nonceRef.current += 1;
    setGotoSel1({ start: range.start, end: range.end, nonce: nonceRef.current });
  }, [lineParam, repo, path]);

  // --- Wave E — pane2=…:line → pane2's buffer selection -------------------
  // Same "external URL drives the selection" idiom as pane1's own effect
  // above, scoped to the `:line` suffix inside `pane2=` — fires whenever
  // pane2's own path/ref/line change (a fresh split, `[f ]f`, a history
  // step, or a hard-navigated `?pane2=` URL), skipped when it's an echo of
  // pane2's OWN cursor sync (`lastSyncedPane2LineRef`, set inside
  // `createPane2CursorUrlSync`'s `replace` callback below).
  const lastSyncedPane2LineRef = useRef<string | null>(null);
  useEffect(() => {
    if (!pane2Loc) return;
    const lineStr = pane2Loc.line !== undefined ? formatLineParam(pane2Loc.line) : "";
    if (lineStr === "" || lineStr === lastSyncedPane2LineRef.current) return;
    const range = parseLineParam(lineStr);
    if (!range) return;
    nonceRef.current += 1;
    setGotoSel2({ start: range.start, end: range.end, nonce: nonceRef.current });
  }, [pane2Loc]);

  // --- A3 — pane1 buffer selection → URL ?line= (debounced) ---------------
  // The sync object lives for as long as pane1 has a file open, regardless
  // of focus — but its `onSelection` is only ever CALLED while pane1 is the
  // focused pane (see the `onSelectionLines` prop below): the unfocused
  // pane's cursor simply never feeds a pending write, so only the active
  // view's position is ever bookmarked in the address bar.
  const cursorSyncRef1 = useRef<CursorUrlSync | null>(null);
  useEffect(() => {
    if (!activeFile) return;
    const sync = createCursorUrlSync({
      replace: (search) => {
        lastSyncedLineRef.current = new URLSearchParams(search).get("line");
        navigate({ search }, { replace: true });
      },
      getSearch: () => window.location.search,
    });
    cursorSyncRef1.current = sync;
    return () => {
      // A pending debounce must not fire after navigating to another
      // file/route — it would stamp the OLD file's line onto the new URL.
      sync.dispose();
      cursorSyncRef1.current = null;
    };
    // `navigate` is identity-unstable across renders by design; the sync
    // only needs A working navigate, not the latest closure.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, activeFile]);

  // --- Wave E — pane2 buffer selection → URL pane2=…:line (debounced) -----
  // Sibling of the sync above, scoped to pane2's own `:line` suffix — same
  // "lives whenever pane2 has a file open, fed only while focused" split.
  const cursorSyncRef2 = useRef<CursorUrlSync | null>(null);
  useEffect(() => {
    if (!pane2Loc) return;
    const sync = createPane2CursorUrlSync({
      getPaneBase: () => (pane2Loc ? { path: pane2Loc.path, ref: pane2Loc.ref } : null),
      replace: (search) => {
        const parsed = parsePane2(new URLSearchParams(search).get("pane2"));
        lastSyncedPane2LineRef.current = parsed?.line !== undefined ? formatLineParam(parsed.line) : "";
        navigate({ search }, { replace: true });
      },
      getSearch: () => window.location.search,
    });
    cursorSyncRef2.current = sync;
    return () => {
      sync.dispose();
      cursorSyncRef2.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, pane2Loc?.path, pane2Loc?.ref]);

  // --- A3 — focus follows the file: opening a file lands focus in the
  // buffer (pending flag set on path/ref change, consumed once the blob is
  // actually rendered) — but never steals focus from an input the user is
  // typing in (e.g. the tree's quick-filter during a live-mirror refetch).
  // Wave E: mirrored per pane, so opening/switching EITHER pane's file
  // auto-focuses THAT pane (the "split with self"/Shift+Enter/working-set
  // gestures all rely on this to land keyboard focus where the operator
  // just asked to look).
  const pendingFocusRef1 = useRef(false);
  useEffect(() => {
    pendingFocusRef1.current = activeFile !== undefined;
  }, [repo, activeFile, gitRef]);
  const blobHash1 = file.data?.encoding === "utf8" ? file.data.blob_hash : undefined;
  useEffect(() => {
    if (!pendingFocusRef1.current || !blobHash1) return;
    if (isEditableTarget(document.activeElement)) return;
    pendingFocusRef1.current = false;
    codeViewRef1.current?.focus();
  }, [blobHash1, gitRef]);

  const pendingFocusRef2 = useRef(false);
  useEffect(() => {
    pendingFocusRef2.current = pane2Loc?.path !== undefined;
  }, [repo, pane2Loc?.path, pane2Loc?.ref]);
  const blobHash2 = pane2File.data?.encoding === "utf8" ? pane2File.data.blob_hash : undefined;
  useEffect(() => {
    if (!pendingFocusRef2.current || !blobHash2) return;
    if (isEditableTarget(document.activeElement)) return;
    pendingFocusRef2.current = false;
    codeViewRef2.current?.focus();
  }, [blobHash2, pane2Loc?.ref]);

  // --- W4.4/Wave C — the Provenance overlay: off | dots | age -------------
  // `dots` is the original W4.4 disclosure-ladder gutter; `age` is Wave C's
  // line-background heatmap (`lib/ageHeatmap.ts`) — both ride the SAME
  // `GET /api/blame` fetch (`blame` below), but only `dots` additionally
  // needs the per-sha join-ladder attribution fan-out (`attributions`):
  // age tinting reads `BlameRegion.author_time` directly, no join lookup.
  // Wave E: keyed on `focusedPath`/`focusedRef` (the FOCUSED pane), not
  // pane1 unconditionally — the shared inspector rail follows focus.
  const [provenanceMode, setProvenanceMode] = useState<ProvenanceMode>("off");
  const provenanceOn = provenanceMode !== "off";
  const blame = useBlame(repo, focusedPath, focusedRef, provenanceOn);
  const attributions = useBlameAttributions(repo, focusedPath, blame.data?.regions, provenanceMode === "dots");
  const attributionBySha = useMemo(() => {
    const map = new Map<string, AttributionOut>();
    for (const [sha, why] of attributions.bySha) map.set(sha, why.attribution);
    return map;
  }, [attributions.bySha]);
  const blameDots = useMemo<Map<number, BlameDotInfo> | null>(() => {
    if (provenanceMode !== "dots" || !blame.data) return null;
    return buildLineDots(blame.data.regions, attributionBySha);
  }, [provenanceMode, blame.data, attributionBySha]);
  const ageLines = useMemo<Map<number, AgeLineInfo> | null>(() => {
    if (provenanceMode !== "age" || !blame.data) return null;
    return buildAgeLineBuckets(blame.data.regions);
  }, [provenanceMode, blame.data]);

  // PRR-U9 — diagnostics gutter marks, keyed on the FOCUSED pane's path
  // (same "follows focus" convention `blameDots`/`ageLines` use above).
  // `useDiagnostics` itself gates on the repo's intel provider covering
  // `focusedPath`'s language, so this fetch is a no-op for most repos/files.
  const diagnostics = useDiagnostics(repo, focusedPath);
  const diagMarks = useMemo<Map<number, DiagnosticGutterMark> | null>(() => {
    if (!diagnostics.data?.diagnostics) return null;
    return diagnosticGutterMarks(diagnostics.data.diagnostics);
  }, [diagnostics.data]);

  const [ladder, dispatchLadder] = useReducer(ladderReducer, initialLadderState);
  const [hoverChip, setHoverChip] = useState<
    { line: number; rect: DOMRect; label: string; solid: boolean } | null
  >(null);

  function handleBlameHover(line: number, rect: DOMRect) {
    dispatchLadder({ type: "hover", line });
    const dot = blameDots?.get(line);
    setHoverChip(dot ? { line, rect, label: dot.label, solid: dot.solid } : null);
  }
  function handleBlameUnhover(line: number) {
    dispatchLadder({ type: "unhover", line });
    setHoverChip((c) => (c && c.line === line ? null : c));
  }
  function handleBlameClick(line: number) {
    dispatchLadder({ type: "click", line });
    setHoverChip(null);
    inspectorRef.current?.openTab("provenance");
  }

  const openRegion =
    ladder.openLine !== null && blame.data ? regionCoveringLine(blame.data.regions, ladder.openLine) : undefined;
  const openWhy = openRegion ? attributions.bySha.get(openRegion.sha) : undefined;
  const whyPanel =
    ladder.openLine !== null ? (
      <>
        <WhyPanel
          repo={repo}
          repoRoot={repoRoot}
          path={focusedPath ?? ""}
          line={ladder.openLine}
          region={openRegion}
          why={openWhy}
        />
        {/* CT-E2 — the file's session timeline (incl. attention-gap divider
            beats) below the per-line why. Rendered inside the `whyPanel`
            node so it only MOUNTS (and only fetches — `hooks/useStory.ts`)
            while the provenance tab is actually open. */}
        {focusedPath !== undefined && <StoryTimeline repo={repo} path={focusedPath} />}
      </>
    ) : null;

  // Reset the ladder's open panel + any lingering hover chip on a file/repo
  // switch (INCLUDING a focus switch between panes — a stale line number
  // from the PREVIOUSLY focused pane's blame result has nothing to show for
  // the newly-focused one).
  useEffect(() => {
    dispatchLadder({ type: "closePanel" });
    setHoverChip(null);
    // V72-J2 — same reasoning: a stale comments/1 hover/active line from the
    // PREVIOUSLY focused pane's file has nothing to show for the new one.
    setCommentHoverChip(null);
    setCommentActiveLine(null);
  }, [repo, focusedPath, focusedRef]);

  // --- W4.6 — annotations gutter + panel -----------------------------------
  // Wave E: keyed on the FOCUSED pane's path — the annotations panel/gutter
  // markers follow focus, same as Provenance above.
  const annotations = useAnnotations(repo, focusedPath);
  // F5 — hoisted out of the InspectorRail prop expression so the mobile
  // entry button's badge (Reader's own header) and the rail's own tab badge
  // read the exact same count without computing it twice.
  const unresolvedAnnotationsCount = annotations.data
    ? unresolvedCount(annotations.data.annotations)
    : 0;
  const annotationMarkers = useMemo<Map<number, LineMarkerSpec> | null>(() => {
    if (!annotations.data) return null;
    const byLine = annotationsByLine(annotations.data.annotations);
    const out = new Map<number, LineMarkerSpec>();
    for (const [line, list] of byLine) {
      out.set(line, {
        className: "kbc-annot-dot",
        title: annotationGutterTitle(list),
      });
    }
    return out;
  }, [annotations.data]);
  const [annotationActiveLine, setAnnotationActiveLine] = useState<number | null>(null);
  // Phase D — the other end of a visual-mode `a` range selection; reset to
  // `null` by every SINGLE-line entry point (gutter click, the outside-
  // buffer `a` keybinding) so a stale range never leaks into the next
  // composer invocation.
  const [annotationActiveLineEnd, setAnnotationActiveLineEnd] = useState<number | null>(null);
  /// V71-E2 — the intent the annotation composer opens ON. `"question"`
  /// when the action menu's "Ask here" put it there (D5 makes that row
  /// REQUIRED on every target kind, and a row that opens a note composer
  /// has not shipped a question channel); `undefined` for every other
  /// door, which keeps `a`'s composer byte-identical.
  const [annotationInitialIntent, setAnnotationInitialIntent] = useState<
    "question" | undefined
  >(undefined);
  const cursorLineRef1 = useRef(1);
  const cursorLineRef2 = useRef(1);

  // --- V72-J2 (D8) — comments/1: the per-file gutter, doc hover freshness's
  // data source, and the claim → annotation bridge ------------------------
  // Wave E convention (same as annotations/blame above): keyed on the
  // FOCUSED pane's path, always fetched (the gutter's "always-fetch
  // discipline" — a mode cycle is a pure client filter over this ONE
  // response, never a re-fetch).
  const commentsFile = useCommentsFile(repo, focusedPath);
  const allFileComments = commentsFile.data?.comments ?? [];
  const commentKeywords = useCommentKeywords();
  const todoFamily = commentKeywords.data?.todo_family ?? [];
  const [commentGutterMode, setCommentGutterMode] = useState<CommentGutterMode>(() =>
    loadCommentGutterMode(),
  );
  function cycleCommentGutterMode() {
    setCommentGutterMode((m) => {
      const next = nextGutterMode(m);
      saveCommentGutterMode(next);
      return next;
    });
  }
  const visibleFileComments = useMemo(
    () => filterCommentsForMode(allFileComments, commentGutterMode),
    [allFileComments, commentGutterMode],
  );
  const commentMarkers = useMemo(
    () => commentGutterMarkers(visibleFileComments),
    [visibleFileComments],
  );
  // The rail's Comments-tab badge — ACTIONABLE (drifted/aged/unreasoned)
  // rows in THIS file, off the wire (`CommentOut.state.state`), never
  // re-derived from the gutter's own display filter.
  const commentsBadgeCount = useMemo(
    () => allFileComments.filter((c) => ACTIONABLE_STATES.includes(c.state.state)).length,
    [allFileComments],
  );
  const [commentHoverChip, setCommentHoverChip] = useState<
    { line: number; rect: DOMRect } | null
  >(null);
  const [commentActiveLine, setCommentActiveLine] = useState<number | null>(null);
  const createClaimAnnotation = useCreateAnnotation(repo, focusedPath ?? "");

  function handleCommentHover(line: number, rect: DOMRect) {
    setCommentHoverChip({ line, rect });
  }
  function handleCommentUnhover(line: number) {
    setCommentHoverChip((c) => (c && c.line === line ? null : c));
  }
  function handleCommentClick(line: number) {
    setCommentHoverChip(null);
    setCommentActiveLine(line);
    inspectorRef.current?.openTab("comments");
  }
  const commentHoverComment = commentHoverChip
    ? commentAtLine(allFileComments, commentHoverChip.line)
    : null;

  /// `comments.next`/`comments.prev` (`]m`/`[m`) — walk the FOCUSED pane's
  /// currently-VISIBLE (mode-filtered) markers from wherever its cursor is.
  function handleCommentNav(dir: 1 | -1) {
    const cursorLine = focusedPane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    const next = nextCommentLine(visibleFileComments, cursorLine, dir);
    if (next === null) return;
    jumpToLine(focusedPane, next);
  }
  /// `comments.open-card` (`Space C o`) — the keyboard door to the same
  /// small card a gutter hover/click opens, for the block at the cursor.
  function handleCommentOpenCard() {
    const cursorLine = focusedPane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    const c = commentAtLine(allFileComments, cursorLine);
    if (!c) {
      toast.warn("No comment near the cursor.");
      return;
    }
    setCommentActiveLine(c.line_start);
    inspectorRef.current?.openTab("comments");
  }
  /// `comments.track-as-annotation` (`Space C t`) — the claim → annotation
  /// bridge's keyboard door. Creates an ordinary annotation through the
  /// EXISTING `POST /api/annotations` path, `intent: "claim"`
  /// (`annotations::INTENT_CLAIM`); never edits source.
  function handleCommentTrackAsAnnotation() {
    const path = focusedPath;
    if (!path) return;
    const cursorLine = focusedPane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    const c = commentAtLine(allFileComments, cursorLine);
    if (!c || !isBridgeable(c, todoFamily)) {
      toast.warn("No trackable TODO-family comment at the cursor.");
      return;
    }
    createClaimAnnotation.mutate(
      { repo, path, line: c.line_start, body: claimAnnotationBody(c), intent: "claim" },
      {
        onError: (e) => toast.err(e instanceof Error ? e.message : "failed to create the tracking annotation"),
      },
    );
  }

  // --- V70-A10 ("Workspaces v0", D26) — save/open the open files + desk
  // snapshot + ref as a named set, with notes -----------------------------
  const activeWorkspace = useActiveWorkspace(repo);
  const workspaceParam = searchParams.get("workspace");
  // `GET /api/sets/{id}` works identically for a workspace as for a plain
  // reading set — `useSet` is reused as-is (`hooks/useSets.ts`'s own
  // "Workspaces v0" doc section) rather than a bespoke `useWorkspace` hook.
  const workspaceSetView = useSet(repo, activeWorkspace.id ?? undefined);
  const patchWorkspaceMut = usePatchSet(repo);
  const [saveWorkspaceOpen, setSaveWorkspaceOpen] = useState(false);

  // Restore: `?workspace=<id>` is a ONE-SHOT trigger (never re-applied on
  // its own re-render — same posture `?desk=`'s own override effect takes,
  // `desk/useDesk.ts`'s doc). A ref guard keyed on the param's OWN value
  // (not a boolean) so navigating between two DIFFERENT workspace links in
  // the same session both still restore.
  const restoredWorkspaceParamRef = useRef<string | null>(null);
  useEffect(() => {
    if (!workspaceParam || restoredWorkspaceParamRef.current === workspaceParam) return;
    restoredWorkspaceParamRef.current = workspaceParam;
    let cancelled = false;
    (async () => {
      try {
        const view = await fetchSet(workspaceParam);
        if (cancelled) return;
        if (view.desk_json) {
          const snapshot = parseWorkspaceSnapshot(view.desk_json);
          if (snapshot) desk.restore(snapshot.desk);
        }
        // Open every saved entry into the working set, IN ORDER — the
        // active file the URL itself named opens the normal way; this
        // backfills the rest of the strip.
        //
        // V71-K2 — `reorder`, not a `touch` loop. A100 shipped the loop and
        // it could not keep the promise this comment makes: the strip is
        // stable INSERTION order and `touch` never moves a path already in
        // it, so the file the restored URL names (the saved focused pane's,
        // `caller.rs` in `workspaces.spec.ts`) was inserted first by the
        // reader's own open effect and the loop could only append the rest
        // behind it. `lib/workspaceDirty.ts` compares live order against
        // saved order, so the workspace reported DIRTY the moment it
        // opened. `reorder` is also insensitive to WHICH of the two effects
        // wins the race: it lays the saved order down whether the open
        // already happened or is still to come (a later `touch` on a
        // present path is a no-op for order, by the same rule).
        workingSet.reorder(view.spans.map((s) => s.path));
        activeWorkspace.set(workspaceParam);
      } catch (e) {
        toast.err(`couldn't open workspace: ${e instanceof Error ? e.message : String(e)}`);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceParam]);

  // Dirty: the LIVE working-set path order vs. the workspace's own saved
  // entries (`lib/workspaceDirty.ts`'s doc — file SET/ORDER only, not
  // per-file line drift).
  const workspaceDirty =
    activeWorkspace.id !== null &&
    isWorkingSetDirty(
      workingSet.entries.map((e) => e.path),
      workspaceSetView.data?.spans.map((s) => s.path) ?? [],
    );
  const activeWorkspaceChip: ActiveWorkspaceChip | null =
    activeWorkspace.id && workspaceSetView.data
      ? { id: activeWorkspace.id, name: workspaceSetView.data.name, url: workspacesUrl(repo), dirty: workspaceDirty }
      : null;

  /// Per-path cursor line captured at save/update time — only for a
  /// working-set entry that IS one of the two open panes right now (see
  /// `lib/workspaceSnapshot.ts`'s doc).
  function capturedWorkspaceLines(): Record<string, number> {
    const lines: Record<string, number> = {};
    if (activeFile) lines[activeFile] = cursorLineRef1.current;
    if (pane2Loc?.path) lines[pane2Loc.path] = cursorLineRef2.current;
    return lines;
  }

  /// The exact file (+ line) in each pane right now — `~workspaces`' Open
  /// action's PRIMARY navigation target (`lib/workspaceSnapshot.ts`'s doc).
  function capturedPanes(): {
    pane1: { path: string; line?: number } | null;
    pane2: { path: string; line?: number } | null;
  } {
    return {
      pane1: activeFile ? { path: activeFile, line: cursorLineRef1.current } : null,
      pane2: pane2Loc?.path ? { path: pane2Loc.path, line: cursorLineRef2.current } : null,
    };
  }

  async function updateActiveWorkspace() {
    if (!activeWorkspace.id) return;
    const lines = capturedWorkspaceLines();
    const { pane1, pane2: capturedPane2 } = capturedPanes();
    const snapshot = buildWorkspaceSnapshot({
      desk: desk.state,
      drawerTabs: desk.drawer.sets.filter((s) => !s.evicted).map((s) => ({ title: s.title, pinned: s.pinned })),
      lines,
      focusedPane,
      pane1,
      pane2: capturedPane2,
    });
    try {
      await patchWorkspaceMut.mutateAsync({
        id: activeWorkspace.id,
        input: {
          desk_json: JSON.stringify(snapshot),
          spans: workingSet.entries.map((e) => ({
            path: e.path,
            line_start: lines[e.path],
            line_end: lines[e.path],
          })),
        },
      });
      toast.ok("workspace updated");
    } catch (e) {
      toast.err(`couldn't update workspace: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // V3.N2 — bookmarks for the whole repo (badge + gm toggle).
  const bookmarksQ = useBookmarks(repo);
  const createBookmarkMut = useCreateBookmark(repo);
  const deleteBookmarkMut = useDeleteBookmark(repo);
  const bookmarkCount = bookmarksQ.data?.bookmarks.length ?? 0;

  function handleAnnotationClick(line: number) {
    setAnnotationActiveLine(line);
    setAnnotationActiveLineEnd(null);
    inspectorRef.current?.openTab("annotations");
  }

  // --- Wave C — History inspector tab + `[c ]c` time travel ----------------
  // Fetched whenever the FOCUSED pane has a file open (NOT gated on the
  // History tab being the active one) — the `[c`/`]c` vim binding must work
  // immediately, before the tab is ever opened, same as the annotations
  // gutter's own always-fetch discipline above.
  const fileHistory = useFileHistory(repo, focusedPath, focusedPath !== undefined);
  const historyEntries = fileHistory.data?.entries ?? [];
  const historyCurIndex = currentHistoryIndex(historyEntries, focusedRef);

  /// Step (or jump directly to) `sha` in `pane`'s OWN ref slot — pane1's
  /// route `?ref=`, or pane2's `pane2=…ref…` — carrying that pane's live
  /// cursor line forward, leaving the OTHER pane untouched.
  function navigateHistory(pane: 1 | 2, sha: string | undefined) {
    if (pane === 1) {
      if (!activeFile) return;
      navigate(buildReaderUrl({ pane1: { path: activeFile, ref: sha, line: cursorLineRef1.current } }));
    } else {
      if (!pane2Loc) return;
      navigate(buildReaderUrl({ pane2: { path: pane2Loc.path, ref: sha, line: cursorLineRef2.current } }));
    }
  }

  function handleHistoryStep(pane: 1 | 2, dir: -1 | 1) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    const result = historyStepTarget(historyEntries, historyCurIndex, dir);
    if (result.kind === "warn") {
      toast.warn(result.message);
      return;
    }
    navigateHistory(pane, result.sha);
  }

  const historyPanel = focusedPath ? (
    <HistoryPanel
      repo={repo}
      entries={historyEntries}
      currentRef={focusedRef}
      onNavigate={(sha) => navigateHistory(focusedPane, sha)}
      onOpenStory={(sha) => navigate(storyUrl(repo, focusedPath, sha))}
      isLoading={fileHistory.isLoading}
      truncated={fileHistory.data?.truncated ?? false}
    />
  ) : null;

  // DCB W3.B — the always-visible "Cited by" slot (InspectorRailProps'
  // `citedBy`, NOT a 7th tab). Gated purely on a file being open; `CitedBy`
  // owns its own `useDocRefs` fetch and renders nothing itself when that
  // fetch resolves to zero claims, so this never needs a second pre-fetch
  // here just to decide null-vs-render (the annotations badge above
  // pre-fetches because it feeds a COUNT into the mobile entry button;
  // nothing else here needs `doc_refs` data outside `CitedBy` itself).
  // A `focusedPath`-derived `key` (m6, W3.B.R review): `CitedBy`'s
  // `useState(false)` disclosure is per-MOUNT, not per-path — without a key
  // that switches on file/pane change, expanding the list on one file
  // leaves it expanded when the reader focuses a different file,
  // contradicting the collapsed-by-default spec. Keying on path forces a
  // remount (fresh `open: false`) on every file switch. PRR-U8R fix: the
  // key carries a PER-SLOT prefix (`citedby:`/`framework:`/`diagnostics:`
  // below) — `citedBy`/`frameworkCard`/`diagnosticsCard` are three DIFFERENT
  // sibling children inside InspectorRail's returned array (`{citedBy}
  // {frameworkCard}{diagnosticsCard}`), so a bare `key={focusedPath}`
  // shared verbatim across all three collided: React's array reconciler
  // (`reconcileChildrenArray`) builds its "existing children" lookup keyed
  // by `key`, and inserting three DIFFERENT-typed fibers under the SAME key
  // silently overwrites earlier entries in that map — the discarded entry
  // never gets scheduled for deletion, leaving a stale fiber (and its DOM
  // node) permanently behind alongside the new one. Reproduced with a
  // production build (StrictMode on OR off — not a dev-only artifact):
  // once both `citedBy` and `frameworkCard` render real content for the
  // same file, `.kbc-inspector` ends up with TWO `[data-kbc-citedby]`
  // divs. Unique per-slot keys make each slot's own remount independent —
  // no more cross-slot collision.
  const citedBy = focusedPath ? (
    <CitedBy key={`citedby:${focusedPath}`} repo={repo} path={focusedPath} />
  ) : null;

  // T1 (design-ui.md §9.4a) — the Framework card, mounted the SAME
  // always-visible way `citedBy` is above: `FrameworkCard` owns its own
  // `useFrameworkEdges` fetch and renders nothing at all until that fetch
  // lands (see that component's doc), so — like `citedBy` — this never
  // needs a second pre-fetch here just to decide null-vs-render.
  const frameworkCard = focusedPath ? (
    <FrameworkCard key={`framework:${focusedPath}`} repo={repo} path={focusedPath} />
  ) : null;

  // PRR-U9 (design-addendum-2.md §D) — the Diagnostics card, mounted the
  // SAME always-visible way `frameworkCard`/`citedBy` are above.
  // `DiagnosticsCard` owns its own `useDiagnostics` fetch and renders
  // nothing at all when no provider covers this file's language (see that
  // component's doc). The jump handler mirrors `onGotoAnnotationLine`'s own
  // "jump then re-focus the buffer" idiom (`jumpToLine`/`paneRepoPath`
  // below) — a diagnostic row is always in the CURRENTLY open file, never a
  // cross-file navigation.
  const diagnosticsCard = focusedPath ? (
    <DiagnosticsCard
      key={`diagnostics:${focusedPath}`}
      repo={repo}
      path={focusedPath}
      onJumpLine={(line, lineEnd) => {
        jumpToLine(focusedPane, line, lineEnd);
        paneRepoPath(focusedPane).viewRef.current?.focus();
      }}
    />
  ) : null;

  // --- W4.5 — live-mirror auto-refresh heuristic, one instance per pane ----
  // Wave E: `registerOpenFileForLiveMirror`'s registry is now keyed
  // (`api/queryClient.ts`), so both panes' open files are independently
  // protected from a silent refetch while their viewer is dirty.
  const viewerDirtyRef1 = useRef(false);
  const liveMirror1 = useLiveMirror("pane1", repo, activeFile, viewerDirtyRef1, () => void file.refetch());
  const viewerDirtyRef2 = useRef(false);
  const liveMirror2 = useLiveMirror("pane2", repo, pane2Loc?.path, viewerDirtyRef2, () => void pane2File.refetch());

  function refreshTree() {
    void queryClient.invalidateQueries({ predicate: (q) => q.queryKey[1] === repo });
    liveMirror1.dismissHeadMoved();
    liveMirror2.dismissHeadMoved();
  }

  // --- A3 — vim action callbacks (dispatched from inside a buffer) --------
  const copiedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  function flashCopied() {
    setCopiedTick((t) => t + 1);
    if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
    copiedTimerRef.current = setTimeout(() => setCopiedTick(0), 1600);
  }
  useEffect(() => () => {
    if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
  }, []);

  /// `Y` — Wave E gives the two panes deliberately DIFFERENT semantics
  /// (documented per the milestone brief): pane1-focused copies the reader's
  /// FULL current view (pane1 + whatever pane2 is open, if any) as one
  /// shareable permalink — pane1 is "the primary file," so sharing it
  /// naturally carries its split along. Pane2-focused instead PROMOTES
  /// pane2's own file/ref into a plain pane1 URL with no `pane2=` at all —
  /// pane2-focused means "I'm reading THIS file right now," and a recipient
  /// with no split context of their own should land exactly on it, not on
  /// pane1's (possibly unrelated) file with pane2 reduced to an easy-to-miss
  /// footnote. Either way only the LINE is refreshed from the live cursor
  /// (`sel`) rather than re-reading the URL, which may be up to the 500ms
  /// debounce behind — same immediacy the pre-Wave-E single-pane `Y` relied on.
  function handlePermalinkForPane(pane: 1 | 2, sel: LineSel) {
    const line = sel.lineEnd !== undefined ? { start: sel.line, end: sel.lineEnd } : sel.line;
    if (pane === 1) {
      if (!activeFile) return;
      copyToClipboard(
        permalinkFor(window.location.origin, { repo, path: activeFile, ref: gitRef, line, pane2: pane2Loc ?? undefined }),
      );
    } else {
      if (!pane2Loc) return;
      copyToClipboard(permalinkFor(window.location.origin, { repo, path: pane2Loc.path, ref: pane2Loc.ref, line }));
    }
    flashCopied();
  }

  // --- B1/B3 — gd / gr / K: the resolve+peek panel --------------------------
  // B1 shipped tier-0 clickable code over `/api/defs` + `/api/xrefs`. B3
  // adds `/api/resolve` (position-based, occurrence-aware) as `gd`'s and
  // `K`'s PRIMARY path — `gd` falls back to the original name-based
  // `fetchDefs` only when resolve itself fails (404/older daemon/network);
  // `gr` stays on `/api/xrefs` unchanged (resolve has no refs listing).
  // None of these carry a `ref` parameter (they resolve against the
  // currently-indexed working tree, not whatever historical `ref` this
  // route happens to be viewing), so every navigation built from a peek
  // result deliberately OMITS the source pane's current `?ref=`.
  //
  // Wave E: the peek PANEL itself is a single shared overlay (not one per
  // pane) — `peekOwnerPaneRef` remembers which pane's own action opened it,
  // so activating a row (or closing the panel) routes back to THAT pane.
  const [peek, dispatchPeek] = useReducer(peekReducer, initialPeekState);
  const [peekAnchor, setPeekAnchor] = useState<PeekAnchor | null>(null);
  const peekOwnerPaneRef = useRef<1 | 2>(1);
  // Bumped on every gd/gr/K press; a resolved fetch whose id has since been
  // superseded by a newer press is dropped — same "ignore a stale async
  // result" discipline as `lastSyncedLineRef`/`liveMirror` elsewhere in this
  // file, since nothing else here cancels the in-flight `fetch` itself.
  const peekReqRef = useRef(0);
  // T1 — the position `K`'s hover card was opened for, so the card's own
  // "usages"/"callers" footer buttons (design-ui.md §9.1) can re-fire
  // `handleFindRefs`/`handleHierarchyCallers` against it. `null` until the
  // first `K` press; never read while `peek.card` is unset.
  const lastHoverPosRef = useRef<{ pane: 1 | 2; pos: WordPos } | null>(null);

  // ── V71-E2 — the Usages dock's state, and the action menu's ───────────
  //
  // The dock's ROWS live here rather than in `drawerSets.ts` on purpose:
  // that reducer is pure and knows no wire type, and the whole point of the
  // dock is that its body is a `usages/2` body, not a flat row list. The
  // drawer still owns the TAB (id, title, eviction, pinning), which is what
  // keeps §P1's "eviction is a view operation, never a data operation" true
  // for this set exactly as for every other.
  interface UsagesDockState {
    setId: string;
    repo: string;
    /// The symbol the query was for — the trail's `subject` and the tab's
    /// title.
    subject: string;
    out: Usages2Out | null;
    loading: boolean;
    error: string | null;
    chips: UsageChips;
    axis: GroupAxis;
    cursor: number;
    mentionsOn: boolean;
    mentions: number | null;
    refName?: string;
  }
  const [usagesDock, setUsagesDock] = useState<UsagesDockState | null>(null);
  const usagesReqRef = useRef(0);

  /// The one action menu (D5). `at` is viewport coordinates for the
  /// popover; `pill` marks a drag-select opening, which renders the
  /// three-row pill first and promotes to the full menu on "…".
  interface ActionMenuState {
    at: { x: number; y: number };
    path: string;
    line: number;
    col: number;
    endLine?: number;
    endCol?: number;
    text?: string;
    target: number;
    out: ActionsOut | null;
    loading: boolean;
    error: string | null;
    pill: boolean;
  }
  const [actionMenu, setActionMenu] = useState<ActionMenuState | null>(null);
  /// V74-L2 — the add-to-board picker's target, when one is open. Held here
  /// (not in `ActionMenuState`) because the leader key opens it WITHOUT the
  /// menu, and the dialog outlives the menu it may have come from.
  const [addToBoard, setAddToBoard] = useState<ActionTarget | null>(null);
  const actionReqRef = useRef(0);

  function capturePeekAnchor(pane: 1 | 2): PeekAnchor | null {
    return paneRepoPath(pane).viewRef.current?.cursorCoords() ?? null;
  }

  // B3 — `gd` NOW resolves position-first (`/api/resolve` at the cursor's
  // own WordPos — `col` is the word's first char, exactly what resolve
  // expects a position ON the identifier to be) instead of name-first. A
  // single candidate jumps straight there (same-file via `jumpToLine`, else
  // `navigateToCandidate` — same B1 reasoning above); several open the
  // peek panel with one row per candidate, each carrying its own
  // `precision` badge (`PeekPanel`'s `PrecisionBadge`); ZERO candidates
  // opens the panel empty (resolve answered honestly — "no definition
  // here" — not a reason to fall back). The fallback below fires ONLY when
  // resolve itself fails (404 on an older daemon, network error, etc.) —
  // feature-detected per attempt, nothing cached — so `gd` never goes dead.
  async function handleGotoDef(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    peekOwnerPaneRef.current = pane;
    dispatchHier({ type: "CLOSE" });
    const reqId = ++peekReqRef.current;

    try {
      const resolved = await fetchResolve({ repo, path: panePath, line: pos.line, col: pos.col });
      if (reqId !== peekReqRef.current) return;
      if (resolved.candidates.length === 1) {
        const c = resolved.candidates[0];
        // V70-A6 (§P7) — a single candidate opens an INLINE peek under the
        // caret line instead of navigating away. That is the whole point of
        // the rung: read the definition in place, three deep if the chain
        // needs it, and `Esc` puts the scroll back exactly. A cross-REPO
        // candidate still navigates: the peek renders one repo's bytes, and
        // `?pane2=` carries no repo (`lib/codeUrl.ts`), so pretending
        // otherwise would show the wrong file.
        if (c.repo !== repo) {
          navigateToCandidate(pane, c.repo, c.path, c.line);
          return;
        }
        openInlinePeek(pane, {
          repo: c.repo,
          path: c.path,
          line: c.line,
          title: c.container ? `${c.container}.${resolved.ident}` : resolved.ident,
          ...(c.class ? { trust: c.class } : {}),
        });
        return;
      }
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({
        type: "OPEN_WITH_ROWS",
        mode: "defs",
        word: resolved.ident,
        rows: resolved.candidates.map(resolveCandidateToRow),
        approximate: false,
        note: resolved.note,
      });
      return;
    } catch {
      // resolve unavailable (404/older daemon/network) — fall through to
      // B1's original name-based path below.
    }
    if (reqId !== peekReqRef.current) return;

    try {
      // Cross-repo on purpose (no `repo` filter) — a hit living in a
      // DIFFERENT configured repo is still a valid landing (Deliverable 2's
      // cross-repo requirement); `PeekPanel` shows the repo name whenever a
      // row's repo differs from the one currently open.
      const defs = await fetchDefs(undefined, pos.word);
      if (reqId !== peekReqRef.current) return;
      const exact = singleExactMatch(defs);
      if (exact) {
        navigateToCandidate(pane, exact.repo, exact.path, exact.line_start);
        return;
      }
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({ type: "OPEN_WITH_ROWS", mode: "defs", word: pos.word, rows: defRowsFrom(defs), approximate: !defs.exact });
    } catch (e) {
      if (reqId !== peekReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({ type: "OPEN", mode: "defs", word: pos.word });
      dispatchPeek({ type: "SET_ERROR", message });
      // F3a — the panel's own error row (above) is the primary surface, but
      // toast too: `gd` can be pressed anywhere in the buffer, and a panel
      // dismissed a moment later (Esc, or a fast subsequent keypress) would
      // otherwise leave no trace that the lookup actually failed.
      toast.err(`gd failed: ${message}`);
    }
  }

  // ── V71-E2 — `gr`, the lens chip and the peek `u` all land HERE ────────
  //
  // Before this unit all three called `/api/xrefs` — a word-boundary grep
  // over the working tree — while `/api/usages`, the classified ladder,
  // had NO SPA consumer at all (recon/usages.md §1.1, gap 1: "the good
  // engine is unreachable from the UI"). They now call `/api/usages/2`
  // (V71-E1's wire) and land in the DRAWER, which is where
  // `desk/placement.ts` has said usages belong since V70-A4.
  //
  // The grep lane is NOT deleted: it survives as the dock's explicit
  // "mentions" chip, fetched only when that chip is on. It answers a
  // different question, and recon §6.3 records what happens when two
  // different questions share one number — kb-code already shipped three
  // disagreeing "usages" counts.
  async function handleFindRefs(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    dispatchPeek({ type: "CLOSE" });
    dispatchHier({ type: "CLOSE" });
    const subject = pos.word;
    const setId = drawerSetId("usages", `${subject}@${repo}/${panePath}:${pos.line}`);
    const reqId = ++usagesReqRef.current;
    // ONE live usages tab at a time. The dock's rows are a `usages/2` BODY
    // held here, not `DrawerSet.rows`, so a second query cannot leave the
    // first tab greyed-but-reopenable the way the drawer's own eviction
    // promises — it would reopen empty. Dropping the previous tab (a data
    // operation, `drawerSets.ts`'s own word for it) is the honest version:
    // the set is gone, and it says so by not being there.
    if (usagesDock && usagesDock.setId !== setId) {
      desk.drawerDispatch({ type: "drop", id: usagesDock.setId });
    }
    setUsagesDock({
      setId,
      repo,
      subject,
      out: null,
      loading: true,
      error: null,
      chips: NO_CHIPS,
      axis: usagesDock?.axis ?? "dir",
      cursor: -1,
      mentionsOn: usagesDock?.mentionsOn ?? false,
      mentions: null,
      refName: gitRef,
    });
    // The tab exists before the fetch lands, so the drawer never flashes
    // empty; `rows` stays empty because this set renders its OWN body.
    desk.keepInDrawer({ id: setId, title: `${subject} · usages`, kind: "usages", rows: [] });
    desk.dispatch({ type: "expand", region: "drawer", user: true });
    try {
      const out = await fetchUsages2({
        repo,
        path: panePath,
        line: pos.line,
        col: pos.col,
        ref: gitRef,
      });
      if (reqId !== usagesReqRef.current) return;
      setUsagesDock((st) =>
        st && st.setId === setId ? { ...st, out, loading: false, subject: usagesTitle(out) } : st,
      );
      desk.keepInDrawer({
        id: setId,
        title: `${usagesTitle(out)} · usages`,
        kind: "usages",
        rows: [],
        // The tab badge is the SERVER's total, not `rows.length` (which is
        // 0 for this set — its body is a `usages/2` payload, not a row
        // list). A tab reading 0 beside a census reading 1,841 would be the
        // disagreeing-count bug in miniature.
        count: out.totals.all,
      });
    } catch (e) {
      if (reqId !== usagesReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setUsagesDock((st) => (st && st.setId === setId ? { ...st, loading: false, error: message } : st));
      toast.err(`gr failed: ${message}`);
    }
  }

  /// The mentions chip: the ONLY caller of the grep lane left in the
  /// reader. Toggling it on fetches once; toggling it off drops the count
  /// rather than keeping a stale one on screen.
  async function toggleUsagesMentions() {
    const st = usagesDock;
    if (!st) return;
    if (st.mentionsOn) {
      setUsagesDock((s) => (s ? { ...s, mentionsOn: false, mentions: null } : s));
      return;
    }
    setUsagesDock((s) => (s ? { ...s, mentionsOn: true } : s));
    try {
      const refs = await fetchXrefs(repo, st.subject);
      setUsagesDock((s) =>
        s && s.setId === st.setId ? { ...s, mentions: refs.results.length } : s,
      );
    } catch (e) {
      setUsagesDock((s) => (s && s.setId === st.setId ? { ...s, mentionsOn: false } : s));
      toast.err(`mentions failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /// The rows `]u`/`[u` walk — the SAME `walkOrder` the dock renders, so
  /// "next" can never mean two different things.
  function usagesWalk(): UsageRow2[] {
    if (!usagesDock?.out) return [];
    const rows = applyChips(
      [...usagesDock.out.exact, ...usagesDock.out.likely, ...usagesDock.out.candidate],
      usagesDock.chips,
    );
    return walkOrder(groupRows(rows, usagesDock.axis));
  }

  function stepUsages(delta: 1 | -1) {
    const rows = usagesWalk();
    if (rows.length === 0) {
      if (usagesDock) toast.err("no usages in the active set");
      return;
    }
    const next = stepCursor(rows.length, usagesDock?.cursor ?? -1, delta);
    setUsagesDock((st) => (st ? { ...st, cursor: next } : st));
    desk.dispatch({ type: "expand", region: "drawer", user: true });
    const row = rows[next];
    if (row) openUsageRow(row, "here");
  }


  // ── V71-E2 — the action menu's four doors ─────────────────────────────
  //
  // ONE delegated handler, at the reader root, resolving the target from
  // the event — never a per-component `contextmenu` listener (risk 8: two
  // menus fighting over one nested DOM event). Shift+right-click passes
  // through to the browser (free on Firefox, which fires no `contextmenu`
  // at all with Shift held; explicit here for Chromium), and the menu's
  // last row SAYS that, because an unadvertised escape hatch is the
  // failure the OpenStreetMap thread records.
  function openActionMenu(at: { x: number; y: number }, pill: boolean) {
    const pane = focusedPane;
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    // LINE-granular, deliberately: `usages/2`'s range target and this
    // menu's `end_col` are both optional, and a line-granular selection is
    // the one the reader already tracks for "+ Set" and the annotation
    // composer. A sub-line selection still resolves — as its own line's
    // range — rather than being refused.
    const sel = (pane === 1 ? lastSelRef1 : lastSelRef2).current;
    const cursorLine = (pane === 1 ? cursorLineRef1 : cursorLineRef2).current;
    const line = sel?.start ?? cursorLine ?? 1;
    const endLine = sel && sel.end > sel.start ? sel.end : undefined;
    const content = (pane === 1 ? file.data : pane2File.data)?.content;
    const text =
      sel && content
        ? content.split("\n").slice(sel.start - 1, sel.end).join("\n")
        : undefined;
    const next: ActionMenuState = {
      at,
      path: panePath,
      line,
      col: 0,
      endLine,
      text: text && text.length > 0 ? text : undefined,
      target: 0,
      out: null,
      loading: true,
      error: null,
      pill,
    };
    setActionMenu(next);
    void loadActions(next);
  }

  /// `Space b a` — the leader's own door to add-to-board. Asks the same
  /// `/api/actions` the panel asks (so the two agree about what "this" is),
  /// takes the DEFAULT target and opens the picker. A failure is an honest
  /// toast; nothing is guessed when the daemon cannot answer.
  async function addCaretToBoard() {
    const pane = focusedPane;
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    const sel = (pane === 1 ? lastSelRef1 : lastSelRef2).current;
    const cursorLine = (pane === 1 ? cursorLineRef1 : cursorLineRef2).current;
    const line = sel?.start ?? cursorLine ?? 1;
    try {
      const out = await fetchActions({
        repo,
        path: panePath,
        line,
        col: 0,
        ref: gitRef,
        endLine: sel && sel.end > sel.start ? sel.end : undefined,
      });
      const target = out.targets[out.active];
      if (!target) {
        toast.warn("no target here — put the caret on a line first");
        return;
      }
      setAddToBoard(target);
    } catch (e) {
      toast.err(`couldn't read actions here: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function loadActions(st: ActionMenuState) {
    const reqId = ++actionReqRef.current;
    try {
      const out = await fetchActions({
        repo,
        path: st.path,
        line: st.line,
        col: st.col,
        ref: gitRef,
        endLine: st.endLine,
        text: st.text,
        target: st.target,
      });
      if (reqId !== actionReqRef.current) return;
      setActionMenu((s) => (s ? { ...s, out, loading: false } : s));
    } catch (e) {
      if (reqId !== actionReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setActionMenu((s) => (s ? { ...s, loading: false, error: message } : s));
    }
  }

  function setActionTarget(index: number) {
    setActionMenu((s) => {
      if (!s) return s;
      const next = { ...s, target: index, loading: true };
      void loadActions(next);
      return next;
    });
  }

  function closeActionMenu() {
    setActionMenu(null);
    // APG: Escape returns focus to the invoking context.
    paneRepoPath(focusedPane).viewRef.current?.focus();
  }

  /// Run one row. The op vocabulary is CLOSED and `resolveOp` is
  /// exhaustive over it (`lib/actionOps.ts`), so a row the server can send
  /// is a row this function can perform — the compile-time version of the
  /// v7.0 dead-surface check.
  function runAction(row: ActionRow) {
    const st = actionMenu;
    const target = st?.out?.targets[st.out.active];
    if (!st || !target) return;
    const resolved = resolveOp(row, target, {
      repo,
      origin: typeof window === "undefined" ? "" : window.location.origin,
      ref: gitRef,
      selectedText: st.text,
    });
    closeActionMenu();
    switch (resolved.kind) {
      case "navigate":
        if (resolved.pane === 2) {
          navigate(buildReaderUrl({ pane2: { path: target.path, ref: gitRef, line: target.line } }));
        } else {
          navigate(resolved.href);
        }
        return;
      case "peek": {
        const pos: WordPos = { line: st.line, col: st.col, word: target.name ?? "" };
        if (resolved.peek === "hover") void handleHover(focusedPane, pos);
        else void handleGotoDef(focusedPane, pos);
        return;
      }
      case "dock": {
        const pos: WordPos = { line: target.line ?? st.line, col: target.col ?? st.col, word: target.name ?? target.label };
        switch (resolved.dock) {
          case "usages":
            void handleFindRefs(focusedPane, pos);
            return;
          case "callers":
            void handleHierarchyCallers(focusedPane, pos);
            return;
          case "definitions":
            void handleGotoDef(focusedPane, pos);
            return;
          case "blame":
          case "why":
            inspectorRef.current?.openTab("provenance");
            handleBlameClick(target.line ?? st.line);
            return;
          case "diagnostics":
          case "framework":
            inspectorRef.current?.openTab("understand");
            return;
          default:
            toast.err(`no dock for ${resolved.dock}`);
            return;
        }
      }
      case "search":
        // The repo lives IN the kbcq/1 query (`repo:<name>`), which the
        // server already put there — so this goes through the Location
        // Contract's own encoder rather than assembling a second search
        // URL grammar beside it.
        navigate(
          encodeLocation({ ...emptyLocation(repo), mode: "search", raw: undefined, query: resolved.query }),
        );
        return;
      case "clipboard":
        void copyToClipboard(resolved.text);
        toast.ok(`copied ${resolved.label}`);
        return;
      case "compose":
        if (resolved.surface === "suggestion") {
          // The suggestion editor is a review-room surface; the reader's
          // own annotation composer is the door the reader has, and the
          // apply path is unchanged and still loopback-only.
          toast.ok("open the review room to compose a suggestion for this range");
          return;
        }
        setAnnotationActiveLine(target.line ?? st.line);
        setAnnotationActiveLineEnd(target.end_line ?? null);
        setAnnotationInitialIntent(resolved.surface === "ask" ? "question" : undefined);
        inspectorRef.current?.openTab("annotations");
        return;
      case "collect":
        // V74-L2 — `collect.board` is the ONE door for "add to board" on every
        // surface the action panel serves (D10). It arrives on the `.` panel,
        // the right-click menu and the drag-select pill from the SAME
        // server-rendered list, so there is no per-surface hand-picked row.
        if (resolved.sink === "board") {
          setAddToBoard(target);
          return;
        }
        inspectorRef.current?.openTab("notes");
        toast.ok("bookmarks live in the rail's notes tab");
        return;
      case "unavailable":
        toast.err(resolved.reason);
        return;
      default: {
        const never: never = resolved;
        return never;
      }
    }
  }

  /// Opening a usage row goes through the Ramp, so the gesture ladder is
  /// identical to every other result surface AND a new-tab open is
  /// TRAIL-LINKED to where the reader came from (`via: "usage_of"`,
  /// `subject` = the symbol) — the operator's "opens the file in a new tab
  /// linked in some way to where we were".
  function openUsageRow(row: UsageRow2, rung: "here" | "other" | "tab") {
    ramp.activate(rung, {
      repo,
      path: row.path,
      line: row.line,
      via: "usage_of",
      subject: usagesDock?.subject,
      trust: row.trust,
      snippet: row.context,
    });
  }

  // B3 — `K` becomes the "what AND why" provenance hover: the TOP
  // `/api/resolve` candidate rendered as a card (`PeekPanel`'s
  // `HoverCardView`), followed by ONE provenance line from `/api/why` at
  // that candidate's def location. Two sequential awaits (resolve, then
  // why) — the `peekReqRef` stale-guard is checked after EACH one, so a
  // newer `gd`/`gr`/`K` press cleanly drops both a slow resolve and a slow
  // why that are no longer relevant.
  async function handleHover(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    peekOwnerPaneRef.current = pane;
    // T1 — remembered so the hover card's footer "usages"/"callers" action
    // hints (design-ui.md §9.1) can re-issue `gr`/`gc` against the SAME
    // cursor position that opened this card, without `PeekPanel` itself
    // needing to know anything about `WordPos`.
    lastHoverPosRef.current = { pane, pos };
    dispatchHier({ type: "CLOSE" });
    setPeekAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "OPEN", mode: "hover", word: pos.word });
    const reqId = ++peekReqRef.current;
    try {
      const resolved = await fetchResolve({ repo, path: panePath, line: pos.line, col: pos.col });
      if (reqId !== peekReqRef.current) return;
      const top = resolved.candidates[0];
      if (!top) {
        dispatchPeek({ type: "SET_ROWS", rows: [], approximate: false, note: resolved.note });
        return;
      }
      const card: HoverCard = { ident: resolved.ident, role: resolved.role, candidate: top };
      dispatchPeek({ type: "SET_CARD", card, note: resolved.note });

      let provenance: HoverProvenance;
      try {
        const why = await fetchWhyLine(top.repo, top.path, top.line);
        if (reqId !== peekReqRef.current) return;
        const confident = why.attribution.confidence === "trailer" || why.attribution.confidence === "exact";
        provenance = confident
          ? {
              none: false,
              displayName: why.attribution.display_name ?? why.attribution.session_id ?? null,
              commitSubject: why.region.subject,
            }
          : { none: true, displayName: null, commitSubject: null };
      } catch {
        if (reqId !== peekReqRef.current) return;
        provenance = { none: true, displayName: null, commitSubject: null };
      }
      dispatchPeek({ type: "SET_CARD_PROVENANCE", provenance });
    } catch (e) {
      if (reqId !== peekReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchPeek({ type: "SET_ERROR", message });
      toast.err(`K failed: ${message}`);
    }
  }

  function handlePeekActivate(row: PeekRow) {
    dispatchPeek({ type: "CLOSE" });
    const pane = peekOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (row.repo === repo && row.path === panePath) {
      // Same file already open — re-center without a navigation (mirrors
      // the outline rail's/annotations' own `jumpToLine` idiom).
      jumpToLine(pane, row.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, row.repo, row.path, row.line);
    }
  }

  function handlePeekClose() {
    dispatchPeek({ type: "CLOSE" });
    paneRepoPath(peekOwnerPaneRef.current).viewRef.current?.focus();
  }

  // --- V3.1-H3a — call/type hierarchy panels (gc / gC / gt) ---------------
  const [hier, dispatchHier] = useReducer(hierarchyReducer, initialHierarchyState);
  const [hierAnchor, setHierAnchor] = useState<PeekAnchor | null>(null);
  const hierOwnerPaneRef = useRef<1 | 2>(1);
  const hierReqRef = useRef(0);

  /**
   * Hierarchy endpoints require a callable def position. When the cursor is
   * on a call site / ref, resolve and use the top candidate's def location.
   */
  async function hierarchyPosWithResolveFallback(
    panePath: string,
    pos: WordPos,
  ): Promise<{ path: string; line: number; col: number; name: string }> {
    try {
      const resolved = await fetchResolve({
        repo,
        path: panePath,
        line: pos.line,
        col: pos.col,
      });
      const top = resolved.candidates[0];
      if (top && (resolved.role === "ref" || resolved.role === "import")) {
        return {
          path: top.path,
          line: top.line,
          col: 0,
          name: resolved.ident || pos.word,
        };
      }
      // On a def role (or unknown), try the cursor first.
      return {
        path: panePath,
        line: pos.line,
        col: pos.col,
        name: resolved.ident || pos.word,
      };
    } catch {
      return { path: panePath, line: pos.line, col: pos.col, name: pos.word };
    }
  }

  async function fetchCallersAt(
    path: string,
    line: number,
    col: number,
  ) {
    try {
      return await fetchHierarchyCallers({ repo, path, line, col });
    } catch (e) {
      // Cursor may have been on a call site without a prior resolve hit —
      // last resort: resolve then retry once.
      const resolved = await fetchResolve({ repo, path, line, col });
      const top = resolved.candidates[0];
      if (!top) throw e;
      return await fetchHierarchyCallers({
        repo,
        path: top.path,
        line: top.line,
        col: 0,
      });
    }
  }

  async function fetchCalleesAt(path: string, line: number, col: number) {
    try {
      return await fetchHierarchyCallees({ repo, path, line, col });
    } catch (e) {
      const resolved = await fetchResolve({ repo, path, line, col });
      const top = resolved.candidates[0];
      if (!top) throw e;
      return await fetchHierarchyCallees({
        repo,
        path: top.path,
        line: top.line,
        col: 0,
      });
    }
  }

  async function handleHierarchyCallers(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== hierReqRef.current) return;
    dispatchHier({ type: "OPEN", mode: "callers", title: target.name });
    try {
      const out = await fetchCallersAt(target.path, target.line, target.col);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildCallersTree(out) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gc failed: ${message}`);
    }
  }

  async function handleHierarchyCallees(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== hierReqRef.current) return;
    dispatchHier({ type: "OPEN", mode: "callees", title: target.name });
    try {
      const out = await fetchCalleesAt(target.path, target.line, target.col);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildCalleesTree(out) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gC failed: ${message}`);
    }
  }

  async function handleHierarchyTypes(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    dispatchHier({ type: "OPEN", mode: "types", title: pos.word });
    try {
      // Resolve to learn kind — toast if not type-ish.
      let name = pos.word;
      let kind: string | null = null;
      try {
        const resolved = await fetchResolve({
          repo,
          path: panePath,
          line: pos.line,
          col: pos.col,
        });
        if (reqId !== hierReqRef.current) return;
        name = resolved.ident || pos.word;
        kind = resolved.candidates[0]?.kind ?? null;
      } catch {
        // Fall through with the word; types endpoint is name-based.
      }
      if (kind && !isTypeIshKind(kind)) {
        dispatchHier({ type: "CLOSE" });
        toast.err("not a type");
        return;
      }
      const out = await fetchHierarchyTypes(repo, name, panePath);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildTypesTree(out, panePath) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gt failed: ${message}`);
    }
  }

  function handleHierActivate(node: HierarchyNode) {
    if (!node.path || node.line <= 0) return;
    dispatchHier({ type: "CLOSE" });
    const pane = hierOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (node.path === panePath) {
      jumpToLine(pane, node.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, node.path, node.line);
    }
  }

  function handleHierClose() {
    dispatchHier({ type: "CLOSE" });
    paneRepoPath(hierOwnerPaneRef.current).viewRef.current?.focus();
  }

  // --- V3.1-H3b — impact panel (`gi`) + ego-graph (`gG`) ------------------
  const [impact, dispatchImpact] = useReducer(impactReducer, initialImpactState);
  const [impactAnchor, setImpactAnchor] = useState<PeekAnchor | null>(null);
  const impactOwnerPaneRef = useRef<1 | 2>(1);
  const impactReqRef = useRef(0);

  type EgoUiState = {
    open: boolean;
    title: string;
    loading: boolean;
    error: string | null;
    layout: EgoLayoutResult | null;
    center: { path: string; line: number; col: number; name: string };
  };
  const [ego, setEgo] = useState<EgoUiState | null>(null);
  const egoOwnerPaneRef = useRef<1 | 2>(1);
  const egoReqRef = useRef(0);

  async function handleImpact(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    impactOwnerPaneRef.current = pane;
    setImpactAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    dispatchHier({ type: "CLOSE" });
    setEgo(null);
    const reqId = ++impactReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== impactReqRef.current) return;
    dispatchImpact({ type: "OPEN", title: target.name });
    try {
      const out = await fetchImpactAnalysis({
        repo,
        path: target.path,
        line: target.line,
        col: target.col || 0,
      });
      if (reqId !== impactReqRef.current) return;
      dispatchImpact({ type: "SET_DATA", data: out });
    } catch (e) {
      if (reqId !== impactReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchImpact({ type: "SET_ERROR", message });
      toast.err(`gi failed: ${message}`);
    }
  }

  function handleImpactActivate(row: ImpactFlatRow) {
    if (!isImpactNavigable(row)) return;
    dispatchImpact({ type: "CLOSE" });
    const pane = impactOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (row.path === panePath) {
      jumpToLine(pane, row.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, row.path, row.line);
    }
  }

  function handleImpactClose() {
    dispatchImpact({ type: "CLOSE" });
    paneRepoPath(impactOwnerPaneRef.current).viewRef.current?.focus();
  }

  async function buildEgoNeighborhood(
    path: string,
    line: number,
    col: number,
    name: string,
  ): Promise<{ layout: EgoLayoutResult; title: string }> {
    const centerId = `c:${path}:${line}`;
    const nodes: EgoLayoutInput["nodes"] = {};
    const inEdges: EgoLayoutInput["inEdges"] = [];
    const outEdges: EgoLayoutInput["outEdges"] = [];

    let centerName = name;
    try {
      const callers = await fetchCallersAt(path, line, col);
      centerName = callers.function.name || name;
      for (const g of callers.callers) {
        const site = g.sites[0];
        if (!site) continue;
        const id = `in:${g.path}:${g.enclosing?.line ?? site.line}`;
        nodes[id] = {
          id,
          name: g.enclosing?.name ?? g.path,
          class: site.class,
          path: g.path,
          line: g.enclosing?.line ?? site.line,
          kind: g.enclosing?.kind,
        };
        inEdges.push({ from: id, to: centerId, class: site.class });
      }
    } catch {
      // callers optional for types / non-callables
    }
    try {
      const callees = await fetchCalleesAt(path, line, col);
      centerName = callees.function.name || centerName;
      for (const s of callees.callees) {
        const id = `out:${s.target?.path ?? s.name}:${s.target?.line ?? s.line}`;
        nodes[id] = {
          id,
          name: s.name,
          class: s.class,
          path: s.target?.path,
          line: s.target?.line ?? s.line,
        };
        outEdges.push({ from: centerId, to: id, class: s.class });
      }
    } catch {
      // callees optional
    }
    // Types: also pull implementors as out-edges when name is type-ish.
    try {
      const types = await fetchHierarchyTypes(repo, centerName, path);
      for (const e of types.subtypes) {
        const id = `impl:${e.target?.path ?? e.via.path}:${e.target?.line ?? e.via.line}`;
        if (nodes[id]) continue;
        nodes[id] = {
          id,
          name: e.name,
          class: e.class,
          path: e.target?.path ?? e.via.path,
          line: e.target?.line ?? e.via.line,
          kind: e.kind,
        };
        outEdges.push({ from: centerId, to: id, class: e.class });
      }
    } catch {
      // ignore
    }

    const layout = layoutEgoGraph({
      center: {
        id: centerId,
        name: centerName,
        class: "exact",
        path,
        line,
      },
      inEdges,
      outEdges,
      nodes,
      depth: 1,
      nodeCap: 40,
    });
    return { layout, title: centerName };
  }

  async function handleEgoGraph(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    egoOwnerPaneRef.current = pane;
    dispatchPeek({ type: "CLOSE" });
    dispatchHier({ type: "CLOSE" });
    dispatchImpact({ type: "CLOSE" });
    const reqId = ++egoReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== egoReqRef.current) return;
    setEgo({
      open: true,
      title: target.name,
      loading: true,
      error: null,
      layout: null,
      center: target,
    });
    try {
      const { layout, title } = await buildEgoNeighborhood(
        target.path,
        target.line,
        target.col || 0,
        target.name,
      );
      if (reqId !== egoReqRef.current) return;
      setEgo({
        open: true,
        title,
        loading: false,
        error: null,
        layout,
        center: { ...target, name: title },
      });
    } catch (e) {
      if (reqId !== egoReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setEgo({
        open: true,
        title: target.name,
        loading: false,
        error: message,
        layout: null,
        center: target,
      });
      toast.err(`gG failed: ${message}`);
    }
  }

  function handleEgoActivate(node: EgoLaidOutNode) {
    if (!node.path || !node.line) return;
    setEgo(null);
    const pane = egoOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (node.path === panePath) {
      jumpToLine(pane, node.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, node.path, node.line);
    }
  }

  async function handleEgoRecenter(node: EgoLaidOutNode) {
    if (!node.path || !node.line) return;
    const reqId = ++egoReqRef.current;
    setEgo((prev) =>
      prev
        ? {
            ...prev,
            loading: true,
            title: node.name,
            center: {
              path: node.path!,
              line: node.line!,
              col: 0,
              name: node.name,
            },
          }
        : prev,
    );
    try {
      const { layout, title } = await buildEgoNeighborhood(
        node.path,
        node.line,
        0,
        node.name,
      );
      if (reqId !== egoReqRef.current) return;
      setEgo({
        open: true,
        title,
        loading: false,
        error: null,
        layout,
        center: { path: node.path, line: node.line, col: 0, name: title },
      });
    } catch (e) {
      if (reqId !== egoReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setEgo((prev) =>
        prev ? { ...prev, loading: false, error: message } : prev,
      );
    }
  }

  function handleEgoClose() {
    setEgo(null);
    paneRepoPath(egoOwnerPaneRef.current).viewRef.current?.focus();
  }

  function handleLensUsages(pane: 1 | 2, decl: LensDeclaration) {
    void handleFindRefs(pane, {
      line: decl.line,
      col: 0,
      word: decl.name,
    });
  }

  function handleLensImpls(pane: 1 | 2, decl: LensDeclaration) {
    void handleHierarchyTypes(pane, {
      line: decl.line,
      col: 0,
      word: decl.name,
    });
  }

  function handleLensAuthor(_pane: 1 | 2, decl: LensDeclaration) {
    inspectorRef.current?.openTab("provenance");
    // Best-effort: open why for the declaration line if gutter is on.
    if (provenanceMode !== "off") {
      handleBlameClick(decl.line);
    }
  }

  async function handleHierToggleExpand(node: HierarchyNode) {
    // Collapse if already expanded.
    if (node.expanded) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: false,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Section headers / already-loaded children: just expand.
    if (node.children.length > 0 || node.cycle || node.depthCapped || node.truncatedNote) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: true,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Root row in call modes already has children from first fetch.
    if (node.depth === 0) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: true,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Lazy fetch next level.
    if (node.depth >= HIERARCHY_DEPTH_CAP) return;
    const mode: HierarchyMode = hier.mode;
    const chain = collectAncestorLocs(hier.roots, node.id) ?? [];
    const ancestors = ancestorKeys(chain);

    dispatchHier({
      type: "PATCH_ROOTS",
      roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: true })),
    });

    const reqId = ++hierReqRef.current;
    try {
      let children: HierarchyNode[] = [];
      if (mode === "callers" || node.dir === "callers") {
        const out = await fetchHierarchyCallers({
          repo,
          path: node.path,
          line: node.line,
          col: node.col,
        });
        if (reqId !== hierReqRef.current) return;
        children = callersToNodes(out.callers, node.depth + 1, "callers", ancestors, out.truncated);
      } else if (mode === "callees" || node.dir === "callees") {
        const out = await fetchHierarchyCallees({
          repo,
          path: node.path,
          line: node.line,
          col: node.col,
        });
        if (reqId !== hierReqRef.current) return;
        children = calleesToNodes(out.callees, node.depth + 1, "callees", ancestors);
      } else {
        // Types: expand a type edge into its own supers/subs one level.
        const out = await fetchHierarchyTypes(repo, node.name, node.path || undefined);
        if (reqId !== hierReqRef.current) return;
        if (node.dir === "supertypes") {
          children = typeEdgesToNodes(
            out.supertypes,
            node.depth + 1,
            "supertypes",
            "supertypes",
            ancestors,
          );
        } else {
          children = typeEdgesToNodes(
            out.subtypes,
            node.depth + 1,
            "subtypes",
            "subtypes",
            ancestors,
          );
        }
      }
      if (reqId !== hierReqRef.current) return;
      dispatchHier({
        type: "PATCH_ROOTS",
        roots: updateNode(hier.roots, node.id, (n) => ({
          ...n,
          loading: false,
          expanded: true,
          children,
        })),
      });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({
        type: "PATCH_ROOTS",
        roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: false })),
      });
      toast.err(`hierarchy expand: ${message}`);
    }
  }

  /// `lineEnd` selects the WHOLE `[line, lineEnd]` span (a `range`
  /// annotation's badge click, Phase D) rather than collapsing to `line`
  /// alone — `GotoSel`'s own `start`/`end` already support this; every
  /// other caller (outline jump, symbol/line annotation goto, peek
  /// activation) just omits it and gets the old single-line behavior.
  function jumpToLine(pane: 1 | 2, line: number, lineEnd?: number) {
    nonceRef.current += 1;
    const sel = { start: line, end: lineEnd ?? line, nonce: nonceRef.current };
    if (pane === 1) setGotoSel1(sel);
    else setGotoSel2(sel);
    // Same-file gd / peek / outline landings — still a jump.
    const p = pane === 1 ? activeFile : pane2Loc?.path;
    if (p) recordJump({ repo, path: p, line, snippet: "" });
  }

  // --- Wave E — the working set: `[f ]f` cycles the FOCUSED pane's file ---
  function handleCycleFile(pane: 1 | 2, dir: -1 | 1) {
    const current = pane === 1 ? activeFile : pane2Loc?.path;
    const next = workingSet.cycle(current, dir);
    if (!next || next === current) return;
    if (pane === 1) {
      navigate(buildReaderUrl({ pane1: { path: next, ref: gitRef } }));
    } else {
      navigate(buildReaderUrl({ pane2: { path: next, ref: pane2Loc?.ref } }));
    }
  }

  // --- Wave E — Ctrl-w v / Ctrl-w q: split-with-self / close the focused
  // pane ----------------------------------------------------------------
  /// `Ctrl-w v` — "split with self," the vim gesture: opens whichever
  /// file/ref `pane` currently shows into pane2 at the SAME cursor line,
  /// then focuses pane2 (a fresh `EditorView`'s auto-focus effect does the
  /// actual `.focus()` call once its data — already cached, same query key
  /// — resolves, typically on the very next tick).
  function handleSplitSelf(pane: 1 | 2) {
    const { path: panePath, ref: paneRef } = paneRepoPath(pane);
    if (!panePath) return;
    const line = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    navigate(buildReaderUrl({ pane2: { path: panePath, ref: paneRef, line } }));
    setFocusedPane(2);
  }

  /// `Ctrl-w q` — closes `pane` (a no-op if there's no second pane to begin
  /// with — nothing to close down TO). Closing pane2 just drops the query
  /// param; closing pane1 PROMOTES pane2's location into pane1's URL slot
  /// (the split collapses to a single view of what pane2 was showing).
  /// Either way exactly one pane remains, so focus always settles on it.
  function handleClosePane(pane: 1 | 2) {
    if (!pane2Loc) return;
    if (pane === 2) {
      navigate(buildReaderUrl({ pane2: null }));
    } else {
      navigate(buildReaderUrl({ pane1: { path: pane2Loc.path, ref: pane2Loc.ref, line: pane2Loc.line }, pane2: null }));
    }
    setFocusedPane(1);
    // Covers the "closing pane2, pane1's own content is unchanged" case
    // immediately (its own focus-follows-file effect won't re-fire since
    // `activeFile`/`gitRef` didn't change); the "promoting pane2's
    // DIFFERENT file into pane1" case is covered by that same effect once
    // the new blobHash lands, same as any other pane1 navigation.
    codeViewRef1.current?.focus();
  }

  function handlePaneFocus(pane: 1 | 2, dir: "prev" | "next") {
    if (pane === 1) {
      if (dir === "prev") {
        // V70-H1 — set `keyboardRegion` to `"tree"` SYNCHRONOUSLY: this IS
        // the scope switch, not a side effect inferred from whatever DOM
        // focus event may or may not eventually land (see `keyboardRegion`'s
        // own doc for why inferring it from `focusin` alone was flaky, not
        // just occasionally wrong — it raced the dock's async expand, the
        // tree's virtualizer, and CM6's own focus handling). `blur()` +
        // `focusContainer()` still run, best-effort, for real DOM/AT focus
        // (a screen-reader user needs a genuine focus target, not just a
        // scope flag) — but nothing above depends on either succeeding.
        (document.activeElement as HTMLElement | null)?.blur?.();
        setKeyboardRegion("tree");
        showDock();
        treeRef.current?.focusContainer();
        // V70-A4 — tell the Desk where the keyboard went, so `Ctrl-w r`'s
        // direction table resolves against the region that actually has
        // focus rather than against a guess.
        desk.setFocusRegion("dock");
        return;
      }
      if (pane2Loc) codeViewRef2.current?.focus();
      // else: no pane2 to move to — a no-op (mirrors the pre-split
      // behavior, where this branch just refocused the buffer it was
      // already in).
      return;
    }
    // pane === 2
    if (dir === "prev") codeViewRef1.current?.focus();
    // dir === "next" from pane2: no pane3 — a dead end, no-op.
  }

  function vimCallbacksForPane(pane: 1 | 2): VimReaderCallbacks {
    return {
      // V70-A4 — the Desk's two region chords. Both are pure shell verbs:
      // nothing about them reads or writes the buffer.
      onResizeMode: () => {
        desk.setFocusRegion("main");
        desk.setResizeMode(true);
      },
      onZoomRegion: () => desk.toggleZoom("main"),
      onAnnotate: (sel) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        setAnnotationActiveLine(sel.line);
        // A visual-mode `a` carries `lineEnd` (`vimReader.ts`'s
        // `lineSelForCallback`) — previously dropped on the floor; Phase D's
        // composer needs it to offer a `range` annotation instead of always
        // collapsing to the selection's start line.
        setAnnotationActiveLineEnd(sel.lineEnd ?? null);
        inspectorRef.current?.openTab("annotations");
      },
      onPermalink: (sel) => handlePermalinkForPane(pane, sel),
      onGotoDef: (pos) => {
        // D24 — the first `gd` in a browser profile earns ONE coach-mark for
        // the commitment Ramp. Claimed here, not in `handleGotoDef`, because
        // the teaching moment is the KEY (the reader already had a dozen ways
        // to reach a definition with the mouse).
        coach.noteGotoDefinition();
        void handleGotoDef(pane, pos);
      },
      onFindRefs: (pos) => void handleFindRefs(pane, pos),
      onHover: (pos) => void handleHover(pane, pos),
      onHistoryStep: (dir) => handleHistoryStep(pane, dir),
      onCycleFile: (dir) => handleCycleFile(pane, dir),
      onPaneFocus: (dir) => handlePaneFocus(pane, dir),
      onSplitSelf: () => handleSplitSelf(pane),
      onClosePane: () => handleClosePane(pane),
      onShowHelp: () => setHelpOpen(true),
      // V3.N1 — nav memory. `onRecordJump` is the sink for G/gg/mark/
      // search-step records from vimReader; Ctrl-o/i + g. are host-owned.
      onRecordJump: (info) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        recordJump({ repo, path: panePath, line: info.line, snippet: info.snippet });
      },
      onJumpBack: () => handleJumpBack(),
      onJumpForward: () => handleJumpForward(),
      // V70-A6 — `u`'s bare-key arm inside the buffer (`vimReader.ts`'s own
      // doc). `nav.back` is registered centrally (`app.tsx`), scope `global`
      // — running it BY ID keeps the trail-origin-or-plain-Back decision in
      // that one place instead of a second copy here.
      onNavBack: () => commandBus.run("nav.back"),
      onRecentLocations: () => setRecentLocsOpen(true),
      onStructurePopup: () => setStructureOpen(true),
      onToggleBookmark: () => void handleToggleBookmark(pane),
      onMnemonicPopup: () => setMnemonicOpen(true),
      onLineHistory: (sel) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        setLineHistory({ path: panePath, line: sel.line, lineEnd: sel.lineEnd });
      },
      onHierarchyCallers: (pos) => void handleHierarchyCallers(pane, pos),
      onHierarchyCallees: (pos) => void handleHierarchyCallees(pane, pos),
      onHierarchyTypes: (pos) => void handleHierarchyTypes(pane, pos),
      onImpact: (pos) => void handleImpact(pane, pos),
      onEgoGraph: (pos) => void handleEgoGraph(pane, pos),
    };
  }

  async function handleToggleBookmark(pane: 1 | 2) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    const line = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    const existing = (bookmarksQ.data?.bookmarks ?? []).find(
      (b) => b.path === panePath && b.line === line,
    );
    try {
      if (existing) {
        await deleteBookmarkMut.mutateAsync(existing.id);
        toast.ok(`bookmark removed · ${panePath}:${line}`);
      } else {
        await createBookmarkMut.mutateAsync({ repo, path: panePath, line });
        toast.ok(`bookmark added · ${panePath}:${line}`);
      }
    } catch (e) {
      toast.err(`bookmark: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  function handleJumpBookmark(loc: { path: string; line: number }) {
    recordJump({ repo, path: loc.path, line: loc.line, snippet: "" });
    if (loc.path !== activeFile) {
      skipNavRecordRef.current = true;
    }
    navigate(buildReaderUrl({ pane1: { path: loc.path, ref: gitRef, line: loc.line } }));
    setFocusedPane(1);
  }

  // --- B1 — linkify: paths/URLs/session-ids inside comments+strings --------
  // Wave E: a path/sha clicked INSIDE a given pane opens into THAT pane
  // (keeps the interaction local to where the click happened), preserving
  // whatever the other pane is showing.
  function linkifyCallbacksForPane(pane: 1 | 2): LinkifyCallbacks {
    return {
      onOpenUrl: (url) => window.open(url, "_blank", "noopener,noreferrer"),
      onOpenPath: (linkedPath) => {
        const { ref: paneRef } = paneRepoPath(pane);
        if (pane === 1) navigate(buildReaderUrl({ pane1: { path: linkedPath, ref: paneRef } }));
        else navigate(buildReaderUrl({ pane2: { path: linkedPath, ref: paneRef } }));
      },
      onOpenSession: (id) => window.open(sessionUrl(id), "_blank", "noopener,noreferrer"),
      // Wave C — a bare commit sha in a comment/string now has a real
      // destination (the commit page hub) — always a full navigation away
      // from the reader entirely, so this intentionally does NOT preserve
      // pane2 (a commit page has no file pane to carry it into).
      onOpenSha: (sha) => navigate(commitUrl(repo, sha)),
    };
  }

  // V70-A5 — the reader chrome's keys, as kbc-cmd/1 handlers (scope `tree`).
  //
  // The hand-rolled `switch (e.key)` this replaces was one of twenty-three
  // such surfaces; the keys and their behaviour are unchanged. What changed
  // is that they are DECLARED — so the `?` sheet renders them, the palette
  // can run them, which-key lists them, and the conflicts gate sees `j`/`k`
  // colliding with the buffer's motions (a designed shadow: the buffer is the
  // deeper scope, and `isInsideBuffer` was always the real arbiter).
  //
  // `?` is gone from here entirely: it is a `scope: global` row now, so it
  // works on all twenty-seven routes instead of two.
  // WHICH scope is live is a question about where FOCUS is, not about which
  // route is mounted: the reader hosts two of them at once (the CM6 buffer
  // and the chrome around it), which is exactly the split `isInsideBuffer`
  // has always arbitrated for keys. Publishing it makes the palette and the
  // `?` sheet agree with the keyboard — ⌘K from the buffer offers `gd`, ⌘K
  // from the tree offers the tree's verbs.
  // V70-H1 — `keyboardRegion` is the SOURCE OF TRUTH for scope, set
  // SYNCHRONOUSLY by the app action that moves keyboard intent (`Ctrl-w h`
  // below), not inferred after the fact from a `focus`/`focusin` DOM
  // event — see `lib/keyboardRegion.ts`'s module doc for the full
  // rationale (a bare `blur()` with nothing else explicitly focused fires
  // NO event at all, and even a real one races the dock's async expand,
  // the tree's virtualizer, and CM6's own focus handling — flaky, not
  // reliably broken or reliably fine). `onFocusIn` below is a
  // CONFIRMATION over `nextKeyboardRegion`, never the sole source.
  // Defaults to "tree", matching the prior design's own effective default
  // (`bufferFocused` started `false`, i.e. scope "tree") — landing on a
  // bare repo route with no file open yet has no buffer to correct this
  // via a real `focusin` event, so a "buffer" default would strand the
  // scope exactly the way the OLD blur-only design used to strand it (a
  // regression this file's own `nextKeyboardRegion` rewrite briefly
  // reintroduced: `day-one.spec.ts`'s bare-repo palette test caught it).
  // A route that DOES open a file on load corrects this quickly via the
  // "focus follows the file" effect above, which fires a genuine event.
  const [keyboardRegion, setKeyboardRegion] = useState<KeyboardRegion>("tree");
  const bufferFocused = keyboardRegion === "buffer";
  useEffect(() => {
    function onFocusIn(e: FocusEvent) {
      setKeyboardRegion((cur) => nextKeyboardRegion(cur, e.target as HTMLElement | null));
    }
    window.addEventListener("focusin", onFocusIn);
    // V70-H1 — a `pointerdown` gets the SAME confirmation treatment as a
    // `focusin`: pressing inside the buffer or the tree names a region
    // exactly as unambiguously as focusing one does (a click on `<body>`,
    // or any other unnamed target, is left alone by the same
    // `nextKeyboardRegion` rule — see that function's own doc). This is
    // additive coverage, not a substitute for `focusin`: a KEYBOARD-only
    // transition (`Ctrl-w h`, `Tab`) never fires a `pointerdown` at all.
    function onPointerDown(e: PointerEvent) {
      setKeyboardRegion((cur) => nextKeyboardRegion(cur, e.target as HTMLElement | null));
    }
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("focusin", onFocusIn);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, []);

  // V72-G1.2 — the dossier's own navigation, all of it through
  // `lib/codeUrl.ts` (root CLAUDE.md #35: one builder, never an ad hoc
  // string). `openEntity` REPLACES the address in place; leaving the dossier
  // is an ordinary reader URL, which is what makes browser Back work with no
  // second stack (the Location Contract's "in-app Back IS the browser's
  // Back").
  const openEntityDossier = useCallback(
    (fqn: string) => {
      setDossierSection(null);
      navigate(entityUrl(repo, fqn, { path, ref: gitRef }));
    },
    [navigate, repo, path, gitRef],
  );
  const leaveDossier = useCallback(() => {
    navigate(codeUrl({ repo, path, ref: gitRef }));
  }, [navigate, repo, path, gitRef]);
  /// `]s`/`[s`. The cursor is the SECTION LIST's own (`lib/dossier.ts`), and
  /// scrolling is a DOM effect of moving it — the section still exists (and
  /// still renders its own empty caption) when it holds nothing, so the
  /// motion never skips unpredictably.
  const stepDossierSection = useCallback((dir: 1 | -1) => {
    setDossierSection((cur) => {
      const next = stepSection(cur, dir);
      const dom = DOSSIER_SECTIONS.find((x) => x.id === next);
      if (dom) {
        document.getElementById(dom.domId)?.scrollIntoView({ block: "start", behavior: "smooth" });
      }
      return next;
    });
  }, []);

  useCommandScope(bufferFocused ? "reader" : "tree", {
    "tree.focused": !bufferFocused,
    "help.open": helpOpen,
    buffer: bufferFocused,
    pane: focusedPane,
    // V70-A6 — the two context keys the new rows gate on. `peek.inline`
    // makes `+`/`-`/`Esc` mean the dial and the dismissal ONLY while an
    // inline peek is up; `pane.provisional` does the same for `p`.
    "peek.inline": inlinePeekIsOpen(inpeek),
    "pane.provisional": provisional.pane !== null,
    // V72-G1.2 — which CENTER is mounted, so the dossier's own keys (`i`,
    // `M`, `]s`/`[s`) are live ONLY there. The vocabulary is
    // `desk/centerModes.ts`'s `CenterMode`, declared in `registry.json`'s
    // `context_keys` so `commands doctor` can reason about disjointness.
    center: dossierMode ? "dossier" : "reader",
  });
  useCommandHandlers({
    // V72-G1.2 — the dossier family. `entity.dossier.open` is the only one
    // reachable outside the dossier center; the other four are gated
    // `center == dossier` in the registry, so they are inert (not merely
    // silent) anywhere else.
    "entity.dossier.open": () => {
      // The entity under the cursor, from the rail's own resolve — never a
      // guess at what a constant looks like. Nothing to open is a NO-OP with
      // a toast, never a navigation to an address we invented.
      if (!entityAddressUnderCursor) {
        toast.err("no class or module under the cursor to open a dossier for");
        return;
      }
      openEntityDossier(entityAddressUnderCursor);
    },
    "entity.dossier.inherited": () => setDossierInherited((v) => !v),
    "entity.dossier.sort": () => setDossierSort((s2) => cycleMemberSort(s2)),
    "entity.dossier.section-next": () => stepDossierSection(1),
    "entity.dossier.section-prev": () => stepDossierSection(-1),
    "rail.tab.dossier": () => selectRailTab("dossier"),
    "tree.focus-next": () => treeRef.current?.moveFocus(1),
    "tree.focus-prev": () => treeRef.current?.moveFocus(-1),
    "tree.open": () => treeRef.current?.activateFocused(undefined),
    "tree.open-pane2": () => treeRef.current?.activateFocused("pane2"),
    // V70-A6 — the jump list is `scope: global` now (recon G5: it used to be
    // unreachable with the buffer unfocused), so the reader registers the
    // handlers the window host calls when focus is in the tree or the chrome.
    // Inside the buffer the CM6 layer still executes them (`vim_kind`).
    "jump.back": () => handleJumpBack(),
    "jump.forward": () => handleJumpForward(),
    "pane.back": () => handleJumpBack(),
    "pane.forward": () => handleJumpForward(),
    "pane.pin": () => setProvisional((st) => (st.pane ? pinPane(st, st.pane) : st)),
    // V70-A6 — the Ramp's own registry rows, on the reader chrome. Inside the
    // CM6 buffer the bare ones (`o`/`O`/`Shift-Enter`) never reach the window
    // host at all (`CommandRoot`'s guard 2 — the vim layer owns bare keys
    // there), so these act on the TREE's focused row, which is the surface
    // that has one when the buffer does not.
    "ramp.open-tab": () => rampFocusedTreeRow("tab"),
    "ramp.open-window": () => rampFocusedTreeRow("window"),
    "ramp.open-pane2": () => rampFocusedTreeRow("other"),
    "peek.hover": () => rampFocusedTreeRow("peek"),
    "dismiss.peek": () => closeInlinePeek(),
    "peek.context-more": () => dispatchInpeek({ type: "CONTEXT", delta: 1 }),
    "peek.context-less": () => dispatchInpeek({ type: "CONTEXT", delta: -1 }),
    "tree.filter": () => {
      showDock();
      treeRef.current?.focusFilter();
    },
    // V71-F1 — kbc-tree/1's own rows. Every one of these is a `scope:
    // "tree"` row in `commands/registry.json`, which means it resolves
    // ONLY while the tree (not the CodeView) holds focus — so `]c`/`[c`
    // stay the buffer's commit-step there and become the tree's
    // changed-file walk here, a designed shadow rather than a conflict
    // (tree depth 10 < reader depth 20). Registered HERE because
    // `CommandRoot` fires whatever `useCommandHandlers` registered
    // regardless of a row's `dispatch` field — a row with no handler is
    // silently dead, which is the whole reason this file's own five
    // `pane.*` rows shipped broken in v7.0.
    "tree.view.next": () => {
      showDock();
      treeRef.current?.cycleView(1);
    },
    "tree.view.prev": () => {
      showDock();
      treeRef.current?.cycleView(-1);
    },
    "tree.filter.mode": () => treeRef.current?.toggleFilterMode(),
    "tree.decorations.cycle": () => treeRef.current?.cycleLanes(),
    "tree.select.toggle": () => treeRef.current?.toggleSelect(),
    "tree.actions": () => {
      showDock();
      treeRef.current?.openActions();
    },
    "tree.next-change": () => {
      if (treeRef.current?.jump("change", 1) === false) {
        toast.warn("no further changed row — turn the git lane on with `d` if it is off");
      }
    },
    "tree.prev-change": () => {
      if (treeRef.current?.jump("change", -1) === false) {
        toast.warn("no earlier changed row — turn the git lane on with `d` if it is off");
      }
    },
    "tree.next-annot": () => {
      if (treeRef.current?.jump("annot", 1) === false) {
        toast.warn("no further row with an open annotation");
      }
    },
    "tree.prev-annot": () => {
      if (treeRef.current?.jump("annot", -1) === false) {
        toast.warn("no earlier row with an open annotation");
      }
    },
    // `Space g f` — reveal from ANYWHERE (a `scope: "global"` row), which
    // is why it is not `gr`: see the row's own note in registry.json.
    "tree.reveal": () => {
      if (!isFile) {
        toast.warn("no file open to reveal");
        return;
      }
      showDock();
      treeRef.current?.reveal(path);
    },
    "desk.toggle.dock": () => toggleDock(),
    // V71-K2 — `Space d`. Declared `scope: global`/`dispatch: central` since
    // V70-A4 and never registered anywhere, so it was one of the 18 dead
    // rows `commands/deadRows.test.ts` now pins: `CommandRoot.onKey`
    // resolved it, found no owner and took its "leave the key alone"
    // branch, silently. The stripe button beside the drawer
    // (`desk/Desk.tsx`'s `leftFoot`) has always been its only door; this is
    // the SAME call that button makes, deliberately un-branched on
    // `isMobile` exactly as the button is (the drawer lives inside the
    // mobile sheet, so toggling the region is right in both shells — only
    // the DOCK needs `toggleDock`'s branch).
    "desk.toggle.drawer": () => desk.toggleRegion("drawer"),
    // V71-K3 — the seventeen rows `commands/deadRows.test.ts` pinned as
    // `KNOWN_UNREGISTERED`: mouse-only since V70-A4/A6, every one of them
    // dispatches EXACTLY the call its own button already makes (see each
    // one's own doc above, on `selectDrawerTabOrdinal`/
    // `expandDrawerIfCollapsed`/`selectRailTab`, and `handleKeepPeekInDrawer`
    // below) — a key is a second door onto an existing action, never a new
    // one. The desk-preset chip's own menu (`Desk.tsx`'s `presetMenu`) is
    // `desk.applyPreset`'s only other caller.
    "desk.preset.read": () => desk.applyPreset("read"),
    "desk.preset.review": () => desk.applyPreset("review"),
    "desk.preset.explore": () => desk.applyPreset("explore"),
    "desk.preset.present": () => desk.applyPreset("present"),
    // `Space {1-9}` — see `selectDrawerTabOrdinal`'s own doc for the
    // ordinal's 1-based addressing and its honest past-the-end no-op.
    // `count` is `null` for every OTHER row this map handles (none of them
    // bind a `{1-9}`/`{0-9}` wildcard), so defaulting to `1` only ever
    // fires here.
    "drawer.tab": ({ count }) => selectDrawerTabOrdinal(count ?? 1),
    // `] d` / `[ d` — the drawer's own `stepSet` action, already wired for
    // `Search.tsx`'s identical result-set stack (`search.stack.next/prev`);
    // wraps over LIVE tabs only, exactly as that precedent and its own
    // unit test (`drawerSets.test.ts`) describe.
    "drawer.tab-next": () => desk.drawerDispatch({ type: "stepSet", delta: 1 }),
    "drawer.tab-prev": () => desk.drawerDispatch({ type: "stepSet", delta: -1 }),
    // `Space u` — `drawerSets.ts` tracks insertion order (`seq`) for every
    // set, live or evicted, but never a separate "closed at" order, so
    // "the most recently closed tab" is not a question the current model
    // can honestly answer (the oldest-inserted evicted tab is not
    // necessarily the most recently closed one). Smallest honest version,
    // per the ruling: re-expand a collapsed drawer, the same no-op-if-
    // already-open gesture the stripe button's "open the drawer" makes.
    "drawer.reopen": () => expandDrawerIfCollapsed(),
    // `Space x` — evict the ACTIVE set (a VIEW operation; the row's rows
    // survive on the greyed tab), the same call `Drawer.tsx`'s own ✕
    // button makes for whichever tab is active.
    "drawer.close": () => {
      if (desk.drawer.activeId) desk.drawerDispatch({ type: "close", id: desk.drawer.activeId });
    },
    // `Space D` — pin/unpin the ACTIVE set, `Drawer.tsx`'s own pin button.
    "drawer.pin": () => {
      if (desk.drawer.activeId) desk.drawerDispatch({ type: "togglePin", id: desk.drawer.activeId });
    },
    // `Space K` — `PeekPanel`'s own "Keep in drawer" button
    // (`data-cmd="drawer.keep"`); `handleKeepPeekInDrawer` already no-ops
    // honestly when there is nothing open to keep (`peek.rows.length === 0`).
    "drawer.keep": () => handleKeepPeekInDrawer(),
    // `Space p` — `InspectorRail`'s own pin toggle (`SubjectChip`'s
    // `data-cmd="rail.pin"` button).
    "rail.pin": () => setRailPinned((v) => !v),
    // `Space R {a,u,h,n,v}` — the five rail stripe buttons, ported via
    // `selectRailTab`'s own doc above.
    "rail.tab.all": () => selectRailTab("all"),
    "rail.tab.understand": () => selectRailTab("understand"),
    "rail.tab.history": () => selectRailTab("history"),
    "rail.tab.notes": () => selectRailTab("notes"),
    "rail.tab.comments": () => selectRailTab("comments"),
    "rail.tab.review": () => selectRailTab("review"),
    // V72-J2 (D8) — comments/1: the gutter mode cycle, buffer navigation
    // (`]m`/`[m`, deliberately no `vim_kind` — see the registry row's own
    // note), and the claim → annotation bridge's keyboard doors. None of
    // these need a vim-layer arm: `Space`-leader chords are never vim-owned
    // (D2: "Space is only the leader"), and `]m`/`[m` follow the `]u`/`[u`/
    // `]d`/`[d`/`]s`/`[s`/`]p`/`[p` mixed-prefix precedent exactly.
    "comments.gutter-mode-cycle": () => cycleCommentGutterMode(),
    "comments.next": () => handleCommentNav(1),
    "comments.prev": () => handleCommentNav(-1),
    "comments.track-as-annotation": () => handleCommentTrackAsAnnotation(),
    "comments.open-card": () => handleCommentOpenCard(),
    // V70-H1 — the registry declares all five `Ctrl-w` pane commands in
    // scope `reader` (`pane.focus-prev/-next/-cycle`, `pane.split`,
    // `pane.close`), but NONE had a registered central handler here.
    // `CommandRoot.onKey`'s "matched" branch is explicit about the
    // consequence of that gap: a chord that resolves to a real command
    // with no owning handler is swallowed (`if (!handler) return; //
    // nothing owns it here`) — `Ctrl-w` itself is consumed as a pending
    // prefix regardless (it isn't a bare token, so the read-only-buffer
    // carve-out never applies to it), so the SECOND key of the chord
    // never even reaches the vim reducer's own `onPaneFocus`/
    // `onSplitSelf`/`onClosePane` path. `focusedPane` is the same "which
    // pane is this for" argument each pane's own CM6 vim instance already
    // hardcodes via `vimCallbacksForPane(pane)` — reading it here instead
    // is the one adaptation a single GLOBAL handler map needs.
    "pane.focus-prev": () => handlePaneFocus(focusedPane, "prev"),
    "pane.focus-next": () => handlePaneFocus(focusedPane, "next"),
    "pane.focus-cycle": () => handlePaneFocus(focusedPane, focusedPane === 1 ? "next" : "prev"),
    "pane.split": () => handleSplitSelf(focusedPane),
    "pane.close": () => handleClosePane(focusedPane),
    "annotate.line": () => {
      if (!(isFile && !diffMode && !storyMode)) return;
      setAnnotationActiveLine(cursorLineRef1.current);
      setAnnotationActiveLineEnd(null);
      setAnnotationInitialIntent(undefined);
      inspectorRef.current?.openTab("annotations");
    },
    "help.keys": () => setHelpOpen((v) => !v),
    "dismiss.help": () => setHelpOpen(false),
    // V71-E2 — the action menu's keyboard door. `.` is a BARE key with no
    // `vim_kind`, so `CommandRoot`'s guard 2 lets it through even with the
    // buffer focused (the V70-K1 predicate, asked of the registry rather
    // than re-derived here); `Shift-F10` and `ContextMenu` are the platform
    // invocations MDN/APG make the author's own responsibility. All three
    // land on this ONE handler, anchored at the caret.
    "action.panel": () => {
      const c = paneRepoPath(focusedPane).viewRef.current?.cursorCoords();
      openActionMenu({ x: c?.left ?? 200, y: c?.bottom ?? 200 }, false);
    },
    // V71-E2 — walk the active usages set from anywhere the reader is up.
    "usages.next": () => stepUsages(1),
    "usages.prev": () => stepUsages(-1),
    // V74-L2 — `Space b a`: add what is under the caret to a board, without
    // going through the menu first. It asks the SAME `/api/actions` the panel
    // asks and takes the default target (index 0, "the one the gesture
    // implies" — `actions.rs`), so the leader and the menu can never disagree
    // about what "this" means.
    "boards.add": () => void addCaretToBoard(),
  });

  // F5 — Esc closes the mobile reader-tools sheet (mirrors `MobileDrawer`'s
  // own Esc handling for the tree drawer). Skips typing targets + anything
  // inside a CM6 buffer (vim's own Escape — e.g. leaving visual mode — must
  // never ALSO close the sheet) and modifier chords; only mounts while the
  // sheet is actually open, same "one small effect scoped to `open`" idiom
  // kb's own `detail.tsx` uses for its equivalent reader-tools sheet.
  useEffect(() => {
    if (!inspectorOpen) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target) || isInsideBuffer(e.target)) return;
      e.preventDefault();
      setInspectorOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inspectorOpen]);


  // ── V71-E2 — the three POINTER doors to the action menu ───────────────
  //
  // ONE delegated listener at the document, scoped to `.kbc-codeview` —
  // never a per-component `contextmenu` handler (risk 8: the CM6 tooltip
  // menu and a diff-row menu both claiming one nested event). Three rules
  // the research makes non-negotiable:
  //
  //  * **Shift+right-click passes through.** Firefox fires no `contextmenu`
  //    at all with Shift held, so this is really the Chromium half; the
  //    menu's own last row advertises it either way, because an
  //    undiscoverable escape hatch is not an escape hatch.
  //  * **Never override on prose or links** — the override is scoped to the
  //    code surface, so the browser menu (extensions, translate,
  //    spell-check, open-in-new-tab) survives everywhere else.
  //  * **Long-press is our OWN pointer timer.** iOS stopped firing
  //    `contextmenu` reliably after 13.1 and `-webkit-touch-callout: none`
  //    is reported broken on 26.1, so the mobile path never relies on
  //    either: it times a 500 ms press, cancels on >10 px of movement, and
  //    opens the SINGLE bottom sheet designed to coexist with the OS
  //    bubble rather than to suppress it (root CLAUDE.md #30's one-mobile-
  //    entry rule).
  useEffect(() => {
    function onContextMenu(e: MouseEvent) {
      if (e.shiftKey) return; // the browser's, and the menu says so
      if (!isInsideBuffer(e.target)) return;
      e.preventDefault();
      openActionMenu({ x: e.clientX, y: e.clientY }, false);
    }
    let pressTimer: number | undefined;
    let pressAt: { x: number; y: number } | null = null;
    function clearPress() {
      if (pressTimer !== undefined) window.clearTimeout(pressTimer);
      pressTimer = undefined;
      pressAt = null;
    }
    function onPointerDown(e: PointerEvent) {
      if (e.pointerType !== "touch" || !isInsideBuffer(e.target)) return;
      pressAt = { x: e.clientX, y: e.clientY };
      pressTimer = window.setTimeout(() => {
        openActionMenu({ x: e.clientX, y: e.clientY }, false);
        clearPress();
      }, 500);
    }
    function onPointerMove(e: PointerEvent) {
      if (!pressAt) return;
      if (Math.abs(e.clientX - pressAt.x) > 10 || Math.abs(e.clientY - pressAt.y) > 10) clearPress();
    }
    document.addEventListener("contextmenu", onContextMenu);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", clearPress);
    document.addEventListener("pointercancel", clearPress);
    return () => {
      clearPress();
      document.removeEventListener("contextmenu", onContextMenu);
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("pointermove", onPointerMove);
      document.removeEventListener("pointerup", clearPress);
      document.removeEventListener("pointercancel", clearPress);
    };
    // `openActionMenu` closes over the focused pane and the open files; the
    // listener is re-installed when either changes rather than reading a
    // stale closure — the same discipline `CommandRoot`'s refs solve
    // differently for its own once-installed window listener.
  }, [focusedPane, path, pane2Loc?.path, repo, gitRef, file.data?.blob_hash]);

  // --- V70-A4: the Desk's own reader-side wiring -------------------------

  /// One verb for "show me the files", whichever shell we are in. On
  /// desktop the dock is a Desk REGION (collapse state lives in the
  /// persisted desk); on mobile it is the pre-existing overlay
  /// `MobileDrawer`. Two mechanisms, one intent — so `b` and `/` never
  /// have to branch at their call sites.
  function showDock() {
    if (isMobile) setTreeVisible(true);
    else desk.dispatch({ type: "expand", region: "dock", user: true });
  }
  function toggleDock() {
    if (isMobile) setTreeVisible((v) => !v);
    else desk.toggleRegion("dock");
  }

  /// V71-K3 — the same "only if collapsed" gate every mouse door onto the
  /// drawer already takes (`Drawer.tsx`'s tab click and toggle button, and
  /// `Desk.tsx`'s stripe button): `expand` is a no-op on an already-open
  /// region EXCEPT that it still marks the desk `dirty` when `user: true`
  /// (`deskState.ts`'s `withDirty`), so dispatching it unconditionally
  /// would spuriously flip "· edited" on a desk nothing actually changed.
  function expandDrawerIfCollapsed() {
    if (effectiveCollapsed(desk.state, "drawer", desk.chrome, desk.zoom)) {
      desk.dispatch({ type: "expand", region: "drawer", user: true });
    }
  }

  /// V71-K3 — `Space {1-9}`'s handler: the SAME gesture a drawer-tab click
  /// makes (`Drawer.tsx`: `dispatch({type:"activate",...}); if (collapsed)
  /// onExpand();`), addressed by ordinal instead of by id. `n` is 1-based
  /// (as typed: `Space 1` is the first tab) against `drawerTabOrder`'s own
  /// order (live tabs first, then evicted — see that function's doc); an
  /// ordinal with nothing there is an HONEST no-op, per the ruling: no
  /// toast, no error, no wrap to the other end.
  function selectDrawerTabOrdinal(n: number) {
    const target = drawerTabOrder(desk.drawer)[n - 1];
    if (!target) return;
    desk.drawerDispatch({ type: "activate", id: target.id });
    expandDrawerIfCollapsed();
  }

  /// V71-K3 — the rail stripe buttons' own `onClick` (`Desk.tsx`'s
  /// `rightButtons`), ported here verbatim: set the tab, then either open
  /// the mobile sheet or expand the (possibly stripe-only) rail region.
  /// `hasInspector` stands in for that button's `railCollapsed` check's
  /// `!railAvailable` half — a rail with no subject renders nothing
  /// either way, so expanding it here is the same harmless no-op the
  /// button already performs in that state.
  function selectRailTab(tab: RailTab) {
    desk.setRailTab(tab);
    if (isMobile) setInspectorOpen(true);
    else if (!hasInspector || effectiveCollapsed(desk.state, "rail", desk.chrome, desk.zoom)) {
      desk.dispatch({ type: "expand", region: "rail", user: true });
    }
  }

  /// The rail's pin (`data-cmd="rail.pin"`). Transient by design: a
  /// pinned rail is a thing you are doing right now, and a reload that
  /// silently kept the rail frozen on yesterday's symbol would look
  /// broken rather than remembered.
  const [railPinned, setRailPinned] = useState(false);

  /// V70-A4 — one result set, kept. `gr`'s reference list lives in a
  /// popup that Esc destroys; "Keep in drawer" turns the SAME rows into
  /// a drawer tab that stays open beside the code. No new server call:
  /// the rows are the ones already on screen.
  function handleKeepPeekInDrawer() {
    const rows: DrawerRow[] = peek.rows.map((r) => ({
      repo: r.repo,
      path: r.path,
      line: r.line,
      text: r.text,
      trustClass: r.trustClass,
      approximate: r.approximate,
    }));
    if (rows.length === 0) return;
    const modeLabel = peek.mode === "refs" ? "references" : "definitions";
    desk.keepInDrawer({
      id: drawerSetId(peek.mode === "refs" ? "usages" : "definitions", `${peek.word}@${repo}`),
      title: `${peek.word} · ${modeLabel}`,
      kind: peek.mode === "refs" ? "usages" : "definitions",
      rows,
    });
    dispatchPeek({ type: "CLOSE" });
  }

  /// Enter opens a drawer row where a file open goes — through the same
  /// `openPath`, so the placement table governs it too.
  function handleOpenDrawerRow(row: DrawerRow) {
    if (row.repo !== repo) {
      navigate(codeUrl({ repo: row.repo, path: row.path, line: row.line }));
      return;
    }
    navigate(buildReaderUrl({ pane1: { path: row.path, ref: gitRef, line: row.line } }));
    setFocusedPane(1);
  }

  function handleSelect(selectedPath: string, kind: EntryKind, target?: "focused" | "pane2") {
    if (kind !== "file") return;
    openPath(selectedPath, target ?? "focused");
    // F5 — on mobile the tree is an overlay drawer; picking a file should
    // return the operator straight to reading it, not leave the drawer
    // covering the screen over the file they just opened.
    if (isMobile) setTreeVisible(false);
  }

  const focusedFileData = focusedPane === 1 ? file.data : pane2File.data;
  const focusedSymbols = focusedFileData && focusedFileData.encoding === "utf8" ? focusedFileData.symbols : [];

  /// V72-G1.2 — the entity ADDRESS under the cursor for `Space e d`.
  ///
  /// It sends a NAME, not a constructed FQN. `extract.rs`'s `container` names
  /// only the NEAREST lexical ancestor (crate invariant 13 says so
  /// explicitly — reconstructing a full constant path is the ENTITY INDEX's
  /// job, over range containment, and is exactly what this client cannot do),
  /// so gluing `container::name` together here would mint an address this
  /// side cannot stand behind. `entity/1` resolves a bare last segment
  /// itself, and answers an ambiguous one with `candidates` rather than
  /// picking — which is the honest division of labour: the client says which
  /// WORD, the daemon says which entity.
  const entityAddressUnderCursor = useMemo(() => {
    const line = cursorLineUi;
    if (line == null || line < 1) return null;
    let innermost: (typeof focusedSymbols)[number] | null = null;
    for (const sym of focusedSymbols) {
      if (line < sym.line_start || line > sym.line_end) continue;
      if (!innermost || sym.line_end - sym.line_start < innermost.line_end - innermost.line_start) {
        innermost = sym;
      }
    }
    if (!innermost) return null;
    // A class/module the cursor sits in IS the address. Anything else (a
    // method, a constant) addresses its own container, which for Ruby is the
    // enclosing class/module name — the same one segment, still a name.
    if (innermost.kind === "class" || innermost.kind === "module") return innermost.name;
    return innermost.container ?? null;
  }, [focusedSymbols, cursorLineUi]);
  // F5 — mobile-only reader-tools sheet is available whenever the outline
  // aside itself would render (see the aside's own condition below); kept as
  // a plain boolean so both the header's entry button and the aside's render
  // condition read the identical gate.
  const hasInspector = !diffMode && !storyMode && !!file.data && file.data.encoding === "utf8";
  // F5 — CSS-only sheet promotion (mirrors kb's own `.detail--inspector-open`
  // / `.kb-pinsp`, root CLAUDE.md invariant #30): gated on `isMobile` so a
  // stray `inspectorOpen=true` left over from a mobile session never adds
  // this modifier after a resize back to desktop — desktop DOM/CSS stays
  // untouched regardless of this flag's value.
  const readerRootClass = "kbc-reader" + (isMobile && inspectorOpen ? " kbc-reader--sheet-open" : "");

  // V70-A4 — what the rail is ABOUT: the caret's narrowest resolvable
  // subject (symbol ▷ line ▷ file), debounced, frozen while pinned. See
  // `desk/railSubject.ts` for the cascade and `hooks/useRailSubject.ts`
  // for why only the LINE is debounced.
  const rail = useRailSubject(
    {
      path: focusedPath ?? null,
      line: cursorLineUi,
      symbols: focusedSymbols,
      symbolsLoaded: !!focusedFileData,
    },
    { pinned: railPinned },
  );

  const readerHeader = (
    <>
      <header className="kbc-reader__top">
        {/* F5 — mobile-only hamburger (CSS-hidden ≥861px): promotes the tree
            aside to an overlay `MobileDrawer` below. Reuses `Icon.List`'s
            3-line glyph, which already reads as a hamburger — see
            `icons.tsx`'s own doc on why no separate Menu icon was added. */}
        <button
          type="button"
          className="kbc-burger"
          onClick={toggleDock}
          aria-label={treeVisible ? "close file tree" : "open file tree"}
          aria-expanded={treeVisible}
          data-kbc-burger
        >
          <Icon.List />
        </button>
        <Breadcrumbs
          repo={repo}
          path={path}
          gitRef={gitRef}
          symbols={focusedSymbols}
          cursorLine={cursorLineUi}
          onJumpSymbol={(line) => jumpToLine(focusedPane, line)}
        />
        {/* F1 — the search launcher moved to the global TopBar (app.tsx),
            mounted once above every route; this header no longer duplicates
            it. */}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (stickyEnabled ? " is-on" : "")}
            aria-pressed={stickyEnabled}
            title="Sticky context lines"
            data-kbc-sticky-toggle
            onClick={() => {
              setStickyEnabled((v) => {
                const next = !v;
                saveStickyContext(next);
                return next;
              });
            }}
          >
            Sticky
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (paramHintsEnabled ? " is-on" : "")}
            aria-pressed={paramHintsEnabled}
            title="Param-name inlay hints at call sites"
            data-kbc-param-hints-toggle
            onClick={() => {
              setParamHintsEnabled((v) => {
                const next = !v;
                saveParamHints(next);
                return next;
              });
            }}
          >
            Params
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (codeLensesEnabled ? " is-on" : "")}
            aria-pressed={codeLensesEnabled}
            title="Code Vision lens chips above declarations"
            data-kbc-lenses-toggle
            onClick={() => {
              setCodeLensesEnabled((v) => {
                const next = !v;
                saveCodeLenses(next);
                return next;
              });
            }}
          >
            Lenses
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (wrapEnabled ? " is-on" : "")}
            aria-pressed={wrapEnabled}
            title="Wrap long lines"
            data-kbc-wrap-toggle
            onClick={() => {
              setWrapEnabled((v) => {
                const next = !v;
                saveWrap(next);
                return next;
              });
            }}
          >
            Wrap
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <div className="kbc-fontsize-stepper" role="group" aria-label="Reader font size" data-kbc-fontsize-stepper>
            <button
              type="button"
              className="kbc-fontsize-stepper__btn"
              aria-label="Decrease reader font size"
              title="Smaller text"
              data-kbc-fontsize-dec
              disabled={readerFontSize <= READER_FONT_SIZE_MIN}
              onClick={() => setReaderFontSize((cur) => saveReaderFontSize(cur - 1))}
            >
              A−
            </button>
            <button
              type="button"
              className="kbc-fontsize-stepper__btn"
              aria-label="Increase reader font size"
              title="Larger text"
              data-kbc-fontsize-inc
              disabled={readerFontSize >= READER_FONT_SIZE_MAX}
              onClick={() => setReaderFontSize((cur) => saveReaderFontSize(cur + 1))}
            >
              A+
            </button>
          </div>
        )}
        {isFile && !diffMode && !storyMode && (
          <div className="kbc-provenance-toggle" role="group" aria-label="Provenance overlay">
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "off" ? " is-active" : "")}
              aria-pressed={provenanceMode === "off"}
              onClick={() => setProvenanceMode("off")}
              data-kbc-provenance-mode="off"
            >
              Off
            </button>
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "dots" ? " is-active" : "")}
              aria-pressed={provenanceMode === "dots"}
              onClick={() => setProvenanceMode((m) => (m === "dots" ? "off" : "dots"))}
              data-kbc-provenance-toggle
              data-kbc-provenance-mode="dots"
              title="Show blame provenance dots in the gutter"
            >
              Dots
            </button>
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "age" ? " is-active" : "")}
              aria-pressed={provenanceMode === "age"}
              onClick={() => setProvenanceMode((m) => (m === "age" ? "off" : "age"))}
              data-kbc-provenance-mode="age"
              title="Tint lines by author age"
            >
              Age
            </button>
          </div>
        )}
        {/* V72-J2 (D8) — the comments/1 gutter's mode chip (`Space C c`
            cycles it). A mode NEVER hides a comment class silently — this
            chip is the on-screen indicator naming which of the three
            filters is currently active, always visible whenever a file is
            open (same gate as the toggles above it). */}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className="kbc-sticky-toggle"
            title="Comment gutter mode (all / quiet / doc-only) — Space C c cycles it"
            data-kbc-comment-gutter-mode={commentGutterMode}
            onClick={cycleCommentGutterMode}
          >
            Comments: {commentGutterMode}
          </button>
        )}
        <RefPicker repo={repo} path={path} activeRef={gitRef} pane2={pane2Loc ?? undefined} />
        {/* Phase E4 — capture the FOCUSED pane's open file (or its current
            selection, when one exists) into a reading set. */}
        {focusedPath !== undefined && !diffMode && !storyMode && (
          <AddToSetMenu repo={repo} path={focusedPath} getSelection={currentPaneSelection} />
        )}
        {/* F5 — mobile-only "reader tools" sheet entry button (CSS-hidden
            ≥861px). Badged with the unresolved-annotation count so an
            operator knows there's something to look at before opening it. */}
        {hasInspector && (
          <button
            type="button"
            className="kbc-inspector-toggle"
            onClick={() => setInspectorOpen((v) => !v)}
            aria-label="reader tools"
            aria-controls="kbc-reader-sheet"
            aria-expanded={inspectorOpen}
            data-kbc-inspector-toggle
          >
            <Icon.Panel />
            {unresolvedAnnotationsCount > 0 && (
              <span className="kbc-inspector-toggle__badge" data-kbc-inspector-toggle-badge>
                {unresolvedAnnotationsCount}
              </span>
            )}
          </button>
        )}
      </header>
      <WorkingSetStrip
        entries={workingSet.entries}
        pane1Path={activeFile}
        pane2Path={pane2Loc?.path}
        onOpen={openPath}
        onPin={workingSet.pin}
        onUnpin={workingSet.unpin}
        onRemove={workingSet.remove}
        activeWorkspace={activeWorkspaceChip}
        onUpdateWorkspace={activeWorkspace.id ? () => void updateActiveWorkspace() : undefined}
        onSaveWorkspaceAs={() => setSaveWorkspaceOpen(true)}
      />
      <RepoStateBanner repo={repo} />
      {liveMirror1.headMoved && (
        <HeadMovedBanner newRef={liveMirror1.headMoved.new} onRefreshTree={refreshTree} onDismiss={liveMirror1.dismissHeadMoved} />
      )}
    </>
  );

  // V70-A4 — the dock's body. ONE `FileTree` instance either way (the
  // Desk hoists it into the existing `MobileDrawer` on mobile), so
  // `treeRef` still never has to pick between two live copies.
  const dockBody = (
    <FileTree
      ref={treeRef}
      repo={repo}
      gitRef={gitRef}
      selectedPath={path}
      onSelect={handleSelect}
      onRamp={(rung, p) => ramp.activate(rung, { repo, path: p, via: "tree" })}
      // V72-G1.2 — in dossier mode the tree narrows to the entity's own files
      // and its namespace children, through kbc-scope/1's EXISTING `ns:` atom
      // (`SCOPE_ATOM_SPECS`, resolved over the V71-G0 entity index). A derived
      // scope, not a second tree: same wire, same projection, same honesty
      // strip — and the daemon still answers `scope_applied: false` with the
      // unscoped tree if it cannot resolve the entity, which is a caption
      // rather than a wrong answer.
      derivedScope={
        dossierMode && entParam
          ? {
              scope: `ns:${entParam}`,
              label: entParam,
              onClear: leaveDossier,
            }
          : null
      }
    />
  );

  const readerBody = (
    <>
        <main
          className="kbc-reader__main"
          id="main"
          data-region="panes"
          onMouseDown={() => desk.setFocusRegion("main")}
        >
          {diffMode ? (
            fromRef ? (
              <DiffView repo={repo} path={path} from={fromRef} to={toRef} />
            ) : (
              <div className="kbc-reader__hint">Missing ?from= for the diff view</div>
            )
          ) : storyMode ? (
            isFile ? (
              <StoryPlayer
                repo={repo}
                path={path}
                atSha={atRef}
                onExit={(sha) => navigate(codeUrl({ repo, path, ref: sha }))}
              />
            ) : (
              <div className="kbc-reader__hint">
                Story mode needs a file — open one from the tree, then use the History tab's Story button.
              </div>
            )
          ) : historySentinelMode ? (
            <div className="kbc-reader__hint">
              This link is missing required parameters — use the History tab, or the Compare/Branches pages, to get
              here.
            </div>
          ) : (
            <div className="kbc-reader__panes">
              <div
                className={"kbc-reader__pane" + (pane2Loc && focusedPane === 1 ? " is-focused" : "")}
                onFocus={() => setFocusedPane(1)}
                data-region="pane-1"
                // V70-A6 — mouse buttons 4/5 walk the history of the pane
                // UNDER THE POINTER, not the focused one (Pane Relief's rule).
                onMouseDown={(e) => {
                  if (e.button === 3) {
                    e.preventDefault();
                    handleJumpBack(1);
                  } else if (e.button === 4) {
                    e.preventDefault();
                    handleJumpForward(1);
                  }
                }}
              >
                <PaneChrome
                  pane={1}
                  provisional={isProvisional(provisional, 1)}
                  onBack={() => handleJumpBack(1)}
                  onForward={() => handleJumpForward(1)}
                  onPin={() => setProvisional((st) => pinPane(st, 1))}
                />
                {liveMirror1.fileChangedOnDisk && (
                  <FileChangedToast
                    paneLabel={fileChangedPaneLabel(1, !!pane2Loc)}
                    onRefresh={() => {
                      void file.refetch();
                      liveMirror1.dismissFileChanged();
                    }}
                    onDismiss={liveMirror1.dismissFileChanged}
                  />
                )}
                {file.isLoading ? (
                  <div className="kbc-reader__hint">Loading…</div>
                ) : file.error ? (
                  <div className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</div>
                ) : file.data ? (
                  file.data.encoding === "base64" ? (
                    <div className="kbc-reader__hint">Binary file — preview not supported</div>
                  ) : (
                    <CodeView
                      ref={codeViewRef1}
                      content={file.data.content}
                      spans={file.data.highlights}
                      blobHash={file.data.blob_hash}
                      gotoSel={gotoSel1}
                      vim={vimCallbacksForPane(1)}
                      onSelectionLines={(sel) => {
                        lastSelRef1.current = sel;
                        if (focusedPane === 1) cursorSyncRef1.current?.onSelection(sel);
                      }}
                      onVimStatus={setVimStat}
                      blameDots={focusedPane === 1 ? blameDots : null}
                      onBlameHover={focusedPane === 1 ? handleBlameHover : undefined}
                      onBlameUnhover={focusedPane === 1 ? handleBlameUnhover : undefined}
                      onBlameClick={focusedPane === 1 ? handleBlameClick : undefined}
                      ageLines={focusedPane === 1 ? ageLines : null}
                      annotationMarkers={focusedPane === 1 ? annotationMarkers : null}
                      onAnnotationClick={focusedPane === 1 ? handleAnnotationClick : undefined}
                      diagnosticMarkers={focusedPane === 1 ? diagMarks : null}
                      commentMarkers={focusedPane === 1 ? commentMarkers : null}
                      onCommentHover={focusedPane === 1 ? handleCommentHover : undefined}
                      onCommentUnhover={focusedPane === 1 ? handleCommentUnhover : undefined}
                      onCommentClick={focusedPane === 1 ? handleCommentClick : undefined}
                      onViewerDirtyChange={(dirty) => {
                        viewerDirtyRef1.current = dirty;
                      }}
                      onCursorLineChange={(line) => {
                        cursorLineRef1.current = line;
                        if (focusedPane === 1) setCursorLineUi(line);
                        noteProvisionalInteraction(1);
                      }}
                      symbols={file.data.symbols}
                      stickyContextEnabled={stickyEnabled}
                      onStickyJump={(line) => jumpToLine(1, line)}
                      paramHintsRepo={repo}
                      paramHintsPath={activeFile}
                      paramHintsEnabled={paramHintsEnabled}
                      lensDeclarations={lenses1.data?.declarations ?? null}
                      codeLensesEnabled={codeLensesEnabled}
                      onLensUsages={(d) => handleLensUsages(1, d)}
                      onLensImpls={(d) => handleLensImpls(1, d)}
                      onLensAuthor={(d) => handleLensAuthor(1, d)}
                      linkify={linkifyCallbacksForPane(1)}
                      conflictActive={!!activeFile && conflictedPaths.has(activeFile)}
                      wrap={wrapEnabled}
                      fontSize={readerFontSize}
                      inlinePeek={inlinePeekHandlers}
                      hoverRepo={repo}
                      hoverPath={activeFile ?? null}
                      hoverRef={gitRef}
                      docComments={focusedPane === 1 ? allFileComments : null}
                    />
                  )
                ) : (
                  <div className="kbc-reader-start">
                    <EmptyState
                      icon={<Icon.List />}
                      title="Select a file from the tree"
                      hint="Or press / to filter it."
                      action={{
                        label: "Search everywhere (⌘K)",
                        onClick: () => window.dispatchEvent(new CustomEvent("kbc:omnibox.open")),
                      }}
                    />
                    <ReaderStartCards repo={repo} onOpen={(p) => openPath(p, "focused")} />
                  </div>
                )}
              </div>
              {pane2Loc && (
                <>
                  <div className="kbc-reader__pane-rule" />
                  <div
                    className={"kbc-reader__pane" + (focusedPane === 2 ? " is-focused" : "")}
                    onFocus={() => setFocusedPane(2)}
                    data-region="pane-2"
                    onMouseDown={(e) => {
                      if (e.button === 3) {
                        e.preventDefault();
                        handleJumpBack(2);
                      } else if (e.button === 4) {
                        e.preventDefault();
                        handleJumpForward(2);
                      }
                    }}
                  >
                    <PaneChrome
                      pane={2}
                      provisional={isProvisional(provisional, 2)}
                      onBack={() => handleJumpBack(2)}
                      onForward={() => handleJumpForward(2)}
                      onPin={() => setProvisional((st) => pinPane(st, 2))}
                    />
                    {liveMirror2.fileChangedOnDisk && (
                      <FileChangedToast
                        paneLabel={fileChangedPaneLabel(2, true)}
                        onRefresh={() => {
                          void pane2File.refetch();
                          liveMirror2.dismissFileChanged();
                        }}
                        onDismiss={liveMirror2.dismissFileChanged}
                      />
                    )}
                    {pane2File.isLoading ? (
                      <div className="kbc-reader__hint">Loading…</div>
                    ) : pane2File.error ? (
                      <div className="kbc-reader__hint kbc-reader__hint--error">
                        {(pane2File.error as Error).message}
                      </div>
                    ) : pane2File.data ? (
                      pane2File.data.encoding === "base64" ? (
                        <div className="kbc-reader__hint">Binary file — preview not supported</div>
                      ) : (
                        <CodeView
                          ref={codeViewRef2}
                          content={pane2File.data.content}
                          spans={pane2File.data.highlights}
                          blobHash={pane2File.data.blob_hash}
                          gotoSel={gotoSel2}
                          vim={vimCallbacksForPane(2)}
                          onSelectionLines={(sel) => {
                            lastSelRef2.current = sel;
                            if (focusedPane === 2) cursorSyncRef2.current?.onSelection(sel);
                          }}
                          onVimStatus={setVimStat}
                          blameDots={focusedPane === 2 ? blameDots : null}
                          onBlameHover={focusedPane === 2 ? handleBlameHover : undefined}
                          onBlameUnhover={focusedPane === 2 ? handleBlameUnhover : undefined}
                          onBlameClick={focusedPane === 2 ? handleBlameClick : undefined}
                          ageLines={focusedPane === 2 ? ageLines : null}
                          annotationMarkers={focusedPane === 2 ? annotationMarkers : null}
                          onAnnotationClick={focusedPane === 2 ? handleAnnotationClick : undefined}
                          diagnosticMarkers={focusedPane === 2 ? diagMarks : null}
                          commentMarkers={focusedPane === 2 ? commentMarkers : null}
                          onCommentHover={focusedPane === 2 ? handleCommentHover : undefined}
                          onCommentUnhover={focusedPane === 2 ? handleCommentUnhover : undefined}
                          onCommentClick={focusedPane === 2 ? handleCommentClick : undefined}
                          onViewerDirtyChange={(dirty) => {
                            viewerDirtyRef2.current = dirty;
                          }}
                          onCursorLineChange={(line) => {
                            cursorLineRef2.current = line;
                            if (focusedPane === 2) setCursorLineUi(line);
                            noteProvisionalInteraction(2);
                          }}
                          symbols={pane2File.data.symbols}
                          stickyContextEnabled={stickyEnabled}
                          onStickyJump={(line) => jumpToLine(2, line)}
                          paramHintsRepo={repo}
                          paramHintsPath={pane2Loc?.path ?? null}
                          paramHintsEnabled={paramHintsEnabled}
                          lensDeclarations={lenses2.data?.declarations ?? null}
                          codeLensesEnabled={codeLensesEnabled}
                          onLensUsages={(d) => handleLensUsages(2, d)}
                          onLensImpls={(d) => handleLensImpls(2, d)}
                          onLensAuthor={(d) => handleLensAuthor(2, d)}
                          linkify={linkifyCallbacksForPane(2)}
                          conflictActive={!!pane2Loc?.path && conflictedPaths.has(pane2Loc.path)}
                          wrap={wrapEnabled}
                          fontSize={readerFontSize}
                          inlinePeek={inlinePeekHandlers}
                          hoverRepo={repo}
                          hoverPath={pane2Loc?.path ?? null}
                          hoverRef={pane2Loc?.ref}
                          docComments={focusedPane === 2 ? allFileComments : null}
                        />
                      )
                    ) : null}
                  </div>
                </>
              )}
            </div>
          )}
          {peek.open && (
            <PeekPanel
              state={peek}
              currentRepo={repo}
              anchor={peekAnchor}
              onMove={(delta) => dispatchPeek({ type: "MOVE", delta })}
              onActivate={handlePeekActivate}
              onClose={handlePeekClose}
              onRamp={handlePeekRamp}
              scentFor={peekRowTarget}
              visitsFor={(row) => ramp.visits(peekRowTarget(row))}
              // T1 — the hover card's "usages"/"callers" footer hints
              // (design-ui.md §9.1) reissue gr/gc against the position `K`
              // was pressed at; absent until a hover has actually happened.
              onFindRefs={
                lastHoverPosRef.current
                  ? () => void handleFindRefs(lastHoverPosRef.current!.pane, lastHoverPosRef.current!.pos)
                  : undefined
              }
              onFindCallers={
                lastHoverPosRef.current
                  ? () => void handleHierarchyCallers(lastHoverPosRef.current!.pane, lastHoverPosRef.current!.pos)
                  : undefined
              }
              // V72-G1.2 — "Open dossier", offered only when the hover card's
              // own candidate says this is a CLASS or MODULE. The gate uses
              // the `kind` the symbols table already sent; it never sniffs
              // the identifier's shape, and it never resolves ahead of the
              // click (D5: a row that cannot prove `exact` must not jump —
              // so this one does not jump at all, it navigates on an
              // explicit press and lets `entity/1` answer honestly).
              onOpenDossier={
                peek.card &&
                (peek.card.candidate.kind === "class" || peek.card.candidate.kind === "module")
                  ? () => openEntityDossier(peek.card!.ident)
                  : undefined
              }
              // V70-A4 — the drawer's ONE tenant: turn this row set into a
              // drawer tab that outlives the popup (`desk/Drawer.tsx`).
              onKeepInDrawer={handleKeepPeekInDrawer}
            />
          )}
          {hier.open && (
            <HierarchyPanel
              state={hier}
              currentRepo={repo}
              anchor={hierAnchor}
              onMove={(delta) => dispatchHier({ type: "MOVE", delta })}
              onActivate={handleHierActivate}
              onToggleExpand={(node) => void handleHierToggleExpand(node)}
              onClose={handleHierClose}
              onRamp={(rung, node) =>
                node.path &&
                ramp.activate(rung, {
                  repo,
                  path: node.path,
                  line: node.line,
                  via: node.dir === "callers" ? "caller_of" : "definition_of",
                  subject: node.name,
                  ...(node.class ? { trust: node.class } : {}),
                })
              }
            />
          )}
          {impact.open && (
            <ImpactPanel
              state={impact}
              currentRepo={repo}
              anchor={impactAnchor}
              onMove={(delta) => dispatchImpact({ type: "MOVE", delta })}
              onActivate={handleImpactActivate}
              onToggleBucket={(bucket) => dispatchImpact({ type: "TOGGLE_BUCKET", bucket })}
              onClose={handleImpactClose}
              onRamp={(rung, row) =>
                ramp.activate(rung, {
                  repo,
                  path: row.path,
                  line: row.line > 0 ? row.line : undefined,
                  via: "usage_of",
                  subject: row.name ?? undefined,
                  trust: row.class,
                })
              }
            />
          )}
          {ego?.open && (
            <EgoGraph
              layout={
                ego.layout ?? {
                  nodes: [],
                  edges: [],
                  truncated: 0,
                  width: 200,
                  height: 100,
                }
              }
              title={ego.title}
              loading={ego.loading}
              error={ego.error}
              onActivate={handleEgoActivate}
              onRecenter={(n) => void handleEgoRecenter(n)}
              onClose={handleEgoClose}
            />
          )}
          {hoverChip && (
            <BlameChip rect={hoverChip.rect} label={hoverChip.label} solid={hoverChip.solid} />
          )}
          {commentHoverChip && commentHoverComment && (
            <CommentGutterCard
              rect={commentHoverChip.rect}
              comment={commentHoverComment}
              onJumpSymbol={
                commentHoverComment.symbol
                  ? () => jumpToLine(focusedPane, commentHoverComment.symbol!.line_start)
                  : undefined
              }
            />
          )}
          {copiedTick > 0 && (
            <div className="kbc-copied-chip" role="status" data-kbc-copied>
              Permalink copied
            </div>
          )}
          {(vimStat.mode !== "normal" || vimStat.pending !== "") && (
            <div className="kbc-vim-status" aria-live="polite" data-kbc-vim-status>
              {vimStat.mode === "visual-line" ? "V-LINE" : vimStat.mode === "visual" ? "VISUAL" : ""}
              {vimStat.pending !== "" && <span className="kbc-vim-status__pending">{vimStat.pending}</span>}
            </div>
          )}
        </main>
    </>
  );

  // V70-A4 — the rail is a SLOT the Desk fills: the shell owns the
  // region (its width, its collapse, its stripe, its mobile-sheet
  // promotion) and hands back the two things only it knows — whether
  // this render is the phone's bottom sheet, and which tab is active.
  const railSlot = (ctx: DeskRailSlotCtx) => (
            <InspectorRail
              ref={inspectorRef}
              tab={ctx.tab}
              onTabChange={ctx.setTab}
              subject={rail.subject}
              caretSubject={rail.caretSubject}
              pinned={railPinned}
              onTogglePin={() => setRailPinned((v) => !v)}
              hasReviewContext={false}
              symbols={focusedSymbols}
              onJumpOutline={(line) => jumpToLine(focusedPane, line)}
              whyPanel={whyPanel}
              historyPanel={historyPanel}
              citedBy={citedBy}
              frameworkCard={frameworkCard}
              diagnosticsCard={diagnosticsCard}
              repo={repo}
              path={focusedPath ?? ""}
              annotationActiveLine={annotationActiveLine}
              annotationActiveLineEnd={annotationActiveLineEnd}
              annotationInitialIntent={annotationInitialIntent}
              bookmarkCount={bookmarkCount}
              onJumpBookmark={handleJumpBookmark}
              onGotoAnnotationLine={(line, lineEnd) => {
                jumpToLine(focusedPane, line, lineEnd);
                // Without this, the view never regains DOM focus (the
                // click originated in the inspector rail, not the
                // buffer) — CM6's own native-selection reconciliation can
                // then clobber the just-dispatched range/line a tick
                // later. Same "jumpToLine + explicit focus" idiom
                // `handleGotoDef`/`handlePeekActivate` already use.
                paneRepoPath(focusedPane).viewRef.current?.focus();
              }}
              unresolvedAnnotations={unresolvedAnnotationsCount}
              commentsBadgeCount={commentsBadgeCount}
              commentsPanel={
                <CommentsPanel
                  repo={repo}
                  path={focusedPath ?? ""}
                  activeLine={commentActiveLine}
                  onGotoLine={(line, lineEnd) => {
                    jumpToLine(focusedPane, line, lineEnd);
                    paneRepoPath(focusedPane).viewRef.current?.focus();
                  }}
                />
              }
              // F5 — `false`/`undefined` on desktop (isMobile is always
              // false there), so this `<aside>`'s markup is byte-identical
              // to pre-F5: no `asSheet` gate exercised, no sheet-head, no
              // `role="dialog"`.
              asSheet={ctx.asSheet}
              onMobileClose={ctx.onMobileClose}
              hasDossierContext={dossierMode}
              dossierPanel={
                dossierMode && dossier.data ? (
                  <DossierRail
                    fqn={dossier.data.entity.fqn}
                    members={dossier.data.members}
                    sort={dossierSort}
                    onOpen={(p2, line) =>
                      navigate(codeUrl({ repo, path: p2, ref: gitRef, ...(line ? { line } : {}) }))
                    }
                  />
                ) : null
              }
              entityPanel={
                focusedPath ? (
                  <EntityRail
                    repo={repo}
                    path={focusedPath}
                    cursorLine={cursorLineUi}
                    symbols={focusedSymbols}
                    lenses={
                      (focusedPane === 1 ? lenses1.data?.declarations : lenses2.data?.declarations) ??
                      null
                    }
                    onOpenGraph={(pos) =>
                      void handleEgoGraph(focusedPane, {
                        line: pos.line,
                        col: pos.col,
                        word: pos.word,
                      })
                    }
                    onOpenImpact={(pos) =>
                      void handleImpact(focusedPane, {
                        line: pos.line,
                        col: pos.col,
                        word: pos.word,
                      })
                    }
                    onJump={(line) => {
                      jumpToLine(focusedPane, line);
                      paneRepoPath(focusedPane).viewRef.current?.focus();
                    }}
                  />
                ) : null
              }
              workspaceNotesPanel={
                activeWorkspace.id ? (
                  <WorkspaceNotesPanel
                    repo={repo}
                    setId={activeWorkspace.id}
                    currentPath={focusedPath ?? null}
                    currentLine={focusedPath ? cursorLineUi : null}
                    onGotoNote={(notePath, line) => {
                      if (notePath === focusedPath) {
                        jumpToLine(focusedPane, line);
                        paneRepoPath(focusedPane).viewRef.current?.focus();
                      } else if (focusedPane === 1) {
                        navigate(buildReaderUrl({ pane1: { path: notePath, line } }));
                      } else {
                        navigate(buildReaderUrl({ pane2: { path: notePath, line } }));
                      }
                    }}
                  />
                ) : null
              }
            />
  );

  return (
    <div className={readerRootClass}>
      <Desk
        repo={repo}
        desk={desk}
        // V72-G1.2 — `?ent=` swaps the CENTER, never the shell. Every region
        // (dock, main, drawer, rail, both stripes) is the one that was
        // already there; `e2e/desk-landmarks.spec.ts` asserts the identical
        // region set for both modes, which is D1's rule made mechanical.
        centerMode={dossierMode ? "dossier" : "reader"}
        isMobile={isMobile}
        dock={dockBody}
        dockOpen={treeVisible}
        onDockOpenChange={setTreeVisible}
        header={readerHeader}
        rail={railSlot}
        railAvailable={hasInspector}
        railBadges={{ notes: unresolvedAnnotationsCount + bookmarkCount }}
        paneCount={pane2Loc ? 2 : 1}
        onOpenDrawerRow={handleOpenDrawerRow}
        onRampDrawerRow={(rung, row) =>
          ramp.activate(rung, {
            repo: row.repo || repo,
            path: row.path,
            line: row.line,
            via: "usage_of",
            ...(row.trustClass ? { trust: row.trustClass } : {}),
            ...(row.text ? { snippet: row.text } : {}),
          })
        }
        sheetOpen={inspectorOpen}
        onSheetOpenChange={setInspectorOpen}
        onSaveWorkspace={() => setSaveWorkspaceOpen(true)}
        renderDrawerBody={(setId) =>
          usagesDock && usagesDock.setId === setId ? (
            usagesDock.error ? (
              <p className="kbc-usages__preview-hint">usages failed: {usagesDock.error}</p>
            ) : !usagesDock.out ? (
              <p className="kbc-usages__preview-hint">resolving usages for {usagesDock.subject}…</p>
            ) : (
              <UsagesDock
                repo={usagesDock.repo}
                out={usagesDock.out}
                chips={usagesDock.chips}
                onChips={(chips) =>
                  setUsagesDock((st) => (st ? { ...st, chips, cursor: -1 } : st))
                }
                axis={usagesDock.axis}
                onAxis={(axis) => setUsagesDock((st) => (st ? { ...st, axis, cursor: -1 } : st))}
                mentions={usagesDock.mentions}
                mentionsOn={usagesDock.mentionsOn}
                onToggleMentions={() => void toggleUsagesMentions()}
                cursor={usagesDock.cursor}
                onCursor={(cursor) => setUsagesDock((st) => (st ? { ...st, cursor } : st))}
                onOpenRow={openUsageRow}
                refName={usagesDock.refName}
              />
            )
          ) : null
        }
      >
        {dossierMode && entParam ? (
          <DossierView
            repo={repo}
            ent={entParam}
            data={dossier.data}
            state={dossier.state}
            error={dossier.error}
            inherited={dossierInherited}
            onInheritedChange={setDossierInherited}
            sort={dossierSort}
            onSortChange={setDossierSort}
            usagesPerKind={dossierUsagesPerKind}
            onUsagesPerKind={setDossierUsagesPerKind}
            activeSection={dossierSection}
            onOpen={(p2, line) =>
              navigate(codeUrl({ repo, path: p2, ref: gitRef, ...(line ? { line } : {}) }))
            }
            onOpenEntity={openEntityDossier}
          />
        ) : (
          readerBody
        )}
      </Desk>
      {/* V71-E2 — the ONE action menu, rendered once at the reader root.
          The drag-select pill is the same response's top three rows. */}
      {actionMenu &&
        (actionMenu.pill && actionMenu.out && !isMobile ? (
          <ActionPill
            at={actionMenu.at}
            rows={pillRows(actionMenu.out)}
            onRun={runAction}
            onMore={() => setActionMenu((s) => (s ? { ...s, pill: false } : s))}
          />
        ) : (
          <ActionMenu
            at={actionMenu.at}
            out={actionMenu.out}
            loading={actionMenu.loading}
            error={actionMenu.error}
            onTarget={setActionTarget}
            onRun={runAction}
            onClose={closeActionMenu}
            asSheet={isMobile}
          />
        ))}
      {addToBoard && (
        <AddToBoardDialog
          repo={repo}
          target={addToBoard}
          onClose={() => setAddToBoard(null)}
        />
      )}
      {saveWorkspaceOpen &&
        (() => {
          const lines = capturedWorkspaceLines();
          const { pane1, pane2: capturedPane2 } = capturedPanes();
          return (
            <SaveWorkspaceDialog
              repo={repo}
              onClose={() => setSaveWorkspaceOpen(false)}
              onSaved={(view) => {
                setSaveWorkspaceOpen(false);
                activeWorkspace.set(view.id);
                toast.ok(`saved workspace "${view.name}"`);
              }}
              defaultRef={gitRef}
              files={workingSet.entries.map((e) => ({ path: e.path, line: lines[e.path] }))}
              desk={desk.state}
              drawerTabs={desk.drawer.sets.filter((s) => !s.evicted).map((s) => ({ title: s.title, pinned: s.pinned }))}
              focusedPane={focusedPane}
              pane1={pane1}
              pane2={capturedPane2}
            />
          );
        })()}
      {/* F5 — mobile scrim under the reader-tools sheet (mirrors the tree
          drawer's own `MobileDrawer`-owned scrim); tap to dismiss. Mobile-only
          so the desktop DOM is unchanged. */}
      {isMobile && inspectorOpen && (
        <div
          className="kbc-sheet-scrim is-open"
          onClick={() => setInspectorOpen(false)}
          aria-hidden
          data-kbc-sheet-scrim
        />
      )}
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} context="reader" />
      {coach.show && <GdCoachMark onClose={coach.close} />}
      <RecentLocations
        open={recentLocsOpen}
        onClose={() => {
          setRecentLocsOpen(false);
          // Return focus to the buffer the popup was opened from.
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        activeRepo={repo}
        onRamp={(rung, target) => ramp.activate(rung, target)}
        onJump={(loc) => {
          // Picking from the popup is itself a navigation the operator
          // initiated — record it (unlike Ctrl-o/i which only move the
          // pointer). Then land. Suppress the file-open effect only when
          // the path actually changes (same-file jumps never re-fire it).
          recordJump({ repo: loc.repo, path: loc.path, line: loc.line, snippet: "" });
          if (loc.repo !== repo || loc.path !== activeFile) {
            skipNavRecordRef.current = true;
          }
          if (loc.repo !== repo) {
            navigate(codeUrl({ repo: loc.repo, path: loc.path, line: loc.line }));
          } else {
            navigate(buildReaderUrl({ pane1: { path: loc.path, ref: gitRef, line: loc.line } }));
            setFocusedPane(1);
          }
        }}
      />
      <StructurePopup
        open={structureOpen}
        onClose={() => {
          setStructureOpen(false);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        symbols={focusedSymbols}
        onJump={(line) => jumpToLine(focusedPane, line)}
        // T1 (design-ui.md §9.2) — copy-symbol-link coordinates for the
        // CURRENTLY FOCUSED pane's file.
        repo={repo}
        path={focusedPath}
        lang={focusedFileData?.lang}
      />
      <MnemonicPopup
        open={mnemonicOpen}
        onClose={() => {
          setMnemonicOpen(false);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        repo={repo}
        onJump={handleJumpBookmark}
      />
      <LineHistoryPopup
        open={lineHistory !== null}
        onClose={() => {
          setLineHistory(null);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        repo={repo}
        path={lineHistory?.path ?? focusedPath ?? ""}
        line={lineHistory?.line ?? 1}
        lineEnd={lineHistory?.lineEnd}
      />
    </div>
  );
}
