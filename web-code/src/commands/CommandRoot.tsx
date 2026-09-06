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
import { KBC_COMMANDS, type KbcCommand, type KbcPreset, type KbcScope } from "./registry.gen";
import WhichKey from "./WhichKey";

// Built once; the registry is a compile-time constant.
const COMMANDS_BY_ID = new Map(KBC_COMMANDS.map((c) => [c.id, c]));

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
/// suite (`CommandRoot.test.ts`): "does CM6 genuinely hold the keyboard
/// RIGHT NOW" is a question about live DOM focus (`activeElement`), never
/// about `keyboardRegion`/`ctx.buffer` — see the call site's own doc for
/// why conflating them regressed the palette door (`:`) after a click on
/// `<body>`, a target `keyboardRegion` deliberately leaves ambiguous.
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
  activeElement: EventTarget | null,
  token: string,
  chordPendingLength: number,
  ctx: Ctx = {},
): boolean {
  if (!inReadOnlyBuffer(activeElement) || !isBareToken(token) || chordPendingLength !== 0) {
    return false;
  }
  const exact = resolve(token, "reader", ctx, "vim");
  if (exact) return exact.vimKind !== undefined;
  const cont = continuations([token], "reader", ctx, "vim");
  return cont.length > 0 && cont.every((c) => c.command.vimKind !== undefined);
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
      // V70-H1 — tests REAL DOM focus (`document.activeElement`), not
      // `ctx.buffer`/`keyboardRegion`: this guard's whole purpose is "does
      // CM6 actually hold the keyboard right now", a question about the
      // live DOM, not about the app's own SCOPE bookkeeping. Reading the
      // region here made them the same variable when they answer two
      // different questions — `keyboardRegion` intentionally does NOT
      // regress on an unnamed `focusin` target (`<body>`, see
      // `lib/keyboardRegion.ts`), so after a click on `<body>` the region
      // can legitimately still read "buffer" while nothing is actually
      // focused there — and this guard, if it shared that value, would
      // keep withholding every bare key (`:`, the palette's own door)
      // forever. `document.activeElement` has no such lag: it is exactly
      // "what has focus this instant."
      if (shouldWithholdFromBuffer(document.activeElement, token, chordRef.current.pending.length, ctx)) return;

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
