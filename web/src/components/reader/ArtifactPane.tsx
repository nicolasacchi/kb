import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQueryClient } from "@tanstack/react-query";
import { useLiveTailPortal } from "../../context/LiveTailPortalContext";
import { useIsMobile } from "../../hooks/useIsMobile";
import { useKbs } from "../../hooks/useKbs";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import type { Anchor, DocSummary, ReviewFile } from "../../api/client";
import {
  artifactKbLadder,
  docIdQueryKey,
  fetchDocByIdLadder,
} from "../../api/artifactLookup";
import {
  recordOpen,
  recordReading,
  recordScroll,
  type ReadingResume,
  type ReadingSectionBeacon,
} from "../../api/history";
import type { ReadingSummary } from "../../api/reading";
import ContextBar from "../chrome/ContextBar";
import QueueBar from "../lists/QueueBar";
import DeskChangedBanner from "../chrome/DeskChangedBanner";

import TocSpy from "../TocSpy";
import { FolioHeader, FolioColophon } from "../FolioChrome";
import SessionContextCard from "./SessionContextCard";
import LiveTailPanel from "./LiveTailPanel";
import AnnotatorBridge, {
  type BridgeApi,
  type SelectionAnchor,
  type SelectionRect,
} from "../AnnotatorBridge";
import SelectionActions from "../SelectionActions";
import ErrataSheet from "../ErrataSheet";
import type { FolderEntry } from "../PreviewInspector";
import {
  useArtifactSessions,
  useSessionByArtifact,
  useSessionOutline,
  useSessionPresence,
} from "../../hooks/useSessions";
import { fetchSession } from "../../api/sessions";
import { sse } from "../../api/sse";
import { artifactOrigin, isOriginOfArtifact } from "../../lib/artifactHost";
import { artifactHref } from "../../lib/artifactHref";
import {
  resolveArtifactHref,
  type ArtifactLinkCtx,
} from "../../lib/artifactLinks";
import type { FlowEntry } from "../../lib/flowStack";
import type { PaneLoc } from "../../lib/paneUrl";
import { withKb } from "../../lib/navItems";
import { buildProvenanceBlock } from "../../lib/quote";
import { toast } from "../../lib/toast";
import PeekCard, {
  type PeekAnchorRect,
  type PeekTarget,
} from "../PeekCard";

// Iframe sandbox flags — must match kb_core::iframe::SANDBOX_FLAGS so
// the artifact origin's own probe behaves consistently with the daemon's
// injected probe. allow-same-origin lets localStorage work; the
// `<id>.artifacts.<root>` origin is kept distinct from the parent.
// allow-popups-to-escape-sandbox is what lets a ctrl/⌘/middle-clicked link
// inside an artifact open a *clean* (non-sandbox-inheriting) top-level tab,
// which the daemon-injected bounce then redirects to the kb SPA wrapper.
const SANDBOX =
  "allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox " +
  "allow-forms allow-modals allow-downloads";

// link-flow — how long a plain hover inside the artifact must settle before
// the peek card appears. Longer than the Alt-hover trigger's 300ms
// (`HotkeyRoot`): that one is explicitly ARMED by holding Alt, while this
// one fires on an unmodified hover, so it needs to be clearly past
// "the pointer swept across a link on its way somewhere else".
const PEEK_DWELL_MS = 350;
// The grace window between leaving the link and hiding the card, so the
// pointer can cross the gap onto it (the card cancels the hide on enter).
const PEEK_GRACE_MS = 300;

// postMessage shapes the SPA listens for from the artifact iframe.
// Excludes cm:* — those are handled by AnnotatorBridge (B3).
type ArtifactMessage =
  // link-flow — `sec` is an OPTIONAL addition to the trampoline payload
  // (`location.hash` without the `#`); absent for a fragment-less link, so
  // every pre-link-flow daemon keeps working byte-identically.
  | { kind: "open-artifact"; id: string; kb?: string; path?: string; sec?: string }
  | { kind: "pm:page"; label: string; src: string }
  | { kind: "kb-probe"; check: string; ok: boolean; detail?: string }
  // v0.6+ H3 runtime — scroll snapshot (debounced 500ms in the iframe).
  | { kind: "kb:scroll"; y: number; max: number }
  // link-flow — the in-artifact link relay. `kb:link-hover` fires on
  // `mouseover` of any `a[href]` (delegated, iframe-side) with the anchor's
  // rect in IFRAME-viewport coordinates; `kb:link-clear` on mouseout /
  // scroll / pagehide; `kb:link-open` after the runtime preventDefaults a
  // click on an artifact-shaped CROSS-ORIGIN link (the same-origin relative
  // path still goes through the trampoline + `location.replace`, #20).
  | {
      kind: "kb:link-hover";
      href: string;
      rect: { x: number; y: number; w: number; h: number };
      text?: string;
    }
  | { kind: "kb:link-clear" }
  | { kind: "kb:link-open"; href: string }
  // RP-track — per-section reading beacon (cumulative per visit).
  | {
      kind: "kb:reading";
      sections: ReadingSectionBeacon[];
      active_ms: number;
      last_section: string | null;
    };

/// The in-artifact selection the reader made (cm:selection), frozen with the
/// section that was active at capture time. Owned by the route (the chooser
/// is one-at-a-time across the reader) but built here, since only the pane
/// knows its own artifact's active section.
export type PaneSelection = {
  anchor: SelectionAnchor;
  rect: SelectionRect;
  sectionId: string | null;
  /// The ARTIFACT's own scroll offset when the rect above was measured (the
  /// iframe reports it on `cm:selection`). The rect is a one-shot snapshot,
  /// so it is only valid at this offset — the `kb:scroll` branch below
  /// compares the two to tell a reader scrolling away from their selection
  /// (rect now stale) from a debounced beacon describing a scroll that
  /// happened BEFORE it (rect still exact).
  scrollY: number;
};

export type ArtifactPaneProps = {
  // ── the pane contract ────────────────────────────────────────────────
  /// The artifact this pane reads: kb + source-relative path (the permalink
  /// grammar `/a/:kb/*`).
  kb: string;
  relPath: string;
  /// This pane's OWN section deep link (heading id — scroll + arrival flash),
  /// or null. W3.P-b: it must be a PROP, not `searchParams.get("sec")`, because
  /// `?sec=` names a heading in the PRIMARY artifact — a second pane reading it
  /// would post a scroll-to-id for a section that belongs to a different
  /// document. Pane 1 gets the route param; pane 2 gets `PaneLoc.sec` (the
  /// third field of the `?pane2=` grammar).
  sec: string | null;
  /// W3.E/S5 — `?turn=<N|end>` deep link, same PROP-not-searchParams
  /// discipline as `sec` above (pane 2 has no turn grammar yet — the
  /// `PaneLoc`/`?pane2=` value carries no `turn` field this wave, a
  /// deliberate scope trim; see the W3 build report). `"end"` targets
  /// `#ses-outcome`; a numeric string is an outline ORDINAL, resolved to the
  /// stable `t-<uuid12>` render id via the lazy `/view?fields=outline` fetch
  /// (`useSessionOutline`) — only fired for a `memory-session` doc.
  turn: string | null;
  /// True when this pane owns the keyboard. Window-level reader keybinds
  /// (`[`/`]` sibling nav, `o`/`b`/`y p`) only bind while focused, so two
  /// panes never both act on one keystroke. W3.P-b: with a split open this
  /// tracks the route's `focusedPane`; without one it is constantly true.
  focused: boolean;
  /// Ask the route to make this the focused pane. Fired when this pane's OWN
  /// artifact origin posts a message (its iframe is where the reading is
  /// happening) — the only focus signal a cross-origin iframe can give the
  /// parent, since a click inside it never reaches us. Never fires while
  /// already focused.
  onFocus?: () => void;
  /// True for the pane that owns the ROUTE — i.e. whatever writes the URL
  /// (`?p=` page switches, the trail params) and renders the route-level
  /// QueueBar. A secondary pane must not rewrite the address bar.
  isPrimary: boolean;

  // ── the resolved artifact (route-owned; one doc query, see #23) ───────
  doc: DocSummary | null;
  id: string;
  error: string | null;
  hostSuffix: string;
  /// The file was deleted from disk (route-level SSE watch).
  removed: boolean;
  /// Bumped on `artifact.indexed` — folded into the iframe key to remount.
  reloadNonce: number;
  descendants: FolderEntry[] | undefined;
  /// ContextBar's position/total/prev/next counter. Derived by the route
  /// from the SAME folder-bounded, filename-sorted direct-sibling walk the
  /// `[`/`]` hotkey below does, so the two can never disagree; it lives up
  /// there because the native-note path (no pane) needs it too.
  siblingNav:
    | {
        position: number;
        total: number;
        onPrev: () => void;
        onNext: () => void;
      }
    | undefined;
  /// Merged (live ∪ server) reading summary — feeds the TOC heatmap.
  summary: ReadingSummary | null;

  // ── reader chrome the ROUTE owns (one URL, one dock) ──────────────────
  immersive: boolean;
  onSetImmersive: (on: boolean) => void;
  folio: boolean;
  onToggleFolio: () => void;
  inspectorOpen: boolean;
  onToggleInspector: () => void;
  /// W3.P-b — the pane's own split verb, rendered in ITS ContextBar (which
  /// is per-artifact chrome; the inspector rail is not — invariant #30).
  /// `"open"` on the primary pane, `"close"` on the second. Undefined ⇒ the
  /// button is hidden (mobile, or an artifact with no sibling to open).
  splitMode?: "open" | "close";
  onSplit?: () => void;

  // ── review / annotate (the dock is a route-level singleton) ───────────
  reviewActive: boolean;
  reviewFile: ReviewFile | null;
  /// This pane's OWN handle on its annotator — the comments dock drives
  /// flash/emphasize through whichever one the focused pane owns. W3.P-b:
  /// one ref per pane (`bridgeRef` / `bridge2Ref` in routes/detail.tsx);
  /// sharing one would have the second mount clobber the first's handle and
  /// send every dock jump into the wrong artifact.
  bridgeRef: React.MutableRefObject<BridgeApi | null>;
  annotateMode: boolean;
  onToggleAnnotate?: () => void;
  commentCount: number;
  errataMode: boolean;
  activeCommentId: string | null;
  onCommentJump: (commentId: string) => void;
  onCommentHover: (commentId: string | null) => void;
  onComposeAnchor: (anchor: Anchor) => void;
  onFocusComment: (commentId: string) => void;
  onExitAnnotate: () => void;
  selection: PaneSelection | null;
  onSelectionChange: (selection: PaneSelection | null) => void;
  /// W2.16-mobile — SelectionActions' new "comment" button (the selection→
  /// comment gap this closes: authoring from a selection used to need
  /// annotate mode + a click on text that was still selected, unreachable
  /// on a phone). Handed the anchor directly rather than reading `selection`
  /// again on the route side — this pane already owns the anchor that was
  /// captured, and passing it through keeps the callback a pure forward.
  onComposeSelection: (anchor: SelectionAnchor) => void;
  /// The PULL lane's answer (`cm:selection-pull`), forwarded verbatim from
  /// this pane's AnnotatorBridge. The route asks via the bridge handle it
  /// already owns (`bridgeRef.current.querySelection()` — the same ref the
  /// dock drives flash/emphasize through), so there is no second trigger
  /// prop; only the ANSWER needs a route-side home, because it arrives
  /// asynchronously on the pane's own origin-checked message listener.
  /// `anchor === null` = the iframe had nothing to report.
  onSelectionPull?: (anchor: SelectionAnchor | null) => void;

  // ── reading flow (link-flow) ─────────────────────────────────────────
  /// A consume-once scroll offset the route popped off the flow stack for
  /// THIS artifact (a return, by chip / `u` / browser Back). Sits between
  /// the `?sec=` deep link and the server's resume value in the kb-probe
  /// ladder below: an explicit deep link still wins, but a return beats a
  /// possibly-stale server snapshot (the flow value is the live offset the
  /// reader actually left at). PRIMARY pane only.
  flowSeedY?: number | null;
  /// Where this pane publishes its LIVE scroll/section for the route's flow
  /// capture — a mutable ref, written from the `kb:scroll` / `kb:section`
  /// handlers so it never costs a render. PRIMARY pane only (the flow stack
  /// follows the route's own artifact; a compare pane isn't a descent).
  flowStateRef?: React.MutableRefObject<{ y: number; sec: string | null }>;
  /// The flow chip rendered in THIS pane's ContextBar (invariant #30 — the
  /// chip is per-artifact chrome, NOT a 7th inspector sub-tab). Omitted on
  /// the compare pane.
  flow?: {
    entries: readonly FlowEntry[];
    onBack: () => void;
    onJump: (depth: number) => void;
  };
  /// "Open beside" from the peek card — the route's own `openSplit`.
  /// Omitted ⇒ the card hides the button (mobile, split already open, or no
  /// second pane possible).
  onPeekSplit?: (loc: PaneLoc) => void;
};

/// The per-visit history lifecycle for ONE artifact: the `POST /history/open`
/// row (`visit_id` + the resume scroll + the reading seed), the beacon
/// pending/in-flight guards the message pump flushes through, and the
/// final-flush teardown.
///
/// W3.P-a — this is a HOOK, not route state, because invariants #8/#19 make a
/// visit strictly per-artifact: the `history` table is append-only and the
/// scroll/reading upserts target the row this ref names. `ArtifactPane` calls
/// it so every pane owns its own visit; the route's native-note path (which
/// renders no pane, but opening a note is still a visit) calls it through the
/// zero-DOM `NoteVisit` helper in `routes/detail.tsx`.
export function useArtifactVisit(kb: string | null, id: string | null) {
  // v0.6+ H4/H6 — history visit state. `visit_id` identifies the
  // current open row (returned by POST /history/open). `pendingScrollY`
  // is the Y the iframe should land on the next time it mounts — seeded
  // by recordOpen, kept fresh by every `kb:scroll` message. The probe
  // handler re-asserts it on every fire (idempotent: scrollTo to the
  // current scrollY is a no-op; all probes for one iframe load fire
  // pre-user-scroll). Persisting across probes is what makes the
  // `artifact.indexed` / annotate-toggle / page-switch remount paths
  // land on the same line.
  //
  const visitRef = useRef<{ id: number | null; pendingScrollY: number }>({
    id: null,
    pendingScrollY: 0,
  });
  // In-flight scroll-POST guard so concurrent kb:scroll messages
  // collapse into one request at a time (the runtime debounces 500ms;
  // we additionally cap at one POST in flight).
  const scrollInflight = useRef(false);
  const scrollPending = useRef<{ y: number; max: number } | null>(null);
  // RP-track — reading beacon mirror of the scroll-flush guards. The iframe
  // posts CUMULATIVE per-visit reading state (kb:reading); we hold the latest
  // snapshot + one in-flight POST. `readingSeed` is the open response's
  // resume baseline, posted into the iframe on probe (seed-on-open) so its
  // cumulative counters never regress across a 30-min resume.
  const readingInflight = useRef(false);
  const readingPending = useRef<{
    sections: ReadingSectionBeacon[];
    active_ms: number;
    last_section: string | null;
  } | null>(null);
  const readingSeed = useRef<ReadingResume | null>(null);
  // W1.reader — the message-listener effect below (re)assigns these to its
  // own `postScrollFlush`/`postReadingFlush` closures on every run. The
  // recordOpen effect calls through them (never duplicating the POST/retry
  // logic) once `visitRef.current.id` finally resolves, so a scroll/reading
  // beacon that arrived — and bailed out — while the visit id was still null
  // isn't stranded. That race is normally sub-millisecond, but the prerender
  // guard below can defer recordOpen for as long as the page sits
  // prerendered, widening it substantially.
  const kickScrollFlush = useRef<() => void>(() => {});
  const kickReadingFlush = useRef<() => void>(() => {});

  // v0.6+ H4 — register the visit and seed the resume scroll. Best
  // effort: a 4xx/5xx here just leaves visit.id = null and the user
  // doesn't get resume + their scroll won't be recorded. Per-mount;
  // reloads of the same iframe inside one visit (via reloadNonce)
  // share the same visit_id (the 30-min gap rule on the server side
  // returns the existing row).
  //
  // W1.reader — invariants #8/#19: `history.open` INSERTs an append-only row
  // and seeds reading state, so it must not fire for a document the browser
  // is only *prerendering* (Speculation Rules may prerender several
  // candidate pages and discard all but one — recording an "open" + a
  // reading seed for a page the user never actually visits would corrupt
  // both). Gate the ENTIRE body behind `document.prerendering` (ambient decl:
  // types/dom-speculation.d.ts) and defer it to the one-shot
  // `prerenderingchange` event fired on activation, aborting cleanly via the
  // `alive` guard if the component unmounts first (the prerendered page never
  // won). Ordinary (non-prerendered) navigation — effectively all traffic
  // today — takes the `else` branch and is byte-identical to before.
  useEffect(() => {
    if (!kb || !id) return;
    let alive = true;
    let offPrerender: (() => void) | undefined;

    function start() {
      visitRef.current = { id: null, pendingScrollY: 0 };
      readingSeed.current = null;
      recordOpen(kb as string, id as string)
        .then((r) => {
          if (!alive) return;
          visitRef.current = { id: r.visit_id, pendingScrollY: r.scroll_y };
          // RP-track seed-on-open: stash the resume baseline; posted into the
          // iframe on the next kb-probe.
          readingSeed.current = r.reading ?? null;
          // A scroll/reading beacon may have arrived (and bailed out, since
          // `visitRef.current.id` was still null) while this deferred open
          // was pending — kick both flush paths now that a visit id exists
          // so neither is left stranded until a NEW beacon happens to fire.
          // Mirrors the onMessage handler's own `!inflight` guard below.
          if (scrollPending.current && !scrollInflight.current) {
            kickScrollFlush.current();
          }
          if (readingPending.current && !readingInflight.current) {
            kickReadingFlush.current();
          }
        })
        .catch((e) => console.warn("[history] open failed", e));
    }

    if (typeof document !== "undefined" && document.prerendering) {
      const onActivate = () => {
        if (alive) start();
      };
      document.addEventListener("prerenderingchange", onActivate, {
        once: true,
      });
      offPrerender = () =>
        document.removeEventListener("prerenderingchange", onActivate);
    } else {
      start();
    }

    return () => {
      alive = false;
      offPrerender?.();
      // RP-track: reliable FINAL reading flush (F4) with the OLD visit id
      // before we reset. The iframe's dying pagehide beacon may not survive
      // the cross-origin unmount, but the parent always holds the latest
      // cumulative snapshot and the server max-merge makes a slightly-stale
      // final flush harmless.
      const pend = readingPending.current;
      const vid = visitRef.current.id;
      if (pend && vid !== null && kb && id) {
        void recordReading(
          kb,
          vid,
          id,
          pend.sections,
          pend.active_ms,
          pend.last_section,
        );
      }
      readingPending.current = null;
      readingInflight.current = false;
      readingSeed.current = null;
      visitRef.current = { id: null, pendingScrollY: 0 };
      scrollPending.current = null;
      // Reset inflight too so any old POST that resolves AFTER this
      // cleanup can't leave the new lifecycle thinking a flush is
      // already in progress. The old POST's .finally() will still
      // run, but it'll see scrollPending=null and exit cleanly.
      scrollInflight.current = false;
    };
  }, [kb, id]);

  return {
    visitRef,
    scrollInflight,
    scrollPending,
    readingInflight,
    readingPending,
    readingSeed,
    kickScrollFlush,
    kickReadingFlush,
  };
}

// W3.P-a — ONE artifact pane: the iframe plus everything that belongs to
// *reading that one artifact* — its ContextBar, the queue/pages chrome, the
// TOC spy, the annotator bridge + selection chooser + errata slip, and
// (load-bearing) its OWN visit lifecycle.
//
// Why the visit lifecycle lives here and not on the route: `history.open`
// INSERTs an append-only row (#8) and seeds reading progress (#19) for ONE
// artifact id, and the `kb:scroll` / `kb:reading` beacons that follow are
// POSTed against that row. Every one of those beacons is attributed by EXACT
// artifact origin (`isOriginOfArtifact`, not the suffix-only trust boundary —
// every artifact iframe passes that), so a second pane's beacons can never
// land in this pane's visit. `visitRef` / the scroll+reading pending/inflight
// guards / the flush kickers are all per-pane state for the same reason.
//
// The route (routes/detail.tsx) keeps what is genuinely singular: the doc
// query, the dock (inspector / comments / versions), the panel mode, and the
// native-note render path. This is a pure extraction — with exactly one pane
// mounted (focused + primary) the rendered DOM is identical to the pre-split
// route, which is the whole success criterion for the split.
export default function ArtifactPane({
  kb,
  relPath,
  sec,
  turn,
  focused,
  onFocus,
  isPrimary,
  doc,
  id,
  error,
  hostSuffix,
  removed,
  reloadNonce,
  descendants,
  siblingNav,
  summary,
  immersive,
  onSetImmersive,
  folio,
  onToggleFolio,
  inspectorOpen,
  onToggleInspector,
  splitMode,
  onSplit,
  reviewActive,
  reviewFile,
  bridgeRef,
  annotateMode,
  onToggleAnnotate,
  commentCount,
  errataMode,
  activeCommentId,
  onCommentJump,
  onCommentHover,
  onComposeAnchor,
  onFocusComment,
  onExitAnnotate,
  selection,
  onSelectionChange,
  onComposeSelection,
  onSelectionPull,
  flowSeedY,
  flowStateRef,
  flow,
  onPeekSplit,
}: ArtifactPaneProps) {
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  // Hooks must run unconditionally (Rules of Hooks) — the LiveTailPanel
  // render below stays gated; only the portal *decision* uses these.
  const isMobileView = useIsMobile();
  const { mobilePortalNode } = useLiveTailPortal();
  // RLs1 — section deep link (?sec=<heading-id>). Beats scroll-resume
  // until "consumed": the kb-probe path posts kb:scroll-to-id (with an
  // arrival flash) instead of the kb:scroll-to resume; the first
  // kb:scroll message — including the one our own programmatic jump
  // emits — flips `secConsumedRef`, after which iframe remounts resume
  // normally (by then the resume position IS the section).
  // (`sec` is a PROP — see the prop doc. Mirrored into a ref for the probe
  // handler, whose listener effect deliberately doesn't rebind on a
  // param-only navigation.)
  const secConsumedRef = useRef(false);
  const secRef = useRef(sec);
  secRef.current = sec;
  // Mirror searchParams for the onMessage closure (same staleness trap
  // as kbRef — the effect doesn't rebind on param-only navigation).
  const searchParamsRef = useRef(searchParams);
  useEffect(() => {
    searchParamsRef.current = searchParams;
  }, [searchParams]);
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [pages, setPages] = useState<{ label: string; src: string }[]>([]);
  // W2.16 — a lightweight, always-on shadow of the runtime's `kb:section`
  // signal (TocSpy and the folio header keep their own reactive copies for
  // their own rendering — this is a ref, not state, purely so a
  // selection-cite built with the panel/folio off can still note "nearest
  // section" without forcing a re-render on every heading crossing).
  const currentSectionRef = useRef<string | null>(null);
  // Destructured so each is the same stable `useRef` object every render —
  // the message pump closes over them without re-binding its listener.
  const {
    visitRef,
    scrollInflight,
    scrollPending,
    readingInflight,
    readingPending,
    readingSeed,
    kickScrollFlush,
    kickReadingFlush,
  } = useArtifactVisit(kb, id);
  // Mirror `kb`/`id` into refs so the scroll-flush path reads the
  // CURRENT values rather than the values captured by the effect
  // closure when the POST started. Without this, a setTimeout(flush)
  // scheduled by an in-flight POST's `.finally()` after the user has
  // navigated would fire with the OLD effect's `kb` even though it
  // reads `visitRef.current.id` (the NEW visit) and
  // `scrollPending.current` (the NEW pending). Same-kb sibling nav
  // doesn't trip this; cross-kb nav would, and the mismatch would
  // POST one artifact's scroll into another artifact's visit row.
  // Deep-review finding K4.
  const kbRef = useRef(kb);
  const idRef = useRef(id);
  useEffect(() => {
    kbRef.current = kb;
    idRef.current = id;
  }, [kb, id]);
  // W2.7 — the folio running header's "current section" signal: a
  // folio-scoped window-message listener mirroring TocSpy's own
  // kb:toc/kb:section join (recon-sanctioned: cheap, and TocSpy keeps its
  // independent copy for its overlay). Only tracked while folio is on.
  const [folioToc, setFolioToc] = useState<{ id: string; text: string }[]>([]);
  const [folioSectionId, setFolioSectionId] = useState<string | null>(null);

  // Fresh artifact — drop the per-pane signals that describe the artifact we
  // just left. (The route resets its own per-artifact UI state in parallel;
  // child effects run first, and the two sets are disjoint.)
  // (`pages` is deliberately NOT cleared — it never was on the route either;
  // the multi-page nav is rebuilt from the new artifact's own `pm:page`
  // probes. Preserved verbatim so this split stays behaviour-free.)
  useEffect(() => {
    if (!kb || !relPath) return;
    // W2.7 — a fresh artifact's TOC/section is unrelated to the previous
    // one's; drop both so the folio header never briefly shows a stale
    // section from the artifact just left.
    setFolioToc([]);
    setFolioSectionId(null);
    currentSectionRef.current = null;
  }, [kb, relPath]);

  // W2.6b — "y p" provenance yank needs the artifact's origin session id
  // when known (`ArtifactSessionOut.authored`). Same hook PreviewInspector
  // already calls for its own sessions-touched panel — reusing the query
  // key here just means react-query dedupes the two call sites into one
  // request rather than adding a new endpoint.
  const { sessions: artifactSessions } = useArtifactSessions(kb ?? null, id);
  const originSessionId = useMemo(
    () => artifactSessions.find((s) => s.authored)?.session_id ?? null,
    [artifactSessions],
  );

  useEffect(() => {
    if (!folio) return;
    function onMessage(e: MessageEvent) {
      // W3.P-a — exact origin: the folio header names THIS pane's section.
      if (!isOriginOfArtifact(e.origin, id, kb, hostSuffix)) return;
      const data = e.data as
        | { kind?: string; id?: string; toc?: { id: string; text: string }[] }
        | null;
      if (!data || typeof data !== "object" || !data.kind) return;
      if (data.kind === "kb:toc" && Array.isArray(data.toc)) {
        setFolioToc(data.toc);
      } else if (data.kind === "kb:section" && typeof data.id === "string") {
        setFolioSectionId(data.id);
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [folio, hostSuffix, id, kb]);
  // W2.16 — the always-on twin of the folio listener just above: same
  // `kb:section` signal, but tracked into a ref (no re-render) regardless
  // of folio so a selection-cite built with folio off can still note the
  // "nearest section". A second listener rather than widening the folio
  // one above so that effect's folio-gating stays untouched.
  useEffect(() => {
    function onMessage(e: MessageEvent) {
      if (!isOriginOfArtifact(e.origin, id, kb, hostSuffix)) return;
      const data = e.data as { kind?: string; id?: string } | null;
      if (
        data &&
        typeof data === "object" &&
        data.kind === "kb:section" &&
        typeof data.id === "string"
      ) {
        currentSectionRef.current = data.id;
        // link-flow — publish it for the route's flow capture (a ref write,
        // no render). The pane owns this signal already; the route needs it
        // at the instant the reader navigates away.
        if (flowStateRef) flowStateRef.current.sec = data.id;
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [hostSuffix, id, kb, flowStateRef]);
  const folioSectionTitle = useMemo(
    () => folioToc.find((t) => t.id === folioSectionId)?.text ?? null,
    [folioToc, folioSectionId],
  );

  // Track U — multi-page active sub-page from `?p=` (e.g. `chapter2.html`).
  // Empty string at the entrypoint; the artifact's own path is the route
  // splat, so the page can't share the path without ambiguity.
  const activePage = searchParams.get("p") ?? "";

  // RLs1 — same-artifact section hops (a trail next/prev between two
  // sections of one artifact, or a TOC-copied link opened in place):
  // only the search params change, the iframe never remounts, so post
  // the jump straight into the live frame. On a fresh mount this fires
  // before the iframe is ready and the message is simply lost — the
  // kb-probe path covers that case.
  useEffect(() => {
    secConsumedRef.current = false;
    if (!sec) return;
    const frame = iframeRef.current?.contentWindow;
    if (!frame || !id) return;
    try {
      // Target the artifact's own origin (not "*") so only the intended
      // cross-origin iframe can receive the jump — invariant #7.
      frame.postMessage(
        { kind: "kb:scroll-to-id", id: sec, flash: true },
        artifactOrigin(id, kb, hostSuffix),
      );
    } catch {
      // iframe not ready — the probe path covers it.
    }
  }, [sec]);

  // W3.E/S3/S5 — the session join: ONE by-artifact fetch feeds BOTH the
  // `SessionContextCard` (S3) and `?turn=N` resolution (S5), whenever the
  // open doc IS a session capture (#11's canonical sqlite join, never the
  // filename regex the deleted `SessionSelfLink` used).
  const isSessionDoc = doc?.kb_category === "memory-session";
  const { data: sessionByArtifact } = useSessionByArtifact(
    isSessionDoc ? kb : null,
    isSessionDoc ? id : null,
  );
  const turnIsNumeric = !!turn && turn !== "end" && /^\d+$/.test(turn);
  const { outline: turnOutline } = useSessionOutline(
    sessionByArtifact?.session?.session_id,
    turnIsNumeric,
  );
  // W3.E/S5 — the resolved `?turn=` target, mirrored into a ref (same
  // pattern as `secRef` above) so the kb-probe handler below can read the
  // CURRENT value without the message-pump effect (which doesn't rebind on
  // a param-only navigation) going stale.
  //
  // W7 (R15/LF-4) — a `t-<uuid12>` render id (the live→captured handoff's
  // deep link) is ALREADY the render-time anchor id — pass it straight
  // through, no outline lookup needed. This is the "existing iframe-src
  // fragment mechanism (the W3 ?turn= plumbing)" the handoff reuses
  // verbatim, per the build notes: turn ids are IDENTICAL in the live
  // delta and the captured render (R2), so this is the ONLY branch that
  // makes the handoff land on the exact same turn with no jump.
  const turnTargetRef = useRef<string | null>(null);
  turnTargetRef.current =
    turn && isSessionDoc
      ? turn === "end"
        ? "ses-outcome"
        : turn.startsWith("t-")
          ? turn
          : (turnOutline.find((row) => row.n === Number(turn))?.id ?? null)
      : null;
  // Own consumed-ref (distinct from `secConsumedRef` — sec and turn are
  // independent deep-link intents, and either may be present). Reset when
  // `turn` itself changes — a fresh deep-link intent — NOT keyed on
  // `turnOutline`, which can still be reflowing (async outline fetch) while
  // the SAME turn value settles; resetting on that would spuriously re-arm
  // (and re-flash) a jump that already landed.
  const turnConsumedRef = useRef(false);
  useEffect(() => {
    turnConsumedRef.current = false;
  }, [turn]);
  useEffect(() => {
    if (!turn || !isSessionDoc) return;
    const targetId = turnTargetRef.current;
    if (!targetId) return; // numeric turn not resolved yet (or out of range)
    const frame = iframeRef.current?.contentWindow;
    if (!frame || !id) return;
    try {
      frame.postMessage(
        { kind: "kb:scroll-to-id", id: targetId, flash: true },
        artifactOrigin(id, kb, hostSuffix),
      );
    } catch {
      // iframe not ready — a fresh (cold) mount can drop this silently: the
      // iframe's own listener isn't registered yet, so postMessage succeeds
      // without throwing but nothing ever receives it. Same failure mode
      // `sec` used to have (RLs1); the kb-probe handler below now retries
      // the SAME `turnTargetRef` target once the iframe actually announces
      // it's listening. This effect alone still covers warm in-app
      // navigations (e.g. `jumpToOutcome`'s `?turn=end` toggle against an
      // already-loaded iframe, where the postMessage above lands for real).
    }
  }, [turn, isSessionDoc, turnOutline, id, kb, hostSuffix]);

  // W7 (R15/LF-1) — Tier-1 presence, polled ONLY while an open session page
  // is mounted (LF-1's "from /sessions and an open session page ONLY"
  // rule); unconfigured/non-loopback daemons just report `enabled:false`
  // (`tier1Available` below), so `presenceSet` stays empty and every
  // consumer falls back to Tier 0.
  const { presenceSet, enabled: tier1Available } = useSessionPresence(isSessionDoc);

  // W7 (R15/LF-2) — follow mode. `?follow=1`, PRIMARY-pane-only (mirrors
  // `?turn=`'s own URL-write gating — LF-2's deferred item explicitly
  // refuses follow-in-pane-2: "multiplies the poll loops"). A secondary
  // compare pane simply never shows the Follow chip (SessionContextCard's
  // `onToggleFollow` prop stays undefined for it — see the render below).
  const followMode = isPrimary && searchParams.get("follow") === "1";
  const toggleFollow = useCallback(() => {
    const next = new URLSearchParams(searchParams);
    if (next.get("follow") === "1") next.delete("follow");
    else next.set("follow", "1");
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams]);

  // W7 (R15/LF-2) — Tier-0 follow: "pinned" tracks whether the reader was
  // at (near) the bottom of the transcript the last time it scrolled —
  // mirrors the `kb:scroll` beacon's own (y, max) pair (#8/#19), read here
  // as a side effect of the SAME message (no second listener). Never
  // persisted (#23-clean) — component state, matches LF-2's "pin state is
  // component state, never persisted".
  const PIN_NEAR_BOTTOM_PX = 96;
  const nearBottomRef = useRef(false);

  // W7 (R15/LF-2) — the live→captured handoff, Tier-0 shape ("reload-on-
  // capture"): while following, a `session.captured` for the SAME
  // session_id (never the filename regex — #11's canonical join) either
  // navigates to the fresh capture (pinned) or leaves a quiet toast
  // (unpinned — "never yank when unpinned"). Debounced per-sid by the
  // browser's own event ordering (one SSE event per capture; a burst
  // within one gate window still only fires once per landed capture here
  // since each carries a distinct artifact_id and this effect doesn't
  // re-navigate on its own past captures).
  const followSessionId = sessionByArtifact?.session?.session_id ?? null;
  useEffect(() => {
    if (!followMode || !isSessionDoc || !followSessionId || !kb) return;
    return sse.subscribeEvent("session.captured", (p) => {
      const payload = p as { session_id?: string };
      if (payload.session_id !== followSessionId) return;
      if (!nearBottomRef.current) {
        // LF-2: "never yank when unpinned" — a quiet notice, no navigation.
        // (Documented simplification: the design's in-reader "divider +
        // pill" is a toast here rather than a bespoke DOM element — see the
        // build report.)
        toast.info("capture landed — refresh to see it");
        return;
      }
      void (async () => {
        const detail = await fetchSession(followSessionId);
        if (!detail) return;
        // Tier-1 (when available): a sid that's dropped OUT of the live set
        // means the session actually Stopped — exit follow mode onto the
        // outcome footer. Tier-0-only (`tier1Available` false, i.e. no
        // `[sessions] live_transcripts_dir` configured or non-loopback)
        // can't tell mid-run from Stopped, so it stays following — an
        // honest degradation, never a false "ended".
        const stillLive = presenceSet.has(followSessionId);
        const href = artifactHref(detail.kb, detail.source_relative, {
          turn: "end",
          follow: !tier1Available || stillLive,
        });
        if (tier1Available && !stillLive) {
          toast.ok("session ended — outcome");
        }
        navigate(href, { replace: true });
      })();
    });
  }, [
    followMode,
    isSessionDoc,
    followSessionId,
    kb,
    presenceSet,
    tier1Available,
    navigate,
  ]);

  // W3.E/S5 — the card's "↧ outcome" action. The PRIMARY pane writes
  // `?turn=end` (replace:true, #20 ethos — no history spam); the effect
  // above then fires the jump. A secondary pane owns no URL (#30 v0.29), so
  // it posts the SAME message directly instead of a URL round-trip — pane 2
  // has no `?turn=` grammar this wave anyway (see the prop doc comment).
  const jumpToOutcome = useCallback(() => {
    if (isPrimary) {
      const next = new URLSearchParams(searchParams);
      next.set("turn", "end");
      setSearchParams(next, { replace: true });
      return;
    }
    const frame = iframeRef.current?.contentWindow;
    if (!frame || !id) return;
    try {
      frame.postMessage(
        { kind: "kb:scroll-to-id", id: "ses-outcome", flash: true },
        artifactOrigin(id, kb, hostSuffix),
      );
    } catch {
      // iframe not ready.
    }
  }, [isPrimary, searchParams, setSearchParams, id, kb, hostSuffix]);

  // G3/G4 hotkeys — `[` previous sibling, `]` next sibling. Walks the
  // DIRECT same-folder subset only (descendants under subfolders are a
  // display-only toggle in the popover; keyboard nav stays
  // folder-bounded so it's deterministic). Skip when the user is typing
  // in an INPUT/TEXTAREA/contenteditable (cross-origin keystrokes
  // inside the iframe don't reach the parent at all, so we only need to
  // guard parent inputs). Skip when modifier keys are held. Wrap
  // around so single-handed reading flow stays snappy.
  //
  // W3.P-a — window-level, so only the FOCUSED pane binds it (one pane
  // today ⇒ always bound, unchanged).
  useEffect(() => {
    if (!focused) return;
    if (!kb || !doc || !descendants) return;
    const direct = descendants.filter((d) => d.folder === doc.folder);
    if (direct.length <= 1) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "[" && e.key !== "]") return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable)
      ) {
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const sortedDirect = [...direct].sort((a, b) =>
        a.filename.localeCompare(b.filename),
      );
      const idx = sortedDirect.findIndex((s) => s.isCurrent);
      if (idx < 0) return;
      e.preventDefault();
      const delta = e.key === "]" ? 1 : -1;
      const next =
        sortedDirect[(idx + delta + sortedDirect.length) % sortedDirect.length];
      navigate(artifactHref(kb, next.sourceRelative));
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [descendants, doc, kb, navigate, focused]);

  // The iframe↔parent message pump for THIS pane's artifact.
  const onFocusRef = useRef(onFocus);
  onFocusRef.current = onFocus;
  const focusedRef = useRef(focused);
  focusedRef.current = focused;
  const isPrimaryRef = useRef(isPrimary);
  isPrimaryRef.current = isPrimary;
  const onSelectionChangeRef = useRef(onSelectionChange);
  onSelectionChangeRef.current = onSelectionChange;
  // The live selection, reachable from the same long-lived pump — the
  // `kb:scroll` branch needs the offset it was captured at (see below).
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  // The message pump below is deliberately long-lived (it rebinds only on
  // kb/id/relPath/hostSuffix), so a breakpoint flip must reach it through a
  // ref like every other live value here — reading `isMobileView` directly
  // would either capture a stale value or force the listener to rebind on
  // every viewport change.
  const isMobileRef = useRef(isMobileView);
  isMobileRef.current = isMobileView;

  // ── link-flow: the in-artifact link peek ──────────────────────────────
  //
  // The artifact iframe is cross-origin, so a `mouseover` inside it never
  // bubbles to this window — the daemon runtime RELAYS it (`kb:link-hover`
  // + `kb:link-clear`, exact-origin-gated like every other beacon,
  // #8/#19). This pane turns that relay into the same `PeekCard` the
  // Alt-hover trigger mounts: one card, two triggers (#30).
  //
  // Timers, not state, for the dwell + hide: a hover that never settles
  // must not re-render the reader, and the grace window is what lets the
  // pointer travel from the link onto the (interactive) card.
  const [peek, setPeek] = useState<{
    target: PeekTarget;
    rect: PeekAnchorRect;
  } | null>(null);
  const peekDwellRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const peekHideRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  /// The href the card currently describes — so a repeated `kb:link-hover`
  /// for the SAME anchor (the relay fires per mouseover) doesn't restart
  /// the dwell and make the card flicker.
  const peekHrefRef = useRef<string | null>(null);
  /// Whether a card is actually ON SCREEN for `peekHrefRef` right now. The
  /// message pump is a long-lived closure that never sees the `peek` state,
  /// and "same href" alone is NOT enough to short-circuit: a mouseout →
  /// mouseover of the SAME anchor inside the grace window cancels the dwell
  /// and would otherwise leave that link permanently un-peekable (nothing
  /// re-arms it until the pointer visits a different anchor).
  const peekShownRef = useRef(false);
  const cancelPeekDwell = useCallback(() => {
    if (peekDwellRef.current) {
      clearTimeout(peekDwellRef.current);
      peekDwellRef.current = null;
    }
  }, []);
  const cancelPeekHide = useCallback(() => {
    if (peekHideRef.current) {
      clearTimeout(peekHideRef.current);
      peekHideRef.current = null;
    }
  }, []);
  const schedulePeekHide = useCallback(() => {
    cancelPeekHide();
    peekHideRef.current = setTimeout(() => {
      peekHideRef.current = null;
      peekHrefRef.current = null;
      peekShownRef.current = false;
      setPeek(null);
    }, PEEK_GRACE_MS);
  }, [cancelPeekHide]);
  const closePeek = useCallback(() => {
    cancelPeekDwell();
    cancelPeekHide();
    peekHrefRef.current = null;
    peekShownRef.current = false;
    setPeek(null);
  }, [cancelPeekDwell, cancelPeekHide]);
  // Desktop only: a touch device has no hover, and every tap would fire a
  // synthetic `mouseover` — a card popping up under the finger that was
  // about to follow the link. Tracked live so a hybrid device that gains a
  // mouse mid-session starts working without a reload.
  const hoverCapableRef = useRef(true);
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(hover: none)");
    const apply = () => {
      hoverCapableRef.current = !mq.matches;
      if (mq.matches) closePeek();
    };
    apply();
    mq.addEventListener?.("change", apply);
    return () => mq.removeEventListener?.("change", apply);
  }, [closePeek]);
  // Drop any open/pending peek when the artifact changes or the pane goes.
  useEffect(() => closePeek, [closePeek, kb, relPath]);

  // The resolver context, refreshed every render and read through a ref by
  // the (deliberately long-lived) message pump below. `kbIds` comes from the
  // shared `["kbs"]` query — cached, `staleTime: Infinity` (#23).
  const kbsQuery = useKbs();
  const kbIds = useMemo(
    () => (kbsQuery.data ?? []).map((k) => k.name),
    [kbsQuery.data],
  );
  const linkCtxRef = useRef<ArtifactLinkCtx>({
    paneKb: kb,
    paneSourceRelative: relPath,
    paneId: id,
    hostSuffix,
    spaOrigin: typeof window !== "undefined" ? window.location.origin : "",
    kbIds,
  });
  linkCtxRef.current = {
    paneKb: kb,
    paneSourceRelative: relPath,
    paneId: id,
    hostSuffix,
    spaOrigin: typeof window !== "undefined" ? window.location.origin : "",
    kbIds,
  };
  const kbIdsRef = useRef(kbIds);
  kbIdsRef.current = kbIds;

  // link-flow — the consume-once return seed (see the prop doc). Mirrored
  // into refs for the probe handler, whose listener doesn't rebind on a
  // prop-only change; re-armed whenever the route hands over a new one.
  const flowSeedRef = useRef<number | null>(flowSeedY ?? null);
  flowSeedRef.current = flowSeedY ?? null;
  const flowSeedConsumedRef = useRef(false);
  useEffect(() => {
    flowSeedConsumedRef.current = false;
  }, [flowSeedY, kb, relPath]);
  const flowStateRefRef = useRef(flowStateRef);
  flowStateRefRef.current = flowStateRef;

  useEffect(() => {
    function postScrollFlush() {
      const pending = scrollPending.current;
      const visitId = visitRef.current.id;
      const curKb = kbRef.current;
      if (!pending || !curKb || visitId === null) {
        scrollInflight.current = false;
        return;
      }
      scrollPending.current = null;
      scrollInflight.current = true;
      recordScroll(curKb, visitId, pending.y, pending.max)
        .catch((err) => console.warn("[history] scroll POST failed", err))
        .finally(() => {
          scrollInflight.current = false;
          // If another scroll arrived while we were in flight, fire
          // one more flush (collapses bursts into ≤1 RPS without
          // dropping the latest position).
          if (scrollPending.current) {
            setTimeout(postScrollFlush, 0);
          }
        });
    }

    function postReadingFlush() {
      const pending = readingPending.current;
      const visitId = visitRef.current.id;
      const curKb = kbRef.current;
      const curId = idRef.current;
      if (!pending || !curKb || !curId || visitId === null) {
        readingInflight.current = false;
        return;
      }
      readingPending.current = null;
      readingInflight.current = true;
      recordReading(
        curKb,
        visitId,
        curId,
        pending.sections,
        pending.active_ms,
        pending.last_section,
      )
        .catch((err) => console.warn("[history] reading POST failed", err))
        .finally(() => {
          readingInflight.current = false;
          if (readingPending.current) {
            setTimeout(postReadingFlush, 0);
          }
        });
    }

    // W1.reader — let the (possibly prerender-deferred) recordOpen effect
    // above kick a catch-up flush through the SAME retry/chaining logic once
    // `visitRef.current.id` resolves, rather than duplicating it.
    kickScrollFlush.current = postScrollFlush;
    kickReadingFlush.current = postReadingFlush;

    function onMessage(e: MessageEvent<ArtifactMessage | unknown>) {
      const msg = e.data as Partial<ArtifactMessage> | null;
      if (!msg || typeof msg !== "object" || !("kind" in msg)) return;
      // W3.P-a — accept ONLY this pane's artifact origin
      // (`<id>.artifacts.<root>`, invariant #7 — the suffix is runtime
      // config, threaded in from /api/identity). Two jobs in one check:
      //   * the trust boundary the suffix-only `isArtifactOrigin` used to
      //     provide (no third-party page can spoof navigation), and
      //   * ATTRIBUTION — `kb:scroll`/`kb:reading` are POSTed against
      //     `visitRef`'s row, so a sibling pane's beacons landing here would
      //     write one artifact's reading into another's history row
      //     (invariants #8/#19). Every artifact iframe passes a suffix-only
      //     check, so only exact-origin equality can tell them apart.
      // The cross-artifact trampoline is served from the CURRENT artifact's
      // own subdomain (routes/artifact.rs: `serve` returns it for a relative
      // link that resolves to another indexed artifact), so `open-artifact`
      // still arrives on this origin.
      if (!isOriginOfArtifact(e.origin, id, kb, hostSuffix)) return;
      // This pane's iframe is where the reading is happening — claim focus.
      // Inert while already focused (i.e. always, with one pane).
      if (!focusedRef.current) onFocusRef.current?.();
      if (msg.kind === "open-artifact" && msg.path) {
        // The trampoline carries the target's source-relative path
        // (Track U) so we navigate straight to the path permalink.
        // link-flow — plus, when the clicked link carried a fragment, the
        // OPTIONAL `sec` field, forwarded onto the permalink's own `?sec=`
        // deep link so a cross-artifact "…#findings" lands on the section
        // instead of the top. Absent ⇒ byte-identical to the old call.
        // The trampoline forwards `location.hash` RAW (still
        // percent-encoded, as it appeared in the URL); `artifactHref`
        // encodes `sec` on the way out, so decode first or a heading id
        // with an escape would be double-encoded and match nothing.
        let openSec: string | undefined;
        if (typeof msg.sec === "string" && msg.sec) {
          try {
            openSec = decodeURIComponent(msg.sec);
          } catch {
            openSec = msg.sec;
          }
        }
        navigate(
          artifactHref(msg.kb ?? kb!, msg.path, openSec ? { sec: openSec } : undefined),
        );
      } else if (msg.kind === "kb:link-hover") {
        // link-flow — a hover inside the artifact. Resolve it PURELY
        // (lib/artifactLinks.ts); only the two artifact-shaped cases open a
        // card, and the fetch behind it is silent on failure (no toast on a
        // hover, ever).
        if (!hoverCapableRef.current) return;
        const href = typeof msg.href === "string" ? msg.href : "";
        // The payload crosses an origin boundary from UNTRUSTED artifact
        // HTML — every numeric field is coerced (finite, never NaN/±∞: a
        // NaN would propagate straight into the card's inline `left`/`top`).
        const r = msg.rect as
          | { x?: number; y?: number; w?: number; h?: number }
          | undefined;
        const num = (v: unknown): number | null =>
          typeof v === "number" && Number.isFinite(v) ? v : null;
        const rx = num(r?.x);
        const ry = num(r?.y);
        if (!href || rx === null || ry === null) return;
        // Same anchor as the one the card is already showing → keep it (the
        // relay re-fires on every mouseover, incl. re-entering the same
        // link). NOT enough on its own: after a mouseout inside the grace
        // window the dwell has been cancelled and nothing would re-arm it,
        // so only short-circuit while a card is up or a dwell is ticking.
        if (peekHrefRef.current === href) {
          cancelPeekHide();
          if (peekShownRef.current || peekDwellRef.current) return;
        }
        cancelPeekDwell();
        cancelPeekHide();
        const resolved = resolveArtifactHref(href, linkCtxRef.current);
        if (resolved.kind !== "artifact" && resolved.kind !== "artifact-id") {
          // An external link / the doc itself — nothing to preview, and any
          // card still open belongs to a different anchor.
          peekHrefRef.current = null;
          peekShownRef.current = false;
          setPeek(null);
          return;
        }
        const box = iframeRef.current?.getBoundingClientRect();
        if (!box) return;
        const w = num(r?.w) ?? 0;
        const h = num(r?.h) ?? 0;
        // The relayed rect is IFRAME-viewport-relative; the card is
        // `position: fixed` in the PARENT viewport, so offset by the
        // iframe's own box.
        const rect: PeekAnchorRect = {
          left: box.left + rx,
          top: box.top + ry,
          right: box.left + rx + w,
          bottom: box.top + ry + h,
        };
        peekHrefRef.current = href;
        const target: PeekTarget = resolved;
        peekDwellRef.current = setTimeout(() => {
          peekDwellRef.current = null;
          peekShownRef.current = true;
          setPeek({ target, rect });
        }, PEEK_DWELL_MS);
      } else if (msg.kind === "kb:link-clear") {
        // Left the anchor (or scrolled / unloaded). GRACE hide — the card is
        // interactive, and `onHoverIn` cancels this when the pointer lands
        // on it.
        cancelPeekDwell();
        schedulePeekHide();
      } else if (msg.kind === "kb:link-open") {
        // link-flow — the runtime preventDefaulted a click on an
        // artifact-shaped CROSS-ORIGIN link and handed it to us (it always
        // posts a synchronous `kb:scroll` first, so the departing scroll
        // position is recorded before we navigate — that beacon lands in the
        // branch below on this same tick). A normal router push, exactly like
        // `open-artifact`: one history entry per artifact (#20).
        closePeek();
        const href = typeof msg.href === "string" ? msg.href : "";
        const resolved = resolveArtifactHref(href, linkCtxRef.current);
        if (resolved.kind === "artifact") {
          navigate(
            artifactHref(
              resolved.kb,
              resolved.sourceRelative,
              resolved.sec ? { sec: resolved.sec } : undefined,
            ),
          );
        } else if (resolved.kind === "artifact-id") {
          // Only an id: look the artifact up (pane's kb first, then the
          // rest) through the SAME cache entry the hover card warms.
          const sec = resolved.sec;
          const ladder = artifactKbLadder(
            resolved.kb,
            kbRef.current,
            kbIdsRef.current,
          );
          void queryClient
            .fetchQuery({
              queryKey: docIdQueryKey(resolved.kb ?? kbRef.current, resolved.id),
              queryFn: ({ signal }) =>
                fetchDocByIdLadder(ladder, resolved.id, signal),
              retry: false,
            })
            .then(({ kb: foundKb, doc: found }) => {
              navigate(
                artifactHref(
                  foundKb,
                  found.source_relative,
                  sec ? { sec } : undefined,
                ),
              );
            })
            // Invariant #32 — a user ACTION that can't proceed says so
            // (unlike the hover path, which stays silent).
            .catch(() => toast.err("that link isn't indexed in any kb"));
        } else if (resolved.kind === "external") {
          toast.err("couldn't resolve that link to an artifact");
        }
      } else if (msg.kind === "pm:page" && msg.label) {
        setPages((prev) =>
          prev.some((p) => p.src === msg.src)
            ? prev
            : [...prev, { label: msg.label!, src: msg.src ?? "" }],
        );
        // RLs5 — preserve the reading-list trail params across a page
        // switch; `sec` is deliberately dropped (the section lives on
        // one page). Only the primary pane writes the address bar.
        if (!isPrimaryRef.current) return;
        const sp = searchParamsRef.current;
        navigate(
          artifactHref(kb!, relPath, {
            page: msg.label,
            list: sp.get("list") ?? undefined,
            entry: sp.get("entry") ?? undefined,
          }),
          { replace: true },
        );
      } else if (msg.kind === "kb-probe") {
        // v0.6+ H4/H6 — once the iframe's probe has fired the artifact
        // is ready to receive scroll-to. The probe arrives multiple
        // times (one per `check` field) and again on every iframe
        // remount (artifact.indexed, annotate toggle, page switch). We
        // re-assert `pendingScrollY` on each fire instead of zeroing
        // after first hit, so refreshes land on the user's latest
        // scroll. scrollTo({top:y}) is idempotent if the iframe is
        // already at y, and same-iframe repeats all fire pre-user-
        // scroll, so there's no clobber risk.
        //
        // W3.E/S5 — an unconsumed `?turn=` deep link wins over EVERYTHING
        // below (sec included): it's the more explicit intent — a specific
        // turn, not just a section — and this is the fix for the cold-mount
        // drop the `turn` effect above can't cover on its own (its
        // postMessage fires before the iframe's listener is wired up on a
        // fresh mount). Own consumed-ref so the probe's OTHER fires within
        // this same mount (one per `check` field, per the comment above)
        // fall through to sec/resume instead of re-jumping (and
        // re-flashing) on every one.
        //
        // RLs1 — an unconsumed ?sec= deep link wins over the resume:
        // the deep-link INTENT is the section, not wherever the reader
        // left off last visit.
        const curSec = secRef.current;
        const curTurn = turnTargetRef.current;
        const frame = iframeRef.current?.contentWindow;
        // Target the artifact's own origin (not "*") for every outbound post
        // below, so only the intended cross-origin iframe receives it
        // (invariant #7). null when the doc/id hasn't resolved — skip posting.
        const targetOrigin = id ? artifactOrigin(id, kb, hostSuffix) : null;
        if (curTurn && !turnConsumedRef.current) {
          if (frame && targetOrigin) {
            try {
              frame.postMessage(
                { kind: "kb:scroll-to-id", id: curTurn, flash: true },
                targetOrigin,
              );
              turnConsumedRef.current = true;
            } catch (err) {
              console.warn("[turn] scroll-to-id post failed", err);
            }
          }
        } else if (curSec && !secConsumedRef.current) {
          if (frame && targetOrigin) {
            try {
              frame.postMessage(
                { kind: "kb:scroll-to-id", id: curSec, flash: true },
                targetOrigin,
              );
            } catch (err) {
              console.warn("[sec] scroll-to-id post failed", err);
            }
          }
        } else {
          // link-flow — the RETURN seed sits between the explicit deep links
          // above and the server's resume below: an explicit `?sec=`/`?turn=`
          // is still the more specific intent, but a flow return carries the
          // offset the reader ACTUALLY left at, while `pendingScrollY` is a
          // debounced server snapshot that can lag it. Consume-once (its own
          // ref, re-armed when the route hands over a new seed) so the
          // probe's other fires within this mount fall through to resume.
          const flowSeed = flowSeedRef.current;
          const useFlowSeed =
            flowSeed !== null && flowSeed > 0 && !flowSeedConsumedRef.current;
          const y = useFlowSeed
            ? (flowSeed as number)
            : visitRef.current.pendingScrollY;
          if (y > 0 && frame && targetOrigin) {
            try {
              frame.postMessage({ kind: "kb:scroll-to", y }, targetOrigin);
              if (useFlowSeed) flowSeedConsumedRef.current = true;
            } catch (err) {
              console.warn("[history] scroll-to post failed", err);
            }
          }
        }
        // RP-track seed-on-open: hand the runtime its resume baseline. The
        // runtime guards with `rSeeded`, so re-posting on every probe is
        // idempotent (probes after the first are ignored by the iframe).
        const seed = readingSeed.current;
        if (seed) {
          const frame = iframeRef.current?.contentWindow;
          if (frame && targetOrigin) {
            try {
              frame.postMessage(
                {
                  kind: "kb:reading-seed",
                  active_ms: seed.active_ms,
                  last_section: seed.last_section ?? null,
                  sections: seed.sections,
                },
                targetOrigin,
              );
            } catch (err) {
              console.warn("[history] reading-seed post failed", err);
            }
          }
        }
      } else if (msg.kind === "kb:scroll") {
        // v0.6+ H4 — debounced scroll snapshot from the runtime. Store
        // latest values; postScrollFlush sends only when no POST is in
        // flight. Bursts of scroll events collapse to one POST/sec.
        // v0.6+ H6 — also refresh `visitRef.pendingScrollY` so the
        // next iframe remount restores to the user's latest position
        // (not the stale seed from recordOpen).
        const y = typeof msg.y === "number" ? msg.y : 0;
        const max = typeof msg.max === "number" ? msg.max : 0;
        if (y < 0 || max < 0) return;
        // W7 (R15/LF-2) — follow mode's "pinned" signal, read as a side
        // effect of this SAME beacon (no second listener): near the bottom
        // ⇒ pinned. `max === 0` (a doc shorter than the viewport, or not
        // yet laid out) counts as pinned — there's nowhere to scroll away
        // TO, so a handoff reload is never "yanking" the reader.
        nearBottomRef.current = max === 0 || max - y <= PIN_NEAR_BOTTOM_PX;
        // W2.16 — a scroll invalidates the selection floater's frozen
        // position (it's a one-shot snapshot, not reactively repositioned).
        //
        // R3 — DESKTOP ONLY. This clear exists purely to protect the
        // rect-anchored desktop floater, which would otherwise sit at a
        // position the text has scrolled away from. On mobile
        // SelectionActions renders as `.kb-selact--bar`, `position: fixed` at
        // the viewport bottom, and ignores `rect` entirely — so there is
        // nothing to invalidate, and clearing is pure harm: dragging a native
        // selection handle near the top/bottom edge AUTO-SCROLLS the
        // document, so the very gesture of refining a selection was nuking
        // the bar mid-gesture. The push relay re-posts a fresh `cm:selection`
        // as the selection settles, so the bar's rect never goes stale on
        // desktop either way.
        //
        // …and only when the artifact has actually MOVED since that rect was
        // frozen. This beacon is debounced 500ms by the iframe runtime, so it
        // describes a scroll that finished half a second ago — routinely one
        // the READER never made, the resume jump this same handler posts
        // above (`kb:scroll-to`, off the `history/open` offset). A selection
        // made after that jump settled has a perfectly valid rect, yet the
        // late beacon used to dismiss it, and nothing re-posts it (the push
        // relay only fires on a fresh selectionchange/mouseup) — the floater
        // simply vanished a beat after appearing, and a click already on its
        // way landed on a detached button. `cm:selection` carries the offset
        // it was captured at, so the comparison is exact: same offset ⇒ the
        // rect still points at the text ⇒ keep the floater; different ⇒ the
        // reader scrolled after selecting ⇒ dismiss, exactly as before. (1px
        // of slack for fractional offsets under zoom/HiDPI. A pane holding no
        // selection keeps the old unconditional clear.)
        const heldSel = selectionRef.current;
        const movedSinceCapture = !heldSel || Math.abs(y - heldSel.scrollY) > 1;
        if (!isMobileRef.current && movedSinceCapture) {
          onSelectionChangeRef.current(null);
        }
        // RLs1 — any scroll (incl. the one our own section jump emits)
        // consumes the ?sec= assert; later remounts resume normally.
        secConsumedRef.current = true;
        scrollPending.current = { y, max };
        visitRef.current.pendingScrollY = y;
        // link-flow — publish the LIVE offset for the route's flow capture
        // (a ref write, no render). This is what makes a return land where
        // the reader actually was, rather than on the debounced server
        // snapshot: the runtime posts a synchronous `kb:scroll` right before
        // every `kb:link-open`, so this value is current at capture time.
        const flowState = flowStateRefRef.current;
        if (flowState) {
          flowState.current = { y, sec: currentSectionRef.current };
        }
        if (!scrollInflight.current) {
          postScrollFlush();
        }
      } else if (msg.kind === "kb:reading") {
        // RP-track — cumulative per-visit reading snapshot. Mirror the
        // scroll-flush: store the latest, POST when no flush is in flight.
        readingPending.current = {
          sections: Array.isArray(msg.sections) ? msg.sections : [],
          active_ms: typeof msg.active_ms === "number" ? msg.active_ms : 0,
          last_section:
            typeof msg.last_section === "string" ? msg.last_section : null,
        };
        if (!readingInflight.current) {
          postReadingFlush();
        }
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [
    kb,
    id,
    relPath,
    navigate,
    hostSuffix,
    queryClient,
    cancelPeekDwell,
    cancelPeekHide,
    schedulePeekHide,
    closePeek,
  ]);

  // U1 / v0.22 — the three viewing-mode keybinds, fired from anywhere SPA
  // chrome holds focus (the cross-origin iframe captures its own keys, so
  // these only fire off the artifact). `o` toggles immersive (Esc exits, in
  // the route's immersive effect); `b` opens the BARE artifact origin in a new
  // tab (no SPA chrome). Skip while typing so neither eats an editor keystroke.
  //
  // W2.6b adds `y` then `p` — a small independent chord tracked with a
  // plain ref + timeout, entirely outside `lib/keymap.ts`'s machine (this
  // is the reader-scope doc-only home the REGISTRY row points at; resolve()
  // only ever runs at `scope: "global"`, from HotkeyRoot). Known narrow
  // collision: `components/lists/QueueBar.tsx` binds bare `p` ("previous
  // trail entry") on its own independent window listener while a reading-
  // list trail is open — typing "y p" in that state fires BOTH actions
  // (QueueBar isn't in this phase's owned files to coordinate against).
  //
  // W3.P-a — window-level ⇒ focused pane only (always, with one pane).
  useEffect(() => {
    if (!focused) return;
    const yPending = { current: false };
    let yTimer: ReturnType<typeof setTimeout> | null = null;

    function onKey(e: KeyboardEvent) {
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
      if (e.key === "o" || e.key === "O") {
        e.preventDefault();
        onSetImmersive(!immersive);
        return;
      }
      if ((e.key === "b" || e.key === "B") && id) {
        // The bare URL is the iframe's own origin (the scrubbed artifact);
        // `noreferrer` makes the daemon's referrer-gated bounce skip so it
        // renders bare instead of redirecting back to the SPA wrapper.
        e.preventDefault();
        const base = artifactOrigin(id, kb, hostSuffix);
        const url = activePage
          ? `${base}/${activePage.replace(/^\//, "")}`
          : `${base}/`;
        window.open(url, "_blank", "noopener,noreferrer");
        return;
      }
      if (e.key === "y" && kb && id) {
        e.preventDefault();
        yPending.current = true;
        if (yTimer) clearTimeout(yTimer);
        yTimer = setTimeout(() => {
          yPending.current = false;
        }, 800);
        return;
      }
      if (yPending.current && e.key === "p") {
        e.preventDefault();
        yPending.current = false;
        if (yTimer) {
          clearTimeout(yTimer);
          yTimer = null;
        }
        if (kb && id) {
          const block = buildProvenanceBlock({
            title: doc?.title || relPath,
            kb,
            sourceRelative: relPath,
            id,
            sessionId: originSessionId,
          });
          navigator.clipboard
            ?.writeText(block)
            .then(() => toast.ok("provenance copied"))
            .catch(() => toast.err("couldn't copy provenance"));
        }
        return;
      }
      if (yPending.current) yPending.current = false;
    }
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      if (yTimer) clearTimeout(yTimer);
    };
  }, [
    immersive,
    onSetImmersive,
    id,
    hostSuffix,
    activePage,
    kb,
    doc?.title,
    relPath,
    originSessionId,
    focused,
  ]);

  // Build the artifact URL. Same protocol + port as the parent so the
  // browser keeps the connection on the daemon's configured suffix
  // (`.artifacts.localhost` for dev, `.artifacts.<root>` for production
  // — see `lib/artifactHost.ts`). Append `?cm=on` whenever the kb has
  // comments enabled (i.e. for the lifetime of this detail view, not
  // gated on the annotate-mode toggle). Keeping the URL stable across
  // annotate-mode toggles lets the iframe `key` stay constant, which
  // prevents React from remounting the iframe — preserves in-iframe
  // tab / <details> / scroll state when the user clicks the ✎ pencil.
  // The daemon's routes::artifact::serve recognises `?cm=on` and
  // injects the annotator (Track A3).
  const artifactBase = artifactOrigin(id, kb, hostSuffix);
  const baseUrl = activePage
    ? `${artifactBase}/${activePage.replace(/^\//, "")}`
    : `${artifactBase}/`;
  const artifactUrl = reviewActive ? `${baseUrl}?cm=on` : baseUrl;

  const filename = relPath.includes("/")
    ? relPath.slice(relPath.lastIndexOf("/") + 1)
    : relPath;
  const isIndexFile = filename.toLowerCase() === "index.html";

  const handleSelection = useCallback(
    (anchor: SelectionAnchor, rect: SelectionRect, scrollY: number) =>
      onSelectionChange({
        anchor,
        rect,
        sectionId: currentSectionRef.current,
        scrollY,
      }),
    [onSelectionChange],
  );

  return (
    <>
      <div className="detail__main">
        {doc && (
          <ContextBar
            kb={kb}
            id={id}
            title={doc.title || filename}
            folder={doc.folder}
            filename={filename}
            isIndex={isIndexFile}
            sourceRelative={doc.source_relative}
            position={siblingNav?.position}
            total={siblingNav?.total}
            onPrev={siblingNav?.onPrev}
            onNext={siblingNav?.onNext}
            onFullscreen={() => onSetImmersive(true)}
            bareUrl={baseUrl}
            reviewActive={reviewActive}
            annotateMode={annotateMode}
            onToggleAnnotate={onToggleAnnotate}
            commentCount={commentCount}
            inspectorOpen={inspectorOpen}
            onToggleInspector={onToggleInspector}
            folio={folio}
            onToggleFolio={onToggleFolio}
            splitMode={splitMode}
            onSplit={onSplit}
            flow={flow}
          />
        )}
        {doc && <DeskChangedBanner kb={kb} id={id} />}

        {isPrimary && searchParams.get("list") && (
          <QueueBar
            kb={kb}
            listId={searchParams.get("list") as string}
            entryId={searchParams.get("entry")}
          />
        )}
        {pages.length > 0 && (
          <nav className="pages" aria-label="artifact pages">
            {pages.map((p) => (
              <button
                key={p.src}
                className={`pages__item ${activePage === p.label ? "is-active" : ""}`}
                onClick={() => {
                  // RLs5 — keep the trail params; drop `sec` (page-local).
                  navigate(
                    artifactHref(kb, relPath, {
                      page: p.label,
                      list: searchParams.get("list") ?? undefined,
                      entry: searchParams.get("entry") ?? undefined,
                    }),
                    { replace: true },
                  );
                }}
              >
                {p.label}
              </button>
            ))}
          </nav>
        )}
        {error && (
          <div className="detail__error" role="alert">
            {error}
          </div>
        )}
        {folio && doc && (
          <FolioHeader kb={kb} folder={doc.folder} section={folioSectionTitle} />
        )}
        {/* W3.E/S3 — the session identity strip: architecture (c), one slim
            strip between ContextBar and the iframe, never a second route or
            rail icon. Hidden in immersive mode with the rest of the chrome
            (this whole block is inside the same reader-chrome tree). */}
        {isSessionDoc && sessionByArtifact?.session && (
          <SessionContextCard
            session={sessionByArtifact.session}
            newest={sessionByArtifact.newest}
            onJumpToOutcome={jumpToOutcome}
            presenceSet={presenceSet}
            followMode={isPrimary ? followMode : undefined}
            onToggleFollow={isPrimary ? toggleFollow : undefined}
          />
        )}
        {/* W7 (R15/LF-2) — Tier-1 live tail panel: reader-column content,
            ContextBar-adjacent (invariant #30 — no new rail icon, no new
            sheet), rendered ONLY while actually following AND a live
            transcript resolves (Tier-1 configured + this session in the
            presence set). */}
        {/* W7 (R15/LF-2) — LiveTailPanel: on mobile, render via portal into
            the inspector sheet; on desktop, render inline. Single instance +
            hook (useLiveTail) per pane, controlled here. */}
        {isPrimary &&
          followMode &&
          isSessionDoc &&
          followSessionId &&
          presenceSet.has(followSessionId) &&
          (() => {
            const panel = (
              <LiveTailPanel
                sessionId={followSessionId}
                harness={sessionByArtifact?.session?.harness ?? "claude"}
              />
            );
            // On mobile + portal target available, render to portal (inside inspector sheet)
            if (isMobileView && mobilePortalNode) {
              return createPortal(panel, mobilePortalNode);
            }
            // On desktop or portal not ready, render inline in reader column
            return panel;
          })()}
        {removed ? (
          <div className="detail__removed" role="status">
            <h2 className="detail__removed-title">artifact removed</h2>
            <p className="detail__removed-msg">
              This artifact's file was removed from disk.
            </p>
            <Link to={withKb("/", kb ?? null)} className="detail__removed-link">
              ← back to gallery
            </Link>
          </div>
        ) : (
          <>
            <iframe
              key={`${artifactUrl}#${reloadNonce}`}
              ref={iframeRef}
              className="detail__frame"
              title={doc?.title || `artifact ${id}`}
              src={artifactUrl}
              sandbox={SANDBOX}
            />
            {/* v0.12 P3 — TOC mini-spy overlays the iframe's right edge.
                Explicit key (recon-flagged) — the folio header above is a new
                sibling that toggles in/out, which would otherwise shift this
                unkeyed element's slot in React's positional reconciliation
                and force a pointless remount (losing its live TOC state) on
                every folio toggle; the iframe is already protected by its own
                `key`. */}
            <TocSpy
              key="toc-spy"
              iframeRef={iframeRef}
              hostSuffix={hostSuffix}
              summary={summary}
              kb={kb}
              relPath={relPath}
              artifactId={id}
            />
            {/* W2.13 — errata slip: a SIBLING of the iframe (never painted
                inside it, #5), pinned to the reader's top margin via
                component-local absolute positioning (z below --z-float).
                Explicit key for the same reconciliation reason as TocSpy
                above — this element toggles in/out with `errataMode`. */}
            {reviewActive && errataMode && (
              <ErrataSheet
                key="errata-sheet"
                file={reviewFile}
                activeCommentId={activeCommentId}
                onSelect={onCommentJump}
                onHover={onCommentHover}
              />
            )}
          </>
        )}
        {folio && doc && <FolioColophon kb={kb} doc={doc} />}
      </div>
      {reviewActive && (
        <AnnotatorBridge
          ref={bridgeRef}
          iframeRef={iframeRef}
          artifactId={id}
          kb={kb}
          hostSuffix={hostSuffix}
          annotateMode={annotateMode}
          onComposeAnchor={onComposeAnchor}
          onFocusComment={onFocusComment}
          onExitAnnotate={onExitAnnotate}
          onSelection={handleSelection}
          onSelectionClear={() => onSelectionChange(null)}
          // R4 — the pull lane's answer. Only the ANCHOR is forwarded: the
          // pull's consumer is the comments composer (an anchor is all it
          // needs), and a rect from a selection the engine may already have
          // collapsed has no floater to position anyway.
          onSelectionPull={
            onSelectionPull ? (anchor) => onSelectionPull(anchor) : undefined
          }
        />
      )}
      {/* link-flow — the in-artifact link peek. Same card the Alt-hover
          trigger mounts (#30); it portals to <body>, so rendering it here
          costs the pane nothing but keeps ownership with the pane that
          received the relay (and whose iframe box positioned it). */}
      {peek && (
        <PeekCard
          target={peek.target}
          rect={peek.rect}
          fallbackKb={kb}
          onClose={closePeek}
          onSplit={onPeekSplit}
          onHoverIn={cancelPeekHide}
          onHoverOut={schedulePeekHide}
        />
      )}
      {/* W2.16 — highlight → comment/cite/list/remember. Only meaningful
          while the annotator is mounted (reviewActive — same gate as
          AnnotatorBridge itself, since cm:selection only ever arrives via
          that script). */}
      {reviewActive && selection && doc && (
        <SelectionActions
          kb={kb}
          artifactId={id}
          sourceRelative={doc.source_relative}
          title={doc.title || filename}
          anchor={selection.anchor}
          rect={selection.rect}
          sectionId={selection.sectionId}
          onComment={() => onComposeSelection(selection.anchor)}
          onDismiss={() => onSelectionChange(null)}
        />
      )}
    </>
  );
}
