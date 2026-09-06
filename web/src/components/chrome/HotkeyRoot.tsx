import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { matchPath, useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { Icon } from "../icons";
import { useExplicitKb } from "../../hooks/useActiveKb";
import { useArtifactHostSuffix } from "../../hooks/useArtifactHost";
import { useFocusTrap } from "../../hooks/useFocusTrap";
import { withKb } from "../../lib/navItems";
import { artifactHref } from "../../lib/artifactHref";
import { isOriginOfArtifact } from "../../lib/artifactHost";
import type { DocSummary } from "../../api/client";
import { getMark, listMarks, saveMark } from "../../lib/marks";
import {
  clearRegister,
  getRegister,
  listRegisters,
  registerContext,
  registerLabel,
  setRegister,
  type Ref,
  type Register,
} from "../../lib/registers";
import { renderRegister } from "../../lib/quote";
// The WIRE anchor (required-nullable fields) — what `AddToListButton`'s
// create body speaks; the hand-written client `Anchor` is looser.
import type { Anchor as GeneratedAnchor } from "../../api/generated/Anchor";
import AddToListButton from "../lists/AddToListButton";
import { toast } from "../../lib/toast";
import HintOverlay, { type HintActivateMode } from "../HintOverlay";
import PeekCard, { type PeekAnchorRect, type PeekTarget } from "../PeekCard";
import {
  groupRegistry,
  initialKeymapState,
  isEditableTarget,
  resolve,
  type KeymapState,
} from "../../lib/keymap";

// v0.10 S2 — global hotkey router. W2.6a replaced the hand-rolled g-chord
// state (a bare `gPending` boolean + an ad-hoc switch) with
// `lib/keymap.ts`'s registry machine: `resolve()` owns the chord grammar
// (which key sequences exist, what they resolve to), this component owns
// the DOM listener + the 800ms chord timer + the actual dispatch (both
// inherently stateful/timing concerns the pure module doesn't touch).
// Behavior is unchanged from the pre-registry version — this is a
// like-for-like swap of the machinery underneath.
//
// The `?` cheat sheet below is now GENERATED from `groupRegistry()` — see
// that module's doc for the "a binding that isn't in the registry doesn't
// exist" rule. Only `scope: "global"` entries are ever dispatched here;
// every other scope is registered purely so the sheet stays honest about
// bindings owned elsewhere (the reader, the gallery/search roving cursor,
// overlays) — the two-layer ownership rule.
//
// j/k gallery/search navigation is `useRovingCursor` (its own window
// listener, mounted from `routes/gallery.tsx` / `routes/search.tsx`) — not
// this component's concern. Both listeners are window-level with no shared
// state; this file only ever preventDefaults a "g"-chord in progress or a
// bare `?`/`/`/`Escape`, so an unrelated `j`/`k` keydown reaches the other
// listener untouched.
const CHORD_TIMEOUT_MS = 800;
// W2.6b — Alt-hover peek's hover delay. Mirrors `Card.tsx`'s own
// `PREFETCH_DELAY_MS` (300ms) rather than importing it — that constant
// isn't exported and `Card.tsx` isn't owned by this phase.
const PEEK_DELAY_MS = 300;
// link-flow — the card became INTERACTIVE (Open / Open beside), so leaving
// the link can no longer hide it instantly: the pointer needs a moment to
// travel the 8px gap onto the card. Same grace the in-artifact peek uses
// (`ArtifactPane`); entering the card cancels it, leaving it re-arms.
const PEEK_GRACE_MS = 300;

/// Any of these being open means keyboard input belongs to that overlay —
/// the "overlay owns the keyboard" rule (see `lib/keymap.ts`'s module
/// doc). Intentional duplicate of `useRovingCursor.ts`'s private
/// `isOverlayOpen` — that helper isn't exported and that file isn't owned
/// by this phase.
function isOverlayOpenElsewhere(): boolean {
  return !!document.querySelector('dialog[open], [role="dialog"][aria-modal="true"]');
}

/// W3.P-c — the four letter-prefixed operations over the shared 26-slot
/// register store: `m` set a mark, `` ` `` jump to one, `"` store a
/// reference, `'` paste one as a citation. The value doubles as the chord
/// indicator's label key (`LETTER_MODE_LABEL` below).
type LetterMode = "mark-set" | "mark-jump" | "register-set" | "register-paste";

const LETTER_MODE_LABEL: Record<LetterMode, string> = {
  "mark-set": "m",
  "mark-jump": "`",
  "register-set": '"',
  "register-paste": "'",
};

export default function HotkeyRoot() {
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  // The chord targets carry the active space forward (a `g m` from a reader
  // lands on /memory scoped to that artifact's kb, not the first kb). Read
  // through a ref so the keydown listener sees the latest kb without
  // re-subscribing on every navigation. Settings stays global (no kb).
  const explicitKb = useExplicitKb();
  const kbRef = useRef(explicitKb);
  kbRef.current = explicitKb;
  // W2.6b — marks needs the CURRENT route's kb + source-relative path (to
  // know what "here" means for `m <letter>`), which `useLocation()` alone
  // can't give the effect below without re-subscribing its listener on
  // every navigation — same ref idiom as `kbRef` above.
  const locationRef = useRef(location);
  locationRef.current = location;
  // W2.6b — the reader's active section id, for marks' "sec" field.
  // `routes/detail.tsx` is a LAZY route chunk (`React.lazy` in app.tsx);
  // this eagerly-loaded chrome component must NOT statically import
  // anything from it — doing so (even a single tiny named export) forced
  // the whole lazy chunk into the eager bundle in a real build (`npm run
  // build`'s Rollup warning: "dynamically imported ... but also
  // statically imported ... dynamic import will not move module into
  // another chunk" — `main.js` grew by the entire reader chunk's ~100KB).
  // So this listens for the SAME `kb:section` postMessage directly and
  // independently instead — the recon-sanctioned "a second listener is
  // fine, TocSpy already proves it" pattern, just with one more consumer.
  const hostSuffix = useArtifactHostSuffix();
  const activeSectionRef = useRef<string | null>(null);
  useEffect(() => {
    activeSectionRef.current = null;
    function onMessage(e: MessageEvent) {
      // W3.P-a — attribute the `kb:section` beacon to EXACTLY the artifact
      // the current route addresses, not to "any artifact iframe" (the
      // suffix-only `isArtifactOrigin` gate every artifact origin passes).
      // `m <letter>` records `sec` for the ROUTE's artifact, so once a
      // second pane is on screen a suffix-only guard would stamp the mark
      // with the OTHER pane's heading. The id comes from the same
      // ["doc", kb, sourceRelative] cache entry `setMarkHere` already reads
      // (populated well before the iframe can post — its `src` is derived
      // from that id), so this stays a plain synchronous cache read, no new
      // fetch/subscribe plumbing (#23).
      // TODO(wave3): read the FOCUSED pane's id once the two-pane reader
      // publishes one; the route path is the primary pane's address today.
      const m = matchPath({ path: "/a/:kb/*" }, locationRef.current.pathname);
      const routeKb = m?.params.kb;
      const routeRel = m?.params["*"];
      if (!routeKb || routeRel === undefined) return;
      const cached = queryClient.getQueryData<DocSummary>(["doc", routeKb, routeRel]);
      if (!isOriginOfArtifact(e.origin, cached?.id, routeKb, hostSuffix)) return;
      const data = e.data as { kind?: string; id?: string } | null;
      if (!data || typeof data !== "object" || data.kind !== "kb:section") return;
      if (typeof data.id === "string") activeSectionRef.current = data.id;
    }
    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      activeSectionRef.current = null;
    };
  }, [hostSuffix, location.pathname, queryClient]);
  const [helpOpen, setHelpOpen] = useState(false);
  const [keymapState, setKeymapState] = useState<KeymapState>(initialKeymapState);
  // W2.6b — awaiting the a–z letter after `m`/`` ` ``. Distinct from
  // `keymapState`: resolve()'s chord machine has no wildcard/parameter
  // token, so the letter is captured directly in `onKey` below rather
  // than through another `resolve()` call — see `lib/keymap.ts`'s Marks
  // registry comment for why.
  //
  // W3.P-c — ONE letter-capture machine, four operations. Registers (`"`
  // store / `'` paste) need exactly the same "next keystroke is an a–z
  // slot" step marks already had, and they address the SAME 26 slots
  // (`lib/registers.ts` — marks are its artifact-position case), so this
  // generalized rather than growing a second parallel `registerMode`
  // state: one prompt, one 800ms window, one abort path.
  const [letterMode, setLetterMode] = useState<LetterMode | null>(null);
  // W2.6b — hint mode. `HintOverlay` owns every keystroke itself once
  // mounted; this is just "is it mounted, and which activation variant".
  const [hintMode, setHintMode] = useState<HintActivateMode | null>(null);
  // W2.6b — Alt-hover peek. `null` when no peek is showing. link-flow: the
  // payload is now the shared `PeekTarget` union (the card serves both this
  // trigger and the in-artifact hover relay), and the hide runs through a
  // grace timer so the pointer can reach the now-interactive card.
  const [peek, setPeek] = useState<{
    target: PeekTarget;
    rect: PeekAnchorRect;
  } | null>(null);
  const peekHideRef = useRef<ReturnType<typeof setTimeout> | null>(null);
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
      setPeek(null);
    }, PEEK_GRACE_MS);
  }, [cancelPeekHide]);
  const closePeek = useCallback(() => {
    cancelPeekHide();
    setPeek(null);
  }, [cancelPeekHide]);

  useEffect(() => {
    let chordTimer: ReturnType<typeof setTimeout> | null = null;

    function dispatch(actionId: string) {
      switch (actionId) {
        case "nav.gallery":
          navigate(withKb("/", kbRef.current));
          return;
        case "nav.atlas":
          navigate(withKb("/?view=atlas", kbRef.current));
          return;
        case "nav.memory":
          navigate(withKb("/memory", kbRef.current));
          return;
        case "nav.lists":
          navigate(withKb("/lists", kbRef.current));
          return;
        case "nav.notes":
          navigate(withKb("/notes", kbRef.current));
          return;
        case "nav.sessions":
          navigate(withKb("/sessions", kbRef.current));
          return;
        // SL4 — `g b`. No `withKb`: a slate is per-project coordination
        // keyed on a slug, not a per-corpus view (see navItems.ts).
        case "nav.slates":
          navigate("/slates");
          return;
        case "nav.history":
          navigate(withKb("/?view=history", kbRef.current));
          return;
        case "nav.settings":
          navigate("/settings");
          return;
        // `g t` toggles the Topics rail (expanded ↔ shrunk). The TocSpy
        // hook listens for this custom event; it's emitted globally so the
        // chord works from any route, but only Detail renders the rail.
        case "nav.tocToggle":
          window.dispatchEvent(new CustomEvent("kb:toc-spy.advance"));
          return;
        case "chrome.toggleHelp":
          setHelpOpen((v) => !v);
          return;
        case "chrome.openPalette":
          // `/` is the single-key alias for ⌘K — simulate one rather than
          // duplicating app.tsx's listener (which owns the real palette
          // open/close state).
          window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
          return;
        // W2.6b — hint mode: HintOverlay does the rest (enumeration,
        // narrowing, activation); this just mounts it.
        case "hint.open":
          setHintMode("click");
          return;
        case "hint.openTab":
          setHintMode("tab");
          return;
        // W2.6b — marks / W3.P-c — registers: arm the SAME 800ms chord
        // window as a `g`-prefix, reusing `chordTimer`; the next a–z
        // keystroke is handled directly in `onKey` below (not via another
        // `resolve()` call).
        case "marks.enterSet":
        case "marks.enterJump":
        case "registers.enterSet":
        case "registers.enterPaste": {
          const mode: LetterMode =
            actionId === "marks.enterSet"
              ? "mark-set"
              : actionId === "marks.enterJump"
                ? "mark-jump"
                : actionId === "registers.enterSet"
                  ? "register-set"
                  : "register-paste";
          if (chordTimer) clearTimeout(chordTimer);
          setLetterMode(mode);
          chordTimer = setTimeout(() => setLetterMode(null), CHORD_TIMEOUT_MS);
          return;
        }
        default:
          return;
      }
    }

    // W2.6b — set a mark at the CURRENT route (kb + source-relative path
    // from the URL, active section from the independent listener above).
    // No-ops with a toast when there's no artifact open here to mark.
    function setMarkHere(letter: string) {
      const match = matchPath({ path: "/a/:kb/*" }, locationRef.current.pathname);
      const kb = match?.params.kb;
      const sourceRelative = match?.params["*"];
      if (!kb || sourceRelative === undefined) {
        toast.err("open an artifact to set a mark here");
        return;
      }
      const cached = queryClient.getQueryData<DocSummary>(["doc", kb, sourceRelative]);
      saveMark(letter, {
        kb,
        sourceRelative,
        title: cached?.title || sourceRelative,
        sec: activeSectionRef.current,
      });
      toast.ok(`mark '${letter}' set`);
    }

    // W2.6b — jump to a saved mark. Zero new machinery: the artifact
    // permalink grammar already carries a section deep-link.
    function jumpToMark(letter: string) {
      const mark = getMark(letter);
      if (!mark) {
        toast.err(`no mark '${letter}'`);
        return;
      }
      navigate(artifactHref(mark.kb, mark.sourceRelative, mark.sec ? { sec: mark.sec } : undefined));
    }

    // W3.P-c — what `"` captures: the most specific reference the CURRENT
    // route can name, with zero new fetches (every field is already in the
    // query cache or the URL). A reader gives an artifact position (the
    // same payload `m` stores, plus the artifact id + a paste-ready
    // title); the session/memory routes' focused-session param gives a
    // session reference. Selections are NOT captured here on purpose —
    // the artifact lives in a cross-origin iframe, so the only correct
    // capture site for a highlight is `SelectionActions` (which owns the
    // postMessage round-trip); the store speaks that kind already.
    function refHere(): Ref | null {
      const loc = locationRef.current;
      const match = matchPath({ path: "/a/:kb/*" }, loc.pathname);
      const kb = match?.params.kb;
      const sourceRelative = match?.params["*"];
      if (kb && sourceRelative !== undefined) {
        const cached = queryClient.getQueryData<DocSummary>(["doc", kb, sourceRelative]);
        return {
          kind: "artifact",
          kb,
          sourceRelative,
          title: cached?.title || sourceRelative,
          sec: activeSectionRef.current,
          id: cached?.id ?? null,
        };
      }
      // `/sessions?focus=<sid>` (the sessions route's own focus param) and
      // `/memory?session=<sid>` (the memory route's session lens) are the
      // two places the SPA already addresses one session by id.
      const params = new URLSearchParams(loc.search);
      const sessionId =
        (loc.pathname === "/sessions" ? params.get("focus") : null) ??
        (loc.pathname === "/memory" ? params.get("session") : null);
      if (sessionId) {
        return {
          kind: "session",
          sessionId,
          title: `session ${sessionId.slice(0, 12)}`,
        };
      }
      return null;
    }

    function storeRegister(letter: string) {
      const ref = refHere();
      if (!ref) {
        toast.err("nothing here to store in a register");
        return;
      }
      setRegister(letter, ref);
      toast.ok(`register '${letter}' set`);
    }

    // W3.P-c — paste = the citation grammar, never a second one:
    // `lib/quote.ts`'s `renderRegister` is the SAME builder the `y p`
    // provenance yank and the comment/selection cite actions go through.
    function pasteRegister(letter: string) {
      const reg = getRegister(letter);
      if (!reg) {
        toast.err(`register '${letter}' is empty`);
        return;
      }
      navigator.clipboard
        ?.writeText(renderRegister(reg.ref))
        .then(() => toast.ok(`register '${letter}' copied`))
        .catch(() => toast.err("couldn't copy register"));
    }

    function onKey(e: KeyboardEvent) {
      // Modifier-held → let the browser handle it (⌘+K is handled
      // separately in app.tsx).
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;

      // Escape gets exact pre-registry parity: it only preventDefaults when
      // it actually closes something (help open, or a chord in flight) —
      // registered in `REGISTRY` for the sheet, but handled directly here
      // since its behavior depends on this component's own `helpOpen`
      // state, not just the chord grammar.
      if (e.key === "Escape") {
        if (helpOpen) {
          e.preventDefault();
          setHelpOpen(false);
        }
        if (letterMode) {
          e.preventDefault();
          setLetterMode(null);
          if (chordTimer) {
            clearTimeout(chordTimer);
            chordTimer = null;
          }
        }
        if (peek) {
          e.preventDefault();
          closePeek();
        }
        if (keymapState.pending.length > 0) {
          if (chordTimer) {
            clearTimeout(chordTimer);
            chordTimer = null;
          }
          setKeymapState(initialKeymapState());
        }
        return;
      }

      // W2.6b — marks / W3.P-c — registers: once `dispatch()` above puts
      // us into a letter mode, the NEXT keystroke is the a–z slot, owned
      // directly here (no wildcard/parameter token exists in resolve()'s
      // chord machine — see `lib/keymap.ts`'s Marks registry comment). A
      // non a–z keystroke aborts the mode silently and falls through to
      // normal handling below, so e.g. an accidental arrow key isn't just
      // eaten.
      if (letterMode) {
        const letter = e.key.toLowerCase();
        if (/^[a-z]$/.test(letter)) {
          e.preventDefault();
          setLetterMode(null);
          if (chordTimer) {
            clearTimeout(chordTimer);
            chordTimer = null;
          }
          if (letterMode === "mark-set") setMarkHere(letter);
          else if (letterMode === "mark-jump") jumpToMark(letter);
          else if (letterMode === "register-set") storeRegister(letter);
          else pasteRegister(letter);
          return;
        }
        setLetterMode(null);
        if (chordTimer) {
          clearTimeout(chordTimer);
          chordTimer = null;
        }
        // fall through — this keystroke might still be a real binding
        // (e.g. "g").
      }

      const wasPending = keymapState.pending.length > 0;
      const result = resolve(keymapState, { key: e.key }, "global");
      setKeymapState(result.state);

      if (result.kind === "matched") {
        e.preventDefault();
        if (chordTimer) {
          clearTimeout(chordTimer);
          chordTimer = null;
        }
        dispatch(result.binding.actionId);
        return;
      }
      if (result.kind === "pending") {
        e.preventDefault();
        if (chordTimer) clearTimeout(chordTimer);
        chordTimer = setTimeout(() => setKeymapState(initialKeymapState()), CHORD_TIMEOUT_MS);
        return;
      }
      // "none" — only consume the keystroke if it aborted an in-flight
      // chord (matches the pre-registry behavior: a bare unmatched key
      // with nothing pending falls through untouched, e.g. to
      // `useRovingCursor`'s own j/k listener on the gallery/search routes).
      if (wasPending) {
        e.preventDefault();
        if (chordTimer) {
          clearTimeout(chordTimer);
          chordTimer = null;
        }
      }
    }
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      if (chordTimer) clearTimeout(chordTimer);
    };
  }, [navigate, keymapState, helpOpen, letterMode, peek, queryClient, closePeek]);

  // SH.D.4 — a UI entry point for the '?' cheat sheet (the audit found ZERO
  // affordance for it beyond the bare keypress): Header's "?" icon button
  // and the Cmdk "Keyboard shortcuts" command both dispatch this same
  // window CustomEvent rather than reaching into `helpOpen` directly — the
  // same "second listener is fine" shape `kb:toc-spy.advance` already uses
  // for `g t` (just the reverse direction: chrome dispatches, this listens),
  // and cheaper than a context provider for a single boolean.
  useEffect(() => {
    function onToggleHelp() {
      setHelpOpen((v) => !v);
    }
    window.addEventListener("kb:keyhelp.toggle", onToggleHelp);
    return () => window.removeEventListener("kb:keyhelp.toggle", onToggleHelp);
  }, []);

  // W2.6b — Alt-hover peek: a single delegate listener at the app-shell
  // level (this component, mounted once at app root) rather than per-
  // component wiring in every one of the 6+ render sites that emit
  // `/a/...` links (cards, search results, inspector nbr rows, TocSpy,
  // backlinks, wikilinks, session-file links) — mirrors the "one home"
  // shape HotkeyRoot/ConfirmProvider already use. Hard boundary: the
  // artifact iframe is cross-origin, so its internal links never bubble a
  // `mouseover` to this window at all — peek only ever sees SPA-chrome
  // links.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    let hoverEl: HTMLAnchorElement | null = null;

    function clearTimer() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    }

    function onMouseOver(e: MouseEvent) {
      if (!e.altKey) return;
      if (isOverlayOpenElsewhere()) return;
      const target = e.target as HTMLElement | null;
      const link = target?.closest('a[href^="/a/"]') as HTMLAnchorElement | null;
      if (!link || link === hoverEl) return;
      hoverEl = link;
      clearTimer();
      cancelPeekHide();
      const match = matchPath({ path: "/a/:kb/*" }, link.pathname);
      const kb = match?.params.kb;
      const sourceRelative = match?.params["*"];
      if (!kb || sourceRelative === undefined) return;
      const rect = link.getBoundingClientRect();
      timer = setTimeout(() => {
        setPeek({
          target: { kind: "artifact", kb, sourceRelative },
          rect: { top: rect.top, left: rect.left, bottom: rect.bottom, right: rect.right },
        });
      }, PEEK_DELAY_MS);
    }

    function onMouseOut(e: MouseEvent) {
      const target = e.target as HTMLElement | null;
      const link = target?.closest('a[href^="/a/"]') as HTMLAnchorElement | null;
      if (link && link === hoverEl) {
        hoverEl = null;
        clearTimer();
        // link-flow — a GRACE hide (not an instant one): the card is
        // interactive now, and the pointer crossing the gap onto it fires
        // `onHoverIn`, which cancels this.
        schedulePeekHide();
      }
    }

    // Releasing Alt closes an already-open peek (or cancels a pending one)
    // immediately rather than leaving it stranded until mouseleave.
    function onKeyUp(e: KeyboardEvent) {
      if (e.key === "Alt") {
        clearTimer();
        closePeek();
      }
    }

    window.addEventListener("mouseover", onMouseOver);
    window.addEventListener("mouseout", onMouseOut);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      window.removeEventListener("mouseover", onMouseOver);
      window.removeEventListener("mouseout", onMouseOut);
      window.removeEventListener("keyup", onKeyUp);
      clearTimer();
      cancelPeekHide();
    };
  }, [cancelPeekHide, schedulePeekHide, closePeek]);

  const chordLabel =
    keymapState.pending.length > 0
      ? keymapState.pending.join(" ")
      : letterMode
        ? LETTER_MODE_LABEL[letterMode]
        : null;

  if (!helpOpen && chordLabel === null && !hintMode && !peek) return null;
  return (
    <>
      {chordLabel !== null && (
        <div className="kb-chord" role="status" aria-label="chord prefix">
          {chordLabel} _
        </div>
      )}
      {helpOpen && <KeyHelp onClose={() => setHelpOpen(false)} />}
      {hintMode && <HintOverlay mode={hintMode} onClose={() => setHintMode(null)} />}
      {peek && (
        <PeekCard
          target={peek.target}
          rect={peek.rect}
          fallbackKb={explicitKb}
          onClose={closePeek}
          onHoverIn={cancelPeekHide}
          onHoverOut={schedulePeekHide}
        />
      )}
    </>
  );
}

function KeyHelp({ onClose }: { onClose: () => void }) {
  const cardRef = useRef<HTMLDivElement | null>(null);
  useFocusTrap(cardRef, true);
  const groups = groupRegistry();
  // W2.6b — "the smallest honest surface": marks have no dedicated route,
  // so their listing rides here, in the one place every binding is
  // already documented. Read once per open (marks rarely change while the
  // sheet is up).
  const marks = useMemo(
    () => [...listMarks()].sort((a, b) => a.letter.localeCompare(b.letter)),
    [],
  );
  // W3.P-c — the registers lens over the SAME 26 slots. Two panels, two
  // verbs, one store: "Your marks" lists the artifact-position slots you
  // can JUMP to (backtick), "Your registers" lists every slot with what it
  // PASTES as. An artifact slot legitimately appears in both — same row,
  // two different actions — which is the point of the subsumption rather
  // than a duplication (`lib/registers.ts`'s module doc has the ruling).
  // `useState` (not `useMemo`) so clearing a slot re-renders the list.
  const [registers, setRegisters] = useState<Register[]>(() =>
    [...listRegisters()].sort((a, b) => a.letter.localeCompare(b.letter)),
  );
  return (
    <div
      className="kb-keyhelp"
      role="dialog"
      aria-modal="true"
      aria-label="keyboard shortcuts"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="kb-keyhelp__card" ref={cardRef}>
        <header className="kb-keyhelp__head">
          <h2>Keyboard shortcuts</h2>
          <button
            type="button"
            className="kb-keyhelp__close"
            onClick={onClose}
            aria-label="close"
          >
            <Icon.X />
          </button>
        </header>
        {groups.map(({ group, bindings }) => (
          <Section key={group} title={group}>
            {bindings.map((b, i) => (
              <Row
                key={`${b.keys}|${b.when ?? i}`}
                k={b.keys}
                desc={b.label}
                scope={b.scope !== "global" ? b.scope : undefined}
              />
            ))}
          </Section>
        ))}
        {marks.length > 0 && (
          <Section title="Your marks">
            {marks.map((m) => (
              <div className="kb-keyhelp__row" key={m.letter}>
                <kbd>{m.letter}</kbd>
                <span>
                  {m.title}
                  <span className="kb-keyhelp__scope"> · {m.kb}</span>
                </span>
              </div>
            ))}
          </Section>
        )}
        {registers.length > 0 && (
          <Section title="Your registers">
            {registers.map((r) => (
              <RegisterRow
                key={r.letter}
                reg={r}
                onClear={() =>
                  setRegisters(
                    [...clearRegister(r.letter)].sort((a, b) =>
                      a.letter.localeCompare(b.letter),
                    ),
                  )
                }
              />
            ))}
          </Section>
        )}
        <p className="kb-keyhelp__foot">
          Chord prefix <kbd>g</kbd> holds for {CHORD_TIMEOUT_MS} ms — type
          the letter before then to jump.
        </p>
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="kb-keyhelp__sect">
      <h3>{title}</h3>
      {children}
    </section>
  );
}

// W3.P-c — one register slot, with its two PASTE actions.
//
//   cite  `lib/quote.ts`'s `renderRegister` → clipboard. Exactly the
//         payload `'<letter>` writes, and exactly the payload the `y p`
//         provenance yank / comment cite already produce — one grammar.
//   list  `AddToListButton`, reused verbatim (not forked): it already
//         knows how to put an artifact — or an anchored slice of one —
//         into a reading list. Only artifact/selection registers carry the
//         artifact id it needs, so a session/commit slot (or a mark
//         migrated forward from W2.6b, which never stored an id) simply
//         doesn't render the button. This is also the answer to "I want to
//         KEEP a set of these": that set is a reading list, and it is one
//         click away.
function RegisterRow({ reg, onClear }: { reg: Register; onClear: () => void }) {
  const ref = reg.ref;
  const context = registerContext(ref);
  const listable =
    (ref.kind === "artifact" || ref.kind === "selection") && !!ref.id
      ? { kb: ref.kb, id: ref.id }
      : null;
  const anchor: GeneratedAnchor | undefined =
    ref.kind === "selection"
      ? {
          kind: "selection",
          css_path: ref.cssPath,
          offset: ref.offset,
          snippet: ref.snippet,
        }
      : undefined;
  const copy = () => {
    navigator.clipboard
      ?.writeText(renderRegister(ref))
      .then(() => toast.ok(`register '${reg.letter}' copied`))
      .catch(() => toast.err("couldn't copy register"));
  };
  return (
    <div className="kb-keyhelp__row kb-keyhelp__row--reg">
      <kbd>{reg.letter}</kbd>
      <span className="kb-keyhelp__reg-what">
        <span className="kb-keyhelp__reg-kind">{ref.kind}</span>
        {registerLabel(ref)}
        {context && <span className="kb-keyhelp__scope"> · {context}</span>}
      </span>
      <span className="kb-keyhelp__reg-acts">
        <button
          type="button"
          data-kb-act="register-cite"
          onClick={copy}
          title="copy this register as a citation"
        >
          cite
        </button>
        {listable && (
          <AddToListButton
            kb={listable.kb}
            artifactId={listable.id}
            anchor={anchor}
            variant="icon"
            dataAct={`register-list-${reg.letter}`}
          />
        )}
        <button
          type="button"
          data-kb-act="register-clear"
          onClick={onClear}
          title={`clear register '${reg.letter}'`}
          aria-label={`clear register ${reg.letter}`}
        >
          <Icon.X />
        </button>
      </span>
    </div>
  );
}

function Row({ k, desc, scope }: { k: string; desc: string; scope?: string }) {
  return (
    <div className="kb-keyhelp__row">
      <kbd>{k}</kbd>
      <span>
        {desc}
        {scope && <span className="kb-keyhelp__scope"> · {scope}</span>}
      </span>
    </div>
  );
}
