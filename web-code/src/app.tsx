import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Route, Routes, useLocation, useNavigate } from "react-router-dom";
import ConfirmProvider from "./components/ConfirmProvider";
import ErrorBoundary from "./components/ErrorBoundary";
import Omnibox from "./components/Omnibox";
import ToastHost from "./components/ToastHost";
import TopBar from "./components/TopBar";
import { useActiveRepo, useExplicitRepo } from "./hooks/useActiveRepo";
import { useIdentity } from "./hooks/useIdentity";
import { useRepos } from "./hooks/useRepos";
import { coldSeedRepo, cycleTheme, loadLastRepo, saveLastRepo } from "./lib/prefs";
import { readerUrl } from "./lib/breadcrumbs";
import {
  boardsPageUrl,
  branchesUrl,
  canvasPageUrl,
  commentsUrl,
  hotspotsUrl,
  prsUrl,
  recipesPageUrl,
  reviewsUrl,
  stacksPageUrl,
  railsUrl,
  todosUrl,
} from "./lib/codeUrl";
import { setsUrl } from "./lib/setsUrl";
import { browserPageUrl } from "./lib/codeUrl";
import TrailOriginChip from "./components/nav/TrailOriginChip";
import { useTrailLink } from "./hooks/useTrailLink";
import { codeUrl } from "./lib/codeUrl";
import { enterRing, goForward } from "./lib/navHistory";
import { parseDeskParam } from "./lib/deskParam";
import CommandRoot, { useCommandHandlers, useCommands } from "./commands/CommandRoot";
import { useLearnMode } from "./commands/learn";
import { deepLinkDisposition } from "./commands/commandRows";
import KeyboardHelp from "./components/KeyboardHelp";
import { toast } from "./lib/toast";

// F3a — every route chunk is now lazy (was a plain static import), each
// wrapped by the ErrorBoundary below so a render throw in ANY of them keeps
// the app chrome (TopBar/Omnibox) alive instead of white-screening the whole
// SPA — same "no boundary existed anywhere" gap kb's own F2 fixed
// (`web/src/app.tsx`).
const Home = lazy(() => import("./routes/Home"));
const Reader = lazy(() => import("./routes/Reader"));
// V70-A4 (§D1) — the pre-Desk reader, kept mounted behind `?shell=legacy`
// (equivalently `?desk=legacy`) for exactly one milestone so a Desk
// regression always has a working way back to the code. Its own chunk, so
// an operator who never asks for it never downloads it.
const ReaderLegacy = lazy(() => import("./routes/ReaderLegacy"));
const Search = lazy(() => import("./routes/Search"));
const SessionDiff = lazy(() => import("./routes/SessionDiff"));
// Phase C-SPA — time-first-class pages. `~commit`/`~compare`/`~branches`
// are repo-scoped, non-file sentinels (see `lib/codeUrl.ts`'s header
// comment): unlike `~diff` (which trails an arbitrary file path and so
// rides Reader's own splat parsing), these three sit at a FIXED position
// right after `:repo`, so each gets its own static `<Route>` below —
// React Router ranks a static segment above a splat regardless of
// declaration order, so these always win over `/r/:repo/*` for a matching
// URL. Reader's splat parsing ALSO excludes these sentinels defensively
// (same belt-and-suspenders the diff sentinel gets), in case a caller
// lands on a malformed URL missing a required param (e.g. `~commit` with
// no `:sha`) that falls through to the catch-all.
const Commit = lazy(() => import("./routes/Commit"));
const Compare = lazy(() => import("./routes/Compare"));
const Branches = lazy(() => import("./routes/Branches"));
// Phase G-server — the review-workflow pages, same repo-scoped/non-file
// sentinel treatment as `~compare`/`~branches` above.
const RangeDiff = lazy(() => import("./routes/RangeDiff"));
const Prs = lazy(() => import("./routes/Prs"));
// Phase E4 — reading sets: `~sets` (the list) and `~sets/:id` (one set's
// detail) are repo-scoped, non-file sentinels, same footing as `~compare`/
// `~branches` above. `~sets/:id/~tour` (tour mode) is its own route rather
// than riding Reader's splat parsing the way `~diff`/`~story` do — a set has
// no file path of its own for a sentinel to trail (`lib/setsUrl.ts`'s header
// doc).
const Sets = lazy(() => import("./routes/Sets"));
const SetDetail = lazy(() => import("./routes/SetDetail"));
const Tour = lazy(() => import("./routes/Tour"));
// V70-A10 ("Workspaces v0") — `~workspaces`: the branch-view list of
// workspaces (a `kind=workspace` reading set), grouped by `ref`. Same
// repo-scoped-sentinel footing as `~sets` above, but ONE route (no
// `~workspaces/:id` detail page — see `lib/setsUrl.ts`'s `workspacesUrl`
// doc for why).
const Workspaces = lazy(() => import("./routes/Workspaces"));
// Phase N — TODO index (repo-scoped sentinel, same footing as ~sets).
const Todos = lazy(() => import("./routes/Todos"));
// V72-J2 (D8) — comments/1 dashboard (repo-scoped sentinel, same footing as
// ~todos above, which it supersedes as the richer, kind-aware surface —
// `~todos` keeps working, unchanged, and links forward to this one).
const Comments = lazy(() => import("./routes/Comments"));
// V72-I2 — `~rails`: the `rails/1` entity-index dashboard. Repo-scoped
// sentinel, mounted EXACTLY like `~todos`/`~workspaces` above — §D1's "any
// new surface lands in an existing region", not a second app shell and not a
// new Desk center mode (`desk/centerModes.ts` still ships `reader` alone).
const Rails = lazy(() => import("./routes/Rails"));
// V3.2-B3 — behavioral attention hotspots (repo-scoped sentinel).
const Hotspots = lazy(() => import("./routes/Hotspots"));
// V3.R2 — local review sessions (list + cockpit detail).
const Reviews = lazy(() => import("./routes/Reviews"));
const ReviewDetail = lazy(() => import("./routes/ReviewDetail"));
// V4.D3 — full-page review diff. Both URL shapes share ONE chunk.
const ReviewDiff = lazy(() => import("./routes/ReviewDiff"));
// PRR-U3's short finding permalink (`codeUrl.ts`'s `findingUrl`) — a thin
// redirect ramp onto `ReviewDiff`'s real URL grammar, V70-A3S.
const FindingEntry = lazy(() => import("./routes/FindingEntry"));
// V3.3-U1 — recipes catalog + stacks surfaces.
const Recipes = lazy(() => import("./routes/Recipes"));
const Stacks = lazy(() => import("./routes/Stacks"));
// V3.4-C2 — working-set canvas (Code Bubbles-style pan-zoom fragments).
const Canvas = lazy(() => import("./routes/Canvas"));
// V74-L2 — kbc-canvas/1 boards. A SEPARATE surface from `~canvas` above,
// which stays mounted and frozen (`routes/Boards.tsx`'s header says why).
const Boards = lazy(() => import("./routes/Boards"));
const BoardDetail = lazy(() => import("./routes/BoardDetail"));
// V3.4-C3 — symbol-first browser (Smalltalk lens).
const Browser = lazy(() => import("./routes/Browser"));
// DCB W2.B — the doc↔code lens: repo-scoped page (`Lens`) plus two
// repo-less entry ramps (`LensEntry` id-addressed, `LensEntryByPath`
// path-addressed) — see the route block below for placement (R2/D14).
const Lens = lazy(() => import("./routes/Lens"));
const LensEntry = lazy(() => import("./routes/LensEntry"));
const LensEntryByPath = lazy(() => import("./routes/LensEntryByPath"));
// S2-A — unified inbox (design-s2.md §S2-A): kb items aren't repo-scoped,
// so `~inbox` is a top-level SIBLING of Home, not a `/r/:repo/~…` sentinel.
const Inbox = lazy(() => import("./routes/Inbox"));

/// V70-A4 — which reader shell to mount. One switch, read from the URL
/// (`?shell=legacy`, or `?desk=legacy` via the A0 grammar in
/// `lib/deskParam.ts`), so the escape hatch is a link the operator can
/// paste rather than a setting they have to find and later un-find.
function ReaderRoute() {
  const loc = useLocation();
  const legacy =
    new URLSearchParams(loc.search).get("shell") === "legacy" ||
    parseDeskParam(loc.search) === "legacy";
  return legacy ? <ReaderLegacy /> : <Reader />;
}

function RouteFallback() {
  return <div className="kbc-reader__hint">Loading…</div>;
}

/// Root route table + the Omnibox overlay's open state (W4.3) - mounted
/// once here (not per-route) so Cmd/Ctrl+K works from anywhere, mirroring
/// kb's own `web/src/app.tsx`'s Cmdk placement. Two open triggers: the
/// global keydown listener below, and the `kbc:omnibox.open` CustomEvent
/// every visible "Search…" affordance dispatches (the global `TopBar`'s
/// button) - the same root-mounted-overlay-reached-from-several-routes
/// idiom kb's own `kb:capture.open` uses for `CaptureSheet`.
///
/// F1 — `TopBar` (repo pill + search launcher) is ALSO mounted once here,
/// above every route, mirroring kb's own `Header` — the operator's ask was
/// "remove the top left box to change the codebase, make it more similar to
/// kb", and kb's own workspace pill lives in exactly this spot.
///
/// V70-A5 — the shell is now MOUNTED INSIDE `CommandRoot` (see `App` at the
/// bottom of this file) so it can register handlers for the `scope: global`
/// registry rows on the one command bus.
function AppShell() {
  const [omniboxOpen, setOmniboxOpen] = useState(false);
  const [omniboxQuery, setOmniboxQuery] = useState("");
  const [helpOpen, setHelpOpen] = useState(false);
  const [pendingCmd, setPendingCmd] = useState<{ kind: "run"; id: string } | null>(null);
  const loc = useLocation();
  const navigate = useNavigate();
  // Fire-and-forget boot fetch - see `useIdentity`'s doc. Its return value
  // has no render-time consumer here; the side effect (rewriting the kb
  // session-link base) is the entire point of mounting it.
  useIdentity();

  // F1 — remember the active repo across sessions, mirroring kb's own K1
  // cold-seed (root CLAUDE.md invariant #33). Write on every change…
  const explicitRepo = useExplicitRepo();
  const activeRepo = useActiveRepo();
  const { data: reposData } = useRepos();
  useEffect(() => {
    if (activeRepo) saveLastRepo(activeRepo);
  }, [activeRepo]);
  // …and read ONCE, on a truly cold entry (bare "/", no explicit repo), to
  // land the operator back in the repo they were last browsing instead of
  // always the first configured one. Ref-guarded one-shot for the app's
  // lifetime; validated against the live repo list so a reconfigured daemon
  // never resurrects a repo that's gone, and skipped when it already equals
  // the repos[0] default (no spurious redirect).
  const coldSeeded = useRef(false);
  useEffect(() => {
    if (coldSeeded.current) return;
    const repos = reposData?.repos ?? [];
    if (repos.length === 0) return; // wait for the repo list to load
    coldSeeded.current = true; // one-shot for the app's lifetime
    const target = coldSeedRepo({
      repos: repos.map((r) => r.name),
      explicitRepo,
      pathname: loc.pathname,
      lastRepo: loadLastRepo(),
    });
    if (target) navigate(readerUrl(target, ""), { replace: true });
  }, [reposData, explicitRepo, loc.pathname, navigate]);

  // V70-A5 — the ad-hoc ⌘K listener that used to live here is GONE. It was
  // the app's entire global keymap (one binding); `CommandRoot` now owns the
  // single window listener and the pending-chord state, and this component
  // registers the handlers for the `scope: global` rows it can service. The
  // A3S typing guard moved with it, unchanged.
  useEffect(() => {
    function onOpenEvent() {
      setOmniboxOpen(true);
    }
    window.addEventListener("kbc:omnibox.open", onOpenEvent);
    return () => window.removeEventListener("kbc:omnibox.open", onOpenEvent);
  }, []);

  // ?cmd=<id> deep links (§P2): auto-execute ONLY a command declared
  // read-only and side-effect-free; anything else pre-fills the palette, so a
  // pasted URL can never write to a review or the working tree on someone
  // else's behalf. One-shot per URL, and the param is stripped after so a
  // refresh does not re-run it.
  const cmdParam = new URLSearchParams(loc.search).get("cmd");
  const cmdRunRef = useRef<string | null>(null);
  useEffect(() => {
    if (!cmdParam || cmdRunRef.current === cmdParam) return;
    cmdRunRef.current = cmdParam;
    const d = deepLinkDisposition(cmdParam);
    const params = new URLSearchParams(loc.search);
    params.delete("cmd");
    const stripped = loc.pathname + (params.toString() ? `?${params}` : "") + loc.hash;
    if (!d) {
      toast.warn(`No command named "${cmdParam}"`);
      navigate(stripped, { replace: true });
      return;
    }
    if (d.kind === "run") {
      setPendingCmd({ kind: "run", id: d.command.id });
    } else {
      toast.warn(`${d.command.title} — ${d.reason}`);
      setOmniboxQuery(`>${d.command.title}`);
      setOmniboxOpen(true);
    }
    navigate(stripped, { replace: true });
  }, [cmdParam, loc.pathname, loc.search, loc.hash, navigate]);

  // V70-A6 — the trail this tab was opened on, if any (`?trail=&step=`).
  // Resolution is bounded and degrades to nothing (`hooks/useTrailLink.ts`).
  const trailLink = useTrailLink();
  const trailHop = trailLink.trail?.steps[trailLink.link?.step ?? -1] ?? null;
  /// Where `u` should go INSTEAD of `history.back()`. Only when this tab
  /// genuinely has nowhere to go back to: a trail-linked tab that has not
  /// navigated since it opened. Once the operator has moved within the tab,
  /// Back is a real Back again and the chip stops overriding it.
  function trailOriginUrl(): string | null {
    if (!trailHop) return null;
    if (window.history.length > 1) return null;
    return trailHop.from;
  }

  // The `scope: global` rows this shell owns. Every one of these was
  // mouse-only or route-only before A5 (recon G1/G3): the 12 destinations,
  // the theme cycle, back/forward, and the two doors into the palette.
  // `nav.*` targets are built from `activeRepo` so a repo-less surface
  // (`~inbox`, `/`) degrades to Home rather than navigating to `/r/undefined`.
  const repoFor = activeRepo ?? reposData?.repos?.[0]?.name ?? null;
  // V70-A6 — through the ONE builder, never a hand-assembled path. This
  // function used to interpolate `/r/${repo}/${sentinel}` itself, which is
  // exactly the drift `lib/codeUrl.ts`'s header doc (root CLAUDE.md #35) and
  // this unit's `nav/rawUrls.test.ts` lint exist to stop.
  function repoRoute(build: (repo: string) => string): string {
    return repoFor ? build(repoFor) : "/";
  }
  useCommandHandlers({
    "cmd.palette.search": () => {
      setOmniboxQuery("");
      setOmniboxOpen(true);
    },
    "cmd.palette.commands": () => {
      setOmniboxQuery(">");
      setOmniboxOpen(true);
    },
    "help.keys": () => setHelpOpen((v) => !v),
    "dismiss.help": () => setHelpOpen(false),
    // V70-A6 — `u` is still plain browser Back, with ONE trail-aware branch:
    // a tab the Ramp opened has an EMPTY back stack (it is entry 1 of a fresh
    // tab), so Back there would leave the app entirely. In that one case `u`
    // PUSHES the origin instead, which keeps the destination exactly one Back
    // away and never tries to focus another tab (a `WindowProxy` handle can
    // silently no-op — §P7). Everywhere else there is no second stack.
    "nav.back": () => {
      const origin = trailOriginUrl();
      if (origin) navigate(origin);
      else navigate(-1);
    },
    "nav.forward": () => navigate(1),
    // V70-A6 — the jump list is `scope: global` (recon G5). `Reader` registers
    // the real handlers while it is mounted (last registration wins); these
    // are the fallback for every OTHER route, where there is no open buffer.
    // `enterRing` deliberately does not record a tip: a list page has no file
    // to record, and stepping past the tip would skip the very place the
    // operator means.
    "jump.back": () => {
      const t = enterRing(1);
      if (t) navigate(codeUrl({ repo: t.repo, path: t.path, line: t.line }));
    },
    "jump.forward": () => {
      const t = goForward(1);
      if (t) navigate(codeUrl({ repo: t.repo, path: t.path, line: t.line }));
    },
    "nav.home": () => navigate("/"),
    "nav.inbox": () => navigate("/~inbox"),
    "nav.search-page": () => navigate("/search"),
    "nav.reviews": () => navigate(repoRoute(reviewsUrl)),
    "nav.branches": () => navigate(repoRoute(branchesUrl)),
    "nav.todos": () => navigate(repoRoute(todosUrl)),
    "nav.comments": () => navigate(repoRoute(commentsUrl)),
    "nav.rails": () => navigate(repoRoute((r) => railsUrl(r))),
    "nav.sets": () => navigate(repoRoute(setsUrl)),
    "nav.prs": () => navigate(repoRoute(prsUrl)),
    "nav.canvas": () => navigate(repoRoute(canvasPageUrl)),
    "nav.boards": () => navigate(repoRoute(boardsPageUrl)),
    "nav.browser": () => navigate(repoRoute(browserPageUrl)),
    "nav.hotspots": () => navigate(repoRoute(hotspotsUrl)),
    "nav.recipes": () => navigate(repoRoute(recipesPageUrl)),
    "nav.stacks": () => navigate(repoRoute(stacksPageUrl)),
    "view.theme-cycle": () => cycleTheme(),
  });

  // D24 learn mode: the first mouse click on a control that HAS a key earns
  // one toast naming it. Once per command, browser-local, never a signal.
  useLearnMode();

  const bus = useCommands();
  useEffect(() => {
    if (!pendingCmd) return;
    setPendingCmd(null);
    if (!bus.run(pendingCmd.id)) {
      toast.warn(`No handler for "${pendingCmd.id}" on this surface`);
    }
  }, [pendingCmd, bus]);

  return (
    <>
      <div className="kbc-app">
        <TopBar />
        {trailHop && (
          <div className="kbc-trailbar" data-region="trailbar">
            <TrailOriginChip
              trail={trailLink.trail}
              step={trailLink.link?.step ?? 0}
              onReturn={(url) => navigate(url)}
            />
          </div>
        )}
        <div className="kbc-approute">
          <ErrorBoundary resetKey={loc.pathname}>
            <Suspense fallback={<RouteFallback />}>
              <Routes>
                <Route path="/" element={<Home />} />
                <Route path="/search" element={<Search />} />
                {/* S2-A — unified inbox: repo-less, top-level, sibling of
                    Home/Search (same footing as the DCB entry ramps just
                    below). */}
                <Route path="/~inbox" element={<Inbox />} />
                {/* DCB W2.B (R2/D14) — the repo-less entry ramps. Plain
                    top-level routes beside `/`/`/search` (they share no
                    `/r/:repo/` prefix, so position relative to the reader
                    splat below doesn't matter); `:docId` is a single
                    non-slash segment so it never collides with the
                    `by-path/*` splat route. */}
                <Route path="/~lens/:kb/:docId" element={<LensEntry />} />
                <Route path="/~lens/:kb/by-path/*" element={<LensEntryByPath />} />
                <Route path="/r/:repo/~commit/:sha" element={<Commit />} />
                <Route path="/r/:repo/~compare" element={<Compare />} />
                <Route path="/r/:repo/~branches" element={<Branches />} />
                <Route path="/r/:repo/~range-diff" element={<RangeDiff />} />
                <Route path="/r/:repo/~prs" element={<Prs />} />
                <Route path="/r/:repo/~sets" element={<Sets />} />
                <Route path="/r/:repo/~sets/:id" element={<SetDetail />} />
                <Route path="/r/:repo/~sets/:id/~tour" element={<Tour />} />
                <Route path="/r/:repo/~workspaces" element={<Workspaces />} />
                <Route path="/r/:repo/~todos" element={<Todos />} />
                <Route path="/r/:repo/~comments" element={<Comments />} />
                <Route path="/r/:repo/~rails" element={<Rails />} />
                <Route path="/r/:repo/~hotspots" element={<Hotspots />} />
                <Route path="/r/:repo/~reviews" element={<Reviews />} />
                <Route path="/r/:repo/~reviews/:id/diff" element={<ReviewDiff />} />
                <Route path="/r/:repo/~reviews/:id/diff/*" element={<ReviewDiff />} />
                <Route path="/r/:repo/~reviews/:id" element={<ReviewDetail />} />
                {/* PRR-U3 short finding permalink — see FindingEntry.tsx's
                    own doc. A distinct pattern from the two `.../diff*`
                    routes above (an extra `/f/:slug` segment), so ranking
                    relative to them is irrelevant. */}
                <Route path="/r/:repo/~reviews/:id/f/:slug" element={<FindingEntry />} />
                <Route path="/r/:repo/~recipes" element={<Recipes />} />
                <Route path="/r/:repo/~stacks" element={<Stacks />} />
                <Route path="/r/:repo/~canvas" element={<Canvas />} />
                <Route path="/r/:repo/~boards" element={<Boards />} />
                <Route path="/r/:repo/~boards/:slug" element={<BoardDetail />} />
                <Route path="/r/:repo/~browser" element={<Browser />} />
                {/* DCB W2.B (R2/D14) — the repo-scoped lens, no splat: the
                    artifact id is the doc key on every DCB wire/route. */}
                <Route path="/r/:repo/~lens/:kb/:docId" element={<Lens />} />
                <Route path="/r/:repo/*" element={<ReaderRoute />} />
                <Route path="/session/:sid/diff" element={<SessionDiff />} />
              </Routes>
            </Suspense>
          </ErrorBoundary>
        </div>
      </div>
      {omniboxOpen && (
        <Omnibox initialQuery={omniboxQuery} onClose={() => setOmniboxOpen(false)} />
      )}
      {/* V70-A5 — the `?` sheet is mounted ONCE, here: it used to be
          reachable from exactly two of twenty-seven routes. `Reader` and
          `ReviewDiff` still mount their own instances with an explicit
          context; this one covers everywhere else, scoped to whatever the
          mounted surface published. */}
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} />
      <ToastHost />
    </>
  );
}

/// The app root. `ConfirmProvider` stays outermost (root CLAUDE.md #32 — one
/// destructive prompt host), `CommandRoot` sits inside it so the palette's
/// mutating rows can reach `useConfirm`, and the shell mounts inside both.
export default function App() {
  return (
    <ConfirmProvider>
      <CommandRoot>
        <AppShell />
      </CommandRoot>
    </ConfirmProvider>
  );
}
