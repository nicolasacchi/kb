import { describe, expect, it } from "vitest";
import { isBareToken, resolve, tokensOf } from "./dispatch";
import { KBC_COMMANDS } from "./registry.gen";
import { shouldWithholdFromBuffer } from "./CommandRoot";

/// A minimal `closest()`-only stand-in for `document.activeElement` —
/// `inReadOnlyBuffer` (which `shouldWithholdFromBuffer` wraps) only ever
/// calls `.closest(selector)`.
function elementIn(...selectors: string[]): EventTarget {
  return {
    closest: (selector: string) => (selectors.includes(selector) ? {} : null),
  } as unknown as EventTarget;
}

const inBuffer = elementIn(".kbc-codeview");

describe("shouldWithholdFromBuffer", () => {
  it("withholds a bare token when the CM6 buffer genuinely holds focus", () => {
    expect(shouldWithholdFromBuffer(elementIn(".kbc-codeview"), "j", 0)).toBe(true);
    expect(shouldWithholdFromBuffer(elementIn(".cm-editor"), "j", 0)).toBe(true);
  });

  it("never withholds when the buffer does NOT genuinely hold focus — the exact regression", () => {
    // V70-H1 — a click on <body> (or any target outside both the buffer
    // and the tree) is a target `keyboardRegion` deliberately leaves
    // AMBIGUOUS (see `lib/keyboardRegion.ts`), so the region can still
    // read "buffer" from a stale prior state. This guard must NOT read
    // that region — it reads live DOM focus instead — so `:` (a bare
    // token, the command palette's own door) must resolve here, not be
    // silently swallowed.
    expect(shouldWithholdFromBuffer(elementIn("body"), ":", 0)).toBe(false);
    expect(shouldWithholdFromBuffer(null, ":", 0)).toBe(false);
    expect(shouldWithholdFromBuffer(elementIn("[data-kbc-tree]"), "j", 0)).toBe(false);
  });

  it("never withholds a modified token, even inside the buffer", () => {
    // `Ctrl-w h` etc. must reach the window host regardless of buffer
    // focus — only BARE tokens are the vim layer's exclusive territory.
    expect(shouldWithholdFromBuffer(elementIn(".kbc-codeview"), "Ctrl-w", 0)).toBe(false);
  });

  it("never withholds while a chord is already pending", () => {
    // A pending prefix (e.g. `g` already pressed) means the window host
    // is mid-chord and must see the continuation, even from inside the
    // buffer — the buffer's OWN chords (like `g d`) are handled by CM6
    // itself before the event ever reaches here in the first place; this
    // is about a chord the WINDOW host is already tracking.
    expect(shouldWithholdFromBuffer(elementIn(".kbc-codeview"), "d", 1)).toBe(false);
  });

  // V70-K1 — the root fix. The OLD predicate withheld every bare token
  // unconditionally once the buffer had genuine focus, which made every
  // bare `scope: global`/`dispatch: central` command unreachable the
  // instant a file was open (focus-follows-the-file lands DOM focus there
  // immediately). The two regressions this milestone found by accident
  // (`:`, `u`) both still resolve to a vim-owned row, so they stay withheld
  // — routed through the vim-layer arm each still needs — but a command
  // the vim layer has no opinion on must now reach the dispatcher.
  describe("V70-K1 — a bare global command the vim layer does not own reaches the dispatcher", () => {
    it("`:` stays withheld with genuine buffer focus — `goto.line` (vim-owned) shadows the palette here", () => {
      // Unlike the V70-H1 case above (no real buffer focus at all), this is
      // the buffer GENUINELY holding focus. `goto.line` (reader scope,
      // vim_kind `goto-line-open`) shadows `cmd.palette.commands` (global)
      // for the same key, exactly as `resolve()`'s own scope-precedence
      // rule and `goto.line`'s own note say: "Inside the buffer `:` opens
      // the go-to-line panel; outside it, `:` opens the command palette."
      expect(shouldWithholdFromBuffer(inBuffer, ":", 0)).toBe(true);
    });

    it("`u` stays withheld — nav.back's vim_kind forwarding arm (V70-A6) is still the live path", () => {
      expect(shouldWithholdFromBuffer(inBuffer, "u", 0)).toBe(true);
    });

    it("`?` stays withheld — help.keys' vim_kind forwarding is still the live path", () => {
      expect(shouldWithholdFromBuffer(inBuffer, "?", 0)).toBe(true);
    });

    it("a Space-leader command (no vim_kind anywhere in the `Space` prefix) reaches the dispatcher", () => {
      // Before this fix `Space` itself was withheld (bare, buffer, no
      // pending chord) — killing all 35 `Space`-leader rows (`nav.home`,
      // `view.theme-cycle`, `desk.preset.*`, `drawer.*`, `rail.*`, …)
      // structurally, the moment a file was open.
      expect(shouldWithholdFromBuffer(inBuffer, "Space", 0)).toBe(false);
    });

    it("un-prefixed global rows with no vim opinion reach the dispatcher (`.`, `U`, `f`, `F`, `s`, `S`)", () => {
      expect(shouldWithholdFromBuffer(inBuffer, ".", 0)).toBe(false); // action.panel
      expect(shouldWithholdFromBuffer(inBuffer, "U", 0)).toBe(false); // nav.forward
      expect(shouldWithholdFromBuffer(inBuffer, "f", 0)).toBe(false); // hint.jump
      expect(shouldWithholdFromBuffer(inBuffer, "F", 0)).toBe(false); // hint.act
      expect(shouldWithholdFromBuffer(inBuffer, "s", 0)).toBe(false); // scope.edit
      expect(shouldWithholdFromBuffer(inBuffer, "S", 0)).toBe(false); // scope.clear
    });

    it("a prefix shared between a vim-owned row and a non-vim global row reaches the dispatcher (`[`, `]`)", () => {
      // `[`/`]` continue into BOTH vim-owned reader rows (`[c`/`[f` —
      // commit.prev/workingset.prev) and non-vim global rows (`[d`/`]d` —
      // drawer.tab-prev/next). Withholding on "any continuation is vim's"
      // would make the global continuation permanently unreachable too —
      // the same bug this unit exists to fix, just one key later. Neither
      // `commit.prev`/`workingset.prev`/`commit.next`/`workingset.next` is
      // ever centrally registered (checked against every
      // `useCommandHandlers` call site in the SPA), so letting `[`/`]`
      // through is safe regardless of which way the sequence resolves.
      expect(shouldWithholdFromBuffer(inBuffer, "[", 0)).toBe(false);
      expect(shouldWithholdFromBuffer(inBuffer, "]", 0)).toBe(false);
    });

    it("pure vim motions and prefix-starters are unaffected — still exclusively the buffer's", () => {
      for (const token of ["j", "k", "h", "l", "w", "b", "e", "g", "z", "m", "'", "y"]) {
        expect(shouldWithholdFromBuffer(inBuffer, token, 0), token).toBe(true);
      }
    });
  });

  // V70-K1 — the recurrence guard. The failure mode this whole unit exists
  // to fix is SILENCE: a future bare-key row added to the registry with
  // `scope: global`/`dispatch: central` and no `vim_kind` looks perfectly
  // normal and simply never fires once a file is open. Two sweeps, over
  // every row actually IN PLAY while the buffer has focus (`scope: reader`
  // or `scope: global` — `inPlay()`'s own rule; a `tree`/`diff`/`board`/…
  // row's own bare `j`/`k` is that OTHER surface's business, never the
  // buffer's, so it is out of scope here) with no `when` (a context-gated
  // row's reachability is legitimately conditional — pinned individually
  // above and in `vimParity.test.ts` instead):
  //
  //  - a row with NO `vim_kind` must be able to reach the window host
  //    through AT LEAST ONE of its declared keys (a row may alias a
  //    shadowed bare key with an unshadowed one, e.g. `cmd.palette.commands`
  //    keeps `Space :` even though bare `:` is legitimately `goto.line`'s
  //    inside the buffer — this is the sweep's main catch, and the one that
  //    would have caught the `Space`-leader family dead on arrival);
  //  - a row WITH `vim_kind`, for each SINGLE-TOKEN key it is the actual
  //    `resolve()` winner for (skipping multi-token keys: a shared prefix
  //    like `[`/`]` deliberately is NOT exclusively vim's once ANY
  //    continuation is a non-vim global row, per this file's own
  //    `shouldWithholdFromBuffer` doc — that is not a regression to catch,
  //    it is the fix), must stay withheld — guarding against a future
  //    accidental double-fire with a central registration.
  describe("every unconditional reader/global bare-key row stays reachable and non-conflicting", () => {
    const inPlayForBuffer = KBC_COMMANDS.filter(
      (c) => !c.when && (c.scope === "reader" || c.scope === "global"),
    );

    it("a row with no vim_kind reaches the dispatcher via at least one of its keys", () => {
      const offenders: string[] = [];
      for (const c of inPlayForBuffer) {
        if (c.vimKind !== undefined) continue;
        if (c.keys.vim.length === 0) continue; // no keyboard binding to check
        const reachable = c.keys.vim.some((key) => {
          const first = tokensOf(key)[0];
          return !isBareToken(first) || !shouldWithholdFromBuffer(inBuffer, first, 0, {});
        });
        if (!reachable) {
          offenders.push(`${c.id} (${c.keys.vim.join(", ")}) — every key is withheld from the buffer`);
        }
      }
      expect(
        offenders,
        "a `vim_kind`-less row has no way to reach CommandRoot's dispatcher from inside the " +
          "buffer — either it needs a vim arm (add `vim_kind`), or an unshadowed alternate key, " +
          "or `shouldWithholdFromBuffer` regressed; see this file's V70-K1 doc comment",
      ).toEqual([]);
    });

    it("a vim_kind row stays withheld at every single-token key it actually wins", () => {
      const offenders: string[] = [];
      for (const c of inPlayForBuffer) {
        if (c.vimKind === undefined) continue;
        for (const key of c.keys.vim) {
          const tokens = tokensOf(key);
          if (tokens.length !== 1) continue; // multi-token: see doc above
          const [first] = tokens;
          if (!isBareToken(first)) continue; // never withheld regardless
          const winner = resolve(first, "reader", {}, "vim");
          if (winner?.id !== c.id) continue; // a different row shadows this key here
          if (!shouldWithholdFromBuffer(inBuffer, first, 0, {})) {
            offenders.push(`${c.id} (${key}) — vim_kind ${c.vimKind} but reachable centrally too`);
          }
        }
      }
      expect(
        offenders,
        "a row the vim layer exclusively owns at this key is ALSO reachable through " +
          "CommandRoot's central dispatch — a future double-fire if that id is ever centrally " +
          "registered; see this file's V70-K1 doc comment",
      ).toEqual([]);
    });
  });
});
