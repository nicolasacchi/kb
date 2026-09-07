// V70-A5 — the ONE window-level keydown host.
//
// Mounted once in `app.tsx`. It replaces the ad-hoc ⌘K listener that used to
// live there, and it owns the single pending-chord state for the whole app
// (the `g`, `Space`, `[`, `]`, `Ctrl-w`, `z`, `m`, `'`, `y` prefixes) —
// retiring `ReviewDiff.tsx`'s private 500 ms chord timer and unifying the
// three timeout policies the recon found on vim's own answer: a prefix never
// expires by itself, Escape cancels it.
//
// WHAT IT DOES AND DOES NOT DISPATCH (the two-layer rule, ported from kb's
// `web/src/lib/keymap.ts`):
//
//   * ONE window listener and ONE pending-chord state, for every scope. That
//     is what retires `ReviewDiff.tsx`'s private 500 ms timer: the diff page
//     no longer owns a chord machine, it owns HANDLERS.
//   * a matched command runs the handler its surface registered with
//     `useCommandHandlers`, and only that one — the most recent registration
//     wins, so a route that re-binds a global verb while it is mounted is
//     honoured, and the binding reverts on unmount. `dispatch: "central" |
//     "surface"` documents WHO is expected to register (the app shell, or the
//     route that renders the thing) — it is a contract for readers and for
//     `kb-code commands doctor`, not a second code path here.
//   * a row with no registered handler is NOT swallowed: the key falls
//     through to the browser rather than silently doing nothing, which is the
//     failure the recon found in three CodeViews (`gd` resolving, returning
//     false, and dying without feedback).
//
// The CM6 buffer keeps its own layer: `vimReader` installs at `Prec.highest`
// and stops bare keys before they reach the window, and guard 2 below makes
// that explicit rather than relying on ordering.
//
// THREE GUARDS, in order:
//
//   1. `isTypingTarget` — the A3S guard, kept verbatim. Never steal a key out
//      of a composer or the editable CM6 suggestion editor.
//   2. the read-only reader buffer — `isTypingTarget` deliberately returns
//      false there (so ⌘K keeps working while browsing code), but the vim
//      layer owns every BARE key inside it. So in a CodeView we dispatch only
//      MODIFIED tokens, which is exactly the pre-A5 behaviour.
//   3. modifier bail for chords — a pending prefix plus a browser chord
//      (Ctrl-t) is the browser's, not ours.

import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import { loadKeyPreset } from "../lib/prefs";
import {
  continuations,
  IDLE,
  isBareToken,
  resolve,
  step,
  tokenOf,
  type ChordState,
  type Ctx,
} from "./dispatch";
import { isTypingTarget } from "../lib/isTypingTarget";
import { KBC_ACTIVE_COMMANDS, KBC_COMMANDS, type KbcCommand, type KbcPreset, type KbcScope } from "./registry.gen";
import WhichKey from "./WhichKey";

// Built once; the registry is a compile-time constant.
// V73-K6 — `KBC_ACTIVE_COMMANDS`: `bus.run(id)` (the palette's own runner,
// and any future deep-link caller) must never resolve a retired id to a
// command object, even if something later mis-registers a handler under
// its name — `resolveById` returning `undefined` is what keeps that path
// fail-closed.
const COMMANDS_BY_ID = new Map(KBC_ACTIVE_COMMANDS.map((c) => [c.id, c]));

/// How long a pending prefix sits before which-key appears. Not a timeout —
/// the prefix survives it — just the pause that earns a hint (§P2's "~400 ms").
export const WHICH_KEY_MS = 400;

/// How long the leader must be HELD before the rehearsal overlay renders key
/// hints on every element carrying an `affordance_anchor` (§P2).
export const REHEARSAL_MS = 250;

export type CommandHandler = (arg: { command: KbcCommand; count: number | null }) => void;

interface CommandBus {
  scope: KbcScope;
  ctx: Ctx;
  preset: KbcPreset;
  /// Run a command by id, from anywhere (the palette, a deep link, a click).
  /// Returns false when nothing is registered for it — an honest miss the
  /// caller can surface rather than a silent no-op.
  run: (id: string, count?: number | null) => boolean;
  /// Register this surface's scope + live context. The last mount wins, which
  /// is what a route swap should do.
  publish: (scope: KbcScope, ctx: Ctx) => void;
  register: (handlers: Record<string, CommandHandler>) => () => void;
  setPreset: (p: KbcPreset) => void;
  /// V71-K4 — "is a chord in flight, and will it CONSUME this keystroke?"
  /// The one channel a local key layer that runs BEFORE this window
  /// listener (the CM6 vim keymap at `Prec.highest`; the peek panel's own
  /// `onKeyDown`, which stops propagation) uses to stand down for a
  /// continuation key instead of acting on it too. Reads the live chord /
  /// scope / ctx / preset refs, so it answers for the keystroke being
  /// dispatched RIGHT NOW — never a render-old copy. See
  /// `chordConsumesKey`.
  chordWillConsume: (e: KeyTokenSource) => boolean;
}

/// The shape `tokenOf` needs — a real `KeyboardEvent`, React's synthetic
/// one, or a literal in a test.
export interface KeyTokenSource {
  key: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
}

const Bus = createContext<CommandBus | null>(null);

/// The bus, or a null-object. A component outside the provider (a unit test
/// rendering one card) gets a bus that resolves nothing rather than a crash.
export function useCommands(): CommandBus {
  return (
    useContext(Bus) ?? {
      scope: "global",
      ctx: {},
      preset: "vim",
      run: () => false,
      publish: () => {},
      register: () => () => {},
      setPreset: () => {},
      chordWillConsume: () => false,
    }
  );
}

/// Declare "this surface is scope X, and here is its live context". Cheap to
/// call on every render; the bus stores the latest in a ref.
export function useCommandScope(scope: KbcScope, ctx: Ctx = {}): void {
  const bus = useCommands();
  const key = JSON.stringify(ctx);
  useEffect(() => {
    bus.publish(scope, JSON.parse(key) as Ctx);
    // `key` is the ctx's value identity — a fresh object each render would
    // otherwise re-publish forever.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, key]);
}

/// Register central handlers for the commands this surface owns. Returns
/// nothing; the unregister runs on unmount.
export function useCommandHandlers(handlers: Record<string, CommandHandler>): void {
  const bus = useCommands();
  const ref = useRef(handlers);
  ref.current = handlers;
  const ids = Object.keys(handlers).sort().join(",");
  useEffect(() => {
    const stable: Record<string, CommandHandler> = {};
    for (const id of Object.keys(ref.current)) {
      stable[id] = (arg) => ref.current[id]?.(arg);
    }
    return bus.register(stable);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ids]);
}

export function inReadOnlyBuffer(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  if (!t || typeof t.closest !== "function") return false;
  return t.closest(".kbc-codeview") !== null || t.closest(".cm-editor") !== null;
}

/// V70-H1 — Guard 2's predicate, pulled out pure and exported for the unit
/// suite (`CommandRoot.test.ts`): "did CM6 genuinely hold the keyboard when
/// this key was PRESSED" is a question about DOM focus, never about
/// `keyboardRegion`/`ctx.buffer` — see the call site's own doc for why
/// conflating them regressed the palette door (`:`) after a click on
/// `<body>`, a target `keyboardRegion` deliberately leaves ambiguous.
///
/// V71-K2 — and it is a question about the focus the KEYDOWN carries
/// (`e.target`), never about live `document.activeElement`. See the call
/// site for the race that distinction settles: by the time the event
/// reaches the window, the vim layer this guard defers to has already run
/// its own callback and moved focus out of the buffer.
///
/// V70-K1 — the predicate used to stop there: ANY bare token, the instant
/// the buffer held focus, was withheld — correct for a key the vim layer
/// (`vimKeys.ts`/`vimReader.ts`) actually claims, but it withheld EVERY bare
/// key, including ones the vim layer has no opinion on at all. Since
/// "focus follows the file" (A3) lands DOM focus in the buffer the instant
/// any file opens, that made every bare `scope: global`/`dispatch: central`
/// command structurally unreachable the moment a file was open — not just
/// the two this milestone found by accident (`:`, V70-H1; `u`, V70-A6, each
/// patched with its own vim-layer arm), but the entire 35-row `Space`-leader
/// family (`nav.home`, `view.theme-cycle`, `desk.preset.*`, `drawer.*`,
/// `rail.*`, …) and a handful of un-prefixed rows (`nav.forward` (`U`),
/// `action.panel` (`.`), `hint.jump`/`hint.act` (`f`/`F`), `scope.edit`/
/// `scope.clear` (`s`/`S`)) — none of which had ever fired from inside the
/// buffer, silently, since nothing short-circuits `CommandRoot`'s "leave the
/// key alone" branch loudly.
///
/// The fix asks the registry instead of re-deriving the boundary by hand:
/// resolve/continue the token in scope `"reader"` (`resolve()`'s own
/// `inPlay` rule already folds in `global` rows, and its own
/// "scope-specific shadows global" precedence is what keeps `:` resolving
/// to `goto.line` over `cmd.palette.commands`, exactly as today) and ask
/// whether every command that could still be typed from here carries a
/// `vim_kind`. A row carries `vim_kind` iff the CM6 reducer really will
/// react to it — `vimParity.test.ts` proves that vocabulary is exactly the
/// `vim_kind` rows, both directions — so:
///   - an EXACT match settles it outright (resolve() already picked the one
///     winning command via scope precedence): withhold iff that command has
///     `vim_kind` (this is what keeps `:`/`u`/`?` routed through their vim
///     arm — they're global+central rows that ALSO carry `vim_kind`, so the
///     forwarding arm each still owns is not retired by this fix);
///   - otherwise, withhold only when EVERY reachable continuation of this
///     prefix carries `vim_kind`. "every", not "any": `[`/`]` continue into
///     BOTH vim-owned rows (`[c`/`[f`, reader scope) and non-vim rows
///     (`[d`/`]d`, `drawer.tab-{prev,next}`, global+central) — withholding
///     `[` because ONE interpretation is vim's would make the other
///     unreachable too, the very bug this unit exists to fix. Letting `[`
///     through is safe either way it resolves: a vim-owned continuation
///     that also had a central registration would double-fire, but none of
///     `commit.prev`/`workingset.prev`/`commit.next`/`workingset.next` are
///     centrally registered (grepped), so central's own resolution of them
///     is a harmless "nothing owns it here" no-op, and vim's independent,
///     unconditional reducer no-ops on any continuation it doesn't
///     recognise (its own `default` arm).
///
/// `vimReader.ts` is installed UNCONDITIONALLY inside the buffer regardless
/// of the app's selected keymap preset (`CodeView.tsx` never reads
/// `preset`), so "vim" is hardcoded here rather than read from the live
/// preset — the buffer's own behaviour never varies by preset either.
/// `ctx` DOES need to be the live bag, not `{}`: a handful of `vim_kind`
/// rows are context-gated (`yank.selection`/`mode.normal` on `mode`,
/// `pane.pin` on `pane.provisional`) and an empty bag can silently flip
/// which command an exact match resolves to.
export function shouldWithholdFromBuffer(
  focusTarget: EventTarget | null,
  token: string,
  chordPendingLength: number,
  ctx: Ctx = {},
): boolean {
  if (!inReadOnlyBuffer(focusTarget) || !isBareToken(token) || chordPendingLength !== 0) {
    return false;
  }
  const exact = resolve(token, "reader", ctx, "vim");
  if (exact) return exact.vimKind !== undefined;
  const cont = continuations([token], "reader", ctx, "vim");
  return cont.length > 0 && cont.every((c) => c.command.vimKind !== undefined);
}

/// V71-K4 — guard 2's DUAL, and the answer to a question guard 2 never
/// asks: who owns a chord's SECOND (and later) keystroke.
///
/// `shouldWithholdFromBuffer` above bails the instant
/// `chordPendingLength !== 0`, by design — once a chord is in flight the
/// keystroke belongs to the chord machine and must reach `step()`. That is
/// correct for CommandRoot. What nothing said out loud is that the CM6 vim
/// keymap (`editor/vimReader.ts`, `Prec.highest`, so it runs BEFORE the
/// window listener) has its own unconditional reducer with no memory that a
/// `Space`-prefixed sequence is under way — so a bare letter that is BOTH
/// some central chord's FINAL token AND a standalone vim key fired both
/// layers, every time. V71-K3 found it empirically: `Space u`
/// (`drawer.reopen`) and `Space R u` (`rail.tab.understand`) also ran vim's
/// `u` (`cb-nav-back`), which unmounted the reader mid-keystroke;
/// `Space R a` also ran `annotate.line`; `Space P v`/`Space R v`,
/// `Space R h`, `Space R n` and the already-shipped `Space p` collided too
/// and merely landed harmlessly. Same class, one key later:
/// `Space g h`/`Space g c`/`Space g t`/… let vim enter its OWN `g` prefix on
/// the middle token and then fire `gh`/`gc`/`gt` beside the central `nav.*`.
///
/// The predicate deliberately does NOT re-derive the boundary (that is how
/// the first version of guard 2 went wrong — V70-K1). It asks the ONE
/// question that actually matters, using CommandRoot's own inputs and its
/// own machine: **will this keystroke be CONSUMED by the pending chord?**
/// `step` is pure, so running it here and again in `onKey` is free of side
/// effects and cannot disagree with itself — the shield is airtight by
/// construction rather than by a table of keys someone has to keep in sync.
///
///   - `pending`/`cancelled` — CommandRoot calls `preventDefault` and holds
///     (or collapses) the chord: consumed. Escape lands here, which is why
///     a chord-cancelling Escape does not ALSO dismiss a panel — one
///     keystroke, one job (`step`'s own `cancelled` doc).
///   - `matched` — consumed only if CommandRoot will really run something:
///     a `planned` row has no executor and a row nothing registered here is
///     the explicit "nothing owns it — leave the key alone" branch. That
///     single clause is what keeps `[c`/`[f`/`]c`/`]f` (`commit.prev/next`,
///     `workingset.prev/next`) and every `Ctrl-w` chord working: they
///     resolve centrally but are `dispatch: "surface"` with no central
///     registration, so central does nothing and the vim layer must still
///     act. Withholding them would have traded K3's double-fire for a set
///     of dead vim chords — the same one-key-later trade V70-K1 refused.
///   - `none` — nothing matched, nothing extends: not consumed, the vim
///     layer keeps the key.
///
/// The FIRST token is not this function's business (`pending.length === 0`
/// ⇒ `false`, always): guard 2 alone rules on it, and a leading COUNT
/// (`5j`) leaves `pending` empty on purpose, so counts still reach vim.
export function chordConsumesKey(
  chord: ChordState,
  token: string,
  scope: KbcScope,
  ctx: Ctx,
  preset: KbcPreset,
  hasHandler: (id: string) => boolean,
): boolean {
  if (chord.pending.length === 0) return false;
  const res = step(chord, token, scope, ctx, preset);
  switch (res.kind) {
    case "pending":
    case "cancelled":
      return true;
    case "matched":
      return res.command.lifecycle === "shipped" && hasHandler(res.command.id);
    case "none":
      return false;
  }
}

export default function CommandRoot({ children }: { children: React.ReactNode }) {
  const [chord, setChord] = useState<ChordState>(IDLE);
  const [whichKey, setWhichKey] = useState(false);
  const [rehearsal, setRehearsal] = useState(false);
  const [preset, setPresetState] = useState<KbcPreset>(() => loadKeyPreset());

  // Scope + context are STATE, not just refs: the palette, the `?` sheet and
  // which-key all render from them, so a route swap has to re-render them.
  // The refs beside them are what the window listener reads (it is installed
  // once and must not close over a stale value).
  const [scope, setScope] = useState<KbcScope>("global");
  const [ctx, setCtx] = useState<Ctx>({});
  const scopeRef = useRef<KbcScope>("global");
  const ctxRef = useRef<Ctx>({});
  scopeRef.current = scope;
  ctxRef.current = ctx;
  const handlersRef = useRef<Map<number, Record<string, CommandHandler>>>(new Map());
  const seqRef = useRef(0);
  const chordRef = useRef<ChordState>(IDLE);
  chordRef.current = chord;
  const presetRef = useRef<KbcPreset>(preset);
  presetRef.current = preset;

  const bus = useMemo<CommandBus>(() => {
    function findHandler(id: string): CommandHandler | undefined {
      // Later registrations (deeper/more recent mounts) win, so a route that
      // re-binds a global verb for its own surface is honoured while it is up.
      let found: CommandHandler | undefined;
      for (const map of handlersRef.current.values()) if (map[id]) found = map[id];
      return found;
    }
    return {
      scope,
      ctx,
      preset,
      run(id, count = null) {
        const command = resolveById(id);
        const h = findHandler(id);
        if (!command || !h) return false;
        h({ command, count });
        return true;
      },
      publish(nextScope, nextCtx) {
        scopeRef.current = nextScope;
        ctxRef.current = nextCtx;
        setScope(nextScope);
        setCtx(nextCtx);
      },
      register(handlers) {
        const key = ++seqRef.current;
        handlersRef.current.set(key, handlers);
        return () => {
          handlersRef.current.delete(key);
        };
      },
      setPreset(p) {
        setPresetState(p);
      },
      chordWillConsume(e) {
        // Every input is read from a REF, not from this closure's captured
        // render values: the vim keymap asks mid-keystroke, and the answer
        // has to be about the chord as it stands at THIS keydown — the same
        // `chordRef`/`scopeRef`/`ctxRef`/`presetRef` the window listener
        // below feeds to `step`.
        return chordConsumesKey(
          chordRef.current,
          tokenOf(e),
          scopeRef.current,
          ctxRef.current,
          presetRef.current,
          (id) => findHandler(id) !== undefined,
        );
      },
    };
  }, [preset, scope, ctx]);

  // which-key: a pending prefix that survives WHICH_KEY_MS raises the sheet.
  // The timer only shows a hint — it never cancels the chord.
  useEffect(() => {
    if (chord.pending.length === 0) {
      setWhichKey(false);
      return;
    }
    const t = window.setTimeout(() => setWhichKey(true), WHICH_KEY_MS);
    return () => window.clearTimeout(t);
  }, [chord]);

  useEffect(() => {
    let rehearsalTimer = 0;

    function onKey(e: KeyboardEvent) {
      if (isTypingTarget(e.target)) return;
      const token = tokenOf(e);
      const scope = scopeRef.current;
      const ctx = ctxRef.current;
      const p = presetRef.current;

      // Guard 2 — inside the read-only reader buffer the vim layer owns every
      // bare key. Only modified tokens (⌘K, Ctrl-w …) reach the window host.
      //
      // V70-H1 — tests REAL DOM focus, not `ctx.buffer`/`keyboardRegion`:
      // this guard's whole purpose is "does CM6 actually hold the
      // keyboard", a question about the DOM, not about the app's own SCOPE
      // bookkeeping. Reading the region here made them the same variable
      // when they answer two different questions — `keyboardRegion`
      // intentionally does NOT regress on an unnamed `focusin` target
      // (`<body>`, see `lib/keyboardRegion.ts`), so after a click on
      // `<body>` the region can legitimately still read "buffer" while
      // nothing is actually focused there — and this guard, if it shared
      // that value, would keep withholding every bare key (`:`, the
      // palette's own door) forever.
      //
      // V71-K2 — but the DOM focus it must read is the one the EVENT
      // carries (`e.target`), NOT live `document.activeElement`. A
      // keyboard event's target is fixed at dispatch and is by definition
      // whatever held focus when the key went down; `document.activeElement`
      // is a live reading of RIGHT NOW, and "right now" is already too
      // late here. `vimReader` installs at `Prec.highest` INSIDE the
      // buffer, so it sees the keystroke first and runs its callback
      // before the event finishes bubbling to this window listener — and
      // those callbacks move focus: `cb-hover` (`K`) opens the peek panel,
      // which focuses itself (`PeekPanel`'s mount effect), and
      // `cb-nav-back` (`u`) navigates, which unmounts the CodeView and
      // drops focus to `<body>`. Reading `activeElement` here therefore
      // asked "is the buffer focused?" AFTER the very handler this guard
      // exists to defer to had already answered "not any more", so the
      // key fell through and fired its central handler TOO: `K` opened
      // both the hover card and an inline peek on the tree's focused row
      // (whose Esc then closed only the inline one, stranding the panel
      // — `resolve.spec.ts`), and `u` ran `nav.back` twice, the second
      // run seeing `history.length === 2` and undoing the first with a
      // plain Back (`nav-ramp.spec.ts`). `e.target` cannot drift that way;
      // it is the same value guard 1 (`isTypingTarget`) already reads one
      // line above.
      if (shouldWithholdFromBuffer(e.target, token, chordRef.current.pending.length, ctx)) return;

      // The rehearsal overlay is for HOLDING the leader, not for typing a
      // chord through it: any second key cancels it, so `Space d` never
      // flashes hints on its way to the drawer.
      if (token === "Space" && chordRef.current.pending.length === 0 && !e.repeat) {
        window.clearTimeout(rehearsalTimer);
        rehearsalTimer = window.setTimeout(() => setRehearsal(true), REHEARSAL_MS);
      } else if (token !== "Space") {
        window.clearTimeout(rehearsalTimer);
        setRehearsal(false);
      }

      const res = step(chordRef.current, token, scope, ctx, p);
      switch (res.kind) {
        case "pending":
          e.preventDefault();
          setChord(res.state);
          return;
        case "cancelled":
          // The Escape that collapsed the chord did its job; do NOT also run
          // the dismiss stack (one keystroke, one job).
          e.preventDefault();
          setChord(res.state);
          return;
        case "none":
          setChord(res.state);
          return;
        case "matched": {
          setChord(res.state);
          const { command, count } = res;
          if (command.lifecycle !== "shipped") {
            // A `planned` row is declared so the sheet, the palette and the
            // conflicts gate can see the key is SPOKEN FOR — but it has no
            // executor. Swallowing the key would be a lie; let it through.
            return;
          }
          let handler: CommandHandler | undefined;
          for (const map of handlersRef.current.values()) {
            if (map[command.id]) handler = map[command.id];
          }
          if (!handler) return; // nothing owns it here — leave the key alone
          e.preventDefault();
          handler({ command, count });
          return;
        }
      }
    }

    function onKeyUp(e: KeyboardEvent) {
      if (tokenOf(e) === "Space") {
        window.clearTimeout(rehearsalTimer);
        setRehearsal(false);
      }
    }
    function onBlur() {
      window.clearTimeout(rehearsalTimer);
      setRehearsal(false);
      setChord(IDLE);
    }

    window.addEventListener("keydown", onKey);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onBlur);
    return () => {
      window.clearTimeout(rehearsalTimer);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  const hints = useMemo(
    () => (chord.pending.length > 0 ? continuations(chord.pending, scope, ctx, preset) : []),
    [chord, preset, scope, ctx],
  );

  return (
    <Bus.Provider value={bus}>
      {children}
      {whichKey && hints.length > 0 && (
        <WhichKey pending={chord} hints={hints} scope={scope} />
      )}
      {rehearsal && <RehearsalOverlay preset={preset} />}
    </Bus.Provider>
  );
}

function resolveById(id: string): KbcCommand | undefined {
  return COMMANDS_BY_ID.get(id);
}

/// §P2's rehearsal overlay: hold the leader and every visible affordance that
/// has a key says so, in place. It reads the DOM once on mount (no observer,
/// no layout thrash while held) and renders absolutely-positioned chips over
/// the elements carrying a `data-cmd` the registry knows.
function RehearsalOverlay({ preset }: { preset: KbcPreset }) {
  const [chips, setChips] = useState<Array<{ key: string; x: number; y: number }>>([]);
  useEffect(() => {
    const out: Array<{ key: string; x: number; y: number }> = [];
    for (const el of Array.from(document.querySelectorAll<HTMLElement>("[data-cmd]"))) {
      const anchor = el.dataset.cmd;
      if (!anchor) continue;
      const cmd = KBC_COMMANDS.find((c) => c.affordanceAnchor === anchor);
      const key = cmd?.keys[preset]?.[0];
      if (!key) continue;
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) continue;
      out.push({ key, x: r.left + r.width / 2, y: r.top + r.height / 2 });
    }
    setChips(out);
  }, [preset]);
  return (
    <div className="kbc-rehearsal" data-kbc-rehearsal aria-hidden="true">
      {chips.map((c, i) => (
        <span key={i} className="kbc-rehearsal__chip" style={{ left: c.x, top: c.y }}>
          {c.key}
        </span>
      ))}
    </div>
  );
}
