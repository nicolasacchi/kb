// V70-A5 — the dispatcher's goldens.
//
// Two kinds of test live here. The first is ordinary unit coverage of the
// resolver, the `when` grammar and the chord machine. The second is a
// GOLDEN per scope: the full "key → command id" table the surface resolves,
// snapshotted as a literal. That is the thing that catches a registry edit
// silently changing what an existing key does — the failure mode the old
// hand-maintained `KeyboardHelp` array had no way to notice.
import { describe, expect, it } from "vitest";
import {
  commandsForScope,
  continuations,
  displayKey,
  evalWhen,
  IDLE,
  isBareToken,
  keysFor,
  parseWhen,
  pendingLabel,
  resolve,
  scopeDepth,
  step,
  tokenOf,
  tokensOf,
  whenDisjoint,
  wildcardMatches,
} from "./dispatch";
import { KBC_COMMANDS, KBC_SCOPES, type KbcPreset, type KbcScope } from "./registry.gen";

describe("the when grammar", () => {
  it("parses the four atom shapes", () => {
    expect(parseWhen("help.open")).toEqual([{ key: "help.open", op: "truthy" }]);
    expect(parseWhen("!diff.menu")).toEqual([{ key: "diff.menu", op: "falsy" }]);
    expect(parseWhen("mode == normal")).toEqual([{ key: "mode", op: "eq", value: "normal" }]);
    expect(parseWhen("mode != normal")).toEqual([{ key: "mode", op: "ne", value: "normal" }]);
    expect(parseWhen("palette.open && !palette.popover")).toHaveLength(2);
    expect(parseWhen(undefined)).toEqual([]);
  });

  it("treats an unknown key as absent — an unpublished context fires nothing gated", () => {
    expect(evalWhen("help.open", {})).toBe(false);
    expect(evalWhen("!help.open", {})).toBe(true);
    expect(evalWhen(undefined, {})).toBe(true);
    expect(evalWhen("mode != normal", {})).toBe(true);
    expect(evalWhen("mode != normal", { mode: "normal" })).toBe(false);
  });

  it("proves disjointness only when one atom negates another", () => {
    expect(whenDisjoint("diff.menu", "!diff.menu")).toBe(true);
    expect(whenDisjoint("mode == normal", "mode == visual")).toBe(true);
    expect(whenDisjoint("board == canvas", "board == lens")).toBe(true);
    expect(whenDisjoint("mode != normal", "mode == normal")).toBe(true);
    // Conservative by design: "might overlap" must read as "collides", so an
    // unratified pair fails the gate rather than being waved through.
    expect(whenDisjoint("help.open", "palette.open")).toBe(false);
    expect(whenDisjoint(undefined, "help.open")).toBe(false);
  });
});

describe("key tokens", () => {
  it("splits sequences and matches wildcards", () => {
    expect(tokensOf("Ctrl-w v")).toEqual(["Ctrl-w", "v"]);
    expect(tokensOf("Space g h")).toEqual(["Space", "g", "h"]);
    expect(wildcardMatches("{a-z}", "q")).toBe(true);
    expect(wildcardMatches("{a-z}", "Q")).toBe(false);
    expect(wildcardMatches("{1-9}", "0")).toBe(false);
    expect(wildcardMatches("{0-9}", "0")).toBe(true);
    expect(wildcardMatches("g", "g")).toBe(false); // not a wildcard
  });

  it("canonicalises a KeyboardEvent, folding Shift into printable characters", () => {
    expect(tokenOf({ key: "k" })).toBe("k");
    expect(tokenOf({ key: "K", shiftKey: true })).toBe("K");
    expect(tokenOf({ key: "k", ctrlKey: true })).toBe("Ctrl-k");
    expect(tokenOf({ key: "k", metaKey: true })).toBe("Meta-k");
    expect(tokenOf({ key: " " })).toBe("Space");
    expect(tokenOf({ key: "Enter", shiftKey: true })).toBe("Shift-Enter");
    expect(tokenOf({ key: "Tab", shiftKey: true })).toBe("Shift-Tab");
    expect(tokenOf({ key: "ArrowDown" })).toBe("Down");
    expect(tokenOf({ key: "w", ctrlKey: true, shiftKey: true })).toBe("Ctrl-w");
  });

  it("knows a bare token from a modified one", () => {
    expect(isBareToken("j")).toBe(true);
    expect(isBareToken("Shift-Enter")).toBe(true); // shift alone is still "bare" to the buffer
    expect(isBareToken("Ctrl-k")).toBe(false);
    expect(isBareToken("Meta-k")).toBe(false);
  });
});

describe("resolve", () => {
  it("finds a scope-local binding, and a global one from any scope", () => {
    expect(resolve("g d", "reader")?.id).toBe("reader.goto-definition");
    expect(resolve("Meta-k", "reader")?.id).toBe("cmd.palette.search");
    expect(resolve("Meta-k", "branches")?.id).toBe("cmd.palette.search");
    expect(resolve("Space g h", "diff")?.id).toBe("nav.home");
    expect(resolve("g d", "diff")).toBeNull();
  });

  it("resolves wildcards and rejects a partial sequence", () => {
    expect(resolve("m q", "reader")?.id).toBe("mark.set");
    expect(resolve("' q", "reader")?.id).toBe("mark.jump");
    expect(resolve("Space 3", "reader")?.id).toBe("drawer.tab");
    expect(resolve("g", "reader")).toBeNull();
    expect(resolve("", "reader")).toBeNull();
  });

  it("honours the when predicate", () => {
    expect(resolve("a", "diff", { "diff.menu": true })?.id).toBe("diff.disposition.agree");
    expect(resolve("d", "diff", { "diff.menu": true })?.id).toBe("diff.disposition.dispute");
    expect(resolve("d", "diff", {})?.id).toBe("diff.disposition");
    expect(resolve("] t", "diff", {})).toBeNull();
    expect(resolve("] t", "diff", { "diff.tour": true })?.id).toBe("diff.tour-next");
  });

  it("picks the INNERMOST dismissal for Escape", () => {
    // The whole Esc class in one assertion: with several layers up, Escape
    // resolves to the lowest dismiss_order whose `when` holds.
    expect(resolve("Escape", "palette", { "palette.open": true })?.id).toBe("dismiss.palette");
    expect(
      resolve("Escape", "palette", { "palette.open": true, "palette.popover": true })?.id,
    ).toBe("dismiss.popover");
    expect(resolve("Escape", "diff", { "diff.menu": true, "help.open": true })?.id).toBe(
      "dismiss.help",
    );
    expect(resolve("Escape", "diff", { "diff.menu": true })?.id).toBe("dismiss.menu");
    expect(resolve("Escape", "reader", { mode: "visual" })?.id).toBe("mode.normal");
  });

  it("reads the requested preset column, never falling back to vim", () => {
    expect(resolve("F12", "reader", {}, "plain")?.id).toBe("reader.goto-definition");
    expect(resolve("g d", "reader", {}, "plain")).toBeNull();
    expect(resolve("g e", "reader", {}, "helix")?.id).toBe("move.doc-end");
    expect(resolve("g e", "reader", {}, "vim")).toBeNull();
    // A row with an empty plain column is palette-only there — not vim's key.
    const pan = KBC_COMMANDS.find((c) => c.id === "graph.ego")!;
    expect(keysFor(pan, "plain")).toEqual([]);
    expect(displayKey(pan, "plain")).toBeNull();
    expect(displayKey(pan, "vim")).toBe("g G");
  });
});

describe("continuations (which-key)", () => {
  it("lists what a pending prefix can still become, group-then-key", () => {
    const g = continuations(["g"], "reader");
    const ids = g.map((c) => c.command.id);
    expect(ids).toContain("reader.goto-definition");
    expect(ids).toContain("structure.open");
    expect(ids).not.toContain("move.left");
    const groups = g.map((c) => c.command.group);
    expect([...groups].sort((a, b) => a.localeCompare(b))).toEqual(groups);
  });

  it("covers the leader from every scope", () => {
    for (const s of KBC_SCOPES) {
      expect(continuations(["Space"], s.id).length, s.id).toBeGreaterThan(10);
    }
  });

  it("returns nothing for a sequence that is already complete", () => {
    expect(continuations(["g", "d"], "reader")).toHaveLength(0);
  });
});

describe("the chord machine", () => {
  const R: KbcScope = "reader";

  it("holds a prefix, then matches — and a prefix NEVER expires by itself", () => {
    const a = step(IDLE, "g", R);
    expect(a.kind).toBe("pending");
    expect(pendingLabel(a.state)).toBe("g");
    const b = step(a.state, "d", R);
    expect(b.kind).toBe("matched");
    expect(b.kind === "matched" && b.command.id).toBe("reader.goto-definition");
    expect(b.state).toEqual(IDLE);
  });

  it("cancels a pending chord on Escape WITHOUT firing the dismiss stack", () => {
    // One keystroke, one job: an Escape that collapses a chord must not also
    // close the panel underneath it.
    const a = step(IDLE, "g", R);
    const b = step(a.state, "Escape", R);
    expect(b.kind).toBe("cancelled");
    expect(b.state).toEqual(IDLE);
    // With nothing pending, the same key IS the dismissal.
    expect(step(IDLE, "Escape", R, { mode: "visual" }).kind).toBe("matched");
  });

  it("accumulates a count and hands it to the matched command", () => {
    let s = IDLE;
    for (const k of ["1", "2"]) s = step(s, k, R).state;
    expect(s.count).toBe("12");
    const g = step(s, "G", R);
    expect(g.kind).toBe("matched");
    expect(g.kind === "matched" && g.count).toBe(12);
    expect(g.kind === "matched" && g.command.id).toBe("move.doc-end");
  });

  it("lets a scope's bare-digit command win over the count accumulator", () => {
    // The review cockpit binds `1`…`5` to its tabs. A count accumulator that
    // swallowed them would make those rows unreachable while still looking,
    // from the registry, like they existed.
    const r = step(IDLE, "1", "review");
    expect(r.kind).toBe("matched");
    expect(r.kind === "matched" && r.command.id).toBe("review.tab.report");
    // …and the reader, which binds no bare digit, still counts.
    expect(step(IDLE, "1", R).kind).toBe("pending");
    expect(step(IDLE, "1", R).state.count).toBe("1");
  });

  it("recovers a NON-leading wildcard digit from the winning key template (V71-K3)", () => {
    // `Space 3` for `drawer.tab`: the digit is the sequence's SECOND
    // token, so the leading-count accumulator (which only starts on the
    // sequence's own first keystroke) never sees it — `step()` must
    // recover it from the `{1-9}` in `drawer.tab`'s own template instead,
    // or the central handler has no way to know which of nine tabs was
    // asked for.
    const a = step(IDLE, "Space", "global");
    expect(a.kind).toBe("pending");
    const b = step(a.state, "3", "global");
    expect(b.kind).toBe("matched");
    expect(b.kind === "matched" && b.command.id).toBe("drawer.tab");
    expect(b.kind === "matched" && b.count).toBe(3);
  });

  it("never invents a wildcard digit for a row whose OWN template binds it literally", () => {
    // The review cockpit's `1`…`5` tabs are literal keys, not `{1-9}`
    // rows — pinning that the recovery above is scoped to a template that
    // actually DECLARES a wildcard, never "the last token happened to be
    // a digit".
    const r = step(IDLE, "1", "review");
    expect(r.kind).toBe("matched");
    expect(r.kind === "matched" && r.count).toBe(null);
  });

  it("keeps bare 0 as line-start when no count is building", () => {
    const z = step(IDLE, "0", R);
    expect(z.kind).toBe("matched");
    expect(z.kind === "matched" && z.command.id).toBe("move.line-start");
    const withCount = step(step(IDLE, "2", R).state, "0", R);
    expect(withCount.kind).toBe("pending");
    expect(withCount.state.count).toBe("20");
  });

  it("fizzles a sequence nothing extends", () => {
    const a = step(IDLE, "g", R);
    const b = step(a.state, "§", R);
    expect(b.kind).toBe("none");
    expect(b.state).toEqual(IDLE);
  });

  it("walks the three-token leader chords", () => {
    let s = IDLE;
    let last = step(s, "Space", "diff");
    expect(last.kind).toBe("pending");
    s = last.state;
    last = step(s, "g", "diff");
    expect(last.kind).toBe("pending");
    s = last.state;
    last = step(s, "i", "diff");
    expect(last.kind).toBe("matched");
    expect(last.kind === "matched" && last.command.id).toBe("nav.inbox");
  });
});

describe("scope goldens", () => {
  // The full key→id table each scope resolves in the default (vim) preset
  // with an EMPTY context — i.e. what a surface offers before it publishes
  // any state. A registry edit that changes what an existing key does has to
  // change one of these literals, which is the entire point: the old
  // hand-maintained `KeyboardHelp` array could (and did) drift from the real
  // bindings with nothing to notice.
  function table(scope: KbcScope, preset: KbcPreset = "vim"): string[] {
    return commandsForScope(scope)
      .flatMap((c) => keysFor(c, preset).map((k) => `${k} → ${c.id}`))
      .sort();
  }

  // Every scope this block has a golden `it(...)` for — kept as its own list
  // (rather than derived from the `it` blocks themselves, which vitest gives
  // no clean introspection over) so the parity guard below has something to
  // check the registry against. V71-E2 landed the `search` scope without
  // adding it here, which is exactly the drift this list exists to catch
  // (V71-X2).
  const GOLDEN_SCOPES: readonly KbcScope[] = [
    "global",
    "tree",
    "reader",
    "diff",
    "review",
    "branches",
    "board",
    "search",
    "recipe",
    "rail",
    "drawer",
    "palette",
  ];

  it("global", () => {
    expect(table("global")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("tree", () => {
    expect(table("tree")).toEqual([
      ". → action.panel",
      "/ → tree.filter",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Alt-f → tree.filter.mode",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "Enter → tree.open",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-Enter → tree.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "T → tree.view.prev",
      "U → nav.forward",
      "[ a → tree.prev-annot",
      "[ c → tree.prev-change",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] a → tree.next-annot",
      "] c → tree.next-change",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "a → tree.actions",
      "b → desk.toggle.dock",
      "d → tree.decorations.cycle",
      "f → hint.jump",
      "j → tree.focus-next",
      "k → tree.focus-prev",
      "m → tree.select.toggle",
      "o → ramp.open-tab",
      "s → scope.edit",
      "t → tree.view.next",
      "u → nav.back",
    ]);
  });

  it("reader", () => {
    expect(table("reader")).toEqual([
      "# → find.word-prev",
      "$ → move.line-end",
      "' {a-z} → mark.jump",
      "* → find.word-next",
      ". → action.panel",
      "/ → find.in-file",
      "0 → move.line-start",
      ": → cmd.palette.commands",
      ": → goto.line",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-b → move.page-up",
      "Ctrl-d → move.half-down",
      "Ctrl-f → move.page-down",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-u → move.half-up",
      "Ctrl-w Ctrl-w → pane.focus-cycle",
      "Ctrl-w h → pane.focus-prev",
      "Ctrl-w l → pane.focus-next",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w q → pane.close",
      "Ctrl-w r → desk.resize",
      "Ctrl-w v → pane.split",
      "Escape → mode.normal",
      "F → hint.act",
      "G → move.doc-end",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "N → find.prev",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "V → select.visual-line",
      "Y → permalink.copy",
      "[ c → commit.prev",
      "[ d → drawer.tab-prev",
      "[ f → workingset.prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] c → commit.next",
      "] d → drawer.tab-next",
      "] f → workingset.next",
      "] m → comments.next",
      "] u → usages.next",
      "^ → move.first-non-blank",
      "b → move.word-prev",
      "e → move.word-end",
      "f → hint.jump",
      "g . → nav.recent",
      "g C → hierarchy.callees",
      "g G → graph.ego",
      "g M → bookmark.mnemonics",
      "g O → structure.open",
      "g c → hierarchy.callers",
      "g d → reader.goto-definition",
      "g g → move.doc-start",
      "g h → history.line",
      "g i → impact.open",
      "g m → bookmark.toggle",
      "g r → reader.find-references",
      "g t → hierarchy.types",
      "h → move.left",
      "j → move.down",
      "k → move.up",
      "l → move.right",
      "m {a-z} → mark.set",
      "n → find.next",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
      "v → select.visual",
      "w → move.word-next",
      "y y → yank.line",
      "y → yank.selection",
      "z b → scroll.bottom",
      "z t → scroll.top",
      "z z → scroll.center",
      "{ → move.para-prev",
      "{1-9} G → move.goto-line-count",
      "} → move.para-next",
    ]);
  });

  it("diff", () => {
    expect(table("diff")).toEqual([
      ". → action.panel",
      // V73-K2a — diff v2. `Space` family, `z` folds and `[ p`/`] p`.,
      ": → cmd.palette.commands",
      "? → help.keys",
      "C → diff.compose-old",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "G → diff.file-last",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      // V73-K2c — kbc-hunk-turns/1's on-demand join.
      "Space T → diff.hunk-turns",
      "Space V → diff.viewed-advance",
      // V73-K2a — diff v2. `Space` family, `z` folds and `[ p`/`] p`.
      "Space W → diff.publish",
      "Space X → diff.drafts-discard",
      "Space b a → boards.add",
      "Space c → diff.context-cycle",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      // V73-K2c — kbc-pseudo/1's four fixed chapter-zero rows.
      "Space f 1 → diff.pseudo.pr-body",
      "Space f 2 → diff.pseudo.review-md",
      "Space f 3 → diff.pseudo.findings",
      "Space f 4 → diff.pseudo.commits",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space h → diff.hunk-viewed",
      "Space l → rails.atom.open",
      "Space m → diff.map-toggle",
      "Space n → diff.noise-toggle",
      "Space p → rail.pin",
      "Space s → diff.split-toggle",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space v → diff.toggle-viewed",
      "Space w → diff.drafts",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "T → diff.thread-prev",
      "U → nav.forward",
      "Y → diff.permalink-copy",
      "[ d → drawer.tab-prev",
      "[ f → diff.file-prev",
      "[ m → comments.prev",
      "[ p → diff.ps-prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] f → diff.file-next",
      "] m → comments.next",
      "] p → diff.ps-next",
      "] u → usages.next",
      "c → diff.compose-new",
      "d → diff.disposition",
      "f → hint.jump",
      "g g → diff.file-first",
      "j → diff.hunk-next",
      "k → diff.hunk-prev",
      "o → diff.overlay-cycle",
      "o → ramp.open-tab",
      "s → scope.edit",
      "t → diff.thread-next",
      "u → nav.back",
      "x → diff.collapse",
      "z a → diff.fold-toggle",
      "z c → diff.fold",
      "z o → diff.unfold",
    ]);
  });

  it("review", () => {
    expect(table("review")).toEqual([
      ". → action.panel",
      "1 → review.tab.report",
      "2 → review.tab.files",
      "3 → review.tab.map",
      "4 → review.tab.order",
      "5 → review.tab.timeline",
      "6 → review.tab.doc",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "Enter → review.open-diff",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      // V73-K2c — the Timeline tab's live GitHub lane toggle.
      "Space G → review.timeline.github-toggle",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      // V73-K2c — the Timeline tab's lane-visibility cycle.
      "Space j → review.timeline.lane-cycle",
      "Space l → rails.atom.open",
      "Space o → doc.card-open",
      "Space p → rail.pin",
      // V73-K2c — the claim register's toggle (mounted on Document + Report).
      "Space q → review.claims-toggle",
      "Space r → doc.cards-fold",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space y → doc.compose-copy",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ r → doc.card-prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] r → doc.card-next",
      "] u → usages.next",
      "a → review.ask",
      "f → hint.jump",
      "j → review.next",
      "k → review.prev",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("branches", () => {
    expect(table("branches")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "Enter → branches.compare",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "j → branches.next",
      "k → branches.prev",
      "o → ramp.open-tab",
      "r → branches.start-review",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("board", () => {
    expect(table("board")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("search", () => {
    expect(table("search")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "Alt-[ → search.stack.prev",
      "Alt-] → search.stack.next",
      "Alt-f → search.facets",
      "Alt-g → search.group.cycle",
      "Alt-h → search.history",
      "Alt-k → search.keep",
      "Alt-p → search.preview",
      "Alt-r → search.refine",
      "Alt-s → search.save",
      "Alt-y → search.copy-cli",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "Down → search.row.next",
      "Enter → search.open",
      "Escape → search.refine.clear",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Shift-Tab → search.group.prev",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "Tab → search.group.next",
      "U → nav.forward",
      "Up → search.row.prev",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("recipe", () => {
    expect(table("recipe")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "Alt-Enter → recipe.run",
      "Alt-[ → recipe.view-prev",
      "Alt-] → recipe.view-next",
      "Alt-c → recipe.census-open",
      "Alt-m → recipe.materialise",
      "Alt-s → recipe.save-as-set",
      "Alt-t → recipe.trust",
      "Alt-y → recipe.copy-cli",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "Down → recipe.row-next",
      "Enter → recipe.open",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Shift-Tab → recipe.step-prev",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "Tab → recipe.step-next",
      "U → nav.forward",
      "Up → recipe.row-prev",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("rail", () => {
    expect(table("rail")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("drawer", () => {
    expect(table("drawer")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("palette", () => {
    expect(table("palette")).toEqual([
      ". → action.panel",
      ": → cmd.palette.commands",
      "? → help.keys",
      "ContextMenu → action.panel",
      "Ctrl-Enter → ramp.open-tab",
      "Ctrl-i → jump.forward",
      "Ctrl-k → cmd.palette.search",
      "Ctrl-o → jump.back",
      "Ctrl-w m → desk.zoom",
      "Ctrl-w r → desk.resize",
      "F → hint.act",
      "K → peek.hover",
      "Meta-k → cmd.palette.search",
      "O → ramp.open-window",
      "S → scope.clear",
      "Shift-Enter → ramp.open-pane2",
      "Shift-F10 → action.panel",
      "Space : → cmd.palette.commands",
      "Space ? → help.keys",
      "Space C c → comments.gutter-mode-cycle",
      "Space C o → comments.open-card",
      "Space C t → comments.track-as-annotation",
      "Space D → drawer.pin",
      "Space K → drawer.keep",
      "Space P e → desk.preset.explore",
      "Space P p → desk.preset.present",
      "Space P r → desk.preset.read",
      "Space P v → desk.preset.review",
      "Space R a → rail.tab.all",
      "Space R c → rail.tab.comments",
      "Space R d → rail.tab.dossier",
      "Space R h → rail.tab.history",
      "Space R n → rail.tab.notes",
      "Space R u → rail.tab.understand",
      "Space R v → rail.tab.review",
      "Space b a → boards.add",
      "Space d → desk.toggle.drawer",
      "Space e d → entity.dossier.open",
      "Space g / → nav.search-page",
      "Space g R → nav.rails",
      "Space g b → nav.branches",
      "Space g c → nav.canvas",
      "Space g e → nav.recipes",
      "Space g f → tree.reveal",
      "Space g h → nav.home",
      "Space g i → nav.inbox",
      "Space g k → nav.stacks",
      "Space g m → nav.comments",
      "Space g o → nav.hotspots",
      "Space g p → nav.prs",
      "Space g r → nav.reviews",
      "Space g s → nav.sets",
      "Space g t → nav.todos",
      "Space g w → nav.boards",
      "Space g x → nav.browser",
      "Space l → rails.atom.open",
      "Space p → rail.pin",
      "Space t → view.theme-cycle",
      "Space u → drawer.reopen",
      "Space x → drawer.close",
      "Space z → rails.schema-fold",
      "Space {1-9} → drawer.tab",
      "U → nav.forward",
      "[ d → drawer.tab-prev",
      "[ m → comments.prev",
      "[ u → usages.prev",
      "] d → drawer.tab-next",
      "] m → comments.next",
      "] u → usages.next",
      "f → hint.jump",
      "o → ramp.open-tab",
      "s → scope.edit",
      "u → nav.back",
    ]);
  });

  it("scope depths are what the conflict rule reads", () => {
    expect(scopeDepth("global")).toBe(0);
    expect(scopeDepth("tree")).toBe(10);
    expect(scopeDepth("reader")).toBe(20);
    expect(scopeDepth("diff")).toBe(20);
    expect(scopeDepth("palette")).toBe(50);
  });

  it("every scope can reach the palette, the help sheet and the leader", () => {
    // The one promise the 16 keyboard-inert routes could not make: wherever
    // you are, the same three doors are open.
    for (const s of KBC_SCOPES) {
      expect(resolve("Meta-k", s.id)?.id, s.id).toBe("cmd.palette.search");
      expect(resolve("?", s.id)?.id, s.id).toBe("help.keys");
      expect(resolve("Space d", s.id)?.id, s.id).toBe("desk.toggle.drawer");
    }
  });

  it("every registry scope has a golden table (V71-X2 recurrence guard)", () => {
    // A registry-defined scope with no `it(...)` above it is exactly what
    // let `search` (V71-E2) go 16 rows deep with nothing pinning its keys —
    // fail loudly, by name, the moment a scope is declared in
    // `registry.gen.ts` without a matching entry in `GOLDEN_SCOPES` (or vice
    // versa, a stale entry naming a scope the registry dropped).
    const registryScopes = KBC_SCOPES.map((s) => s.id);
    const missingGolden = registryScopes.filter((s) => !GOLDEN_SCOPES.includes(s));
    const staleGolden = GOLDEN_SCOPES.filter((s) => !registryScopes.includes(s));
    expect({ missingGolden, staleGolden }).toEqual({ missingGolden: [], staleGolden: [] });
  });
});
