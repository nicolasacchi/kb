import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  excludeArtifact,
  fetchDoc,
  fetchTags,
  isAbortError,
  updateArtifactMeta,
  type DocSummary,
} from "../api/client";
import { useKbs } from "../hooks/useKbs";
import { useCodeRefs } from "../hooks/useCodeRefs";
import {
  useDocLens,
  useDocLensScorecard,
  useSetDocLensPin,
} from "../hooks/useDocLens";
import CodeRefsSection from "./CodeRefsSection";
import { useConfirm } from "./ConfirmProvider";
import { useInspectorCollapsed } from "../hooks/useInspectorCollapsed";
import {
  INSPECTOR_TABS,
  useInspectorTab,
  type InspectorTab,
} from "../hooks/useInspectorTab";
import { Icon } from "./icons";
import CapabilityGlyphs, { glyphsFor } from "./CapabilityGlyphs";
import ReadingChip from "./ReadingChip";
import type { ReadingSummary } from "../api/reading";
import RelatedMemoriesPanel from "./RelatedMemoriesPanel";
import LiftedFromPanel from "./LiftedFromPanel";
import TagPill from "./TagPill";
import FolderCrumbLinks from "./FolderCrumbLinks";
import MoveArtifactModal from "./MoveArtifactModal";
import { artifactHref } from "../lib/artifactHref";
import { folderIndexNote } from "../lib/folderIndexNote";
import { galleryUrl } from "../lib/galleryUrl";
import { toast } from "../lib/toast";
import { slugifyTag, tagColor, tagsFor } from "../lib/derive";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  useArtifactSessions,
  useSessionByArtifact,
  useSessionDetail,
} from "../hooks/useSessions";
import { commitBadge, harnessGlyph } from "../lib/sessionChips";
import { sessionPresence } from "../lib/sessionPresence";
import { useRelatedMemories } from "../hooks/useRelatedMemories";
import { useVersions } from "../hooks/useVersions";
import { useEdges } from "../hooks/useEdges";
import { useFacetCounts } from "../hooks/useFacetCounts";
import { sessionColorFor } from "../lib/sessionColor";
import BiographyTab from "./BiographyTab";
import {
  parseSiblingSort,
  siblingComparator,
  siblingDateUnix,
  type SiblingSortKey,
} from "../lib/siblingSort";
import type { ChildFolder } from "../lib/folderTree";
import { compactDate } from "../lib/time";
import { useLiveTailPortal } from "../context/LiveTailPortalContext";
import { useArtifactHostSuffix } from "../hooks/useArtifactHost";
import { isOriginOfArtifact } from "../lib/artifactHost";
import { parsePane2 } from "../lib/paneUrl";
import { tocJumpParams } from "../lib/tocJumpTarget";

// Same shape detail.tsx builds for the descendants list (G3/G4) — kept
// as a structural prop so the inspector doesn't need to reach into the
// route's lifecycle. `subPath` is precomputed by the route so nested
// rows can render their `extra/` prefix without re-deriving it here.
export type FolderEntry = {
  id: string;
  sourceRelative: string;
  title: string;
  filename: string;
  subPath: string;
  folder: string;
  isCurrent: boolean;
  mtime: number | null;
  indexed: number | null;
  /** fs birth time when available (btime-less FS leaves this unset). */
  created?: number;
  /** v0.33 Y2 — first_indexed_unix; stable mid-chain for "created" sort. */
  firstIndexed?: number;
  progress?: { pct: number; isDone: boolean };
};

// v0.22 — half-width of the "files modified around this time" window the mtime
// passport link opens (±7 days, in seconds), centred on this doc's mtime.
const MTIME_WINDOW_SECS = 7 * 24 * 60 * 60;

type SiblingsPrefs = { sort: SiblingSortKey; includeSubfolders: boolean };
const SIBLINGS_KEY = "kb:siblings";
// F1 — NEW-user default is updated desc (mtime); stored name/title keep working.
const DEFAULT_PREFS: SiblingsPrefs = {
  sort: "updated",
  includeSubfolders: false,
};

// Category autocomplete hints — the common kb-category values. Free-form
// (the field accepts anything); these just seed the datalist.
const CATEGORY_SUGGESTIONS = [
  "notes",
  "research",
  "review",
  "design",
  "reference",
  "memory-user",
];

// I1 — the inspector sub-tab rail: icon + label per tab. v0.22 — the merged
// "all" tab is FIRST and labelled "About": it stacks every section (the old
// single-scroll view) and absorbed the standalone "about" overview. Reuses the
// shared Icon set so the rail is the dock-pill vocabulary, not a new language.
const TAB_META: Record<InspectorTab, { label: string; icon: ReactNode }> = {
  all: { label: "About", icon: <Icon.Doc /> },
  meta: { label: "Meta", icon: <Icon.Spark /> },
  links: { label: "Links", icon: <Icon.Graph /> },
  folder: { label: "Folder", icon: <Icon.List /> },
  sessions: { label: "Sessions", icon: <Icon.Terminal /> },
  // W2.1 — the biography timeline (origin session → touches → comments →
  // versions).
  story: { label: "Story", icon: <Icon.BookOpen /> },
};

// SH.B3 — the shape of a `kb:toc` heading entry (mirrors TocSpy's own
// local `TocItem`, minus the `top` field TocSpy uses for scroll-spy math
// that this section has no use for).
type TocHeading = { id: string; text: string; level: number };

function readSiblingsPrefs(): SiblingsPrefs {
  if (typeof localStorage === "undefined") return DEFAULT_PREFS;
  try {
    const raw = localStorage.getItem(SIBLINGS_KEY);
    if (!raw) return DEFAULT_PREFS;
    const parsed = JSON.parse(raw) as Partial<SiblingsPrefs>;
    return {
      sort: parseSiblingSort(parsed.sort),
      includeSubfolders: parsed.includeSubfolders === true,
    };
  } catch {
    return DEFAULT_PREFS;
  }
}

function writeSiblingsPrefs(prefs: SiblingsPrefs): void {
  try {
    localStorage.setItem(SIBLINGS_KEY, JSON.stringify(prefs));
  } catch {
    /* noop */
  }
}

// v0.10 P2 — Preview inspector rail.
//
// Right column on the Detail route: reading progress · identifier kv ·
// metrics · tags · capabilities · neighbors · backlinks. All read-only
// metadata pulled from the existing doc summary + atlas edges + the
// reading-progress map. The CommentsPanel takes precedence in the same
// slot when annotate mode is open (Detail handles the toggle).
//
// Neighbors derives from the kb's cross-artifact edges (the atlas
// endpoint). When the current doc isn't an edge endpoint the list is
// empty — no error, no flicker. Backlinks count is the same one the
// gallery card already shows; rendering the actual referrer paths
// requires reverse-lookup support the route doesn't yet expose
// (deferred — backlinks stays count-only here).
/// RP-track — `372000` → `6m 12s` for the "Read by you" block.
function fmtReadDur(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export default function PreviewInspector({
  kb,
  doc,
  progress,
  summary,
  descendants,
  folderCwd,
  onFolderCwd,
  childFolders: childFolderRows,
  onMetaChange,
  docked = false,
  asSheet = false,
  onMobileClose,
  panelMode = "inspect",
  onSwitchToInspect,
  onComments,
  onVersions,
  commentCount = 0,
  panelSlot,
}: {
  kb: string;
  doc: DocSummary;
  progress?: { pct: number; isDone: boolean };
  // RP-track — the cross-visit reading summary (completion, read %, active
  // time, stop-point). Drives the "Read by you" block; hidden when absent.
  summary?: ReadingSummary | null;
  descendants?: FolderEntry[];
  // F2 — panel-local browse location + drill-down (owned by detail.tsx).
  // Defaults to the open doc's folder when omitted (back-compat).
  folderCwd?: string;
  onFolderCwd?: (path: string) => void;
  /// Immediate subfolders of `folderCwd` (from the folders tree, pure).
  childFolders?: ChildFolder[];
  // When provided, the Tags + Category sections become editable; the
  // callback lifts the post-edit values into the route's `doc` so it stays
  // the single source of truth. Absent ⇒ read-only pills (back-compat).
  onMetaChange?: (patch: { tags?: string[]; kb_category?: string | null }) => void;
  // kb2 R2 — rendered inside the ReaderDock, which is now just the column
  // chrome; the inspector owns the single icon rail (sub-tabs + comments +
  // versions). Suppress the inspector's own collapse header/strip when docked.
  docked?: boolean;
  // v0.23 — when raised as the mobile bottom sheet (phone only), the rail is a
  // modal dialog: `asSheet` adds role=dialog / aria-modal / id="kb-reader-sheet"
  // (the ContextBar entry button's aria-controls target). False on desktop, so
  // the docked <aside> semantics + e2e are byte-identical there.
  asSheet?: boolean;
  // Mobile-only — when the inspector is shown as a bottom sheet on a phone,
  // this renders a phone-only dismiss header (CSS-hidden on desktop). The
  // ContextBar reader-tools toggle is the other way to close it.
  onMobileClose?: () => void;
  // v0.21 — the merged dock rail. `panelMode` selects what the body shows:
  // the inspector sub-tabs ("inspect") or the comments / versions panel passed
  // as `panelSlot`. The rail (this component) is the single persistent tab row;
  // the old `[inspect|comments|versions]` text pills are gone. `onComments`
  // undefined hides the comments icon (no review on this artifact).
  panelMode?: "inspect" | "comments" | "versions";
  onSwitchToInspect?: () => void;
  onComments?: () => void;
  onVersions?: () => void;
  commentCount?: number;
  panelSlot?: ReactNode;
}) {
  // Hooks first — Rules of Hooks forbids conditional returns above
  // the hook list. The collapsed-state branch is below at the render
  // step.
  const { collapsed, toggle: toggleCollapsed } = useInspectorCollapsed("detail");
  // v0.24 X4 — the About tab's "exclude from index" action. Confirm-guarded
  // (invariant #32); on success we leave the reader for the kb gallery, since
  // the KeepUserData cascade is about to pull this doc from the index (the
  // detail queries would 404 on the SSE invalidation).
  const confirm = useConfirm();
  const navigate = useNavigate();
  // DCB W1.D — upgraded from the read-only `[searchParams]` destructure so
  // the Code section's repo picker (`pickRepo`, below) can write `?repo=`.
  // Deviation-with-rationale: every other `setSearchParams` call site lives
  // in a route file (detail.tsx, gallery.tsx, search.tsx, sessions.tsx —
  // grep-verified zero hits under web/src/components/), not a child
  // component; `useSearchParams` is safe to call from any component in the
  // tree (it reads/writes the router's shared location state, the same
  // mechanism detail.tsx's immersive/folio toggles already rely on), so
  // this introduces no new primitive, only a new call SITE of an existing
  // safe hook — threading a repo/onRepoChange prop pair through
  // PreviewInspector's already-large prop surface for one inspector-
  // internal selection was rejected as strictly worse (13-w1d-kb-spa.md
  // §4.3).
  const [searchParams, setSearchParams] = useSearchParams();
  const [excluding, setExcluding] = useState(false);
  // F4 — move dialog for the open artifact (Folder tab header action).
  const [moveOpen, setMoveOpen] = useState(false);
  const onExclude = async () => {
    const ok = await confirm({
      title: "Exclude from index?",
      body: `Exclude “${doc.title || doc.source_relative}” from search and the gallery? The file stays on disk and its comments + reading history survive — re-include it any time from Settings → Excluded.`,
      confirmLabel: "Exclude",
    });
    if (!ok) return;
    setExcluding(true);
    excludeArtifact(kb, doc.source_relative)
      .then(() => {
        toast.ok("excluded from index");
        navigate(galleryUrl(kb));
      })
      .catch((err) =>
        toast.err(
          `exclude failed: ${err instanceof Error ? err.message : String(err)}`,
        ),
      )
      .finally(() => setExcluding(false));
  };
  // I1 — which sub-panel is showing; persisted so re-opening an artifact
  // restores the last tab. `show(t)` (below) also matches the "All" overview.
  const { tab, setTab } = useInspectorTab("detail");
  // v0.22 — rail badge counts. Both ride the persistent rail (mounted
  // regardless of the active sub-tab), so the counts are always current.
  // `useRelatedMemories` is the SAME query the Sessions-tab panel reads
  // (shared cache); `useVersions` is enabled eagerly here so the badge is
  // populated and the Versions panel opens to a warm cache. Both refresh via
  // the SSE bridge (['memories'] / ['versions']) — invariant #24.
  // D3 — re-anchor "related memories" to THIS artifact (title + summary) so
  // recall returns memories genuinely about this file, not the kb-wide recency
  // dump. Both the badge count and the Sessions-tab panel read the same query.
  const memoryQuery = [doc.title, doc.summary]
    .filter(Boolean)
    .join(". ")
    .slice(0, 240);
  const { count: memoriesCount } = useRelatedMemories(kb, memoryQuery);
  // W3.E/S4 — the adaptive rail: when the open artifact IS a session capture,
  // the SAME six sub-tabs swap their section content to session-shaped
  // equivalents (Folder→Work, Sessions→memories-produced+self-link). Gated
  // on the by-artifact join (#11's canonical sqlite lookup), NOT the old
  // filename-regex `SessionSelfLink` (deleted below). `useSessionDetail`
  // reuses the /sessions rail's own combined 9-fetch query — "existing
  // per-sid hooks", not a new fetch surface.
  const isSession = doc.kb_category === "memory-session";
  const { data: byArtifact } = useSessionByArtifact(
    isSession ? kb : null,
    isSession ? doc.id : null,
  );
  const sessionId = byArtifact?.session?.session_id ?? null;
  const sessionDetail = useSessionDetail(isSession ? sessionId : null);
  // W7/LF-2 — mobile portal node registration. On mobile + session artifact,
  // register a container for the LiveTailPanel to portal into (inside the
  // inspector sheet). On desktop or non-session docs, clear the node.
  const { setMobilePortalNode } = useLiveTailPortal();
  useEffect(() => {
    if (!asSheet || !isSession) {
      // Desktop or non-session: no portal target needed
      setMobilePortalNode(null);
      return;
    }
    // On mobile + session: create and register the portal target
    const portal = document.createElement("div");
    portal.className = "kb-inspector__live-tail-portal";
    setMobilePortalNode(portal);
    return () => {
      setMobilePortalNode(null);
    };
  }, [asSheet, isSession, setMobilePortalNode]);

  const { versions: versionList } = useVersions(kb, doc.id, true);
  const versionCount = versionList.length;
  // W2.1 — the Story tab's own timeline is self-fetching (mirrors
  // SessionsTouchedPanel/RelatedMemoriesPanel), so it reports its assembled
  // length back up here for the rail badge rather than PreviewInspector
  // duplicating the sessions/review/versions fetch just for a count.
  const [storyCount, setStoryCount] = useState(0);
  // D1 — corpus-wide facet counts for the "Explore from here" chip strip.
  const facetCounts = useFacetCounts(kb);
  const [outlinks, setOutlinks] = useState<NeighborRow[]>([]);
  const [backlinks, setBacklinks] = useState<NeighborRow[]>([]);
  const [siblingsPrefs, setSiblingsPrefs] = useState<SiblingsPrefs>(() =>
    readSiblingsPrefs(),
  );
  useEffect(() => {
    writeSiblingsPrefs(siblingsPrefs);
  }, [siblingsPrefs]);

  // F2 — browse location for this panel (defaults to the open doc's folder).
  const cwd = folderCwd ?? doc.folder;
  const subfolders = childFolderRows ?? [];
  // Filter (direct-only vs all descendants of cwd) + sort once per render.
  // The server already returned cwd + descendants, so direct-only means
  // `d.folder === cwd`. Sort keys live in siblingSort.ts.
  const folderRows = useMemo<FolderEntry[]>(() => {
    if (!descendants) return [];
    const list = siblingsPrefs.includeSubfolders
      ? descendants
      : descendants.filter((d) => d.folder === cwd);
    return [...list].sort(siblingComparator(siblingsPrefs.sort));
  }, [descendants, siblingsPrefs, cwd]);
  const folderTotal = folderRows.length;
  const folderPos = folderRows.findIndex((r) => r.id === doc.id) + 1;
  // v0.33 Y4 — index.md in the CURRENT browse cwd (direct children only).
  const cwdIndexNote = useMemo(() => {
    if (!descendants) return null;
    const direct = descendants.filter((d) => d.folder === cwd);
    return folderIndexNote(
      direct.map((d) => ({
        source_relative: d.sourceRelative,
        title: d.title,
        id: d.id,
      })),
      cwd,
    );
  }, [descendants, cwd]);
  // F2 visibility: siblings >1, OR ambient subfolders, OR doc lives in a
  // folder (so the tab is useful even for a lone file with no siblings).
  // The lone-file widening applies only when the browser is WIRED — pane-2
  // focus withholds the browse props (#30), and an unwired browser would
  // render hollow chrome (breadcrumb + sort with no rows, no empty state).
  const browserWired = onFolderCwd !== undefined;
  const showFolderBrowser =
    folderTotal > 1 ||
    subfolders.length > 0 ||
    (browserWired && doc.folder !== "");
  // v0.21 — the link-graph count badge rides the always-visible Links rail
  // icon, so it must stay current per artifact regardless of panelMode. The
  // corpus edge list is identical for every doc in the kb, so it's fetched
  // ONCE per kb through react-query (["edges", kb], SSE-refreshed on reindex)
  // instead of re-downloaded on every reader→reader navigation. The badge is
  // derived from a cheap in-memory dedup scan of that list — no doc fetches.
  const { edges } = useEdges(kb);
  const { outIds, backIds } = useMemo(() => {
    const outSeen = new Set<string>();
    const backSeen = new Set<string>();
    const out: string[] = [];
    const back: string[] = [];
    for (const e of edges) {
      if (e.src === doc.id && e.dst !== doc.id && !outSeen.has(e.dst)) {
        outSeen.add(e.dst);
        out.push(e.dst);
      }
      if (e.dst === doc.id && e.src !== doc.id && !backSeen.has(e.src)) {
        backSeen.add(e.src);
        back.push(e.src);
      }
    }
    return { outIds: out, backIds: back };
  }, [edges, doc.id]);
  // The up-to-16 fetchDoc fan-out (title/path projection for the neighbor
  // rows) only runs when the Links tab — or the merged "About" tab that
  // stacks it — is actually visible; the badge count above needs none of it.
  const wantLinks = tab === "links" || tab === "all";
  const outKey = outIds.join(",");
  const backKey = backIds.join(",");
  useEffect(() => {
    if (!wantLinks) return;
    const ctl = new AbortController();
    Promise.all([
      resolveRows(outIds.slice(0, 8), kb, ctl.signal),
      resolveRows(backIds.slice(0, 8), kb, ctl.signal),
    ])
      .then(([out, bl]) => {
        setOutlinks(out);
        setBacklinks(bl);
      })
      .catch((e) => {
        if (!isAbortError(e)) {
          setOutlinks([]);
          setBacklinks([]);
        }
      });
    return () => ctl.abort();
    // outKey/backKey are the content-stable dep for the id arrays.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kb, wantLinks, outKey, backKey]);

  // DCB W1.D — the Code section's two data paths. `useCodeRefs` is
  // same-origin (normal TanStack, rides the SSE bridge via docsGate);
  // `codeUrl`/scorecard/lens are kb-code's cross-origin doc-lens surface
  // (the FOURTH #23 exception, api/doclens.ts + useDocLens.ts). `ctx` is
  // sourced from the already-warm `useKbs()` cache (no extra network
  // request) — the same pattern search.tsx uses for
  // `default_search_category`.
  const { data: kbs } = useKbs();
  const ctx = kbs?.find((k) => k.name === kb);
  // W1.D.R #11 — `code_url` is operator-entered kb.toml config, not a wire
  // type the daemon validates; an invalid/relative value (e.g. the empty-
  // ish "/") would otherwise reach `fetch()` as-is and resolve against
  // *this* origin (same-origin, so no CORS error to catch it) instead of
  // failing loudly. `new URL()` requires an absolute URL with a scheme —
  // exactly what every cross-origin fetcher in api/doclens.ts assumes — so
  // a value that can't parse is treated the same as "unset": state 1's
  // "not linked to a code repo" note, never a silent same-origin fetch.
  const codeUrl = (() => {
    const raw = ctx?.code_url;
    if (!raw) return null;
    try {
      new URL(raw);
      return raw;
    } catch {
      return null;
    }
  })();
  const codeRefsQuery = useCodeRefs(kb, doc.id, wantLinks);
  const codeRefs = codeRefsQuery.data?.refs ?? [];
  const neverScanned = codeRefsQuery.data?.never_scanned === true;
  const hasCodeRefs = codeRefsQuery.data !== undefined && codeRefs.length > 0;
  const showCodeSection =
    codeRefsQuery.data !== undefined && (neverScanned || codeRefs.length > 0);
  // W1.D.R #4 — the heading count + truncation caption read the SERVER
  // total/flag (`CodeRefsResponse.ref_count`/`.truncated`), never
  // `codeRefs.length` — the two agree today (the single-doc route never
  // truncates `refs` beyond what `ref_count` reflects), but the field name
  // says what it's actually counting.
  const codeRefCount = codeRefsQuery.data?.ref_count ?? codeRefs.length;
  const codeRefsTruncated = codeRefsQuery.data?.truncated === true;
  const repoParam = searchParams.get("repo");
  const setRepoParam = (repo: string | null) => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (repo) next.set("repo", repo);
        else next.delete("repo");
        return next;
      },
      { replace: true },
    );
  };
  const scorecardQuery = useDocLensScorecard(codeUrl, kb, doc.id, hasCodeRefs);
  // R5 — `pinned_repo` pre-selects the switcher when `?repo=` is absent
  // from the URL (Decision 1's "last pick remembered per-doc" half); "seed
  // once per DOC" (W1.D.R #2 — was "once per mount": PreviewInspector stays
  // mounted across in-app reader→reader navigation with no `key=` at either
  // detail.tsx call site, so a plain boolean latch only ever fired for the
  // FIRST doc viewed in a session). Tracking the seeded doc id lets the
  // effect re-arm on every doc change while still never fighting the user's
  // own subsequent `?repo=` navigation on the doc it already seeded.
  const seededPinRef = useRef<string | null>(null);
  useEffect(() => {
    if (seededPinRef.current === doc.id || repoParam || !scorecardQuery.data)
      return;
    seededPinRef.current = doc.id;
    if (scorecardQuery.data.pinned_repo) {
      setRepoParam(scorecardQuery.data.pinned_repo);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [doc.id, scorecardQuery.data, repoParam]);
  const docLensQuery = useDocLens(
    codeUrl,
    kb,
    doc.id,
    hasCodeRefs ? repoParam : null,
  );
  // CT-E3 — the Links rail icon's drift indicator: lit only when the doclens
  // lane has ACTUAL lens data with drifted > 0 (an unreachable/unpinned/
  // refused lane keeps it dark — an indicator that can't verify never
  // claims drift either way, same never-fake-fresh ethos as the summary
  // line the CodeRefsSection renders from the same queries).
  const codeDrifted =
    hasCodeRefs && repoParam ? (docLensQuery.data?.counts.drifted ?? 0) : 0;
  const setPin = useSetDocLensPin(codeUrl, kb, doc.id);
  // §7.1's scorecard onClick calls pickRepo, not setRepoParam directly, so
  // the pin write and the URL change happen together, in one place.
  const pickRepo = (repo: string) => {
    setRepoParam(repo);
    setPin.mutate({ repo, docHash: codeRefsQuery.data?.doc_hash ?? null });
  };

  // Metadata editor (tags + category). Enabled only when the route wires
  // `onMetaChange` (the Detail rail does); edits PATCH the source file and
  // the response carries the effective values after re-parse.
  const canEdit = typeof onMetaChange === "function";
  const [tagSaving, setTagSaving] = useState(false);
  const [catSaving, setCatSaving] = useState(false);
  const [metaErr, setMetaErr] = useState<string | null>(null);
  const [tagInput, setTagInput] = useState("");
  const [allTags, setAllTags] = useState<string[]>([]);
  const [catInput, setCatInput] = useState(doc.kb_category ?? "");
  // Re-sync the category buffer whenever the doc's category changes
  // (including when a save reconciles it to the server value).
  useEffect(() => {
    setCatInput(doc.kb_category ?? "");
  }, [doc.kb_category]);
  // Tag autocomplete: the kb's full tag facet (best-effort).
  useEffect(() => {
    if (!canEdit) return;
    const ctl = new AbortController();
    fetchTags(kb, ctl.signal)
      .then((ts) => setAllTags(ts.map((t) => t.name)))
      .catch(() => {
        /* autocomplete is best-effort */
      });
    return () => ctl.abort();
  }, [kb, canEdit]);

  // SH.B3 — Topics: the About stack's own heading/TOC jump list. Mirrors
  // TocSpy's `kb:toc`/`kb:section` postMessage subscription verbatim (same
  // messages, same exact-origin guard — invariant #23's iframe-relayed-state
  // exception family, not a new fetch) rather than plumbing a fresh data
  // path. `useArtifactHostSuffix` shares TocSpy/ArtifactPane's module-level
  // `/api/identity` cache, so calling it again here is not a second request.
  //
  // PreviewInspector has no `iframeRef` — it's a sibling of ArtifactPane,
  // not a child, and the rail is keyed to whichever pane is FOCUSED (#30),
  // so a row click can't post `kb:scroll-to-id` straight into the live
  // frame the way TocSpy's `jumpTo` does. It writes into the URL instead —
  // `?sec=` (primary artifact) or the third field of `?pane2=` (when the
  // open doc IS pane 2's — `tocJumpParams`, pure/tested) — the SAME param
  // ArtifactPane's own RLs1 effect already relays into the live frame for
  // every other same-artifact jump (permalinks, register marks, trail
  // nav), which is what keeps the flow-stack/scroll-snapshot semantics
  // (#20/#31) intact for free.
  const hostSuffix = useArtifactHostSuffix();
  const [toc, setToc] = useState<TocHeading[]>([]);
  const [activeSection, setActiveSection] = useState<string | null>(null);
  useEffect(() => {
    function onMessage(e: MessageEvent) {
      if (!isOriginOfArtifact(e.origin, doc.id, kb, hostSuffix)) return;
      const data = e.data as
        | { kind?: string; toc?: TocHeading[]; id?: string }
        | null;
      if (!data || typeof data !== "object" || !data.kind) return;
      if (data.kind === "kb:toc" && Array.isArray(data.toc)) {
        setToc(data.toc as TocHeading[]);
      } else if (data.kind === "kb:section" && typeof data.id === "string") {
        setActiveSection(data.id);
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [doc.id, kb, hostSuffix]);
  // Fresh artifact — drop the previous doc's headings so the section
  // doesn't briefly flash stale content (mirrors TocSpy's own reset on an
  // iframe change).
  useEffect(() => {
    setToc([]);
    setActiveSection(null);
  }, [doc.id]);
  // Same ~20-row cap TocSpy applies, for the same reason: keep the rail
  // compact rather than unbounded for a deeply-nested artifact.
  const tocItems = useMemo(() => toc.slice(0, 20), [toc]);
  const pane2Loc = useMemo(
    () => parsePane2(searchParams.get("pane2")),
    [searchParams],
  );
  const jumpToSection = (headingId: string) => {
    const target = tocJumpParams(headingId, kb, doc.source_relative, pane2Loc);
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.set(target.param, target.value);
        return next;
      },
      { replace: true },
    );
  };

  if (collapsed && !docked) {
    return (
      <aside className="kb-pinsp kb-pinsp--collapsed" aria-label="artifact inspector (collapsed)">
        <button
          type="button"
          className="kb-pinsp__expand"
          onClick={toggleCollapsed}
          title="expand inspector"
          aria-label="expand inspector"
          aria-expanded="false"
        >
          ‹
        </button>
      </aside>
    );
  }

  const tags = tagsFor(doc);
  const accentTag = tags[0];
  const glyphs = glyphsFor(doc);
  const show = (t: InspectorTab) => tab === t || tab === "all";
  // Derived from the deduped edge scan (uncapped) so the badge is correct
  // without the doc fan-out; the rendered neighbor rows below stay capped at 8.
  const linkCount = outIds.length + backIds.length;
  // D1 — "Explore from here": every facet of this artifact (folder · category ·
  // each tag) becomes a chip that pivots into the pre-filtered gallery, badged
  // with the corpus-wide count (how many artifacts share it). Counts intersect
  // the doc's facets with the cached /folders, /facets, /tags rollups.
  const exploreChips: {
    key: string;
    label: string;
    count?: number;
    to: string;
  }[] = [];
  if (doc.folder) {
    exploreChips.push({
      key: "folder",
      label: doc.folder.split("/").filter(Boolean).pop() ?? doc.folder,
      count: facetCounts.folder(doc.folder),
      to: galleryUrl(kb, { folder: doc.folder }),
    });
  }
  if (doc.kb_category) {
    exploreChips.push({
      key: "cat",
      label: doc.kb_category,
      count: facetCounts.category(doc.kb_category),
      to: galleryUrl(kb, { category: doc.kb_category }),
    });
  }
  for (const t of tags) {
    exploreChips.push({
      key: `tag:${t}`,
      label: `#${t}`,
      count: facetCounts.tag(t),
      to: galleryUrl(kb, { tags: [t] }),
    });
  }
  // v0.22 — ONE badge grammar across every rail icon: a numeric count when
  // >0, else nothing (the meta tab keeps its separate error DOT). The folder
  // count tracks what the Folder tab shows (siblings, only meaningful when >1);
  // sessions badges the related-memories count; links is neighbors+backlinks.
  // Comments + versions are badged on their own icons below (own counts).
  const itabBadge = (t: InspectorTab): number => {
    switch (t) {
      case "links":
        return linkCount;
      case "folder":
        // W3.E/S4 — Folder→Work on a session artifact: decisions+commits,
        // not the (session-irrelevant) sibling-capture count.
        return isSession
          ? sessionDetail.decisions.length + sessionDetail.commits.length
          : folderTotal > 1
            ? folderTotal
            : 0;
      case "sessions":
        return memoriesCount;
      case "story":
        return storyCount;
      default:
        return 0;
    }
  };

  // Estimated reading time at the Engadget standard of ~240 wpm.
  const wpm = 240;
  const readMin = doc.word_count
    ? Math.max(1, Math.round(doc.word_count / wpm))
    : null;

  // Optimistic write-through: push the change into the route's `doc`
  // immediately, PATCH, then reconcile with the server's effective values
  // (or revert + surface the error). `tags` above is the current effective
  // set; saving any edit promotes it to explicit in the source.
  const commitTags = (next: string[], prev: string[]) => {
    setMetaErr(null);
    setTagSaving(true);
    onMetaChange?.({ tags: next });
    updateArtifactMeta(kb, doc.id, { tags: next })
      .then((res) => onMetaChange?.({ tags: res.tags, kb_category: res.kb_category }))
      .catch((e) => {
        onMetaChange?.({ tags: prev });
        setMetaErr(e instanceof Error ? e.message : String(e));
      })
      .finally(() => setTagSaving(false));
  };
  const addTag = () => {
    const slug = slugifyTag(tagInput);
    setTagInput("");
    if (!slug || tags.includes(slug)) return;
    commitTags([...tags, slug], tags);
  };
  const removeTag = (t: string) => {
    if (tagSaving) return;
    commitTags(
      tags.filter((x) => x !== t),
      tags,
    );
  };
  const commitCategory = () => {
    const next = catInput.trim();
    if (next === (doc.kb_category ?? "").trim()) return;
    setMetaErr(null);
    setCatSaving(true);
    onMetaChange?.({ kb_category: next || null });
    updateArtifactMeta(kb, doc.id, { category: next })
      .then((res) => onMetaChange?.({ tags: res.tags, kb_category: res.kb_category }))
      .catch((e) => {
        onMetaChange?.({ kb_category: doc.kb_category ?? null });
        setCatInput(doc.kb_category ?? "");
        setMetaErr(e instanceof Error ? e.message : String(e));
      })
      .finally(() => setCatSaving(false));
  };

  // v0.23 — the sheet-head label names the active body so the reader always
  // knows what the sheet is showing (reader tools / comments / versions).
  const sheetLabel =
    panelMode === "comments"
      ? "comments"
      : panelMode === "versions"
        ? "versions"
        : "reader tools";
  return (
    <aside
      className="kb-pinsp"
      {...(asSheet
        ? {
            role: "dialog" as const,
            "aria-modal": true,
            id: "kb-reader-sheet",
            "aria-label": "reader tools",
          }
        : { "aria-label": "artifact inspector" })}
    >
      {onMobileClose && (
        <header className="kb-pinsp__sheet-head">
          {/* Grab pill — reads as a native bottom sheet; centred at the top
              via CSS. Decorative (dismiss is the ✕ / scrim / Esc). */}
          <span className="kb-pinsp__grab" aria-hidden />
          <span className="kb-pinsp__lab">{sheetLabel}</span>
          <button
            type="button"
            className="kb-pinsp__sheet-x"
            onClick={onMobileClose}
            title="close reader tools"
            aria-label="close reader tools"
          >
            <Icon.X />
          </button>
        </header>
      )}
      {!docked && (
        <header className="kb-pinsp__head">
          <span className="kb-pinsp__lab">inspector</span>
          <button
            type="button"
            className="kb-pinsp__collapse"
            onClick={toggleCollapsed}
            title="collapse inspector"
            aria-label="collapse inspector"
            aria-expanded="true"
          >
            ›
          </button>
        </header>
      )}
      <nav
        className="kb-pinsp__icons"
        role="tablist"
        aria-label="inspector & panels"
      >
        {INSPECTOR_TABS.map((t) => {
          const sel = panelMode === "inspect" && tab === t;
          return (
            <button
              key={t}
              type="button"
              role="tab"
              data-kb-itab={t}
              aria-selected={sel}
              className={`kb-pinsp__itab ${sel ? "is-on" : ""}`}
              onClick={() => {
                onSwitchToInspect?.();
                setTab(t);
              }}
              title={TAB_META[t].label}
              aria-label={TAB_META[t].label}
            >
              {TAB_META[t].icon}
              {itabBadge(t) > 0 && (
                <span className="kb-pinsp__itab-badge">{itabBadge(t)}</span>
              )}
              {t === "meta" && metaErr && (
                <span className="kb-pinsp__itab-dot" aria-hidden />
              )}
              {/* CT-E3 — verified code-citation drift only (see codeDrifted
                  above); mirrors the meta error dot's grammar with its own
                  warn tint. */}
              {t === "links" && codeDrifted > 0 && (
                <span
                  className="kb-pinsp__itab-dot kb-pinsp__itab-dot--drift"
                  role="img"
                  aria-label={`${codeDrifted} code citation${codeDrifted === 1 ? "" : "s"} drifted`}
                  title={`${codeDrifted} code citation${codeDrifted === 1 ? "" : "s"} drifted`}
                />
              )}
            </button>
          );
        })}
        {/* v0.21 — comments + versions joined the rail as icons (the old dock
            text-pills are gone); a right-pushed gap separates them from the
            inspector sub-tabs. */}
        {(onComments || onVersions) && (
          <span className="kb-pinsp__rail-gap" aria-hidden />
        )}
        {onComments && (
          <button
            type="button"
            role="tab"
            data-kb-act="dock-comments"
            aria-selected={panelMode === "comments"}
            className={`kb-pinsp__itab ${panelMode === "comments" ? "is-on" : ""}`}
            onClick={onComments}
            title={panelMode === "comments" ? "hide comments" : "comments"}
            aria-label={`comments${commentCount > 0 ? ` (${commentCount})` : ""}`}
          >
            <Icon.Note />
            {commentCount > 0 && (
              <span
                className="kb-pinsp__itab-badge"
                data-kb-act="comments-badge"
              >
                {commentCount}
              </span>
            )}
          </button>
        )}
        {onVersions && (
          <button
            type="button"
            role="tab"
            data-kb-act="dock-versions"
            aria-selected={panelMode === "versions"}
            className={`kb-pinsp__itab ${panelMode === "versions" ? "is-on" : ""}`}
            onClick={onVersions}
            title={panelMode === "versions" ? "hide versions" : "versions"}
            aria-label={`versions${versionCount > 0 ? ` (${versionCount})` : ""}`}
          >
            <Icon.History />
            {versionCount > 0 && (
              <span className="kb-pinsp__itab-badge" data-kb-act="versions-badge">
                {versionCount}
              </span>
            )}
          </button>
        )}
      </nav>
      {panelMode !== "inspect" ? (
        panelSlot
      ) : (
      <div className="kb-pinsp__body">
        {show("all") && (
          <>
        {exploreChips.length > 0 && (
          <>
            <h4>Explore from here</h4>
            <div className="kb-pinsp__chips">
              {exploreChips.map((c) => (
                <Link
                  key={c.key}
                  className="kb-pinsp__chip"
                  to={c.to}
                  title={`explore ${c.label} in the gallery`}
                >
                  <span className="kb-pinsp__chip-lbl">{c.label}</span>
                  {c.count !== undefined && (
                    <span className="kb-pinsp__chip-n">{c.count}</span>
                  )}
                </Link>
              ))}
            </div>
          </>
        )}
        {/* SH.B3 — heading/TOC jump list. Empty state renders nothing (no
            bare "Topics" header) — the artifact simply has no headings, or
            the runtime hasn't posted `kb:toc` yet. This is the reader
            sheet's only section-nav affordance on mobile now that TocSpy's
            floating rail is hidden at ≤860px (SH.B2). */}
        {tocItems.length > 0 && (
          <>
            <h4>Topics</h4>
            <div className="kb-pinsp__toc">
              {tocItems.map((h) => (
                <button
                  key={h.id}
                  type="button"
                  className={`kb-pinsp__toc-row kb-pinsp__toc-row--l${h.level}${
                    h.id === activeSection ? " is-on" : ""
                  }`}
                  onClick={() => jumpToSection(h.id)}
                  title={h.text}
                >
                  {h.text}
                </button>
              ))}
            </div>
          </>
        )}
        <h4>Reading</h4>
        <div className="kb-pinsp__reading">
          {readMin !== null && (
            <span>
              est. <b>{readMin} min</b>
            </span>
          )}
          {progress && (
            <>
              <span className="kb-pinsp__sep">·</span>
              <span>
                <b className="kb-pinsp__pct">{progress.pct}%</b> read
              </span>
            </>
          )}
        </div>
        {progress && (
          <div className="kb-pinsp__progress" aria-hidden>
            <i style={{ width: `${progress.pct}%` }} />
          </div>
        )}

        {summary && summary.visit_count > 0 && (
          <>
            <h4>Read by you</h4>
            <div className="kb-pinsp__reading">
              <span>
                <b className="kb-pinsp__pct">{summary.completion_pct}%</b> scrolled
              </span>
              <span className="kb-pinsp__sep">·</span>
              <span>
                <b>{summary.read_pct}%</b> read
              </span>
            </div>
            <div className="kb-pinsp__readby">
              active {fmtReadDur(summary.active_ms_total)} · {summary.visit_count}{" "}
              visit{summary.visit_count === 1 ? "" : "s"}
            </div>
            {summary.stopped_at && (
              <div className="kb-pinsp__readby kb-pinsp__readby--stop">
                stopped at “{summary.stopped_at.text}” (~{summary.stopped_at.pct}%)
              </div>
            )}
          </>
        )}

        <h4>Identifier</h4>
        <KV label="file">{shortFilename(doc)}</KV>
        {/* v0.22 passport — folder/category/mtime are now gallery deep-links;
            the id copies to clipboard. Each pivots into a pre-filtered,
            refinable gallery (folder is descendant-inclusive). */}
        <KV label="folder">
          <FolderCrumbLinks
            kb={kb}
            folder={doc.folder}
            linkClass="kb-pinsp__kv-link"
          />
        </KV>
        {doc.kb_category && (
          <KV label="category">
            <Link
              className="kb-pinsp__kv-link"
              to={galleryUrl(kb, { category: doc.kb_category })}
              title={`all ${doc.kb_category} artifacts`}
            >
              {doc.kb_category}
            </Link>
          </KV>
        )}
        <KV label="id">
          <IdCopy id={doc.id} />
        </KV>
        {doc.mtime_unix && (
          <KV label="mtime">
            <Link
              className="kb-pinsp__kv-link"
              to={galleryUrl(kb, {
                from: doc.mtime_unix - MTIME_WINDOW_SECS,
                to: doc.mtime_unix + MTIME_WINDOW_SECS,
                sort: "recent",
                dir: "desc",
              })}
              title="files modified around this time (±7d)"
            >
              {formatUnix(doc.mtime_unix)}
            </Link>
          </KV>
        )}
        {doc.created_unix && (
          <KV label="created">{formatUnix(doc.created_unix)}</KV>
        )}
        {doc.indexed_at_unix && (
          <KV label="indexed">{formatUnix(doc.indexed_at_unix)}</KV>
        )}

        <h4>Metrics</h4>
        {doc.word_count !== null && doc.word_count !== undefined && (
          <KV label="words">{doc.word_count.toLocaleString()}</KV>
        )}
        {/* v0.22 D2 — backlinks/outlinks are null on the single-doc GET, so the
            old KV rows here never rendered; the Links sub-tab now shows the real
            (edge-derived) directed counts instead. */}
        {doc.svg_count !== null && doc.svg_count !== undefined && doc.svg_count > 0 && (
          <KV label="svgs">{doc.svg_count}</KV>
        )}
        {doc.table_count !== null && doc.table_count !== undefined && doc.table_count > 0 && (
          <KV label="tables">{doc.table_count}</KV>
        )}
        {doc.code_block_count !== null && doc.code_block_count !== undefined && doc.code_block_count > 0 && (
          <KV label="code blocks">{doc.code_block_count}</KV>
        )}

        <h4>Index</h4>
        <button
          type="button"
          className="kb-pinsp__exclude"
          data-kb-act="exclude"
          disabled={excluding}
          onClick={() => void onExclude()}
          title="exclude this artifact from search + gallery (comments and reading history survive)"
        >
          ⊘ {excluding ? "excluding…" : "Exclude from index"}
        </button>
        <div className="kb-pinsp__hint">
          Reversible — re-include from Settings → Excluded.
        </div>
          </>
        )}

        {show("meta") && (
          <>
        {canEdit ? (
          <>
            <h4>Tags</h4>
            <div className="kb-pinsp__tags kb-pinsp__tags--edit">
              {tags.length === 0 && (
                <span className="kb-pinsp__tags-empty">no tags</span>
              )}
              {tags.map((t) => (
                <span
                  key={t}
                  className="kb-pinsp__tagchip"
                  style={{ borderColor: tagColor(t), color: tagColor(t) }}
                >
                  {/* v0.22 — the chip LABEL deep-links to the tag-filtered
                      gallery; the × still removes it (two distinct targets). */}
                  <Link
                    className="kb-pinsp__tagchip-link"
                    to={galleryUrl(kb, { tags: [t] })}
                    title={`filter the gallery by #${t}`}
                  >
                    {t}
                  </Link>
                  <button
                    type="button"
                    className="kb-pinsp__tagx"
                    onClick={() => removeTag(t)}
                    disabled={tagSaving}
                    title={`remove ${t}`}
                    aria-label={`remove tag ${t}`}
                  >
                    <Icon.X />
                  </button>
                </span>
              ))}
            </div>
            <input
              className="kb-pinsp__taginput"
              type="text"
              list="kb-pinsp-taglist"
              placeholder="add tag…"
              value={tagInput}
              disabled={tagSaving}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === ",") {
                  e.preventDefault();
                  addTag();
                }
              }}
              onBlur={() => {
                if (tagInput.trim()) addTag();
              }}
              aria-label="add tag"
            />
            <datalist id="kb-pinsp-taglist">
              {allTags.map((t) => (
                <option key={t} value={t} />
              ))}
            </datalist>

            <h4>Category</h4>
            <input
              className="kb-pinsp__catinput"
              type="text"
              list="kb-pinsp-catlist"
              placeholder="(none)"
              value={catInput}
              disabled={catSaving}
              onChange={(e) => setCatInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  (e.target as HTMLInputElement).blur();
                }
              }}
              onBlur={commitCategory}
              aria-label="artifact category"
            />
            <datalist id="kb-pinsp-catlist">
              {CATEGORY_SUGGESTIONS.map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>

            {metaErr && (
              <div className="kb-pinsp__meta-err" role="alert">
                {metaErr}
              </div>
            )}
          </>
        ) : (
          tags.length > 0 && (
            <>
              <h4>Tags</h4>
              <div className="kb-pinsp__tags">
                {tags.map((t) => (
                  <TagPill
                    key={t}
                    tag={t}
                    accent={t === accentTag}
                    to={galleryUrl(kb, { tags: [t] })}
                  />
                ))}
              </div>
            </>
          )
        )}

        {glyphs.length > 0 && (
          <>
            <h4>Capabilities</h4>
            <CapabilityGlyphs doc={doc} />
          </>
        )}
          </>
        )}

        {show("links") && outlinks.length > 0 && (
          <>
            <h4>Outlinks → {outlinks.length}</h4>
            {outlinks.map((n) => (
              <Link
                key={n.id}
                to={artifactHref(kb, n.source_relative)}
                className="kb-pinsp__nbr"
                title={`links to: ${n.title}`}
              >
                <span
                  className="kb-pinsp__nbr-dot"
                  style={{
                    background: n.accent ?? "var(--accent)",
                  }}
                />
                <span className="kb-pinsp__nbr-title">{n.title}</span>
                {n.distance !== undefined && (
                  <span className="kb-pinsp__nbr-d">
                    {n.distance.toFixed(2)}
                  </span>
                )}
              </Link>
            ))}
          </>
        )}

        {/* W3.E/S4 — Folder→Work: on a session artifact this tab shows the
            session's decisions/commits/research/files instead of sibling
            captures (measured noise, per the S4 evidence — a transcript's
            "folder" is just its own capture directory). */}
        {show("folder") && isSession && (
          <SessionWorkSection detail={sessionDetail} />
        )}
        {show("folder") && !isSession && showFolderBrowser && (
          <>
            <h4>
              Folder
              {folderPos > 0 ? (
                <span className="kb-pinsp__folder-pos">
                  {" · "}
                  {folderPos} of {folderTotal}
                </span>
              ) : null}
            </h4>
            {/* F2 — panel-local breadcrumb (sets cwd; not gallery links).
                F4 — "move" opens MoveArtifactModal for the OPEN artifact. */}
            <FolderBrowserNav
              kb={kb}
              cwd={cwd}
              docFolder={doc.folder}
              onFolderCwd={onFolderCwd}
              onMove={() => setMoveOpen(true)}
              indexNoteRel={cwdIndexNote?.source_relative}
              indexNoteTitle={cwdIndexNote?.title}
            />
            {moveOpen && (
              <MoveArtifactModal
                kb={kb}
                artifactId={doc.id}
                sourceRel={doc.source_relative}
                onClose={() => setMoveOpen(false)}
                onMoved={(newRel) => {
                  // Navigate to the new path-based permalink; old id is
                  // dead after the move. Preserve immersive/folio view when
                  // present (cheap URL carry — not a new state home).
                  let href = artifactHref(kb, newRel);
                  const view = searchParams.get("view");
                  if (view === "immersive" || view === "folio") {
                    href += `${href.includes("?") ? "&" : "?"}view=${view}`;
                  }
                  // replace so back doesn't land on the dead old id URL
                  navigate(href, { replace: true });
                }}
              />
            )}
            <div className="kb-pinsp__folder-meta">
              <select
                className="kb-pinsp__folder-sort"
                value={siblingsPrefs.sort}
                onChange={(e) =>
                  setSiblingsPrefs((p) => ({
                    ...p,
                    sort: parseSiblingSort(e.target.value),
                  }))
                }
                aria-label="sort folder list"
              >
                <option value="updated">updated</option>
                <option value="created">created</option>
                <option value="name">name</option>
                <option value="title">title</option>
              </select>
              <label className="kb-pinsp__folder-toggle">
                <input
                  type="checkbox"
                  checked={siblingsPrefs.includeSubfolders}
                  onChange={(e) =>
                    setSiblingsPrefs((p) => ({
                      ...p,
                      includeSubfolders: e.target.checked,
                    }))
                  }
                />
                <span>subfolders</span>
              </label>
            </div>
            <div className="kb-pinsp__folder-list">
              {/* Subfolder rows first (ambient — no checkbox gate). */}
              {subfolders.map((sf) => (
                <button
                  key={sf.path}
                  type="button"
                  className="kb-pinsp__folder-row kb-pinsp__folder-row--dir"
                  onClick={() => onFolderCwd?.(sf.path)}
                  title={`${sf.path} (${sf.count})`}
                >
                  <span className="kb-pinsp__folder-text">
                    <span className="kb-pinsp__folder-name">
                      <span className="kb-pinsp__folder-glyph" aria-hidden>
                        ▸
                      </span>
                      {sf.name}
                    </span>
                  </span>
                  <span className="kb-pinsp__folder-aside">
                    <span className="kb-pinsp__folder-count">{sf.count}</span>
                  </span>
                </button>
              ))}
              {folderRows.map((row) => {
                const isCurrent = row.id === doc.id;
                const nested = row.folder !== cwd;
                const subPrefix = nested
                  ? row.subPath.slice(0, row.subPath.length - row.filename.length)
                  : "";
                const dateUnix = siblingDateUnix(row, siblingsPrefs.sort);
                const dateLabel =
                  dateUnix != null ? compactDate(dateUnix) : null;
                return (
                  <Link
                    key={row.id}
                    to={artifactHref(kb, row.sourceRelative)}
                    className={`kb-pinsp__folder-row${isCurrent ? " is-current" : ""}${nested ? " kb-pinsp__folder-row--nested" : ""}`}
                    title={row.title || row.filename}
                    aria-current={isCurrent ? "page" : undefined}
                  >
                    <span className="kb-pinsp__folder-text">
                      <span className="kb-pinsp__folder-name">
                        {subPrefix && (
                          <span className="kb-pinsp__folder-prefix">
                            {subPrefix}
                          </span>
                        )}
                        {row.filename}
                      </span>
                      {row.title && row.title !== row.filename && (
                        <span className="kb-pinsp__folder-title">
                          {row.title}
                        </span>
                      )}
                    </span>
                    <span className="kb-pinsp__folder-aside">
                      {dateLabel && (
                        <span className="kb-pinsp__folder-date">{dateLabel}</span>
                      )}
                      {row.progress && (
                        <span className="kb-pinsp__folder-chip">
                          <ReadingChip
                            pct={row.progress.pct}
                            isDone={row.progress.isDone}
                            compact
                          />
                        </span>
                      )}
                    </span>
                  </Link>
                );
              })}
              {/* Empty states only once the browse query resolved —
                  `descendants` is undefined while loading (no flash). */}
              {descendants && folderRows.length === 0 && subfolders.length > 0 && (
                <div className="kb-pinsp__folder-empty">no files here</div>
              )}
              {descendants &&
                folderRows.length === 0 &&
                subfolders.length === 0 && (
                  <div className="kb-pinsp__folder-empty">empty folder</div>
                )}
            </div>
          </>
        )}

        {show("sessions") && (
          <>
        {/* W3.E/S4 — on a session artifact: what THIS session produced
         * (memories) + the real by-artifact join link into the /sessions
         * worklog. The filename-regex `SessionSelfLink` is DELETED — sid
         * recovery now goes through the canonical sqlite join (#11), same
         * as every other sessions read, so it works on renamed/imported
         * files the old regex silently missed. */}
        {isSession && sessionId && byArtifact?.session && (
          <SessionProducedSection
            session={byArtifact.session}
            memories={sessionDetail.memories}
          />
        )}

        {/* CT-A1 (U3 parse-back) — EXACT provenance: memories that name this
         * artifact as their highlight origin. Rendered ABOVE the
         * similarity-based panel below (renders nothing when empty). */}
        <LiftedFromPanel kb={kb} artifactId={doc.id} />

        {/* L10 — memories visible to this kb (V0010 global `*` + any
         * explicit link to this kb name). Read-mostly: each row links
         * out to its source artifact + offers a one-click "pin to
         * this kb" / "unpin" inline action. */}
        <RelatedMemoriesPanel kb={kb} query={memoryQuery} />

        {/* AS — the sessions that worked with this artifact, filterable by
         * read/wrote/edited; origin session badged; reasoning on the rows
         * that changed it. Exact match on the corpus-resolved artifact id.
         * For a session artifact this is almost always just its own capture
         * writes (measured noise, S4) — swapped out above. */}
        {!isSession && <SessionsTouchedPanel kb={kb} artifactId={doc.id} />}
          </>
        )}

        {/* W2.11 — the ONE home (invariant #30) for the generation-prompt
         * block is inside `BiographyTab`, at the head of this `story`
         * section (`PromptPanel`) — not a new rail icon/sub-tab. It rides
         * `show("story")` exactly like everything else here, so it also
         * appears for free inside the merged "all" tab. */}
        {show("story") && (
          <BiographyTab
            kb={kb}
            doc={doc}
            onComments={onComments}
            onVersions={onVersions}
            onCountChange={setStoryCount}
          />
        )}

        {show("links") && backlinks.length > 0 && (
          <>
            <h4>← Backlinks {backlinks.length}</h4>
            {backlinks.map((n) => (
              <Link
                key={n.id}
                to={artifactHref(kb, n.source_relative)}
                className="kb-pinsp__nbr"
                title={`links here: ${n.title}`}
              >
                <span
                  className="kb-pinsp__nbr-dot"
                  style={{ background: n.accent ?? "var(--accent)" }}
                />
                <span className="kb-pinsp__nbr-title">{n.title}</span>
              </Link>
            ))}
          </>
        )}
        {/* DCB W1.D — the Code section. Gated exactly like Outlinks/
            Backlinks above; `showCodeSection` covers BOTH the
            never-scanned state (§6 state 5 — "not scanned yet", never
            silently nothing) and the genuine-refs-present state, so it is
            deliberately NOT `codeRefs.length > 0` alone. */}
        {show("links") && showCodeSection && (
          <CodeRefsSection
            kb={kb}
            docId={doc.id}
            docPath={doc.source_relative}
            refCount={codeRefCount}
            extractedAt={codeRefsQuery.data?.extracted_at ?? null}
            codeRefsTruncated={codeRefsTruncated}
            neverScanned={neverScanned}
            codeUrl={codeUrl}
            repoParam={repoParam}
            onPickRepo={pickRepo}
            scorecardQuery={scorecardQuery}
            docLensQuery={docLensQuery}
          />
        )}
        {show("links") &&
          outlinks.length === 0 &&
          backlinks.length === 0 &&
          codeRefsQuery.data !== undefined &&
          codeRefsQuery.data.refs.length === 0 &&
          !neverScanned && (
            <div className="kb-pinsp__hint">No links to or from this artifact.</div>
          )}
      </div>
      )}
    </aside>
  );
}

function KV({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="kb-pinsp__kv">
      <span>{label}</span>
      <b>{children}</b>
    </div>
  );
}

// F2 — compact panel-local breadcrumb for the Folder browser. Each segment
// sets cwd (drill-up); root = "". Gallery pivot is a separate affordance
// via galleryUrl (#35) — deliberately not reusing FolderCrumbLinks, which
// always navigates to the gallery.
function FolderBrowserNav({
  kb,
  cwd,
  docFolder,
  onFolderCwd,
  onMove,
  indexNoteRel,
  indexNoteTitle,
}: {
  kb: string;
  cwd: string;
  docFolder: string;
  onFolderCwd?: (path: string) => void;
  /** F4 — open the move dialog for the open artifact. */
  onMove?: () => void;
  /** v0.33 Y4 — source_relative of cwd's index.md when present. */
  indexNoteRel?: string;
  indexNoteTitle?: string;
}) {
  const segs = cwd ? cwd.split("/").filter(Boolean) : [];
  const cumulative: string[] = [];
  segs.forEach((seg, i) => {
    cumulative.push(i === 0 ? seg : `${cumulative[i - 1]}/${seg}`);
  });
  const awayFromDoc = cwd !== docFolder;
  const galleryTo = cwd ? galleryUrl(kb, { folder: cwd }) : galleryUrl(kb);
  return (
    <div className="kb-pinsp__folder-nav">
      <nav className="kb-pinsp__folder-crumbs" aria-label="folder location">
        <button
          type="button"
          className={`kb-pinsp__folder-crumb${cwd === "" ? " is-here" : ""}`}
          onClick={() => onFolderCwd?.("")}
          title="kb root"
          disabled={!onFolderCwd}
        >
          (root)
        </button>
        {segs.map((seg, i) => (
          <span key={cumulative[i]} className="kb-pinsp__folder-crumb-wrap">
            <span className="kb-pinsp__folder-crumb-sep" aria-hidden>
              /
            </span>
            <button
              type="button"
              className={`kb-pinsp__folder-crumb${cwd === cumulative[i] ? " is-here" : ""}`}
              onClick={() => onFolderCwd?.(cumulative[i])}
              title={cumulative[i]}
              disabled={!onFolderCwd}
            >
              {seg}
            </button>
          </span>
        ))}
      </nav>
      <div className="kb-pinsp__folder-nav-acts">
        {indexNoteRel && (
          <Link
            className="kb-pinsp__folder-note"
            to={artifactHref(kb, indexNoteRel)}
            title={indexNoteTitle || "folder note"}
            data-kb-act="folder-note"
          >
            note
          </Link>
        )}
        {awayFromDoc && onFolderCwd && (
          <button
            type="button"
            className="kb-pinsp__folder-here"
            onClick={() => onFolderCwd(docFolder)}
            title={
              docFolder
                ? `back to ${docFolder}`
                : "back to this file's folder (root)"
            }
          >
            this file
          </button>
        )}
        {onMove && (
          <button
            type="button"
            className="kb-pinsp__folder-move"
            onClick={onMove}
            title="move or rename this artifact"
            data-kb-act="move-artifact"
          >
            move
          </button>
        )}
        <Link
          className="kb-pinsp__folder-gallery"
          to={galleryTo}
          title={
            cwd
              ? `open ${cwd} in gallery (+ subfolders)`
              : "open kb root in gallery"
          }
        >
          gallery
        </Link>
      </div>
    </div>
  );
}

// v0.22 — the artifact id row: shows the 12-char prefix and copies the FULL id
// to the clipboard on click (toast feedback per invariant #32). A button, not a
// bare truncation, so the (otherwise un-selectable) id is one click to grab.
function IdCopy({ id }: { id: string }) {
  return (
    <button
      type="button"
      className="kb-pinsp__id-copy"
      title="copy full artifact id"
      aria-label="copy full artifact id"
      onClick={() => {
        navigator.clipboard
          ?.writeText(id)
          .then(() => toast.ok("id copied"))
          .catch(() => toast.err("couldn't copy id"));
      }}
    >
      {id.slice(0, 12)}
    </button>
  );
}

function shortFilename(doc: DocSummary): string {
  const fn = doc.source_relative.split("/").pop() ?? doc.source_relative;
  if (fn.length <= 28) return fn;
  return fn.slice(0, 25) + "…";
}

function formatUnix(unix: number): string {
  const d = new Date(unix * 1000);
  return d.toISOString().slice(0, 16).replace("T", " ");
}

type NeighborRow = {
  id: string;
  source_relative: string;
  title: string;
  distance?: number;
  accent?: string;
};

// Resolve a list of artifact ids to neighbor rows (title + path + accent),
// dropping any that fail to fetch. Shared by Outlinks + Backlinks. The
// directed dedup scan that feeds this (and the always-visible link-count
// badge) lives inline in PreviewInspector so the badge needs no doc fetches;
// this fan-out only runs when the Links/About tab is actually shown. Corpus-
// local edges only (invariant #29). Capped to bound the per-row doc fan-out.
async function resolveRows(
  ids: string[],
  kb: string,
  signal: AbortSignal,
): Promise<NeighborRow[]> {
  if (ids.length === 0) return [];
  const rows = await Promise.all(
    ids.map(async (id) => {
      try {
        const d = await fetchDoc(kb, id, signal);
        const ts = tagsFor(d);
        const accent = ts[0] ? tagColor(ts[0]) : undefined;
        return {
          id: d.id,
          source_relative: d.source_relative,
          title: d.title,
          accent,
        } as NeighborRow;
      } catch {
        return null;
      }
    }),
  );
  return rows.filter((r): r is NeighborRow => r !== null);
}

// Transcript → /sessions backlink. When the open artifact is itself a
// captured session transcript, the canonical session id is the trailing
// segment of the `session-<ts>-<sid>.html` filename (invariant #27 — the
// source-relative name is stable). Surface a jump to its worklog record so
// the reader ↔ /sessions link is bidirectional.
// W3.E/S4 — Folder→Work: the session's decisions/commits/research/files,
// reusing `useSessionDetail`'s already-fetched data (the same combined query
// the /sessions rail uses — "existing per-sid hooks", no new fetch surface).
// A deliberately compact rendering (not the full `sessions.tsx` sections) —
// S3's shared `SessionFactsSections.tsx` extraction is DEFERRED (see the W3
// build report); this is functionally equivalent, just its own small JSX.
function SessionWorkSection({
  detail,
}: {
  detail: ReturnType<typeof useSessionDetail>;
}) {
  const { decisions, commits, research, files, loading } = detail;
  if (loading) return <p className="kb-pinsp__hint">loading…</p>;
  const edited = files.filter((f) => f.action !== "read");
  if (
    decisions.length === 0 &&
    commits.length === 0 &&
    research.length === 0 &&
    edited.length === 0
  ) {
    return <p className="kb-pinsp__hint">no decisions, commits, or edits recorded.</p>;
  }
  return (
    <>
      {commits.length > 0 && (
        <>
          <h4>Commits {commits.length}</h4>
          <ul className="kb-pinsp__folder-list">
            {commits.map((c, i) => (
              <li key={i} className="kb-pinsp__hint">
                {c.sha && <code>{c.sha}</code>} {c.subject ?? c.kind}
              </li>
            ))}
          </ul>
        </>
      )}
      {decisions.length > 0 && (
        <>
          <h4>Decisions {decisions.length}</h4>
          <ul className="kb-pinsp__folder-list">
            {decisions.map((d, i) => (
              <li key={i} className="kb-pinsp__hint">
                {d.kind === "plan" ? "✓ plan approved" : d.prompt}
              </li>
            ))}
          </ul>
        </>
      )}
      {edited.length > 0 && (
        <>
          <h4>Files edited {edited.length}</h4>
          <ul className="kb-pinsp__folder-list">
            {edited.slice(0, 20).map((f, i) => (
              <li key={i} className="kb-pinsp__hint">
                {f.basename}
              </li>
            ))}
          </ul>
        </>
      )}
      {research.length > 0 && (
        <>
          <h4>Research {research.length}</h4>
          <ul className="kb-pinsp__folder-list">
            {research.map((r, i) => (
              <li key={i} className="kb-pinsp__hint">
                {r.kind}: {r.query}
              </li>
            ))}
          </ul>
        </>
      )}
    </>
  );
}

// W3.E/S4/S3 — Sessions→memories-produced + the real by-artifact self-link.
// Replaces the DELETED filename-regex `SessionSelfLink`: the sid comes from
// the by-artifact join (#11's canonical sqlite lookup — works on renamed/
// imported files the old `session-<ts>-<sid>.html` regex silently missed).
// W3.E/S4/S3 — Sessions→memories-produced. "ONE action home" (synthesis-
// memo R11/S-S3): the worklog + replay jumps now live SOLELY on the
// `SessionContextCard` above the iframe (S3) — this rail section no longer
// duplicates them (it used to, via the deleted `SessionSelfLink`'s worklog
// link + an equivalent replay link); it shows only what's genuinely rail-
// shaped (the produced-memories list + the session identity line).
function SessionProducedSection({
  session,
  memories,
}: {
  session: { harness: string; commit_count: number; ended_at: number };
  memories: { id: string; kb: string; title: string; source_relative: string }[];
}) {
  // W5/R9b/LF-1 Tier 0 — the outcome section header's staleness/presence
  // chip: derived-only, no server probe.
  const presence = sessionPresence(session);
  return (
    <>
      {memories.length > 0 && (
        <>
          <h4>Memories produced {memories.length}</h4>
          <ul className="kb-pinsp__folder-list">
            {memories.map((m) => (
              <li key={`${m.kb}:${m.id}`}>
                <Link
                  className="kb-pinsp__kv-link"
                  to={artifactHref(m.kb, m.source_relative)}
                >
                  {m.title || m.id}
                </Link>
              </li>
            ))}
          </ul>
        </>
      )}
      <h4>
        Session <span title={session.harness}>{harnessGlyph(session.harness)}</span>
        {commitBadge(session.commit_count) && (
          <span> {commitBadge(session.commit_count)}</span>
        )}
        {" "}
        <span
          className={`kb-pinsp__session-presence kb-pinsp__session-presence--${presence.status}`}
          title={presence.copy}
          data-testid="session-inspector-presence"
        >
          {presence.copy}
        </span>
      </h4>
      <p className="kb-pinsp__hint">
        worklog + replay live on the session card above the transcript.
      </p>
    </>
  );
}

// AS — "Sessions": every Claude Code session that worked with this artifact,
// filterable by what it did (wrote / edited / read). Matched EXACTLY on the
// corpus-resolved artifact id (no same-name false positives), so each row is
// genuinely about this file. The origin session (the one the artifact was born
// in, via <meta kb-session>) gets a badge. Reasoning (prompt + decisions +
// commits) is carried on the rows that changed it. Lazy + SSE-refreshed via
// useArtifactSessions; hidden entirely when nothing touched the artifact
// (keeps the rail quiet for static docs).
type SessionFilter = "all" | "wrote" | "edited" | "read";

function SessionsTouchedPanel({
  kb,
  artifactId,
}: {
  kb: string;
  artifactId: string;
}) {
  const { sessions, loading } = useArtifactSessions(kb, artifactId);
  const [filter, setFilter] = useState<SessionFilter>("all");

  // "Wrote" counts creation: an explicit Write OR the origin session (which may
  // have authored the file out-of-band, with no in-transcript tool call).
  const counts = useMemo(
    () => ({
      all: sessions.length,
      wrote: sessions.filter((s) => s.wrote || s.authored).length,
      edited: sessions.filter((s) => s.edited).length,
      read: sessions.filter((s) => s.read).length,
    }),
    [sessions],
  );

  if (loading || sessions.length === 0) return null;

  const shown = sessions.filter((s) => {
    switch (filter) {
      case "wrote":
        return s.wrote || s.authored;
      case "edited":
        return s.edited;
      case "read":
        return s.read;
      default:
        return true;
    }
  });

  // All is always offered; the action pills appear only when they'd select
  // something, so a read-only artifact doesn't show an empty "Edited 0".
  const pills = (
    [
      { key: "all", label: "All", n: counts.all },
      { key: "wrote", label: "Wrote", n: counts.wrote },
      { key: "edited", label: "Edited", n: counts.edited },
      { key: "read", label: "Read", n: counts.read },
    ] as { key: SessionFilter; label: string; n: number }[]
  ).filter((p) => p.key === "all" || p.n > 0);

  return (
    <>
      <h4>Sessions · {counts.all}</h4>
      <div
        className="kb-pinsp__sess-filters"
        role="tablist"
        aria-label="Filter sessions by action"
      >
        {pills.map((p) => (
          <button
            key={p.key}
            type="button"
            role="tab"
            aria-selected={filter === p.key}
            className={`kb-pinsp__sess-pill ${filter === p.key ? "is-on" : ""}`}
            onClick={() => setFilter(p.key)}
          >
            {p.label} <span className="kb-pinsp__sess-pill-n">{p.n}</span>
          </button>
        ))}
      </div>
      {shown.length === 0 ? (
        <div className="kb-pinsp__hint">No sessions {filter} this artifact.</div>
      ) : (
        shown.map((s) => {
          // Reasoning preview: a shipped commit beats a decision beats the bare
          // prompt (all "detected, not ground truth"). Empty for read-only rows.
          const snippet =
            s.commits?.[0]?.subject ??
            s.decisions?.[0]?.answer ??
            s.decisions?.[0]?.prompt ??
            null;
          return (
            <Link
              key={`${s.kb}:${s.session_id}`}
              to={`/sessions?focus=${encodeURIComponent(s.session_id)}`}
              className="kb-pinsp__why-row"
              title={`${actionSummary(s)} · ${s.kb}`}
            >
              <span
                className="kb-pinsp__session-dot"
                style={{ background: sessionColorFor(s.session_id) }}
                aria-hidden
              />
              <span className="kb-pinsp__why-text">
                <span className="kb-pinsp__sess-head">
                  <span className="kb-pinsp__session-name">
                    {s.display_name}
                  </span>
                  {/* CT-A6 — join-tier honesty: this panel is the exact
                      `session_files.target_artifact_id` join (see the atlas
                      overlay's own "inferred · text scan" note for the
                      OTHER, fuzzier transcript-scan signal). Reuses the
                      origin badge's chip class — same visual weight, no
                      new CSS. */}
                  <span
                    className="kb-pinsp__origin-badge"
                    title="Exact tier: tool-call-verified session_files join, not a transcript text-scan guess"
                  >
                    exact
                  </span>
                  {s.authored && (
                    <span
                      className="kb-pinsp__origin-badge"
                      title="this artifact was created in this session"
                    >
                      <Icon.Star aria-hidden="true" /> origin
                    </span>
                  )}
                </span>
                {s.first_user_prompt && (
                  <span className="kb-pinsp__why-prompt">
                    {s.first_user_prompt}
                  </span>
                )}
                {snippet && (
                  <span className="kb-pinsp__why-snippet">→ {snippet}</span>
                )}
              </span>
              <span className="kb-pinsp__sess-meta">
                <ActionChips s={s} />
                <span className="kb-pinsp__sess-ago">
                  {agoUnix(s.started_at)}
                </span>
              </span>
            </Link>
          );
        })
      )}
    </>
  );
}

// The action badges on a session row: W(rote) · E(dited) · R(ead). Only the
// actions actually taken are shown; an origin-only session (no file row) shows
// none — the ★ badge already conveys it.
function ActionChips({
  s,
}: {
  s: { read: boolean; wrote: boolean; edited: boolean };
}) {
  return (
    <span className="kb-pinsp__act-chips" aria-hidden>
      {s.wrote && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--wrote">W</span>
      )}
      {s.edited && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--edited">E</span>
      )}
      {s.read && (
        <span className="kb-pinsp__act-chip kb-pinsp__act-chip--read">R</span>
      )}
    </span>
  );
}

// Human summary of a session's relationship to the artifact (for the row title).
function actionSummary(s: {
  read: boolean;
  wrote: boolean;
  edited: boolean;
  authored: boolean;
}): string {
  const parts: string[] = [];
  if (s.authored) parts.push("created");
  if (s.wrote && !s.authored) parts.push("wrote");
  if (s.edited) parts.push("edited");
  if (s.read) parts.push("read");
  return parts.length ? parts.join(" · ") : "touched";
}

// Compact "Nd ago" for unix seconds — rail-friendly. (commentFmt.relTime is for
// kb-comments ISO timestamps; sessions carry unix seconds.)
function agoUnix(unix: number): string {
  if (!unix) return "";
  const sec = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (sec < 60) return `${sec}s ago`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
  return `${Math.floor(sec / 86400)}d ago`;
}
