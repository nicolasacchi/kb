// W2.6a — the keyboard grammar FOUNDATION.
//
// One declarative table (`REGISTRY`) plus a pure chord-resolution machine
// modeled on web-code's `editor/vimKeys.ts` shape: `(state, key) => {state,
// result}`, no DOM, no timers. `vimKeys.ts` pairs with a CM6 glue layer
// (`vimReader.ts`) that owns the actual keydown listener + guards; this file
// plays the same "pure reducer" role for kb's chrome-level grammar, paired
// with `components/chrome/HotkeyRoot.tsx` (the global chord dispatcher) and
// `hooks/useRovingCursor.ts` (the gallery/search cursor — FileTree.tsx's
// `moveFocus`/`activateFocused` model, not this machine — see its own doc).
//
// THE RULE: a binding that isn't in REGISTRY doesn't exist. The `?` cheat
// sheet (`HotkeyRoot.tsx`'s `KeyHelp`) is GENERATED from this table, never
// hand-copied — that's what killed the two lying rows ("a — coming with S2
// follow-up", "p — coming") this table replaces: there is no longer a
// second, hand-maintained place for a fictitious binding to live.
//
// TWO-LAYER OWNERSHIP: only `scope: "global"` entries are actually
// DISPATCHED by `resolve()` below (HotkeyRoot's single window listener).
// Every other scope ("gallery" | "search" | "reader" | "overlay") is
// registered here purely for the sheet's sake — a second, route/component-
// local piece of code still owns real execution (mirrors FileTree.tsx's
// window-level j/k handler owning the tree while CM6's vim layer owns the
// buffer). `when` names that home + any mount precondition; it's a doc-only
// string, never read by `resolve()`.
//
// For non-global (doc-only) entries `keys` may be a human-readable combined
// label (`"[ / ]"`, `"j / k / ↑ / ↓"`) rather than a single parseable
// sequence — safe, because `resolve()` only ever gets called with
// `scope: "global"` in this codebase (today's only real dispatcher is
// HotkeyRoot), and every `scope: "global"` entry below IS a real, single
// space-separated key sequence `resolve()` can parse.

export type Scope =
  | "global"
  | "gallery"
  | "search"
  | "reader"
  | "overlay"
  // W3.R-c — the session-replay reader (`routes/replay.tsx`). Its own scope
  // because it reuses keys that already mean something elsewhere (`[`/`]` is
  // sibling nav in the reader, `j`/`k` is the roving cursor in the gallery)
  // and those surfaces are never mounted at the same time as this one.
  | "replay"
  // SL4 — the slate board (`routes/slateBoard.tsx`). Its own scope for the
  // same reason `replay` has one: it reuses `j`/`k` (the gallery roving
  // cursor), `m` (marks) and `p` (the reading-list trail), and it is never
  // mounted at the same time as any of those surfaces.
  | "slates";

export interface Binding {
  /// Space-separated sequence of `KeyboardEvent.key` tokens for a REAL
  /// (`scope: "global"`) entry — `"?"`, `"Escape"`, `"g a"`. Doc-only
  /// entries may instead hold a human-readable combined label (see the
  /// module doc above).
  keys: string;
  scope: Scope;
  /// Cheat-sheet section heading (rows are grouped + rendered in this
  /// table's own iteration order — see `groupRegistry()`).
  group: string;
  label: string;
  actionId: string;
  /// Doc-only precondition / owning-file annotation. Never consumed by
  /// `resolve()` — differentiates entries that legitimately share a
  /// scope+keys combo because they're mutually exclusive by mount (e.g.
  /// three `Escape` handlers in the reader, one per overlay layer), and is
  /// asserted distinct by the uniqueness test in `keymap.test.ts`.
  when?: string;
}

// ── the registry ──────────────────────────────────────────────────────────

export const REGISTRY: readonly Binding[] = [
  // Navigation — real, dispatched by HotkeyRoot's resolve() machine. The
  // `g` prefix holds for 800ms (the component's timer, not this module's
  // concern) before fizzling.
  { keys: "g g", scope: "global", group: "Navigation", label: "Gallery (recent)", actionId: "nav.gallery" },
  { keys: "g a", scope: "global", group: "Navigation", label: "Atlas", actionId: "nav.atlas" },
  { keys: "g m", scope: "global", group: "Navigation", label: "Memory", actionId: "nav.memory" },
  { keys: "g l", scope: "global", group: "Navigation", label: "Reading lists", actionId: "nav.lists" },
  { keys: "g n", scope: "global", group: "Navigation", label: "Notes", actionId: "nav.notes" },
  { keys: "g s", scope: "global", group: "Navigation", label: "Sessions", actionId: "nav.sessions" },
  // SL4 — the slate board. The design names `g s`, but `g s` has been
  // Sessions since W2.6a and `findExact` matches a complete binding
  // immediately, so `g s <anything>` is structurally unreachable: one of the
  // two had to move, and silently re-pointing an existing chord is worse
  // than picking a free letter. `g b` (board) is unclaimed by every row in
  // this table. Recorded as a deviation in the SL4 report.
  { keys: "g b", scope: "global", group: "Navigation", label: "Slates (the board)", actionId: "nav.slates" },
  { keys: "g h", scope: "global", group: "Navigation", label: "History", actionId: "nav.history" },
  { keys: "g ,", scope: "global", group: "Navigation", label: "Settings", actionId: "nav.settings" },
  { keys: "g t", scope: "global", group: "Navigation", label: "Topics rail: expand / shrink", actionId: "nav.tocToggle" },

  // Reader — doc-only; `detail.tsx` (+ its QueueBar/annotate.ts children)
  // still owns real execution.
  { keys: "[ / ]", scope: "reader", group: "Reader", label: "Previous / next sibling in folder", actionId: "reader.sibling", when: "routes/detail.tsx" },
  { keys: "o / O", scope: "reader", group: "Reader", label: "Toggle full-screen reading", actionId: "reader.toggleImmersive", when: "routes/detail.tsx" },
  { keys: "b / B", scope: "reader", group: "Reader", label: "Open the bare artifact in a new tab", actionId: "reader.openBare", when: "routes/detail.tsx" },
  // W2.6b — "y p": copy the artifact's provenance block (title + permalink
  // + id + origin session, when known — see `lib/quote.ts`'s
  // `buildProvenanceBlock`). Doc-only like `o/O`/`b/B` just above; the
  // SAME viewing-mode keybind effect in `routes/detail.tsx` owns real
  // execution (its own small `y`-then-`p` chord, independent of this
  // module's machine — resolve() only ever runs at `scope: "global"`).
  { keys: "y p", scope: "reader", group: "Reader", label: "Copy the artifact's provenance block (title + permalink + id + origin session)", actionId: "reader.yankProvenance", when: "routes/detail.tsx" },
  { keys: "Escape", scope: "reader", group: "Reader", label: "Exit full-screen reading", actionId: "reader.exitImmersive", when: "routes/detail.tsx — while immersive" },
  { keys: "Escape", scope: "reader", group: "Reader", label: "Close the mobile reader-tools sheet", actionId: "reader.closeSheet", when: "routes/detail.tsx — mobile inspector sheet open" },
  { keys: "Escape", scope: "reader", group: "Reader", label: "Exit annotate / compose", actionId: "reader.exitAnnotate", when: "routes/detail.tsx + scripts/annotate.ts — while annotating/composing" },
  { keys: "n", scope: "reader", group: "Reader", label: "Next entry in the reading-list trail", actionId: "lists.queueNext", when: "components/lists/QueueBar.tsx — trail open" },
  { keys: "p", scope: "reader", group: "Reader", label: "Previous entry in the reading-list trail", actionId: "lists.queuePrev", when: "components/lists/QueueBar.tsx — trail open" },
  // link-flow — `u` (up) returns to the artifact you followed a link FROM,
  // restoring the scroll offset you left it at (`lib/flowStack.ts`). The
  // ContextBar's `↩` chip is the same action; both call one `flowBack`.
  // Doc-only like the rows above — `routes/detail.tsx`'s own key handler
  // (beside the `w` pane chords) owns execution, for the same measured
  // reason those aren't global: `useRovingCursor` runs an independent
  // window listener. `n`/`p` (the obvious vim-ish pair) are TAKEN by the
  // reading-list trail two rows up, hence `u`.
  { keys: "u", scope: "reader", group: "Reader", label: "Back up the reading flow — the artifact you came from, at the offset you left", actionId: "reader.flowBack", when: "routes/detail.tsx — flow stack non-empty" },

  // Panes (W3.P-b) — the two-pane artifact compare split (`?pane2=`, see
  // `lib/paneUrl.ts`). Doc-only, `scope: "reader"`, and the `w` prefix is
  // load-bearing for TWO measured reasons:
  //
  //   * `Ctrl-w` is UNAVAILABLE. `HotkeyRoot` returns early on ANY modifier
  //     (`if (e.metaKey || e.ctrlKey || e.altKey) return;`), and Ctrl/⌘-W is
  //     the browser's own close-tab — unclaimable.
  //   * these must NOT be `scope: "global"`. `useRovingCursor` runs an
  //     INDEPENDENT window keydown listener, so a global `w h` / `w l` would
  //     ALSO move the gallery cursor — `preventDefault()` on one listener
  //     does not stop a sibling listener on the same target. So these rows
  //     exist purely for the `?` cheat sheet (the W2.6a two-layer rule) and
  //     the real chord is a small independent `w`-prefix handler inside
  //     `routes/detail.tsx`, cloned from the `y`-then-`p` ref+timer pattern
  //     and guarded by `isEditableTarget`.
  { keys: "w v", scope: "reader", group: "Panes", label: "Split — open the next sibling artifact beside this one", actionId: "panes.split", when: "routes/detail.tsx — desktop reader (>860px)" },
  { keys: "w q", scope: "reader", group: "Panes", label: "Close the second pane", actionId: "panes.close", when: "routes/detail.tsx — split open" },
  { keys: "w h", scope: "reader", group: "Panes", label: "Focus the left pane", actionId: "panes.focusLeft", when: "routes/detail.tsx — split open" },
  { keys: "w l", scope: "reader", group: "Panes", label: "Focus the right pane", actionId: "panes.focusRight", when: "routes/detail.tsx — split open" },
  { keys: "w o", scope: "reader", group: "Panes", label: "Only — close the second pane and focus the first", actionId: "panes.only", when: "routes/detail.tsx — split open" },

  // Replay (W3.R-c) — the session-replay reader's playhead. Doc-only rows
  // (the W2.6a two-layer rule): `routes/replay.tsx` owns a single route-local
  // window keydown handler, guarded by `isEditableTarget`, and that route is
  // the only place these keys are live. They are NOT `scope: "global"` for
  // the same measured reason the `w` pane chords aren't: `useRovingCursor`
  // runs an independent window listener, so a global `j`/`k` would ALSO move
  // the gallery cursor. The playhead is INDEX-space (one stop per beat) —
  // sessions have hour-long idle gaps and a time-linear scrubber would be
  // mostly dead air — so every step below is "one beat", never "one second".
  // There is deliberately NO play/pause row: the replay has no autoplay, no
  // timer, no media (the operator scrubs; pull-only).
  { keys: "← / →", scope: "replay", group: "Replay", label: "Step the playhead one beat back / forward", actionId: "replay.stepBeat", when: "routes/replay.tsx" },
  { keys: "j / k", scope: "replay", group: "Replay", label: "Step the playhead one beat forward / back", actionId: "replay.stepBeatVim", when: "routes/replay.tsx" },
  { keys: "[ / ]", scope: "replay", group: "Replay", label: "Jump to the previous / next prompt segment", actionId: "replay.stepSegment", when: "routes/replay.tsx" },
  { keys: "Home / End", scope: "replay", group: "Replay", label: "Jump to the first / last beat", actionId: "replay.jumpEnds", when: "routes/replay.tsx" },

  // Gallery — doc-only; `hooks/useRovingCursor.ts` (mounted from
  // `routes/gallery.tsx`) owns real execution, FileTree.tsx-style (its own
  // window listener, CSS-class focus, never DOM focus theft).
  { keys: "j / k / ↑ / ↓", scope: "gallery", group: "Gallery", label: "Move the roving cursor", actionId: "gallery.cursorMove", when: "useRovingCursor (routes/gallery.tsx)" },
  { keys: "h / l / ← / →", scope: "gallery", group: "Gallery", label: "Move across columns (grid view)", actionId: "gallery.cursorMoveCol", when: "useRovingCursor (routes/gallery.tsx) — grid view only" },
  { keys: "Enter", scope: "gallery", group: "Gallery", label: "Open the focused card", actionId: "gallery.activate", when: "useRovingCursor (routes/gallery.tsx)" },
  { keys: "Escape", scope: "gallery", group: "Gallery", label: "Clear the cursor", actionId: "gallery.clearCursor", when: "useRovingCursor (routes/gallery.tsx)" },

  // Search results — doc-only; same `useRovingCursor` hook, mounted from
  // `routes/search.tsx` over the flat (non-grouped) results list.
  { keys: "j / k / ↑ / ↓", scope: "search", group: "Search results", label: "Move the roving cursor", actionId: "search.cursorMove", when: "useRovingCursor (routes/search.tsx)" },
  { keys: "Enter", scope: "search", group: "Search results", label: "Open the focused result", actionId: "search.activate", when: "useRovingCursor (routes/search.tsx)" },
  { keys: "Escape", scope: "search", group: "Search results", label: "Clear the cursor", actionId: "search.clearCursor", when: "useRovingCursor (routes/search.tsx)" },

  // Overlays — doc-only; each overlay owns its own keydown handling and
  // `stopPropagation`s so no lower layer double-handles (the "overlay owns
  // the keyboard" rule).
  { keys: "Escape", scope: "overlay", group: "Overlays", label: "Close the command palette", actionId: "cmdk.close", when: "components/Cmdk.tsx" },
  { keys: "Tab", scope: "overlay", group: "Overlays", label: "Cycle search mode (hybrid → keyword → semantic)", actionId: "cmdk.cycleMode", when: "components/Cmdk.tsx" },
  { keys: "⌘Enter", scope: "overlay", group: "Overlays", label: "Open the full search page", actionId: "cmdk.fullSearch", when: "components/Cmdk.tsx" },
  { keys: "↑ / ↓", scope: "overlay", group: "Overlays", label: "Move the command-palette cursor", actionId: "cmdk.cursorMove", when: "components/Cmdk.tsx" },
  { keys: "Enter", scope: "overlay", group: "Overlays", label: "Activate the focused command-palette row", actionId: "cmdk.activate", when: "components/Cmdk.tsx" },
  { keys: "j / k / ↑ / ↓ / ← / →", scope: "overlay", group: "Overlays", label: "Move through the resurface review queue", actionId: "resurface.move", when: "components/ResurfaceReview.tsx" },
  { keys: "Enter", scope: "overlay", group: "Overlays", label: "Open the current resurface-review item", actionId: "resurface.activate", when: "components/ResurfaceReview.tsx" },
  { keys: "Escape", scope: "overlay", group: "Overlays", label: "Close the resurface review", actionId: "resurface.close", when: "components/ResurfaceReview.tsx" },
  { keys: "Escape", scope: "overlay", group: "Overlays", label: "Close the atlas cameras popover", actionId: "atlas.closePopover", when: "components/AtlasView.tsx" },

  // Search & help — real (global) except ⌘K itself, which app.tsx's own
  // window listener owns (guarded out here before it ever reaches
  // HotkeyRoot — see that component's modifier-key bail).
  { keys: "⌘K", scope: "global", group: "Search & help", label: "Open command palette", actionId: "chrome.openPalette", when: "app.tsx — its own window keydown, not this registry's machine" },
  { keys: "/", scope: "global", group: "Search & help", label: "Open command palette (alias)", actionId: "chrome.openPalette" },
  { keys: "?", scope: "global", group: "Search & help", label: "Toggle this help", actionId: "chrome.toggleHelp" },
  { keys: "Escape", scope: "global", group: "Search & help", label: "Close this help / cancel a pending chord", actionId: "chrome.escape" },

  // Hints (W2.6b) — `f` opens hint mode: chip labels over every clickable
  // target ([data-kb-act], a[href], button — `lib/hints.ts`'s composite
  // selector); typing a label activates it. `F` is the same enumeration
  // but opens an `<a href>` target in a new tab instead of clicking it.
  // Both are real (global), dispatched by HotkeyRoot, which mounts
  // `HintOverlay` — that overlay then owns EVERY subsequent keystroke
  // itself (its own capture-phase listener + stopPropagation, the W2.6a
  // two-layer rule) until it resolves a target or Esc/scroll/resize closes
  // it, so the actual a–z hint labels never touch this module's chord
  // machine — the two rows below are doc-only, for the sheet's sake.
  { keys: "f", scope: "global", group: "Hints", label: "Hint mode — click a labeled target", actionId: "hint.open" },
  { keys: "F", scope: "global", group: "Hints", label: "Hint mode — open a link in a new tab", actionId: "hint.openTab" },
  { keys: "a-z", scope: "overlay", group: "Hints", label: "Type a hint's label to activate it", actionId: "hint.select", when: "components/HintOverlay.tsx" },
  { keys: "Escape", scope: "overlay", group: "Hints", label: "Close hint mode", actionId: "hint.close", when: "components/HintOverlay.tsx" },

  // Slates (SL4) — the board's own grammar. DOC-ONLY (`scope: "slates"`),
  // dispatched by `routes/slateBoard.tsx`'s own window listener, for exactly
  // the reason the Panes rows above give: `j`/`k` would also move
  // `useRovingCursor`'s cursor and `m`/`p` would also fire HotkeyRoot's
  // marks/trail handlers, and preventDefault() on one window listener does
  // not stop a sibling on the same target. The route guards with
  // `isEditableTarget` (this module's own export) and bails while the
  // composer or the history drawer is open.
  { keys: "j / k", scope: "slates", group: "Slates", label: "Move the cursor across cards in reading order", actionId: "slate.cursor", when: "routes/slateBoard.tsx" },
  { keys: "Enter", scope: "slates", group: "Slates", label: "Unfold the focused card's body", actionId: "slate.unfold", when: "routes/slateBoard.tsx — a card focused" },
  { keys: "m", scope: "slates", group: "Slates", label: "Mark the focused card (+1)", actionId: "slate.mark", when: "routes/slateBoard.tsx — a card focused" },
  { keys: "x", scope: "slates", group: "Slates", label: "Drop the focused card", actionId: "slate.drop", when: "routes/slateBoard.tsx — a card focused" },
  { keys: "e", scope: "slates", group: "Slates", label: "Edit the focused card (opens the composer, submits as a supersede)", actionId: "slate.edit", when: "routes/slateBoard.tsx — desktop, a card focused" },
  { keys: "p", scope: "slates", group: "Slates", label: "Pin / unpin the focused card", actionId: "slate.pin", when: "routes/slateBoard.tsx — desktop, operator identity" },
  { keys: "c", scope: "slates", group: "Slates", label: "Compose a post", actionId: "slate.compose", when: "routes/slateBoard.tsx" },
  { keys: "t", scope: "slates", group: "Slates", label: "Topic filter", actionId: "slate.topic", when: "routes/slateBoard.tsx" },
  { keys: "h", scope: "slates", group: "Slates", label: "History — dropped and superseded posts", actionId: "slate.history", when: "routes/slateBoard.tsx" },

  // Marks (W2.6b) — cross-kb bookmarks (`lib/marks.ts`, global — not
  // per-kb: a mark is meant to jump across corpora). `m` then a–z sets a
  // mark at the artifact (+ active section, when on a reader) currently
  // open; backtick then a–z jumps to one. `m` and `` ` `` are each real
  // (global) SINGLE-key bindings — resolve() matches them immediately (no
  // ambiguous longer chord shares either prefix) — and HotkeyRoot then
  // awaits the next a–z keystroke with its OWN small piece of state
  // (bypassing this module's chord machine, which has no wildcard/
  // parameter token — recon's assessed alternative to 52 literal "m a".."
  // ` z" rows).
  { keys: "m", scope: "global", group: "Marks", label: "Set a mark here (then a–z)", actionId: "marks.enterSet" },
  { keys: "`", scope: "global", group: "Marks", label: "Jump to a mark (then a–z)", actionId: "marks.enterJump" },

  // Registers (W3.P-c) — the GENERALIZATION of the Marks pair just above.
  // One store (`lib/registers.ts`, `kb:registers`), 26 letter slots, a
  // TAGGED reference in each (artifact position / selection / session /
  // commit). A mark is precisely the artifact-position case, so `m` and
  // `` ` `` above are a lens on the same 26 slots — `` ` a `` jumps to
  // whatever slot `a` holds, whether `m a` or `" a` stored it. There is
  // deliberately no second store: see that module's doc for the
  // one-container-primitive justification and the recorded CLI-parity
  // exemption (registers never cross the network).
  //
  // Mechanically identical to Marks: both are real (global) SINGLE-key
  // bindings resolve() matches immediately, after which HotkeyRoot awaits
  // the a–z letter with its own small state (the SAME letter-capture
  // machine marks use — this module's chord machine still has no
  // wildcard/parameter token). `Ctrl`/`⌘` variants are unavailable
  // (HotkeyRoot bails on any modifier), and `"`/`'` are unclaimed by every
  // other row in this table.
  { keys: '"', scope: "global", group: "Registers", label: "Store this reference in a register (then a–z)", actionId: "registers.enterSet" },
  { keys: "'", scope: "global", group: "Registers", label: "Paste a register as a citation (then a–z)", actionId: "registers.enterPaste" },
];

// ── grouping (pure — the cheat sheet iterates this) ────────────────────────

export interface RegistryGroup {
  group: string;
  bindings: Binding[];
}

/// Buckets `REGISTRY` by `group`, preserving first-seen order — the single
/// thing `HotkeyRoot`'s generated `KeyHelp` iterates.
export function groupRegistry(): RegistryGroup[] {
  const order: string[] = [];
  const byGroup = new Map<string, Binding[]>();
  for (const b of REGISTRY) {
    if (!byGroup.has(b.group)) {
      byGroup.set(b.group, []);
      order.push(b.group);
    }
    byGroup.get(b.group)!.push(b);
  }
  return order.map((group) => ({ group, bindings: byGroup.get(group)! }));
}

// ── the chord machine ───────────────────────────────────────────────────────

export interface KeymapState {
  /// Keys typed so far in the in-flight sequence (`[]` = idle).
  pending: readonly string[];
  /// Accumulated count digits — reserved for a future vim-style count
  /// prefix; no binding reads this yet, it just rides along and resets
  /// with the sequence (mirrors `vimKeys.ts`'s `pendingCount` string).
  pendingCount: string;
}

export function initialKeymapState(): KeymapState {
  return { pending: [], pendingCount: "" };
}

/// What the 800ms chord-timeout in `HotkeyRoot.tsx` calls when a prefix
/// goes stale — this module owns no timer itself (the hook layer does),
/// but exposes the pure state transition so the timeout's effect is
/// unit-testable without faking a clock.
export function resetPending(_state: KeymapState): KeymapState {
  return initialKeymapState();
}

export interface KeyInput {
  key: string;
}

export type ResolveResult =
  | { kind: "matched"; binding: Binding; state: KeymapState }
  | { kind: "pending"; state: KeymapState }
  | { kind: "none"; state: KeymapState };

function tokensOf(keys: string): string[] {
  return keys.split(" ");
}

function scopeMatches(b: Binding, scope: Scope): boolean {
  return b.scope === scope || b.scope === "global";
}

function startsWith(arr: readonly string[], prefix: readonly string[]): boolean {
  return prefix.every((p, i) => arr[i] === p);
}

/// An exact-length, exact-token match for `seq` within `scope` (global
/// bindings included as a fallback). When both a scope-specific and a
/// global binding share the sequence, the scope-specific one wins — scope
/// precedence, specific over global.
function findExact(seq: readonly string[], scope: Scope): Binding | undefined {
  const matches = REGISTRY.filter(
    (b) =>
      scopeMatches(b, scope) &&
      tokensOf(b.keys).length === seq.length &&
      startsWith(tokensOf(b.keys), seq),
  );
  if (matches.length === 0) return undefined;
  return matches.find((b) => b.scope === scope) ?? matches[0];
}

/// The core state machine — `HotkeyRoot.tsx` calls this once per guarded
/// keydown (after the modifier/`isEditableTarget` bail) and applies the
/// returned `state` + acts on `kind`/`binding`. Nothing else in this
/// codebase needs to know the chord grammar.
///
/// - `"matched"` — the sequence resolved to a binding; state resets to idle.
/// - `"pending"` — a longer binding shares this prefix; state carries the
///   partial sequence forward (the caller arms/holds its own timeout).
/// - `"none"` — no binding matches and none extends this prefix; state
///   resets to idle (the sequence fizzled).
export function resolve(state: KeymapState, key: KeyInput, scope: Scope): ResolveResult {
  // Escape always wins — collapses any in-flight sequence regardless of
  // what was pending (mirrors `vimKeys.ts`'s Escape handling).
  if (key.key === "Escape") {
    const binding = findExact(["Escape"], scope);
    return binding
      ? { kind: "matched", binding, state: initialKeymapState() }
      : { kind: "none", state: initialKeymapState() };
  }

  // Digit accumulator — see the `pendingCount` doc above; unused today.
  if (state.pending.length === 0 && /^[1-9]$/.test(key.key)) {
    return { kind: "pending", state: { ...state, pendingCount: state.pendingCount + key.key } };
  }

  const seq = [...state.pending, key.key];
  const exact = findExact(seq, scope);
  if (exact) return { kind: "matched", binding: exact, state: initialKeymapState() };

  const hasLongerPrefix = REGISTRY.some(
    (b) =>
      scopeMatches(b, scope) &&
      tokensOf(b.keys).length > seq.length &&
      startsWith(tokensOf(b.keys), seq),
  );
  if (hasLongerPrefix) {
    return { kind: "pending", state: { pending: seq, pendingCount: state.pendingCount } };
  }

  return { kind: "none", state: initialKeymapState() };
}

// ── shared input guard ──────────────────────────────────────────────────────

/// The copy-pasted "don't eat keystrokes while the user is typing" idiom
/// (HotkeyRoot.tsx, QueueBar.tsx, ResurfaceReview.tsx, detail.tsx all had
/// their own copy at HEAD) — consolidated here for the two new consumers
/// this phase adds (`HotkeyRoot.tsx`, `useRovingCursor.ts`); the pre-
/// existing sites are out of this phase's owned files and keep their own
/// inline copies.
export function isEditableTarget(target: EventTarget | null): boolean {
  const t = target as { tagName?: string; isContentEditable?: boolean } | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || !!t.isContentEditable);
}
