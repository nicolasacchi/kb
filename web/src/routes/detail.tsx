import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  type Anchor,
  ApiError,
  fetchDocByPath,
  fetchDocsPage,
  fetchFolders,
  type DocSummary,
} from "../api/client";
import { sse } from "../api/sse";
import { features } from "../config/features";
import ContextBar from "../components/chrome/ContextBar";
import DeskChangedBanner from "../components/chrome/DeskChangedBanner";

import QueueBar from "../components/lists/QueueBar";
import PreviewInspector, {
  type FolderEntry,
} from "../components/PreviewInspector";
import { FolioHeader, FolioColophon } from "../components/FolioChrome";
import type {
  BridgeApi,
  SelectionAnchor,
} from "../components/AnnotatorBridge";
import ArtifactPane, {
  useArtifactVisit,
  type PaneSelection,
} from "../components/reader/ArtifactPane";
import CommentsPanel from "../components/CommentsPanel";
import VersionsPanel from "../components/VersionsPanel";
import NoteEditor from "../components/NoteEditor";
import { useReview } from "../hooks/useReview";
import { useIsMobile } from "../hooks/useIsMobile";
import { useBodyScrollLock } from "../hooks/useBodyScrollLock";
import { useArtifactHostSuffix } from "../hooks/useArtifactHost";
import { useReadingProgress } from "../hooks/useReadingProgress";
import { useReading } from "../hooks/useReading";
import { useLiveReading } from "../hooks/useLiveReading";
import type { ReadingSummary } from "../api/reading";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { artifactOrigin } from "../lib/artifactHost";
import { artifactHref } from "../lib/artifactHref";
import { childFolders } from "../lib/folderTree";
import { parseAtParam } from "../lib/memento";
import { formatPane2, parsePane2, samePane, type PaneLoc } from "../lib/paneUrl";
import { isEditableTarget } from "../lib/keymap";
import {
  consumeFlowSeed,
  flowDropAbove,
  flowPeek,
  flowPop,
  flowPush,
  flowSnapshot,
  subscribeFlow,
  writeFlowSeed,
} from "../lib/flowStack";
import { toast } from "../lib/toast";

// Detail view — iframed artifact + floating chrome + (multi-file) pages
// panel. v0.2 adds:
//   - pen-icon mode toggle on the FloatingPill (when features.comments)
//   - ?cm=on is appended whenever the kb has comments enabled (NOT
//     gated on the live annotate-mode toggle) so the daemon injects
//     the annotator script + comments envelope at first load and the
//     iframe URL never changes on toggle — preserves in-iframe tab /
//     <details> / scroll state across pencil clicks. Annotate-on/off
//     flows purely through the `cm:mode` postMessage path the
//     AnnotatorBridge re-asserts on every cm:probe.
//   - AnnotatorBridge mounted to relay cm:add → POST + cm:flash/refresh/mode
//   - CommentsPanel rendered to the right of the iframe in annotate mode
export default function Detail() {
  // Track U — the permalink is `/a/:kb/*` where the splat is the
  // artifact's source-relative path (e.g. `ideas/foo/bar.html`). The
  // artifact id (used for the iframe origin + history/review) is no
  // longer in the URL; we resolve it from the path via the by-path
  // endpoint and read it off the fetched doc. The multi-page active
  // sub-page rides as a `?p=` query param.
  const params = useParams<{ kb: string; "*": string }>();
  const { kb } = params;
  const relPath = params["*"] ?? "";
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  // Z4 — one-shot gate for the inbox `?panel=comments` deep-link (consumed
  // once on mount, then stripped from the URL so the action-model owns panel
  // state thereafter — invariant #30).
  const panelDeepLinkRef = useRef(false);
  // The annotator handle the comments dock drives (flash / emphasize). Owned
  // here because the DOCK is the route-level singleton; `ArtifactPane` hands
  // it to the AnnotatorBridge it mounts.
  // W3.P-b — ONE bridge per pane, resolved through the focused pane by
  // `focusedBridgeRef` below. (Two panes sharing one ref would have the second
  // mount silently clobber the first's handle, so every dock flash/emphasize
  // would land in the wrong artifact.)
  const bridgeRef = useRef<BridgeApi | null>(null);
  const bridge2Ref = useRef<BridgeApi | null>(null);
  // Pending "file removed" timer — see the live-updates effect below.
  const removalTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // TQ1 — the by-path doc rides the query cache (["doc", kb, relPath]);
  // a route change swaps keys so `doc` drops to undefined synchronously
  // (the old setDoc(null) reset) while the new fetch resolves. The SSE
  // bridge invalidates ["doc", kb] on artifact churn. Meta edits patch
  // the cache in place via `patchDoc` below (optimistic; the reindex's
  // artifact.indexed reconciles).
  const queryClient = useQueryClient();
  const docQuery = useQuery({
    queryKey: ["doc", kb, relPath] as const,
    enabled: !!kb && !!relPath,
    queryFn: ({ signal }) => fetchDocByPath(kb as string, relPath, signal),
  });
  const doc = docQuery.data ?? null;
  const patchDoc = useCallback(
    (patch: Partial<DocSummary>) => {
      queryClient.setQueryData<DocSummary>(["doc", kb, relPath], (d) =>
        d ? { ...d, ...patch } : d,
      );
    },
    [queryClient, kb, relPath],
  );
  // Track U — the artifact id, resolved from the path via the doc. Null
  // until the by-path fetch lands (or if it 404s). Everything downstream
  // (iframe origin, history, review, sibling nav) keys on this.
  const id = doc?.id ?? null;
  const error = docQuery.error ? String(docQuery.error) : null;
  const [annotateMode, setAnnotateMode] = useState(false);
  // Panel visibility — decoupled from the pencil (annotateMode). Persists
  // across in-session artifact navigation (NOT reset in the path effect
  // below); the panel content re-keys to the open artifact via useReview.
  const [panelOpen, setPanelOpen] = useState(false);
  // Track V — Versions/Diff panel visibility. Mutually exclusive with the
  // comments panel for the right column (toggling one closes the other).
  const [versionsOpen, setVersionsOpen] = useState(false);
  // W2.13 — errata-slip display toggle, owned here (not inside CommentsPanel)
  // because the sheet it drives (ErrataSheet) mounts as a SIBLING of the
  // iframe, outside the comments dock entirely. A skin, not a new panel home
  // (#30): it persists across in-session artifact navigation exactly like
  // `panelOpen` above (not reset in the path-change effect below), and is
  // independent of whether the comments dock is currently open — turning it
  // on shows the sheet whenever there are open comments, panel open or not.
  const [errataMode, setErrataMode] = useState(false);
  // v0.23 — mobile reader-tools sheet gate. Re-scoped: this is now "the mobile
  // bottom sheet is open in ANY panelMode" (inspect / comments / versions), not
  // "inspector-mode only". The single ContextBar "inspect" toggle flips it; the
  // merged rail (inspector sub-tabs + comments + versions) rides inside the
  // sheet so every panel switches from within it. Orthogonal to panelMode — the
  // toggle handlers below no longer force it closed, so switching bodies keeps
  // the sheet open. Inert on desktop (the rail is the docked column regardless;
  // `.detail--inspector-open` has no desktop CSS rule).
  const [inspectorOpen, setInspectorOpen] = useState(false);
  // v0.23 — gate the sheet's scrim DOM node + role=dialog semantics to mobile
  // so the desktop reader DOM stays byte-identical (the rail is a plain docked
  // <aside> there; e2e drives it at 1280×720).
  const isMobile = useIsMobile();
  // SH.B2 — lock body scroll while the mobile reader-tools sheet is up.
  // Gated on mobile (like MobileDrawer) since `.detail--inspector-open`'s
  // sheet CSS is mobile-only (#30) — on desktop the rail is the always-
  // docked column, never a scroll-locking overlay.
  useBodyScrollLock(isMobile && inspectorOpen);

  // ── W3.P-b — the second pane (two-pane artifact COMPARE mode) ─────────
  //
  // Derived PURELY from `?pane2=` with a `useMemo`. There is deliberately NO
  // separate "is a split open" React state: that is the discipline that keeps
  // back/forward and permalink-sharing honest (#23) — the same shape `?view=`
  // already uses above. `parsePane2` is TOTAL (malformed ⇒ null), so a
  // hand-mangled param degrades to "no split" rather than a half-built pane.
  //
  // THE SCOPE RULING: only an ARTIFACT may occupy a pane — no search pane, no
  // gallery pane, no list pane. The general "any view in any pane" version
  // collides with four singletons (`useScrollRestoration` keys on
  // pathname+search and assumes WINDOW scroll — #31; `useRovingCursor` owns
  // ONE unscoped window keydown listener; `useDocumentTitle`/`lastGalleryUrl`
  // are app singletons; every route view sits behind a `React.lazy` boundary
  // that exists to keep CodeMirror/the atlas out of first paint).
  //
  // MOBILE NEVER SPLITS. The v0.23 contract is ONE button, ONE sheet, and two
  // cross-origin iframes each wanting the full viewport height does not
  // degrade by stacking — so the rule is simply "single pane, ignore ?pane2".
  // The param is left untouched, so widening the viewport restores the split.
  const pane2Param = searchParams.get("pane2");
  const pane2Loc = useMemo(() => parsePane2(pane2Param), [pane2Param]);
  // Refuse a self-split. Two panes on the SAME artifact would share one
  // origin, and `isOriginOfArtifact` — the only guard that can tell two
  // artifact iframes apart — would then be unable to attribute a
  // `kb:scroll`/`kb:reading` beacon to one pane's append-only visit row
  // (#8/#19). Cheap here, structurally impossible to get wrong later.
  const splitOpen =
    !!pane2Loc &&
    !isMobile &&
    !samePane(pane2Loc, { kb: kb ?? "", sourceRelative: relPath });
  // Same by-path query shape (and the same ["doc", kb, rel] cache key family)
  // as the primary doc above, so the SSE bridge's ["doc", kb] invalidation
  // covers both panes for free (#23).
  const pane2Query = useQuery({
    queryKey: ["doc", pane2Loc?.kb, pane2Loc?.sourceRelative] as const,
    enabled: splitOpen,
    queryFn: ({ signal }) =>
      fetchDocByPath(
        pane2Loc?.kb as string,
        pane2Loc?.sourceRelative as string,
        signal,
      ),
  });
  const pane2Doc = splitOpen ? (pane2Query.data ?? null) : null;
  const pane2Id = pane2Doc?.id ?? null;
  const pane2Error =
    splitOpen && pane2Query.error ? String(pane2Query.error) : null;
  // Which pane owns the keyboard + feeds the single inspector rail. Plain UI
  // state: NOT the URL (a focus ring is not worth a history entry, and a
  // shared link shouldn't dictate it) and NOT server state (#23).
  const [focusedPane, setFocusedPane] = useState<1 | 2>(1);
  // Mirror of the primary's `reloadNonce` — the second pane's iframe is
  // cross-origin too, so a reindex can only be reflected by remounting it.
  const [pane2Nonce, setPane2Nonce] = useState(0);
  // Collapsing the split (or a viewport that drops below the breakpoint) must
  // never strand focus on a pane that is no longer rendered.
  useEffect(() => {
    if (!splitOpen) setFocusedPane(1);
  }, [splitOpen]);
  const focused2 = splitOpen && focusedPane === 2;

  // Track H — comment whose panel row is "active" (its in-page marker was
  // clicked). Drives the row highlight + scroll-into-view in CommentsPanel.
  const [activeCommentId, setActiveCommentId] = useState<string | null>(null);
  // Anchor the user clicked inside the artifact (cm:compose) — opens the
  // panel's inline composer (the shared tabbed editor) for a new comment.
  const [composeAnchor, setComposeAnchor] = useState<Anchor | null>(null);
  // W2.16 — a non-collapsed text selection made inside the artifact
  // (cm:selection, mode-independent — see AnnotatorBridge). Drives the
  // SelectionActions floater (cite / add-to-list) the pane renders.
  // `sectionId` is frozen at capture time by the pane (it owns the
  // `kb:section` shadow), not re-read reactively. Held here because the
  // chooser is one-at-a-time across the reader and the Esc layer chain
  // below treats it as the topmost layer.
  const [selection, setSelection] = useState<PaneSelection | null>(null);
  // Bumped on `artifact.indexed` for this file — folded into the iframe
  // `key` to force a remount (the artifact is cross-origin, so the
  // parent can't call `contentWindow.location.reload()`).
  const [reloadNonce, setReloadNonce] = useState(0);
  // N-track — notes (kb_category=note) render NATIVELY (interactive
  // checklist + inline editor) instead of the cross-origin iframe. `rawView`
  // falls back to the rendered-artifact iframe so the served-HTML permalink
  // is still reachable.
  const [rawView, setRawView] = useState(false);
  // Set true when this file is deleted from disk (after a grace window).
  const [removed, setRemoved] = useState(false);
  // MB3 — immersive reading. Hides all chrome (header / ribbon / ctxbar /
  // statusbar) so the artifact iframe owns the viewport. A floating button
  // toggles it (mobile) and Esc exits. The flag rides a `body.kb-immersive`
  // class so global chrome outside this route hides via CSS without any
  // prop threading.
  //
  // v0.22 — immersive is now URL-derived (`?view=immersive`) so it is
  // SHAREABLE + restorable (one of the three explicit viewing modes:
  // embedded / immersive / bare). The iframe `src`/`key` is id-based, so
  // toggling `?view` never reloads the artifact (replace:true keeps one
  // history entry).
  const immersive = searchParams.get("view") === "immersive";
  // CT-F6 — `?at=<unix>`: read this artifact AS OF an instant (RFC 7089's
  // per-resource Memento). URL-derived like `?view=`, so the coordinate is
  // shareable and survives a reload; TOTAL parse, so a malformed value
  // degrades to the ordinary reader rather than a fabricated instant.
  // Per-artifact only — it selects a VERSION of the open doc and nothing
  // else (no filtering, no ranking, no corpus timeline).
  const atUnix = parseAtParam(searchParams.get("at"));
  const setImmersive = useCallback(
    (on: boolean) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (on) next.set("view", "immersive");
          else next.delete("view");
          return next;
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );
  useEffect(() => {
    document.body.classList.toggle("kb-immersive", immersive);
    return () => document.body.classList.remove("kb-immersive");
  }, [immersive]);
  useEffect(() => {
    if (!immersive) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setImmersive(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [immersive, setImmersive]);
  // v0.23 — Esc closes the mobile reader-tools sheet (mirrors the drawer /
  // immersive dismiss). Only mounts while the sheet is open; skips while typing
  // in an input/textarea/composer so it never eats an editor keystroke, and
  // skips modifier chords. Disjoint from the immersive Esc effect above (the
  // ContextBar that opens the sheet is hidden in immersive, so both are never
  // open at once).
  useEffect(() => {
    if (!inspectorOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable)
      ) {
        return;
      }
      e.preventDefault();
      setInspectorOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inspectorOpen]);
  // G5 — per-artifact reading-progress map (built from history). Used
  // to enrich descendants below; the PreviewInspector's Folder section
  // renders a per-row ReadingChip from each entry's `progress`.
  const progress = useReadingProgress(kb);
  // RP-track — cross-visit reading summary for the open artifact; drives the
  // TocSpy heatmap + the inspector "Read by you" block. Null until the fetch
  // lands / when there's no capture, so both consumers degrade silently.
  const readingSummary = useReading(kb, id);

  // Authoritative artifact-subdomain suffix from /api/identity. The
  // heuristic in artifactHost.ts can't recover this when the SPA is
  // reached via a bare IP, so we use the daemon's own value.
  const hostSuffix = useArtifactHostSuffix();

  // W2.7 — folio: an optional reading DENSITY (centered measure + a running
  // header/colophon), not a chrome-hiding mode like immersive — the
  // ContextBar/ribbon stay visible. Joins the SAME `?view=` URL grammar
  // (mutually exclusive with immersive: both read/write one param, so
  // toggling either clears the other) — shareable, and the iframe `key` stays
  // id-based so toggling never reloads the artifact. No keybind: `f` is
  // reserved for the Wave-2 hint-mode toggle elsewhere.
  const folio = searchParams.get("view") === "folio";
  const setFolio = useCallback(
    (on: boolean) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (on) next.set("view", "folio");
          else next.delete("view");
          return next;
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );
  // (The folio running header's live `kb:section`/`kb:toc` join lives in
  // `ArtifactPane` — it is per-artifact, and its origin guard has to name the
  // artifact exactly.)

  // R1 — live reading progress from the existing scroll/reading postMessage
  // stream (client-only, no new network/SSE). Merged with the server values
  // below (max(), so the live single-visit value never pulls the cross-visit
  // truth down). Called at the top (id may be null) to keep the hook order stable.
  const { liveProgress, liveSummary } = useLiveReading(id, kb, hostSuffix);

  useDocumentTitle(doc?.title ?? null);

  // Track U — multi-page active sub-page from `?p=` (e.g. `chapter2.html`).
  // Empty string at the entrypoint; the artifact's own path is the route
  // splat, so the page can't share the path without ambiguity. (The pane
  // reads the same param for its own iframe URL + pages nav; this copy only
  // feeds the native-note path's bare URL below.)
  const activePage = searchParams.get("p") ?? "";
  const reviewActive = features.comments && !!kb && !!id;
  const review = useReview(kb ?? "", id ?? "");
  // W3.P-b — the second pane's own review file. A SECOND `useReview` rather
  // than re-pointing the first: each pane's AnnotatorBridge paints markers
  // from the file it is handed, so one shared (focus-following) file would
  // repaint the UNFOCUSED pane with the other artifact's comments. Disabled
  // (empty kb/id ⇒ `enabled: false` inside the hook) when there's no split.
  const pane2ReviewActive = features.comments && !!pane2Loc && !!pane2Id;
  const review2 = useReview(pane2Loc?.kb ?? "", pane2Id ?? "");

  // ── the focused pane feeds the ONE inspector rail (invariant #30) ─────
  // "One home per action; ONE merged inspector rail." ContextBar IS
  // duplicated per pane (it is per-artifact chrome); the rail is NOT — there
  // is exactly one `.kb-pinsp` for the whole reader, and it names whichever
  // pane holds focus. No second rail, and no 7th sub-tab: the count stays 6.
  const focusedKb = focused2 && pane2Loc ? pane2Loc.kb : (kb ?? "");
  // (The focused artifact ID is read off `focusedDoc` at the use sites — the
  // dock only renders once `focusedDoc` is non-null, so there is no separate
  // nullable id to keep in sync.)
  const focusedDoc = focused2 ? pane2Doc : doc;
  const focusedReview = focused2 ? review2 : review;
  const focusedReviewActive = focused2 ? pane2ReviewActive : reviewActive;
  const focusedBridgeRef = focused2 ? bridge2Ref : bridgeRef;

  useEffect(() => {
    if (!kb || !relPath) return;
    // Fresh artifact — clear prior per-artifact UI state. The doc itself
    // is query-cache-keyed on (kb, relPath), so it drops to null and
    // re-resolves without manual plumbing; `id` (derived) goes null with
    // it, parking the id-keyed effects (history/open, iframe origin)
    // until the new fetch lands.
    setRemoved(false);
    setReloadNonce(0);
    setRawView(false);
    // (The pane clears its own per-artifact signals — TOC / active section —
    // in its own reset effect, which runs first: child effects before parent.)
    // Clear per-artifact review UI state so a stale row isn't left active
    // and a half-typed compose for the old artifact can't bleed into the
    // new one. `panelOpen` is intentionally NOT reset — it persists across
    // navigation so the panel stays open as the user moves between files.
    setActiveCommentId(null);
    setComposeAnchor(null);
    // W2.16 — a fresh artifact's selection chooser is meaningless (the
    // iframe is about to remount, dropping any DOM selection anyway).
    setSelection(null);
    // v0.23 — a fresh artifact should never inherit an open sheet covering it
    // (panelMode persists via panelOpen, so re-opening restores the last body).
    setInspectorOpen(false);
    // W3.P-b — a fresh primary artifact starts focused on pane 1. (The split
    // itself rides `?pane2=`, and every in-app artifact navigation rebuilds
    // the URL through `artifactHref`, so it drops out on its own.)
    setFocusedPane(1);
    // Z4 — re-arm the `?panel=comments` deep-link consumer for the new
    // artifact (so a second inbox → reader hop in the same Detail mount
    // re-opens the sheet on mobile).
    panelDeepLinkRef.current = false;
  }, [kb, relPath]);

  // ── link-flow — the reading FLOW stack (lib/flowStack.ts) ─────────────
  //
  // Following a link out of an artifact is a descent; the chip / `u` /
  // browser Back are the ascent. Three effects, and their DECLARATION ORDER
  // is load-bearing (React runs effects in declaration order):
  //
  //   A  capture   — on a primary-artifact change: is this a RETURN (the
  //                  top of the stack is the artifact we just landed on)?
  //                  Then pop it and park its scroll offset as the seed.
  //                  Otherwise push the artifact we just LEFT, with the live
  //                  offset the pane published. Reads `prevArtifactRef`,
  //                  which effect C below only updates AFTERWARDS — that is
  //                  why it must run first.
  //   B  consume   — claim the seed for this artifact (once) and hand it to
  //                  the pane as a prop. Runs after A so a return seeded in
  //                  the same commit is picked up immediately; going through
  //                  storage (rather than straight from A's local variable)
  //                  is what makes a return survive a Detail REMOUNT.
  //   C  remember  — record the artifact now on screen as "where we came
  //                  from" for the next navigation, with its resolved title.
  //
  // The stack is per-tab sessionStorage; nothing here touches the URL (#35)
  // or the scroll-restoration slots (#31 — that hook owns WINDOW scroll on
  // the gallery/search; this is the artifact IFRAME's offset, a different
  // axis entirely).
  const flowEntries = useSyncExternalStore(
    subscribeFlow,
    flowSnapshot,
    flowSnapshot,
  );
  /// The primary pane's live scroll/section, published by ref (no renders).
  const flowStateRef = useRef<{ y: number; sec: string | null }>({
    y: 0,
    sec: null,
  });
  /// The artifact this route showed BEFORE the current one.
  const prevArtifactRef = useRef<{
    kb: string;
    sourceRelative: string;
    id: string;
    title: string;
  } | null>(null);
  /// The consumed return seed handed down to the primary pane. It carries
  /// the artifact it belongs TO, not a bare number, for two reasons: a seed
  /// must never leak onto the next artifact, and effect B below can re-run
  /// against an already-consumed slot (React StrictMode double-invokes
  /// effects in dev) — matching on identity keeps the second pass a no-op
  /// instead of wiping the seed the first pass just claimed.
  const [flowSeed, setFlowSeed] = useState<{
    kb: string;
    sourceRelative: string;
    y: number;
  } | null>(null);

  // A — capture.
  useEffect(() => {
    if (!kb || !relPath) return;
    const prev = prevArtifactRef.current;
    const live = flowStateRef.current;
    const top = flowPeek();
    if (top && top.kb === kb && top.sourceRelative === relPath) {
      // A RETURN — by the chip, by `u`, or by the browser's own Back button
      // (all three land here, which is exactly why the detection is "does
      // the stack's top match where we ARE" rather than a flag one of those
      // three paths would have to set).
      //
      // Deliberately NOT gated on `prev` being set: leaving the reader
      // entirely (→ gallery, which UNMOUNTS this route and resets `prev`)
      // and coming back to the artifact on top of the stack is a return
      // too. Gating on `prev` there left the stack un-popped and the chip
      // pointing at the artifact already on screen — one click that could
      // never clear itself.
      const popped = flowPop();
      if (popped) {
        writeFlowSeed({
          kb: popped.kb,
          sourceRelative: popped.sourceRelative,
          y: popped.scrollY,
        });
      }
    } else if (prev && (prev.kb !== kb || prev.sourceRelative !== relPath)) {
      // A DESCENT — remember where we were, at the offset we left it.
      flowPush({
        kb: prev.kb,
        id: prev.id,
        sourceRelative: prev.sourceRelative,
        title: prev.title,
        scrollY: live.y,
        ...(live.sec ? { sec: live.sec } : {}),
        ts: Date.now(),
      });
    }
    // The live signal belongs to the artifact we just left; the new pane
    // republishes on its first `kb:scroll`.
    flowStateRef.current = { y: 0, sec: null };
  }, [kb, relPath]);

  // B — consume the seed for THIS artifact (once).
  useEffect(() => {
    if (!kb || !relPath) return;
    const seed = consumeFlowSeed(kb, relPath);
    if (seed) {
      setFlowSeed(seed);
      return;
    }
    // No (unclaimed) seed: keep one already claimed for THIS artifact, drop
    // anything belonging to the artifact we just left.
    setFlowSeed((prev) =>
      prev && prev.kb === kb && prev.sourceRelative === relPath ? prev : null,
    );
  }, [kb, relPath]);

  // C — remember the artifact now on screen (re-runs when its title/id
  // resolve, so a descent captured later carries a real title).
  useEffect(() => {
    if (!kb || !relPath) return;
    prevArtifactRef.current = {
      kb,
      sourceRelative: relPath,
      id: id ?? "",
      title: doc?.title || relPath,
    };
  }, [kb, relPath, id, doc?.title]);

  /// The ONE return implementation, shared by the chip and the `u` keybind:
  /// navigate to the stack's top and let effect A recognise the arrival as a
  /// return (pop + seed). A plain router push — one history entry per
  /// artifact (#20) — and deliberately WITHOUT the entry's `?sec=`: the
  /// seeded scroll offset is strictly more precise than the section, and a
  /// `sec` param would out-rank it in the pane's probe ladder.
  const flowBack = useCallback(() => {
    const top = flowPeek();
    if (!top) return;
    navigate(artifactHref(top.kb, top.sourceRelative));
  }, [navigate]);
  /// The popover's jump: drop the `depth` rows above the target so it
  /// becomes the top, then return to it through the same path.
  const flowJump = useCallback(
    (depth: number) => {
      const target = flowDropAbove(depth);
      if (!target) return;
      navigate(artifactHref(target.kb, target.sourceRelative));
    },
    [navigate],
  );

  // Z4 — inbox deep-link: `?panel=comments` opens the comments panel on the
  // FIRST mount, then is stripped from the URL (replace, so it leaves no
  // history entry and can't re-fire on back/forward). Runs after the
  // path-reset effect above so its `setInspectorOpen(false)` doesn't undo the
  // mobile-sheet open. Guarded once per mount; the ContextBar action-model
  // (#30) owns panel state after this hand-off.
  //
  // W1.reader — an optional `?comment=<id>` rides alongside `?panel=comments`
  // (built by lib/quote.ts's citation links). Once the panel is open, jump
  // to that comment the same way clicking its in-page marker does
  // (AnnotatorBridge's onFocusComment below): activate the row — which
  // scrolls + highlights it inside the panel, and works for the native-note
  // view too, where there's no iframe to flash — and best-effort flash its
  // in-artifact anchor. The iframe-side flash can race a fresh mount (the
  // artifact may not have loaded yet); same best-effort caveat as the `?sec=`
  // deep link above. The row activation has no such race and is the primary
  // signal, so this degrades gracefully either way.
  //
  // CT-F6 — `?panel=versions` joins the same one-shot: it is the SAME
  // action ("which reader panel opens on mount"), so it rides the same
  // param rather than growing a second deep-link flag (#30's one-home
  // rule). It pairs with `?at=`, which is deliberately NOT stripped — the
  // panel state is transient, but the Memento coordinate is the shareable
  // question the link was asking, and VersionsPanel re-reads it on every
  // render.
  useEffect(() => {
    if (panelDeepLinkRef.current) return;
    const panel = searchParams.get("panel");
    if (panel !== "comments" && panel !== "versions") return;
    panelDeepLinkRef.current = true;
    setPanelOpen(panel === "comments");
    setVersionsOpen(panel === "versions");
    if (isMobile) setInspectorOpen(true);
    const commentId = searchParams.get("comment");
    if (panel === "comments" && commentId) {
      setActiveCommentId(commentId);
      bridgeRef.current?.flash(commentId);
    }
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.delete("panel");
        next.delete("comment");
        return next;
      },
      { replace: true },
    );
  }, [searchParams, isMobile, setSearchParams]);

  // (The `?sec=` live jump + the whole `history.open` visit lifecycle —
  // visitRef, the scroll/reading flush guards, the final-flush cleanup — now
  // live in `ArtifactPane`, one per pane. Invariants #8/#19: a visit row
  // belongs to exactly one artifact, so the state that feeds it must belong
  // to exactly one pane.)

  // F2 — panel-local browse location ("cwd") for the Folder tab browser.
  // Ephemeral React state: defaults to the open doc's folder, RESET when
  // the open artifact changes (keyed on doc.id). NOT URL/localStorage —
  // a sidebar browser, deliberately ephemeral. Lifted here so the
  // siblings query stays the ONE fetch home (inspector stays fetch-free
  // for this surface, #23 spirit).
  // Reset-on-id-change uses the React "adjust state during render" pattern
  // so the siblings query never fires one frame against a stale cwd.
  const docFolder = doc?.folder;
  const [folderCwd, setFolderCwd] = useState<string>("");
  const [folderCwdDocId, setFolderCwdDocId] = useState<string | null>(null);
  if (doc && doc.id !== folderCwdDocId) {
    // Queue state for the next render; cwd below uses doc.folder this frame
    // so the siblings query never keys off a stale browse location.
    setFolderCwdDocId(doc.id);
    setFolderCwd(doc.folder);
  }
  const cwd =
    doc == null
      ? ""
      : doc.id === folderCwdDocId
        ? folderCwd
        : doc.folder;

  // Map a slim docs page into FolderEntry[] relative to `base` (the folder
  // the page was fetched for). Shared by the browse list and the nav list.
  const mapFolderEntries = useCallback(
    (
      page: { docs: DocSummary[] } | undefined,
      base: string,
      isError: boolean,
    ): FolderEntry[] | undefined => {
      if (isError) return [];
      if (!page) return undefined;
      return page.docs.map((d) => {
        const fn = d.path.split("/").pop() ?? d.path;
        let subPath: string;
        if (d.folder === base) {
          subPath = fn;
        } else if (base === "") {
          subPath = `${d.folder}/${fn}`;
        } else {
          subPath = `${d.folder.slice(base.length + 1)}/${fn}`;
        }
        return {
          id: d.id,
          sourceRelative: d.source_relative,
          title: d.title,
          filename: fn,
          subPath,
          folder: d.folder,
          isCurrent: d.id === id,
          mtime: d.mtime_unix ?? null,
          indexed: d.indexed_at_unix ?? null,
          ...(d.created_unix != null ? { created: d.created_unix } : {}),
          // v0.33 Y2 — stable created-sort anchor (see siblingSort.createdOrMtime).
          ...(d.first_indexed_unix != null
            ? { firstIndexed: d.first_indexed_unix }
            : {}),
          progress: progress.get(d.id),
        };
      });
    },
    [id, progress],
  );

  // Same-folder + descendant artifacts for the PreviewInspector's
  // Folder browser (G3/G4/G7 + F2), slim projection (S5 — on a 50k kb
  // this is a few KB instead of ~10 MB). TQ1: rides the query cache
  // under the ["docs", kb] prefix, so the SSE bridge's burst-gated
  // artifact.indexed/removed invalidation (and the gap resync) keeps it
  // live. Keyed on the panel cwd (F2), not just the open doc's folder.
  const siblingsQuery = useQuery({
    queryKey: ["docs", kb, "siblings", cwd] as const,
    enabled: !!kb && !!doc,
    queryFn: ({ signal }) =>
      fetchDocsPage(kb as string, {
        folder: cwd || undefined,
        projection: "slim",
        offset: 0,
        limit: 500,
        signal,
      }),
  });
  // Each entry carries the direct-vs-nested marker (`folder`/`subPath`)
  // the section needs, plus optional reading `progress` (G5) — enriched
  // here (not in the queryFn) so a progress update never refetches.
  const descendants = useMemo(
    () =>
      doc
        ? mapFolderEntries(siblingsQuery.data, cwd, siblingsQuery.isError)
        : undefined,
    [doc, mapFolderEntries, siblingsQuery.data, siblingsQuery.isError, cwd],
  );

  // ContextBar + `[`/`]` hotkeys need the OPEN DOC's folder siblings even
  // while the panel browses elsewhere. Same query key family — when
  // cwd === doc.folder React Query dedupes to one network request.
  const navSiblingsQuery = useQuery({
    queryKey: ["docs", kb, "siblings", docFolder] as const,
    enabled: !!kb && docFolder !== undefined && cwd !== docFolder,
    queryFn: ({ signal }) =>
      fetchDocsPage(kb as string, {
        folder: docFolder || undefined,
        projection: "slim",
        offset: 0,
        limit: 500,
        signal,
      }),
  });
  const navDescendants = useMemo(() => {
    if (!doc || docFolder === undefined) return undefined;
    if (cwd === docFolder) return descendants;
    return mapFolderEntries(
      navSiblingsQuery.data,
      docFolder,
      navSiblingsQuery.isError,
    );
  }, [
    doc,
    docFolder,
    cwd,
    descendants,
    mapFolderEntries,
    navSiblingsQuery.data,
    navSiblingsQuery.isError,
  ]);

  // Folders tree (same cache key as useFacetCounts) → immediate children
  // of cwd for subfolder rows. Pure helper; count is descendant-inclusive.
  const foldersQuery = useQuery({
    queryKey: ["folders", kb],
    enabled: !!kb,
    queryFn: ({ signal }) => fetchFolders(kb as string, signal),
    staleTime: Infinity,
  });
  const folderChildren = useMemo(
    () => childFolders(foldersQuery.data?.folders ?? [], cwd),
    [foldersQuery.data, cwd],
  );
  const onFolderCwd = useCallback((path: string) => {
    setFolderCwd(path);
  }, []);

  // A-w1 (wave-1 wire-don't-build) — ContextBar's position/total/onPrev/onNext
  // props already render fully (ContextBar.tsx) but no caller ever passed
  // them. Feed them from the SAME direct-sibling walk as the `[`/`]` hotkeys
  // (now in `ArtifactPane`, folder-bounded + filename-sorted) so the header
  // counter and the keyboard shortcut can never disagree. Wraps around like
  // the hotkey does; undefined (hides the counter + buttons) when there's
  // nothing to page through, mirroring the hotkey's own `direct.length <= 1`
  // bail. Computed here — not in the pane — because the native-note render
  // path below has no pane and needs the same counter.
  //
  // W3.P-b — split into the walk itself + its two consumers (the ContextBar
  // counter and the split target) so both read ONE ordering.
  // F2 — always walks the OPEN DOC's folder (navDescendants), never the
  // panel browse cwd.
  const siblingWalk = useMemo(() => {
    if (!kb || !doc || !navDescendants) return undefined;
    const direct = [
      ...navDescendants.filter((d) => d.folder === doc.folder),
    ].sort((a, b) => a.filename.localeCompare(b.filename));
    if (direct.length <= 1) return undefined;
    const idx = direct.findIndex((d) => d.isCurrent);
    if (idx < 0) return undefined;
    return { direct, idx };
  }, [kb, doc, navDescendants]);
  const siblingNav = useMemo(() => {
    if (!kb || !siblingWalk) return undefined;
    const { direct, idx } = siblingWalk;
    const prev = direct[(idx - 1 + direct.length) % direct.length];
    const next = direct[(idx + 1) % direct.length];
    return {
      position: idx + 1,
      total: direct.length,
      onPrev: () => navigate(artifactHref(kb, prev.sourceRelative)),
      onNext: () => navigate(artifactHref(kb, next.sourceRelative)),
    };
  }, [kb, siblingWalk, navigate]);

  // W3.P-b — what "open beside" opens: the NEXT direct sibling in the folder,
  // off the very same walk, so the verb has one obvious meaning. Two reasons
  // it is never the CURRENT artifact: (a) comparing a document with itself is
  // not the feature, and (b) two identical artifact ORIGINS would defeat
  // `isOriginOfArtifact` — the exact-origin guard is the only thing that can
  // attribute a `kb:scroll`/`kb:reading` beacon to one pane's visit row
  // (#8/#19), and it works on the origin, not the frame. `direct.length > 1`
  // (enforced by the walk) guarantees `next !== current`.
  const splitTarget = useMemo<PaneLoc | null>(() => {
    if (!kb || !siblingWalk) return null;
    const { direct, idx } = siblingWalk;
    const next = direct[(idx + 1) % direct.length];
    return next ? { kb, sourceRelative: next.sourceRelative } : null;
  }, [kb, siblingWalk]);

  // Every pane URL write is `{ replace: true }` — toggling a split is a
  // viewport arrangement, not a place; littering history with it would make
  // the browser back button un-navigate the reader instead of leaving the
  // artifact.
  const openSplit = useCallback(
    (loc: PaneLoc) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          next.set("pane2", formatPane2(loc));
          return next;
        },
        { replace: true },
      );
      setFocusedPane(2);
    },
    [setSearchParams],
  );
  const closeSplit = useCallback(() => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.delete("pane2");
        return next;
      },
      { replace: true },
    );
    setFocusedPane(1);
  }, [setSearchParams]);
  // The primary offers "open beside" only when there IS somewhere to open and
  // we're not on a phone; the second pane always offers "close this pane".
  const canSplit = !isMobile && !!splitTarget && !splitOpen;

  // The `w`-prefix pane chord (`w v` split · `w q` close · `w h`/`w l` focus
  // left/right · `w o` only). Two measured dead ends this deliberately walks
  // around — see the "Panes" block in lib/keymap.ts for the full note:
  //   * `Ctrl-w` is unavailable (HotkeyRoot bails on any modifier; Ctrl/⌘-W is
  //     the browser's close-tab).
  //   * these must NOT be registered at `scope: "global"` — `useRovingCursor`
  //     runs an independent window keydown listener, so a global `w h`/`w l`
  //     would ALSO move the gallery cursor (`preventDefault()` on one listener
  //     does not stop a sibling listener).
  // So the registry row is doc-only (the `?` cheat sheet) and the real chord
  // is this small independent ref+timer handler, cloned from the `y`-then-`p`
  // pattern in ArtifactPane and guarded by `isEditableTarget`.
  useEffect(() => {
    const pending = { current: false };
    let timer: ReturnType<typeof setTimeout> | null = null;
    const disarm = () => {
      pending.current = false;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    };
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (pending.current) {
        const k = e.key.toLowerCase();
        if (k === "v" || k === "q" || k === "h" || k === "l" || k === "o") {
          e.preventDefault();
          disarm();
          if (k === "v") {
            if (!isMobile && splitTarget) openSplit(splitTarget);
          } else if (k === "q" || k === "o") {
            if (splitOpen) closeSplit();
            else setFocusedPane(1);
          } else if (k === "h") {
            setFocusedPane(1);
          } else if (k === "l") {
            if (splitOpen) setFocusedPane(2);
          }
          return;
        }
        // Not a pane verb — drop the prefix, then let this same keystroke
        // still arm a fresh `w` below (mirrors the y-chord's fall-through).
        disarm();
      }
      // link-flow — `u` = up out of this link descent (the chip's keybind;
      // `n`/`p` are TAKEN by the reading-list trail queue). Same guards as
      // the pane chords above; silent when the stack is empty, since the
      // chip that advertises the action isn't rendered then either.
      if (e.key === "u" || e.key === "U") {
        if (!flowPeek()) return;
        e.preventDefault();
        flowBack();
        return;
      }
      if (e.key === "w" || e.key === "W") {
        e.preventDefault();
        pending.current = true;
        timer = setTimeout(() => {
          pending.current = false;
          timer = null;
        }, 800);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      if (timer) clearTimeout(timer);
    };
  }, [isMobile, splitOpen, splitTarget, openSplit, closeSplit, flowBack]);

  // Live filesystem updates for the viewed artifact. The id is the
  // hash of the source-relative path, so editing the file keeps the
  // id — `artifact.indexed` just means "reload the iframe in place".
  // A removal is debounced first: editors and git ops routinely
  // delete-then-recreate, and an `artifact.indexed` for the same id
  // arriving within the grace window cancels the pending "removed".
  //
  // v0.7.1 P2 — matches on artifact_id, not path. Two reasons: paths
  // can be relative or absolute depending on the daemon's path-prefix
  // config (the `path` field on events is the source-relative one;
  // doc.path can differ on Windows / with symlinks), while the id is
  // path-derived and stable. And per-id matching naturally filters
  // the initial-walk `artifact.indexed` flood for other artifacts —
  // gallery.tsx needs its own debounce because it cares about every
  // event, but detail.tsx only cares about *this* artifact.
  useEffect(() => {
    if (!kb || !id) return;
    const offIndexed = sse.subscribeEvent("artifact.indexed", (p) => {
      if (p.kb !== kb || p.artifact_id !== id) return;
      if (removalTimer.current) {
        clearTimeout(removalTimer.current);
        removalTimer.current = null;
      }
      setRemoved(false);
      setReloadNonce((n) => n + 1);
      // Refresh the doc so the inspector's metadata (tags / category /
      // metrics) reflects the re-index — covers external edits (CLI, a
      // file edit) and reconciles a local tag/category patch with the
      // authoritative index. The SSE bridge also invalidates ["doc", kb]
      // behind its burst gate; this targeted invalidation skips the gate
      // for the artifact the user is looking at.
      void queryClient.invalidateQueries({ queryKey: ["doc", kb, relPath] });
    });
    const offRemoved = sse.subscribeEvent("artifact.removed", (p) => {
      if (p.kb !== kb || p.artifact_id !== id) return;
      if (removalTimer.current) clearTimeout(removalTimer.current);
      removalTimer.current = setTimeout(() => {
        removalTimer.current = null;
        setRemoved(true);
      }, 1500);
    });
    return () => {
      offIndexed();
      offRemoved();
      if (removalTimer.current) {
        clearTimeout(removalTimer.current);
        removalTimer.current = null;
      }
    };
  }, [kb, id, relPath]);

  // W3.P-b — the same live-reindex watch for the second pane, minus the
  // removal debounce (a compare pane whose file vanishes just stops
  // refreshing; the primary owns the "artifact removed" screen).
  useEffect(() => {
    if (!pane2Loc || !pane2Id) return;
    return sse.subscribeEvent("artifact.indexed", (p) => {
      if (p.kb !== pane2Loc.kb || p.artifact_id !== pane2Id) return;
      setPane2Nonce((n) => n + 1);
      void queryClient.invalidateQueries({
        queryKey: ["doc", pane2Loc.kb, pane2Loc.sourceRelative],
      });
    });
  }, [pane2Loc, pane2Id, queryClient]);

  // Push annotate mode into the iframe whenever it toggles. The bridge
  // handles missed-on-load by re-sending on cm:probe (annotator's first
  // hello) — but the simple path here is to re-send on every change.
  // W3.P-b — the FOCUSED pane's bridge: `annotateMode` is a route-level flag
  // and the pencil acts on whichever artifact the reader is working in.
  useEffect(() => {
    if (!focusedReviewActive) return;
    focusedBridgeRef.current?.setMode(annotateMode);
  }, [annotateMode, focusedReviewActive, focusedBridgeRef]);

  // Repaint the iframe's markers/highlights whenever the review file
  // changes — a panel edit/resolve/reply, a routed new comment, or an SSE
  // refetch. (Previously only the in-iframe cm:add path self-refreshed;
  // routing creation through the panel needs this general sync.)
  useEffect(() => {
    if (!reviewActive || !review.file) return;
    bridgeRef.current?.refresh(review.file);
  }, [reviewActive, review.file]);
  // W3.P-b — the same repaint for the second pane, driven by ITS review file.
  useEffect(() => {
    if (!pane2ReviewActive || !review2.file) return;
    bridge2Ref.current?.refresh(review2.file);
  }, [pane2ReviewActive, review2.file]);

  // (The iframe↔parent message pump — scroll/reading flush, the kb-probe
  // resume + reading seed, `open-artifact` / `pm:page` — is `ArtifactPane`'s.
  // Its origin guard is EXACT (`isOriginOfArtifact`), which is what makes a
  // second pane's beacons unable to land in this artifact's visit row.)

  // Vim-style Esc: drop one layer at a time. Drafting → cancel draft;
  // pencil-on → turn pencil off (panel stays open, mirrors
  // handleToggleAnnotate's off-path). Declared before the early returns
  // below so the hook count stays stable across renders (React #310).
  const handleEscAnnotate = useCallback(() => {
    // W2.16 — the selection chooser is the topmost layer when open;
    // SelectionActions delegates Esc to this chain rather than listening
    // itself (invariant #30 — one home per action), so returning here is
    // what stops annotate mode / the compose draft from also dropping in
    // the same keypress.
    if (selection) {
      setSelection(null);
      return;
    }
    if (composeAnchor) {
      setComposeAnchor(null);
      return;
    }
    if (annotateMode) {
      setAnnotateMode(false);
    }
  }, [selection, composeAnchor, annotateMode]);

  // Parent-window Esc: catches keystrokes when SPA chrome (not the
  // iframe) holds focus. Only mounted while there's a layer to drop;
  // textarea Esc is handled by the textarea itself (which stops
  // propagation), so we don't intercept those.
  useEffect(() => {
    if (!annotateMode && !composeAnchor && !selection) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable)
      ) {
        return;
      }
      e.preventDefault();
      handleEscAnnotate();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [annotateMode, composeAnchor, selection, handleEscAnnotate]);

  // (The `o` / `b` / `y p` reader keybinds live in `ArtifactPane` — they act
  // on ONE artifact, so they bind only while that pane is focused.)

  if (!kb || !relPath) {
    return <div className="empty">missing artifact path in URL</div>;
  }
  // Track U — `id` is resolved asynchronously from the path. While the
  // by-path fetch is in flight `id` is null; show a loading state. A
  // resolution failure (e.g. a stale hash URL after the clean cutover,
  // or a deleted file) surfaces as an error here.
  if (!id) {
    if (error) {
      // A 404 is often "captured but not indexed YET" (the capture toast's
      // "Open" lands here inside the watcher's debounce window). The SSE
      // bridge invalidates ["doc", kb] on artifact.indexed, so this query
      // refetches and the page swaps to the artifact on its own — say so
      // instead of presenting a dead end.
      if (docQuery.error instanceof ApiError && docQuery.error.status === 404) {
        return (
          <div className="detail__error" role="alert">
            Nothing indexed at <code>{relPath}</code> — if it was just
            captured or moved here, it will appear automatically once the
            indexer picks it up.
          </div>
        );
      }
      return (
        <div className="detail__error" role="alert">
          {error}
        </div>
      );
    }
    return <div className="empty">loading…</div>;
  }

  // The BARE artifact URL (same protocol + port as the parent, on the
  // daemon's configured suffix — see `lib/artifactHost.ts`). The pane builds
  // its own iframe `src` from the same pieces (plus `?cm=on`); this copy is
  // for the native-note path below, which renders no iframe but still offers
  // "view the rendered artifact".
  const artifactBase = artifactOrigin(id, kb, hostSuffix);
  const baseUrl = activePage
    ? `${artifactBase}/${activePage.replace(/^\//, "")}`
    : `${artifactBase}/`;

  // Pencil ↔ panel are linked but not identical: turning the pencil ON
  // also opens the panel; turning it OFF leaves the panel open; closing
  // the panel also turns the pencil off (annotating without the panel
  // visible would be confusing). Read closure state directly rather than
  // nesting setState updaters.
  // v0.23 — these no longer touch `inspectorOpen`: on mobile the sheet stays
  // open while the reader switches between comments / versions / inspector tabs
  // from inside its rail (the old setInspectorOpen(false) calls were the
  // "two systems fight" that made each mobile sheet a railless dead-end).
  const handleToggleAnnotate = () => {
    const next = !annotateMode;
    setAnnotateMode(next);
    if (next) {
      setPanelOpen(true);
      setVersionsOpen(false);
    }
  };
  const handleTogglePanel = () => {
    const next = !panelOpen;
    setPanelOpen(next);
    if (next) {
      setVersionsOpen(false);
    }
    if (!next) setAnnotateMode(false);
  };
  // Track V — versions panel toggle; mutually exclusive with comments.
  const handleToggleVersions = () => {
    const next = !versionsOpen;
    setVersionsOpen(next);
    if (next) {
      setPanelOpen(false);
      setAnnotateMode(false);
    }
  };
  // W2.13 — errata-slip toggle. Purely a display-mode flip; it never touches
  // panelOpen/versionsOpen (the sheet is independent of which dock is open).
  const handleToggleErrata = () => setErrataMode((v) => !v);
  // Shared jump/hover — the SAME actions a panel row's "↗ jump" button and
  // hover glow use (onRequestFlash/onHoverComment below), reused verbatim by
  // ErrataSheet so there is exactly one jump/flash implementation.
  // W3.P-b — the dock speaks to the FOCUSED pane's annotator, so a jump from
  // the comments list flashes the artifact the list is actually showing.
  const handleCommentJump = (commentId: string) => {
    focusedBridgeRef.current?.flash(commentId);
  };
  const handleCommentHover = (commentId: string | null) => {
    setActiveCommentId(null);
    focusedBridgeRef.current?.emphasize(commentId ?? "", commentId != null);
  };

  // R4 — the PULL trigger. Reaches the focused pane's annotator through the
  // SAME bridge handle the dock already drives flash/emphasize through (no
  // new registration plumbing): "tell me what's selected in there right now".
  // The answer lands asynchronously in `handleSelectionPull` below, on the
  // pane's own origin-checked listener.
  //
  // Why this exists at all when SelectionActions' own comment button already
  // does the job on paper: that button only exists while the PUSH relay has
  // the selection state raised, and the push relay is hostage to each
  // engine's selection-event ordering — on Gecko a tap collapses the
  // selection before `click`, so the bar can vanish under the finger that
  // was reaching for it. Pulling is immune: nothing about the iframe's event
  // timing can stop an explicit tap on chrome the SPA itself owns, and
  // annotate.ts answers from its cached anchor when the live range is
  // already gone.
  const composeFromSelection = () => {
    focusedBridgeRef.current?.querySelection();
  };
  // The answer. Runs the SAME body as each pane's `onComposeSelection`
  // (compose + raise the panel + raise the phone sheet, and pointedly NOT
  // `setAnnotateMode(true)` — see that handler's comment for why a
  // selection-initiated compose must never arm tap-anywhere-to-compose).
  const handleSelectionPull = (
    pane: 1 | 2,
    anchor: SelectionAnchor | null,
  ) => {
    if (!anchor) {
      // Invariant #32 — a user action that cannot proceed SAYS SO. Silence
      // here would read as "the button is broken", which is precisely the
      // impression the Firefox bug already made.
      toast.err("select some text in the artifact first");
      return;
    }
    setFocusedPane(pane);
    setPanelOpen(true);
    setInspectorOpen(true);
    setComposeAnchor(anchor);
    setSelection(null);
  };

  const openCount = focusedReview.file
    ? focusedReview.file.comments.filter((c) => c.status === "open").length
    : 0;
  // Each pane's own ContextBar badge (per-artifact chrome — the badge must
  // name ITS artifact, not whatever the dock is currently showing).
  const primaryOpenCount = review.file
    ? review.file.comments.filter((c) => c.status === "open").length
    : 0;
  const pane2OpenCount = review2.file
    ? review2.file.comments.filter((c) => c.status === "open").length
    : 0;

  const showPanel = focusedReviewActive && panelOpen;
  // Track V — Versions panel claims the right column when open and the
  // comments panel isn't (comments wins if somehow both are set).
  const showVersions = !showPanel && versionsOpen && focusedDoc !== null;
  // P2 — show the read-only PreviewInspector when neither panel claims the
  // right column.
  const showInspector = !showPanel && !showVersions && focusedDoc !== null;
  // v0.23 — the mobile sheet is mode-agnostic: `.detail--inspector-open` gates
  // the bottom sheet in ANY panelMode (comments/versions/inspect), so it rides
  // on all three modifiers, not just with-inspector. Inert on desktop.
  const sheetClass = inspectorOpen ? " detail--inspector-open" : "";
  // W2.7 — folio is an independent modifier (not mutually exclusive with
  // panel/versions/inspector — a reader can open comments mid-folio-read).
  const folioClass = folio ? " detail--folio" : "";
  // W3.P-b — an independent modifier like folio; the pane GRID itself lives on
  // `.detail__panes` (only rendered when split), so this is purely a hook for
  // mode-aware chrome tweaks and never changes the single-pane cascade.
  const splitClass = splitOpen ? " detail--split" : "";
  const detailClass =
    (showPanel
      ? `detail detail--with-panel${sheetClass}`
      : showVersions
        ? `detail detail--with-versions${sheetClass}`
        : showInspector
          ? `detail detail--with-inspector${sheetClass}`
          : "detail") +
    folioClass +
    splitClass;
  const docProgress = doc ? progress.get(doc.id) : undefined;
  // R1 — what the inspector/TocSpy actually show: the live value while scrolling,
  // but never below the server's cross-visit truth (live is single-visit).
  const shownProgress = liveProgress
    ? {
        pct: Math.max(liveProgress.pct, docProgress?.pct ?? 0),
        isDone: liveProgress.isDone || (docProgress?.isDone ?? false),
      }
    : docProgress;
  const shownSummary: ReadingSummary | null = liveSummary
    ? {
        ...liveSummary,
        completion_pct: Math.max(
          liveSummary.completion_pct,
          readingSummary?.completion_pct ?? 0,
        ),
        read_pct: Math.max(liveSummary.read_pct, readingSummary?.read_pct ?? 0),
        visit_count: readingSummary?.visit_count ?? liveSummary.visit_count,
        stopped_at: readingSummary?.stopped_at ?? liveSummary.stopped_at,
        first_read_at: readingSummary?.first_read_at ?? null,
        last_read_at: readingSummary?.last_read_at ?? null,
      }
    : readingSummary;

  // P1 — ContextBar above the iframe carries breadcrumb + per-artifact
  // actions (annotate, panel, share, copy-link). Sibling navigation
  // ([/]) plus the Folder section in the PreviewInspector rail handle
  // lateral movement; the retired FloatingPill is gone.
  const filename = relPath.includes("/") ? relPath.slice(relPath.lastIndexOf("/") + 1) : relPath;

  // N-track — a note renders natively (no cross-origin iframe): interactive
  // checklist + inline editor, fed by the same-origin JSON API. The id +
  // permalink are unchanged; `rawView` swaps back to the served-HTML iframe
  // (falls through to the normal return below). PreviewInspector + the
  // Versions panel still work (they read `doc`, no iframe needed).
  // Mirror the server's `notes::is_note`: a note is *Markdown* + category
  // note. HTML artifacts merely tagged `note` are ordinary content and must
  // render via the iframe (the notes API 404s on them), so require `.md`.
  const isNote =
    doc?.kb_category === "note" && !!doc?.source_relative?.endsWith(".md");
  if (isNote && !rawView && !removed) {
    // Comments thread in-place on the native note view via the same
    // CommentsPanel the iframe path uses — but file-scope only (no iframe
    // annotator, so the iframe-coupled props are omitted; they're optional).
    // `showPanel`/`showVersions`/`showInspector` are the same right-column
    // selectors computed above (comments > versions > inspector).
    //
    // W2.16 — the highlight → cite/list chooser is NOT wired here. It
    // rides annotate.ts's cm:selection capture (mode-independent, but
    // still iframe-only), and there's no iframe on the native-note path.
    // A parity version over window.getSelection() directly against the
    // rendered/editor DOM (a different CodeMirror-vs-rendered-view
    // selection story, and a different css_path-analogue anchor scheme)
    // is a real gap for this phase, not a "cheap to also do" — flagged in
    // the build report rather than attempted here.
    return (
      <div className={detailClass}>
        {/* Opening a note is a VISIT too (#8/#19). The iframe path gets that
            from `ArtifactPane`; this path has no pane, so run the same
            lifecycle here through a zero-DOM helper rather than letting the
            note quietly stop appearing in history. */}
        <NoteVisit kb={kb} id={id} />
        <div className="detail__main">
          {doc && (
            <ContextBar
              kb={kb}
              id={id}
              title={doc.title || filename}
              folder={doc.folder}
              filename={filename}
              isIndex={false}
              sourceRelative={doc.source_relative}
              position={siblingNav?.position}
              total={siblingNav?.total}
              onPrev={siblingNav?.onPrev}
              onNext={siblingNav?.onNext}
              onFullscreen={() => setImmersive(true)}
              bareUrl={baseUrl}
              reviewActive={reviewActive}
              annotateMode={false}
              commentCount={openCount}
              inspectorOpen={inspectorOpen}
              onToggleInspector={() => setInspectorOpen((v) => !v)}
              folio={folio}
              onToggleFolio={() => setFolio(!folio)}
              // link-flow — a note is an ordinary step in a link descent
              // (and `u` is bound route-level, so the chip must be here too
              // or the key would act with no visible affordance).
              flow={
                flowEntries.length > 0
                  ? { entries: flowEntries, onBack: flowBack, onJump: flowJump }
                  : undefined
              }
            />
          )}
          {doc && <DeskChangedBanner kb={kb} id={id} />}

          {searchParams.get("list") && (
            <QueueBar
              kb={kb}
              listId={searchParams.get("list") as string}
              entryId={searchParams.get("entry")}
            />
          )}
          {/* W2.7 — note path has no iframe runtime, hence no `kb:section`
              signal: "kb · folder" only. */}
          {folio && doc && <FolioHeader kb={kb} folder={doc.folder} />}
          {/* Explicit stable keys (recon-flagged) — folio's header is a new
              PRECEDING sibling that toggles in/out; without a key, React's
              positional reconciliation would treat these divs (and the
              NoteEditor they wrap) as different-typed slots on that toggle
              and remount NoteEditor, dropping any in-progress edit. */}
          <div className="detail__note-bar" key="note-bar">
            <span className="detail__note-tag">note</span>
            <button
              type="button"
              className="kb-note__btn"
              onClick={() => setRawView(true)}
              title="view the rendered artifact (read-only)"
            >
              View rendered
            </button>
          </div>
          <div className="detail__note-scroll" key="note-scroll">
            <NoteEditor kb={kb} id={id} onDeleted={() => navigate("/notes")} />
          </div>
          {folio && doc && <FolioColophon kb={kb} doc={doc} />}
        </div>
        {/* v0.23 — mobile scrim under the reader-tools sheet (mirrors the
            drawer scrim); tap to dismiss. Mobile-only so the desktop DOM is
            unchanged. */}
        {isMobile && inspectorOpen && (
          <div
            className="kb-pinsp-scrim is-open"
            onClick={() => setInspectorOpen(false)}
            aria-hidden
          />
        )}
        {doc && (showInspector || showPanel || showVersions) && (
          <ReaderDock>
            <PreviewInspector
              kb={kb}
              doc={doc}
              progress={shownProgress}
              summary={shownSummary}
              descendants={descendants}
              folderCwd={cwd}
              onFolderCwd={onFolderCwd}
              childFolders={folderChildren}
              onMetaChange={patchDoc}
              docked
              asSheet={isMobile}
              onMobileClose={() => setInspectorOpen(false)}
              panelMode={
                showPanel ? "comments" : showVersions ? "versions" : "inspect"
              }
              commentCount={openCount}
              onSwitchToInspect={() => {
                setPanelOpen(false);
                setVersionsOpen(false);
              }}
              onComments={reviewActive ? handleTogglePanel : undefined}
              onVersions={handleToggleVersions}
              panelSlot={
                showVersions ? (
                  <VersionsPanel kb={kb} id={id} at={atUnix} />
                ) : showPanel ? (
                  review.file ? (
                    <CommentsPanel
                      kb={kb}
                      artifactId={id}
                      sourceRelative={doc.source_relative}
                      file={review.file}
                      loading={review.loading}
                      error={review.error}
                      staleCommentIds={review.staleCommentIds}
                      onAddComment={review.addComment}
                      onAddReply={review.addReply}
                      onResolveComment={review.resolveComment}
                      onUnresolveComment={review.unresolveComment}
                      onEditComment={review.editComment}
                      onDeleteComment={review.deleteComment}
                      onDetachAttachment={review.detachAttachment}
                      onDetachReplyAttachment={review.detachReplyAttachment}
                      activeCommentId={null}
                    />
                  ) : (
                    <aside className="comments-panel" aria-label="comments">
                      <div className="cp__hint">loading…</div>
                    </aside>
                  )
                ) : null
              }
            />
          </ReaderDock>
        )}
      </div>
    );
  }

  // ── W3.P-b — the two panes ───────────────────────────────────────────
  // `focusedPane` is plain UI state, so a pane's chrome is live only while it
  // holds focus: `focused` gates the pane's window-level keybinds (`[`/`]`,
  // `o`/`b`/`y p`) so two panes never both act on one keystroke, and the
  // route-level singletons that CAN'T be duplicated (the selection chooser,
  // the errata slip, the active comment row, the pencil) are handed to the
  // focused pane only. ContextBar IS per-pane (per-artifact chrome); the
  // inspector rail below is NOT (invariant #30 — exactly one `.kb-pinsp`).
  const primaryFocused = !splitOpen || focusedPane === 1;
  const primaryPane = (
    <ArtifactPane
      kb={kb}
      relPath={relPath}
      // `?sec=` names a heading in the PRIMARY artifact — pane 2 gets its own
      // (the third field of the `?pane2=` grammar) rather than this one.
      sec={searchParams.get("sec")}
      // W3.E/S5 — `?turn=<N|end>`, same PRIMARY-only discipline as `sec`
      // (the `?pane2=` grammar carries no turn field this wave).
      turn={searchParams.get("turn")}
      focused={primaryFocused}
      onFocus={() => setFocusedPane(1)}
      isPrimary
      doc={doc}
      id={id}
      error={error}
      hostSuffix={hostSuffix}
      removed={removed}
      reloadNonce={reloadNonce}
      descendants={navDescendants}
      siblingNav={siblingNav}
      summary={primaryFocused ? shownSummary : null}
      immersive={immersive}
      onSetImmersive={setImmersive}
      folio={folio}
      onToggleFolio={() => setFolio(!folio)}
      inspectorOpen={inspectorOpen}
      onToggleInspector={() => setInspectorOpen((v) => !v)}
      // "Open beside" lives on the PRIMARY pane only, and only when there is
      // a sibling to open on a viewport wide enough to show it.
      splitMode={canSplit ? "open" : undefined}
      onSplit={
        canSplit && splitTarget ? () => openSplit(splitTarget) : undefined
      }
      reviewActive={reviewActive}
      reviewFile={review.file}
      bridgeRef={bridgeRef}
      annotateMode={primaryFocused && annotateMode}
      onToggleAnnotate={
        reviewActive
          ? () => {
              setFocusedPane(1);
              handleToggleAnnotate();
            }
          : undefined
      }
      commentCount={primaryOpenCount}
      errataMode={primaryFocused && errataMode}
      activeCommentId={primaryFocused ? activeCommentId : null}
      onCommentJump={handleCommentJump}
      onCommentHover={handleCommentHover}
      onComposeAnchor={(anchor) => {
        // Clicking in the artifact opens the panel's inline composer
        // (compose needs the panel visible — annotateMode alone no
        // longer renders it). v0.23 — also raise the reader-tools sheet
        // so the composer is visible on a phone (inert on desktop).
        setFocusedPane(1);
        setAnnotateMode(true);
        setPanelOpen(true);
        setInspectorOpen(true);
        setComposeAnchor(anchor);
      }}
      onFocusComment={(commentId) => {
        // Clicking an in-page comment icon/marker opens the panel
        // (without forcing annotate mode — focusing is a read action),
        // activates the matching row, and flashes the in-page anchor.
        // v0.23 — surface the sheet on a phone so the focused row shows.
        setFocusedPane(1);
        setPanelOpen(true);
        setInspectorOpen(true);
        setActiveCommentId(commentId);
        bridgeRef.current?.flash(commentId);
      }}
      onExitAnnotate={handleEscAnnotate}
      selection={primaryFocused ? selection : null}
      onSelectionChange={(s) => {
        if (s) setFocusedPane(1);
        setSelection(s);
      }}
      onComposeSelection={(anchor) => {
        // W2.16-mobile — SelectionActions' "comment" button. Raises the
        // SAME composer `onComposeAnchor` above opens (`cp__compose`), but
        // deliberately does NOT `setAnnotateMode(true)` the way
        // `onComposeAnchor` does: that pencil arms tap-anywhere-to-compose,
        // which a selection-initiated compose must never switch on — worst
        // on a phone, where every stray tap on the artifact would then pop
        // a fresh composer on top of the one just opened. `setSelection`
        // is dropped (not just left to the composer's own Esc) so the Esc
        // chain in `handleEscAnnotate` — selection → composeAnchor →
        // annotateMode — has exactly one layer open at a time; leaving the
        // selection set here would stack a chooser UNDER a composer that
        // has no visible trigger to reopen it.
        setFocusedPane(1);
        setPanelOpen(true);
        setInspectorOpen(true); // raises the phone sheet (v0.23); inert on desktop
        setComposeAnchor(anchor);
        setSelection(null);
      }}
      onSelectionPull={(anchor) => handleSelectionPull(1, anchor)}
      // link-flow — the reading flow is the ROUTE's (the primary artifact
      // is the one you descend from and return to); pane 2 is a compare
      // view, never a step in the descent.
      flowSeedY={
        flowSeed && flowSeed.kb === kb && flowSeed.sourceRelative === relPath
          ? flowSeed.y
          : null
      }
      flowStateRef={flowStateRef}
      flow={
        flowEntries.length > 0
          ? { entries: flowEntries, onBack: flowBack, onJump: flowJump }
          : undefined
      }
      // "Open beside" from a peek card — the same `openSplit` the ContextBar
      // verb and the `w v` chord use, offered only when the reader can
      // actually split right now.
      onPeekSplit={!isMobile && !splitOpen ? openSplit : undefined}
    />
  );
  // The compare pane. Deliberately leaner than the primary: no sibling
  // counter / `[`-`]` walk (that's the ROUTE's folder, and the route belongs
  // to pane 1), no removal screen, and it never writes the address bar
  // (`isPrimary={false}` — a secondary pane rewriting `?p=` would hijack the
  // permalink). Everything load-bearing it DOES own: its own visit row, its
  // own review file, its own annotator bridge, its own exact-origin guard.
  const secondPane =
    splitOpen && pane2Loc ? (
      pane2Doc && pane2Id ? (
        <ArtifactPane
          kb={pane2Loc.kb}
          relPath={pane2Loc.sourceRelative}
          sec={pane2Loc.sec ?? null}
          // W3.E/S5 — pane 2 has no `?turn=` grammar this wave (`PaneLoc`
          // carries no turn field) — a deliberate scope trim, see the W3
          // build report.
          turn={null}
          focused={focusedPane === 2}
          onFocus={() => setFocusedPane(2)}
          isPrimary={false}
          doc={pane2Doc}
          id={pane2Id}
          error={pane2Error}
          hostSuffix={hostSuffix}
          removed={false}
          reloadNonce={pane2Nonce}
          descendants={undefined}
          siblingNav={undefined}
          summary={focusedPane === 2 ? shownSummary : null}
          immersive={immersive}
          onSetImmersive={setImmersive}
          folio={folio}
          onToggleFolio={() => setFolio(!folio)}
          inspectorOpen={inspectorOpen}
          onToggleInspector={() => setInspectorOpen((v) => !v)}
          splitMode="close"
          onSplit={closeSplit}
          reviewActive={pane2ReviewActive}
          reviewFile={review2.file}
          bridgeRef={bridge2Ref}
          annotateMode={focusedPane === 2 && annotateMode}
          onToggleAnnotate={
            pane2ReviewActive
              ? () => {
                  setFocusedPane(2);
                  handleToggleAnnotate();
                }
              : undefined
          }
          commentCount={pane2OpenCount}
          errataMode={focusedPane === 2 && errataMode}
          activeCommentId={focusedPane === 2 ? activeCommentId : null}
          onCommentJump={handleCommentJump}
          onCommentHover={handleCommentHover}
          onComposeAnchor={(anchor) => {
            setFocusedPane(2);
            setAnnotateMode(true);
            setPanelOpen(true);
            setInspectorOpen(true);
            setComposeAnchor(anchor);
          }}
          onFocusComment={(commentId) => {
            setFocusedPane(2);
            setPanelOpen(true);
            setInspectorOpen(true);
            setActiveCommentId(commentId);
            bridge2Ref.current?.flash(commentId);
          }}
          onExitAnnotate={handleEscAnnotate}
          selection={focusedPane === 2 ? selection : null}
          onSelectionChange={(s) => {
            if (s) setFocusedPane(2);
            setSelection(s);
          }}
          onComposeSelection={(anchor) => {
            // Mirrors the primary pane's onComposeSelection above (see its
            // comment for the annotateMode-stays-off reasoning and the
            // Esc-chain ordering); this copy targets pane 2.
            setFocusedPane(2);
            setPanelOpen(true);
            setInspectorOpen(true);
            setComposeAnchor(anchor);
            setSelection(null);
          }}
          onSelectionPull={(anchor) => handleSelectionPull(2, anchor)}
        />
      ) : (
        // The by-path lookup hasn't landed (or 404'd). Keep the column
        // stable rather than collapsing the grid under the reader.
        <div className="detail__main">
          <div className="empty">{pane2Error ?? "loading…"}</div>
        </div>
      )
    ) : null;

  return (
    <div className={detailClass}>
      {/* MB3 — mobile-only full-screen reading toggle (CSS-hidden on
          desktop). Fixed over the iframe so it's reachable in immersive
          mode; ✕ / Esc exits. */}
      <button
        type="button"
        className={`kb-immersive-toggle ${immersive ? "is-on" : ""}`}
        onClick={() => setImmersive(!immersive)}
        aria-label={immersive ? "exit full screen" : "read full screen"}
        title={immersive ? "exit full screen (Esc)" : "read full screen"}
      >
        {immersive ? (
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M8 3v3a2 2 0 0 1-2 2H3M21 8h-3a2 2 0 0 1-2-2V3M3 16h3a2 2 0 0 1 2 2v3M16 21v-3a2 2 0 0 1 2-2h3" />
          </svg>
        ) : (
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M8 3H5a2 2 0 0 0-2 2v3M16 3h3a2 2 0 0 1 2 2v3M8 21H5a2 2 0 0 1-2-2v-3M16 21h3a2 2 0 0 0 2-2v-3" />
          </svg>
        )}
      </button>
      {/* W3.P-a/W3.P-b — the artifact pane(s). Each renders the SAME
          `.detail__main` subtree + AnnotatorBridge + SelectionActions
          (a fragment), carries its own visit lifecycle (#8/#19), and guards
          every message by EXACT artifact origin. In single-pane mode the
          primary is rendered BARE, with no wrapper element, so the non-split
          reader DOM is byte-identical to before the split shipped. */}
      {splitOpen ? (
        <div className="detail__panes" data-kb-split="2">
          <div
            className={`detail__pane${focusedPane === 1 ? " is-focused" : ""}`}
            data-kb-pane="1"
            onMouseDownCapture={() => setFocusedPane(1)}
            onFocusCapture={() => setFocusedPane(1)}
          >
            {primaryPane}
          </div>
          <div
            className={`detail__pane${focusedPane === 2 ? " is-focused" : ""}`}
            data-kb-pane="2"
            onMouseDownCapture={() => setFocusedPane(2)}
            onFocusCapture={() => setFocusedPane(2)}
          >
            {secondPane}
          </div>
        </div>
      ) : (
        primaryPane
      )}
      {/* v0.23 — mobile scrim under the reader-tools sheet; tap to dismiss.
          Mobile-only so the desktop DOM is unchanged. */}
      {isMobile && inspectorOpen && (
        <div
          className="kb-pinsp-scrim is-open"
          onClick={() => setInspectorOpen(false)}
          aria-hidden
        />
      )}
      {/* Invariant #30 — ONE `.kb-pinsp` rail for the whole reader, fed by
          the FOCUSED pane's kb/doc/id. Not a second rail per pane, and not a
          7th inspector sub-tab: the count stays 6. `descendants` (and the
          reading-progress overlay it enriches) belong to the ROUTE's folder
          browser (pane 1's cwd), so they are withheld while pane 2 holds
          focus rather than shown against the wrong artifact. */}
      {focusedDoc && (showInspector || showPanel || showVersions) && (
        <ReaderDock>
          <PreviewInspector
            kb={focusedKb}
            doc={focusedDoc}
            progress={focused2 ? undefined : shownProgress}
            summary={focused2 ? null : shownSummary}
            descendants={focused2 ? undefined : descendants}
            folderCwd={focused2 ? undefined : cwd}
            onFolderCwd={focused2 ? undefined : onFolderCwd}
            childFolders={focused2 ? undefined : folderChildren}
            onMetaChange={focused2 ? undefined : patchDoc}
            docked
            asSheet={isMobile}
            onMobileClose={() => setInspectorOpen(false)}
            panelMode={
              showPanel ? "comments" : showVersions ? "versions" : "inspect"
            }
            commentCount={openCount}
            onSwitchToInspect={() => {
              setPanelOpen(false);
              setVersionsOpen(false);
              setAnnotateMode(false);
            }}
            onComments={focusedReviewActive ? handleTogglePanel : undefined}
            onVersions={handleToggleVersions}
            panelSlot={
              showVersions ? (
                <VersionsPanel
                  kb={focusedKb}
                  id={focusedDoc.id}
                  // CT-F6 — `?at=` names ONE instant, and the URL names ONE
                  // primary artifact (pane 2 rides `?pane2=`). So the
                  // coordinate applies to pane 1 only; focusing pane 2 must
                  // not silently re-point "as it stood at T" at a different
                  // artifact than the link asked about (the #30 v0.29
                  // per-pane exactness rule).
                  at={primaryFocused ? atUnix : null}
                />
              ) : showPanel ? (
                focusedReview.file ? (
                  <CommentsPanel
                    kb={focusedKb}
                    artifactId={focusedDoc.id}
                    sourceRelative={focusedDoc.source_relative}
                    file={focusedReview.file}
                    loading={focusedReview.loading}
                    error={focusedReview.error}
                    staleCommentIds={focusedReview.staleCommentIds}
                    onAddComment={focusedReview.addComment}
                    onAddReply={focusedReview.addReply}
                    onResolveComment={focusedReview.resolveComment}
                    onUnresolveComment={focusedReview.unresolveComment}
                    onEditComment={focusedReview.editComment}
                    onDeleteComment={focusedReview.deleteComment}
                    onDetachAttachment={focusedReview.detachAttachment}
                    onDetachReplyAttachment={focusedReview.detachReplyAttachment}
                    onRequestFlash={handleCommentJump}
                    onHoverComment={handleCommentHover}
                    activeCommentId={activeCommentId}
                    composeAnchor={composeAnchor}
                    onCloseCompose={() => setComposeAnchor(null)}
                    annotateMode={annotateMode}
                    onToggleAnnotate={handleToggleAnnotate}
                    errataMode={errataMode}
                    onToggleErrata={handleToggleErrata}
                    // R4 — the mobile-only pull affordance. Passed only on
                    // the iframe render path: the native-note path below has
                    // no annotator to query, same convention as
                    // `onRequestFlash`/`onToggleAnnotate` (omitted ⇒ hidden).
                    onCommentOnSelection={composeFromSelection}
                  />
                ) : (
                  // Keep the column stable during the brief window after a
                  // navigation where the new artifact's review file hasn't loaded.
                  <aside className="comments-panel" aria-label="comments">
                    <div className="cp__hint">loading…</div>
                  </aside>
                )
              ) : null
            }
          />
        </ReaderDock>
      )}
    </div>
  );
}

// v0.21 — the reader's right column. The old [inspect|comments|versions] text
// pills are gone: PreviewInspector now owns the SINGLE icon rail (inspector
// sub-tabs + comments + versions icons) and switches the body itself
// (invariant #30), so the dock is just the column chrome (border + scroll
// container). The three panels stay distinct components — PreviewInspector /
// CommentsPanel / VersionsPanel — and their markup + testids are unchanged.
// W3.P-a — the native-note path's visit lifecycle. Renders nothing; it exists
// only so `useArtifactVisit` (which lives with the pane, since a visit is
// strictly per-artifact — invariants #8/#19) can run on a render path that has
// no `ArtifactPane`. Before the pane split this ran unconditionally as a route
// effect; keeping it is what makes the split behaviour-free for notes.
function NoteVisit({ kb, id }: { kb: string; id: string }) {
  useArtifactVisit(kb, id);
  return null;
}

function ReaderDock({ children }: { children: ReactNode }) {
  return (
    <div className="kb-dock" role="region" aria-label="reader dock">
      {children}
    </div>
  );
}
