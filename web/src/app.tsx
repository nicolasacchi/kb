import { lazy, Suspense, useEffect, useRef, useState, type ReactNode } from "react";
import {
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
  useSearchParams,
} from "react-router-dom";
import Header from "./components/chrome/Header";
import ContextLine from "./components/chrome/ContextLine";
import { QueryStatsProvider } from "./components/chrome/queryStats";
import StatusBar, { StatusBarProvider } from "./components/chrome/StatusBar";
import Sidebar from "./components/chrome/Sidebar";
import MobileDrawer from "./components/chrome/MobileDrawer";
import NavList from "./components/chrome/NavList";
import BottomTabBar from "./components/chrome/BottomTabBar";
import DaemonDownBanner from "./components/chrome/DaemonDownBanner";
import HotkeyRoot from "./components/chrome/HotkeyRoot";
import { useIsMobile } from "./hooks/useIsMobile";
import StatusPill from "./components/StatusPill";
import Cmdk from "./components/Cmdk";
import CaptureSheet from "./components/CaptureSheet";
import Toasts from "./components/Toasts";
import ConfirmProvider from "./components/ConfirmProvider";
import ErrorBoundary from "./components/ErrorBoundary";
import { useIdentity } from "./hooks/useArtifactHost";
import { useActiveKb, useExplicitKb } from "./hooks/useActiveKb";
import { useKbs } from "./hooks/useKbs";
import {
  loadLastKb,
  saveLastKb,
  coldSeedKb,
  coldSeedShell,
  loadHome,
} from "./api/prefs";
import { verifyAgainstIdentity } from "./lib/artifactHost";
import { parseCapturedParam } from "./lib/capturedParam";
import { toast } from "./lib/toast";

// Route views are code-split (React.lazy): each loads its own chunk on first
// navigation, so CodeMirror (detail/notes editors), the markdown renderer, and
// the atlas canvas stay out of the initial paint. The shell/chrome imported
// above stays eager — it owns the SSE/Cmdk wiring that must not flash
// (architecture invariant §24).
const Gallery = lazy(() => import("./routes/gallery"));
const Detail = lazy(() => import("./routes/detail"));
const Settings = lazy(() => import("./routes/settings"));
const AnchorsRoute = lazy(() => import("./routes/anchors"));
const Memory = lazy(() => import("./routes/memory"));
const ListsRoute = lazy(() => import("./routes/lists"));
const ListDetailRoute = lazy(() => import("./routes/listDetail"));
const BoardRoute = lazy(() => import("./routes/board"));
const SessionsRoute = lazy(() => import("./routes/sessions"));
const NotesRoute = lazy(() => import("./routes/notes"));
const SearchRoute = lazy(() => import("./routes/search"));
const InboxRoute = lazy(() => import("./routes/inbox"));
// SL4 — the slate list and the board. Lazy like every other route: the
// board pulls the markdown pipeline and the composer's CM6 editor, and
// nobody who is not looking at a slate should pay for either.
const SlatesRoute = lazy(() => import("./routes/slates"));
const SlateBoardRoute = lazy(() => import("./routes/slateBoard"));
// W3.R-c — the session-replay reader. Its own route (NOT a 7th inspector
// sub-tab: invariant #30 pins that rail at 6), lazy like every other, so its
// chunk + `styles/replay.css` only ship when a replay is opened.
const ReplayRoute = lazy(() => import("./routes/replay"));
// Unit 2 — the desk radiator: a full-screen, low-chrome ambient display for
// a spare monitor. Lazy like every other route, so its chunk (and
// `styles/ambient.css`) only ships to someone who opens it.
const AmbientRoute = lazy(() => import("./routes/ambient"));

// SPA shell. Persistent two-row chrome (topbar + main body with left
// rail) wraps the routed view. The left rail mirrors HOME-tab sidebar
// in the TUI; the topbar holds branding + Cmd+K affordance.
//
// Detail view bypasses the left rail because the iframed artifact
// needs the full body width — it's its own visual context.
export default function App() {
  const [cmdkOpen, setCmdkOpen] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [captureOpen, setCaptureOpen] = useState(false);
  const [params] = useSearchParams();
  // Resolved active kb (path kb on a reader, else ?kb=, else first kb) so the
  // command palette defaults its search scope to the space the user is in —
  // not just the query param, which is empty while reading an artifact.
  const activeKb = useActiveKb() ?? undefined;
  const loc = useLocation();
  const navigate = useNavigate();
  const isMobile = useIsMobile();

  // K1 — remember the active corpus across sessions. Write on every change…
  const explicitKb = useExplicitKb();
  const { data: kbs } = useKbs();
  useEffect(() => {
    if (activeKb) saveLastKb(activeKb);
  }, [activeKb]);
  // …and read ONCE, on a truly cold entry (bare "/", no explicit kb), to land
  // the user back in their last space instead of always the first corpus.
  // Per-tab live selection stays the URL (so two tabs on different kbs never
  // fight); lastKb only seeds the very first view. Validated against the live
  // kb list so a reconfigured daemon never resurrects a dead corpus (#13), and
  // skipped when it already equals the kbs[0] default (no spurious ?kb=).
  //
  // W3.M-d rides the SAME one-shot: `Prefs.home` decides only where a truly
  // bare "/" lands (`coldSeedShell`, which refuses anything carrying a query
  // string). It defaults to "grid" and the operator must flip it in Settings
  // — the evidence gate for map-home (criterion recorded in api/prefs.ts).
  // Both seeds fold into ONE `navigate`, so a map-home operator who also has
  // a remembered corpus gets `/?kb=…&shell=map` in a single replace.
  const coldSeeded = useRef(false);
  useEffect(() => {
    if (coldSeeded.current) return;
    if (!kbs || kbs.length === 0) return; // wait for the kb list to load
    coldSeeded.current = true; // one-shot for the app's lifetime
    const target = coldSeedKb({
      kbs: kbs.map((k) => k.name),
      explicitKb,
      pathname: loc.pathname,
      lastKb: loadLastKb(),
    });
    const shell = coldSeedShell({
      home: loadHome(),
      pathname: loc.pathname,
      search: loc.search,
    });
    if (!target && !shell) return;
    const p = new URLSearchParams();
    if (target) p.set("kb", target);
    if (shell) p.set("shell", shell);
    navigate(`/?${p.toString()}`, { replace: true });
  }, [kbs, explicitKb, loc.pathname, loc.search, navigate]);
  // Detail (/a/:kb/*) has no filter rail, so the drawer shows nav only there.
  const isDetail = loc.pathname.startsWith("/a/");

  // kb2 C2 — the left filter rail shows ONLY on the filterable corpus views:
  // `/` with view grid/list/atlas. Everywhere else (history, memory, lists,
  // notes, sessions, anchors, settings, detail, 404) renders full-width. The
  // old per-route `body--with-rail` literals over-showed the rail on views
  // where tag/folder/cap filtering is meaningless.
  // W3.M-d — the map-home shell owns the full body width (a full-bleed map
  // plus its own projection panel), so the filter rail steps aside exactly
  // like it does on history/canvas.
  const galleryView = params.get("view");
  const shellMap = params.get("shell") === "map";
  const showRail =
    loc.pathname === "/" &&
    !shellMap &&
    (galleryView === null ||
      galleryView === "grid" ||
      galleryView === "list" ||
      galleryView === "atlas");

  // Close the mobile drawer when the route changes. Keyed on pathname ONLY —
  // a nav row also closes it via onNavigate, while filter taps in the drawer
  // change `?…` search params and must NOT slam it shut, so several
  // tags/folders can be toggled in one pass.
  useEffect(() => {
    setDrawerOpen(false);
  }, [loc.pathname]);
  useEffect(() => {
    if (!isMobile) setDrawerOpen(false);
  }, [isMobile]);

  // Cross-check the window.location-derived artifact host suffix against
  // the daemon's authoritative /api/identity. Mismatches mean a
  // mis-wired reverse proxy (artifact iframes will 404); surface them
  // non-fatally on the bottom-right status pill (hover/click to read the
  // detail) rather than a full-width banner — the heuristic stays the
  // runtime source of truth (see lib/artifactHost.ts).
  const identity = useIdentity();
  const [identityWarnings, setIdentityWarnings] = useState<string[]>([]);
  useEffect(() => {
    if (!identity) return;
    const w = verifyAgainstIdentity(identity);
    setIdentityWarnings(w);
    for (const msg of w) console.warn(`[kb] ${msg}`);
  }, [identity]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      // Open with ⌘K / Ctrl+K. Don't intercept inside text inputs unless
      // the modal is the input — `setCmdkOpen` is fine since the modal
      // owns its own focus once mounted.
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setCmdkOpen(true);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // U4 — CaptureSheet is mounted once here (like Cmdk/Toasts), not
  // per-route, so both entry points (the Cmdk command + the drawer row)
  // reach it via the same CustomEvent AddToListButton already uses for its
  // own cross-component open.
  useEffect(() => {
    const onOpen = () => setCaptureOpen(true);
    window.addEventListener("kb:capture.open", onOpen);
    return () => window.removeEventListener("kb:capture.open", onOpen);
  }, []);

  // U4/U5 — one-shot handoff for the Web Share Target redirect
  // (`routes/capture.rs`'s `POST /capture` 303s to `/?captured=<kb>:<rel>`
  // once the upload is staged; indexing is async, so there's no artifact to
  // jump to yet — just a toast). Ref-guarded so it never refires; the param
  // is dropped via a raw `history.replaceState` (the Tabs.tsx hash-cleanup
  // precedent), not `setSearchParams`, so this stays a plain URL-bar
  // cleanup independent of react-router's own location bookkeeping.
  const capturedHandledRef = useRef(false);
  useEffect(() => {
    if (capturedHandledRef.current) return;
    const ref = parseCapturedParam(params.get("captured"));
    if (!ref) return;
    capturedHandledRef.current = true;
    const name = ref.sourceRelative.split("/").pop() || ref.sourceRelative;
    toast.ok(`Captured "${name}" — indexing…`);
    const url = new URL(window.location.href);
    url.searchParams.delete("captured");
    window.history.replaceState(null, "", url.toString());
  }, [params]);

  return (
    <StatusBarProvider>
    <QueryStatsProvider>
    <ConfirmProvider>
    <div className="app">
      {/* A-w3 (wave-1 wire-don't-build) — the `.skip-link` CSS existed with
          no element rendering it. First focusable node in the shell so a
          keyboard/screen-reader user can jump straight past Header/ContextLine
          to the routed content (`<main id="main">`, set on every route below). */}
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      <Header
        onOpenCmdk={() => setCmdkOpen(true)}
        onToggleDrawer={() => setDrawerOpen((o) => !o)}
      />
      <ContextLine />
      <DaemonDownBanner />
      {/* One Suspense at the Routes boundary: React Router renders exactly one
          matched lazy element, so a single fallback covers every route's chunk
          fetch. The fallback reuses .body/.empty so layout doesn't jump. */}
      <ErrorBoundary resetKey={loc.pathname}>
      <Suspense fallback={<RouteFallback />}>
      <Routes>
        {/* Gallery is the only route that can show the filter rail. */}
        <Route
          path="/"
          element={
            <RailShell rail={showRail}>
              <Gallery />
            </RailShell>
          }
        />
        {/* Detail owns the full body (iframe + reader chrome) — no .view. */}
        <Route
          path="/a/:kb/*"
          element={
            <main id="main" className="body body--full">
              <Detail />
            </main>
          }
        />
        {/* All standalone routes are full-width (no filter rail). */}
        <Route
          path="/settings"
          element={
            <RailShell rail={false}>
              <Settings />
            </RailShell>
          }
        />
        <Route
          path="/anchors"
          element={
            <RailShell rail={false}>
              <AnchorsRoute />
            </RailShell>
          }
        />
        {/* v0.10 K3 — legacy redirect. The /anchors view's "Stale" tab
            replaces the dedicated dashboard; existing bookmarks land
            on the right tab. */}
        <Route
          path="/stale-anchors"
          element={<Navigate to="/anchors?filter=stale" replace />}
        />
        <Route
          path="/memory"
          element={
            <RailShell rail={false}>
              <Memory />
            </RailShell>
          }
        />
        <Route
          path="/lists"
          element={
            <RailShell rail={false}>
              <ListsRoute />
            </RailShell>
          }
        />
        <Route
          path="/lists/:kb/:id"
          element={
            <RailShell rail={false}>
              <ListDetailRoute />
            </RailShell>
          }
        />
        {/* W2.4 — Boards v1: a reading list's entries on a JSON Canvas
            geometry sidecar. Full-width, like the list detail above. */}
        <Route
          path="/board/:kb/:listId"
          element={
            <RailShell rail={false}>
              <BoardRoute />
            </RailShell>
          }
        />
        <Route
          path="/sessions"
          element={
            <RailShell rail={false}>
              <SessionsRoute />
            </RailShell>
          }
        />
        <Route
          path="/notes"
          element={
            <RailShell rail={false}>
              <NotesRoute />
            </RailShell>
          }
        />
        {/* W3.R-c — one captured session, replayed beat by beat. Full
            width (no filter rail): the route owns two panes + a scrubber. */}
        <Route
          path="/replay/:kb/:sid"
          element={
            <RailShell rail={false}>
              <ReplayRoute />
            </RailShell>
          }
        />
        {/* Unit 2 — the desk radiator: full-width, no filter rail, no
            interactivity beyond an artifact link. */}
        <Route
          path="/ambient"
          element={
            <RailShell rail={false}>
              <AmbientRoute />
            </RailShell>
          }
        />
        {/* Z4 — fleet-wide open-comments inbox (full-width, no filter rail). */}
        <Route
          path="/inbox"
          element={
            <RailShell rail={false}>
              <InboxRoute />
            </RailShell>
          }
        />
        {/* SL4 — the slate list + the board. Full-width (rail={false}),
            the same shell /inbox uses; ?topic= filters and ?history=1 opens
            the drawer, both read off the URL by the route itself. */}
        <Route
          path="/slates"
          element={
            <RailShell rail={false}>
              <SlatesRoute />
            </RailShell>
          }
        />
        <Route
          path="/slates/:slug"
          element={
            <RailShell rail={false}>
              <SlateBoardRoute />
            </RailShell>
          }
        />
        {/* Track F — full search page. RailShell rail=false: the page owns
            its own search-specific filter rail internally (the gallery
            Sidebar's tag/cap/since filters aren't honored by /api/search). */}
        <Route
          path="/search"
          element={
            <RailShell rail={false}>
              <SearchRoute />
            </RailShell>
          }
        />
        <Route
          path="*"
          element={
            <main id="main" className="body body--full">
              <div className="empty">404 — no route here</div>
            </main>
          }
        />
      </Routes>
      </Suspense>
      </ErrorBoundary>
      <StatusBar warnings={identityWarnings} />
      <StatusPill warnings={identityWarnings} />
      {isMobile && !isDetail && (
        <BottomTabBar
          cmdkOpen={cmdkOpen}
          onOpenCmdk={() => setCmdkOpen(true)}
        />
      )}
      <HotkeyRoot />
      {cmdkOpen && <Cmdk kb={activeKb} onClose={() => setCmdkOpen(false)} />}
      {captureOpen && <CaptureSheet onClose={() => setCaptureOpen(false)} />}
      <MobileDrawer
        open={drawerOpen && isMobile}
        onClose={() => setDrawerOpen(false)}
        title="kb"
        ariaLabel="navigation and filters"
      >
        <NavList onNavigate={() => setDrawerOpen(false)} />
        {!isDetail && (
          <>
            <div className="kb-drawer__sect">Filters</div>
            <Sidebar />
          </>
        )}
      </MobileDrawer>
      <Toasts />
    </div>
    </ConfirmProvider>
    </QueryStatsProvider>
    </StatusBarProvider>
  );
}

// Suspense fallback while a lazy route chunk loads. Reuses the existing
// .body/.empty classes so the shell stays put and nothing reflows under it.
function RouteFallback() {
  return (
    <main id="main" className="body body--full">
      <div className="empty" aria-busy="true">
        Loading…
      </div>
    </main>
  );
}

// Body shell for the routed view. `rail` toggles the left filter rail
// (gallery grid/list/atlas only); every other route renders full-width.
// Detail + 404 keep their own bare body--full (no .view wrapper).
function RailShell({
  rail,
  children,
}: {
  rail: boolean;
  children: ReactNode;
}) {
  return (
    <main id="main" className={`body ${rail ? "body--with-rail" : "body--full"}`}>
      {rail && <Sidebar />}
      <section className="view">{children}</section>
    </main>
  );
}
