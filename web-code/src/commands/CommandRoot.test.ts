import { describe, expect, it } from "vitest";
import { isBareToken, resolve, step, tokensOf, type ChordState } from "./dispatch";
import { KBC_COMMANDS, type KbcScope } from "./registry.gen";
import { chordConsumesKey, shouldWithholdFromBuffer } from "./CommandRoot";

/// A minimal `closest()`-only stand-in for the guard's focus target —
/// `inReadOnlyBuffer` (which `shouldWithholdFromBuffer` wraps) only ever
/// calls `.closest(selector)`.
///
/// V71-K2 — that target is the KEYDOWN's own `e.target`, not live
/// `document.activeElement`: the vim layer sits at `Prec.highest` INSIDE
/// the buffer, so it runs first and its callbacks move focus (the peek
/// panel focuses itself; `nav.back` navigates and drops focus to `<body>`),
/// which means "who has focus right now" is already the wrong question by
/// the time the event reaches `CommandRoot`'s window listener. Nothing in
/// the pure cases below changes — the predicate takes whatever it is handed
/// — because the distinction is a CALL-SITE one, pinned end to end by
/// `resolve.spec.ts:60` (`K` must not ALSO fire the central `peek.hover`)
/// and `nav-ramp.spec.ts:108` (`u` must not ALSO fire the central
/// `nav.back`).
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

// ── V71-K4 — the chord shield ─────────────────────────────────────────────
//
// Guard 2 above rules on a chord's FIRST token only. `chordConsumesKey` is
// the dual, and the question the CM6 vim keymap (`editor/vimReader.ts`, at
// `Prec.highest`, so it runs BEFORE CommandRoot's window listener) asks
// before it interprets any later token: "will the in-flight chord consume
// this keystroke?". Without it a bare letter that is both some chord's
// final token and a standalone vim key fired BOTH layers — V71-K3 measured
// `Space u`/`Space R u` also running vim's `u` (`nav.back`, which unmounted
// the reader) and `Space R a` also running `annotate.line`.

const CHORD_IDLE: ChordState = { pending: [], count: "" };

/// Does the CM6 vim reducer claim this bare token on its own? `vim_kind` is
/// that layer's proven vocabulary (`vimParity.test.ts`), and it is read
/// straight off the registry so a `when` the resolver cannot satisfy never
/// hides a real collision.
function vimClaimsBareKey(token: string): boolean {
  return KBC_COMMANDS.some((c) => c.vimKind !== undefined && c.keys.vim.includes(token));
}

const OWNS_EVERYTHING = () => true;
const OWNS_NOTHING = () => false;

/// Walk a whole sequence through the shield the way the two layers do,
/// one keystroke at a time: for each token, what the vim layer is told
/// ("consumed"), and what `step` does with it centrally.
function drive(
  keys: string,
  hasHandler: (id: string) => boolean = OWNS_EVERYTHING,
  scope: KbcScope = "reader",
): Array<{ token: string; consumed: boolean; kind: string; id?: string }> {
  let chord = CHORD_IDLE;
  const out: Array<{ token: string; consumed: boolean; kind: string; id?: string }> = [];
  for (const token of tokensOf(keys)) {
    const consumed = chordConsumesKey(chord, token, scope, {}, "vim", hasHandler);
    const res = step(chord, token, scope, {}, "vim");
    chord = res.state;
    out.push({
      token,
      consumed,
      kind: res.kind,
      id: res.kind === "matched" ? res.command.id : undefined,
    });
  }
  return out;
}

describe("chordConsumesKey (V71-K4)", () => {
  it("shields the FINAL token of every chord K3 wired whose last key is also a vim key", () => {
    // The six the brief names, each through its own real chord. `consumed`
    // on the last token is the vim layer standing down; `matched` with the
    // right id is the command still firing, exactly once, centrally.
    const cases: Array<[string, string]> = [
      ["Space u", "drawer.reopen"], // vim `u` = nav.back (unmounted the reader)
      ["Space R a", "rail.tab.all"], // vim `a` = annotate.line
      ["Space R h", "rail.tab.history"], // vim `h` = move.left
      ["Space R v", "rail.tab.review"], // vim `v` = visual mode
      ["Space R n", "rail.tab.notes"], // vim `n` = find.next
      ["Space p", "rail.pin"], // vim `p` = pane.pin
    ];
    for (const [keys, id] of cases) {
      const steps = drive(keys);
      const last = steps[steps.length - 1];
      expect(steps[0].consumed, `${keys}: the FIRST token is guard 2's, never this shield's`).toBe(
        false,
      );
      expect(last.consumed, `${keys}: the vim layer must stand down on the final token`).toBe(true);
      expect(last.kind, `${keys}: still matched centrally`).toBe("matched");
      expect(last.id, `${keys}: and it is the right command`).toBe(id);
    }
  });

  it("shields a MIDDLE token too, so vim never enters its own prefix inside a chord", () => {
    // `Space g h` = nav.home. Left to itself the vim reducer takes `g` as
    // ITS prefix and then fires `g h` (cb-line-history) beside the central
    // navigation — the same class of double-fire, one key earlier.
    const steps = drive("Space g h");
    expect(steps.map((s) => s.consumed)).toEqual([false, true, true]);
    expect(steps[1].kind).toBe("pending");
    expect(steps[2]).toMatchObject({ kind: "matched", id: "nav.home" });
  });

  it("shields the drawer's ordinal digits, which vim would otherwise accumulate as a count", () => {
    const steps = drive("Space 3");
    expect(steps[1]).toMatchObject({ consumed: true, kind: "matched", id: "drawer.tab" });
  });

  it("does NOT shield a continuation the vim layer alone owns — `[c`, `[f`, `Ctrl-w v`", () => {
    // The clause that makes this the general fix rather than a trade: these
    // rows resolve centrally but are `dispatch: "surface"` with no central
    // registration, so CommandRoot's own "nothing owns it here" branch
    // leaves the key alone and the vim arm is the ONLY executor. Shielding
    // them would have killed four shipped vim chords (V70-K1's own "one key
    // later" refusal).
    for (const keys of ["[ c", "] c", "[ f", "] f", "Ctrl-w v", "Ctrl-w q", "Ctrl-w h"]) {
      const steps = drive(keys, OWNS_NOTHING);
      expect(
        steps[steps.length - 1].consumed,
        `${keys}: nothing is centrally registered, so the vim layer must keep the key`,
      ).toBe(false);
    }
  });

  it("shields `[d`/`]d`/`[u`/`]u` — same prefix, central rows, no vim arm", () => {
    for (const [keys, id] of [
      ["[ d", "drawer.tab-prev"],
      ["] d", "drawer.tab-next"],
      ["[ u", "usages.prev"],
      ["] u", "usages.next"],
    ] as Array<[string, string]>) {
      const steps = drive(keys);
      expect(steps[1], keys).toMatchObject({ consumed: true, kind: "matched", id });
    }
  });

  it("makes a row BOTH layers own fire once in its chord form — `Space ?`", () => {
    // `help.keys` is a `global` + `dispatch: central` row that ALSO carries
    // `vim_kind` (V70-K1's own example). Its bare `?` form stays the vim
    // layer's, withheld by guard 2; its `Space ?` form is the chord's, and
    // the shield is what stops the CM6 reducer firing `cb-show-help` beside
    // the central handler.
    const steps = drive("Space ?");
    expect(steps[1]).toMatchObject({ consumed: true, kind: "matched", id: "help.keys" });
    expect(shouldWithholdFromBuffer(inBuffer, "?", 0)).toBe(true);
  });

  it("consumes the Escape that cancels a chord — one keystroke, one job", () => {
    expect(chordConsumesKey({ pending: ["Space"], count: "" }, "Escape", "reader", {}, "vim", OWNS_EVERYTHING)).toBe(
      true,
    );
    // …but an Escape with nothing pending is the dismiss stack's, untouched.
    expect(chordConsumesKey(CHORD_IDLE, "Escape", "reader", {}, "vim", OWNS_EVERYTHING)).toBe(false);
  });

  it("never consumes when a matched row would not actually run", () => {
    // CommandRoot's own two escape hatches, mirrored exactly: a `planned`
    // row has no executor, and an unregistered row hits the "nothing owns
    // it here — leave the key alone" branch. Either way the keystroke is
    // NOT consumed, so the vim layer must keep it.
    expect(chordConsumesKey({ pending: ["Space"], count: "" }, "u", "reader", {}, "vim", OWNS_NOTHING)).toBe(
      false,
    );
    const planned = KBC_COMMANDS.find((c) => c.lifecycle !== "shipped" && tokensOf(c.keys.vim[0] ?? "").length > 1);
    if (planned) {
      const toks = tokensOf(planned.keys.vim[0]);
      expect(
        chordConsumesKey(
          { pending: toks.slice(0, -1), count: "" },
          toks[toks.length - 1],
          planned.scope,
          {},
          "vim",
          OWNS_EVERYTHING,
        ),
        `${planned.id} is ${planned.lifecycle}, so nothing runs and the key is not consumed`,
      ).toBe(false);
    }
  });

  it("is inert with no chord pending — every vim arm behaves exactly as before", () => {
    // (b) of the brief: the shield's whole surface is "a chord is in
    // flight". With none, `u` is still nav.back's vim arm and `a` is still
    // annotate.line's, both withheld from central by guard 2, both fired by
    // the CM6 reducer alone.
    for (const token of ["u", "a", "h", "v", "n", "p", "j", "g", "["]) {
      expect(chordConsumesKey(CHORD_IDLE, token, "reader", {}, "vim", OWNS_EVERYTHING), token).toBe(
        false,
      );
    }
    expect(resolve("u", "reader", {}, "vim")).toMatchObject({ id: "nav.back", vimKind: "cb-nav-back" });
    expect(shouldWithholdFromBuffer(inBuffer, "u", 0)).toBe(true);
    // `a` is pinned off the registry row rather than through `resolve`,
    // because `resolve` cannot reach it: `annotate.line`'s `when` is
    // `"buffer || pane == 1"` and `parseWhen`'s grammar has no `||` (by
    // design — see its doc), so the whole string parses as ONE atom keyed
    // `"buffer || pane"`, a context key nothing publishes. The row is
    // therefore unavailable to `resolve`/`continuations` under any ctx.
    // Harmless as it stands — `a` is `dispatch: "surface"` and its vim arm
    // fires from the CM6 reducer unconditionally, and central resolving
    // NOTHING for bare `a` is precisely why it never double-fired on its
    // own — but it is why guard 2 does not withhold `a`, and why the
    // recurrence sweep below asks the registry rather than the resolver
    // which tokens the vim layer claims. Reported, not fixed, by V71-K4.
    expect(KBC_COMMANDS.find((c) => c.id === "annotate.line")).toMatchObject({
      vimKind: "cb-annotate",
      keys: { vim: ["a"] },
    });
    expect(resolve("a", "reader", {}, "vim")).toBeNull();
    expect(shouldWithholdFromBuffer(inBuffer, "a", 0)).toBe(false);
  });

  it("a leading COUNT is not a pending chord — `5j` still reaches vim", () => {
    // `step` keeps a leading count in `state.count` with `pending` EMPTY,
    // which is exactly why the shield keys on `pending.length`.
    const after5 = step(CHORD_IDLE, "5", "reader", {}, "vim");
    expect(after5.state).toMatchObject({ pending: [], count: "5" });
    expect(chordConsumesKey(after5.state, "j", "reader", {}, "vim", OWNS_EVERYTHING)).toBe(false);
  });

  describe("recurrence guard — the CLASS, not the six keys", () => {
    it("every central multi-token row ending in a vim-owned key is shielded", () => {
      // Sweeps the registry so a row added later with a colliding final
      // token is covered without anyone remembering this file. `hasHandler`
      // is `() => true` — "assume the row is registered" — because whether
      // it really is, is `deadRows.test.ts`'s job (V71-K2/K3); the two
      // together are what make the shield airtight.
      const offenders: string[] = [];
      for (const c of KBC_COMMANDS) {
        if (c.lifecycle !== "shipped") continue;
        if (c.scope !== "reader" && c.scope !== "global") continue;
        if (c.vimKind !== undefined) continue; // the vim layer's own row
        for (const key of c.keys.vim) {
          const toks = tokensOf(key);
          if (toks.length < 2) continue;
          const last = toks[toks.length - 1];
          if (!isBareToken(last)) continue;
          // Does the vim layer independently claim this final token? Asked
          // of the REGISTRY, not of `resolve`: `vim_kind` is the proven
          // vocabulary of the CM6 reducer (`vimParity.test.ts`, both
          // directions) and the reducer reacts to those keys whatever any
          // `when` says — `annotate.line` (`a`) is exactly the row that
          // slips through a `resolve`-based check, and it is one of the two
          // destructive collisions K3 measured.
          if (!vimClaimsBareKey(last)) continue;
          const consumed = chordConsumesKey(
            { pending: toks.slice(0, -1), count: "" },
            last,
            c.scope,
            {},
            "vim",
            OWNS_EVERYTHING,
          );
          if (!consumed) offenders.push(`${c.id} (${key}) — vim also owns bare \`${last}\``);
        }
      }
      expect(
        offenders,
        "a central chord's final token is ALSO a standalone vim key and the shield does not " +
          "consume it — pressing that chord inside the buffer fires both layers; see this " +
          "file's V71-K4 doc comment",
      ).toEqual([]);
    });
  });
});
