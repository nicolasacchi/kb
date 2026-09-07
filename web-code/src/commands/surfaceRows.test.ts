// V73-K6 — the surface-rows dead-row gate.
//
// `deadRows.test.ts` proves every shipped `dispatch: "central"` row has a
// `useCommandHandlers` registration SOMEWHERE — but it says so itself: "this
// asks the weaker, mechanical question … minus the 'in every reachable
// route' half," and it gates only `dispatch: "central"` rows (the ones
// `CommandRoot`'s own window listener fires). A `dispatch: "surface"` row is
// — by design (`web-code/CLAUDE.md`'s keyboard section, the two-layer rule)
// — executed by whichever route/component renders the surface, or by the
// CM6 vim layer for a row carrying `vimKind`. Nothing proved any of those
// 200+ rows actually had an executor at all, and `V74-L2` found the gap for
// real while shipping the boards feature: `board.row-next`/`board.row-prev`/
// `board.drill`/`board.pan` and the whole `lens.*` family had shipped with
// no `useCommandHandlers` registration and no `vimKind` — silently dead
// keys, closed for L2's OWN rows by `boards.test.ts` but left open for
// everyone else's. This file is the general form V74-L2's own header named
// as "a future unit's job" (the phrase `searchCommands.ts`'s header also
// used, for the same reason).
//
// A shipped `dispatch: "surface"` row is claimed by exactly one of:
//
//   (a) a `useCommandHandlers({ "<id>": … })` registration anywhere in
//       `web-code/src` — the exact mechanical check `deadRows.test.ts` runs
//       for central rows, unchanged here (e.g. `Tour.tsx`'s
//       `player.prev`/`player.next`, `BoardDetail.tsx`'s whole `boards.*`
//       family);
//   (b) a `vimKind` claim — `vimParity.test.ts` already proves that
//       vocabulary is real, bidirectionally complete, and reader/global
//       scoped only, so a row carrying one has a real executor (the CM6
//       layer) by construction;
//   (c) an `owner` field naming a registered surface (`OWNER_FILES` below)
//       whose combined source contains the literal marker `"<id>":`. Two
//       shapes satisfy it: a real handler-map entry (`searchCommands.ts`'s
//       `SearchHandlers` pattern — `browserCommands.ts`/`lensCommands.ts`
//       are its own copies, wired through `commands/dispatch.ts`'s shared
//       `resolve()`, the same resolver `CommandRoot` uses), or an inline
//       `// kbc-owns: "<id>":` comment beside the code that really executes
//       it, for a surface where routing through the shared resolver isn't
//       possible at all (`board.pan`'s bare `Alt` — `tokenOf` cannot even
//       represent that token, see `Canvas.tsx`'s own doc) or would mean
//       rewriting several independent, already-correct components for no
//       behaviour change (`overlayPanels`, `pickers` — see
//       `HierarchyPanel.tsx`'s and `Omnibox.tsx`'s own docs for why).
//
// A row with NO key bound in ANY preset (`help.which-key`/
// `view.keys-preset`/`learn.rehearsal`) is exempt outright: it documents a
// passive behaviour or an out-of-band UI choice (a `<select>`, a timer), not
// a keystroke, so "claimed by a handler" is not a question that applies.
//
// HOW TO USE THE ALLOW-LIST: exactly `deadRows.test.ts`'s ledger discipline.
// It is a debt list, not a config. Shrinking it — by registering a real
// handler or naming a real, verified owner — is the fix; a row may only be
// ADDED with a reason, and doing so is a deliberate, reviewer-visible act.
// Pinned exactly, so a row that gets claimed but stays listed fails too.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { KBC_ACTIVE_COMMANDS, type KbcCommand } from "./registry.gen";

const SRC = fileURLToPath(new URL("..", import.meta.url));

/// Every `owner` value the registry may name, and the files (relative to
/// `web-code/src/`) whose source is searched for that owner's rows. A row is
/// claimed the moment ANY file in its owner's set contains the literal
/// `"<id>":` — see the header for the two shapes that satisfies.
const OWNER_FILES: Readonly<Record<string, readonly string[]>> = {
  browser: ["routes/Browser.tsx", "routes/browserCommands.ts"],
  canvas: ["routes/Canvas.tsx"],
  lens: ["hooks/useLensKeys.ts", "hooks/lensCommands.ts"],
  storyPlayer: ["components/story/StoryPlayer.tsx"],
  overlayPanels: [
    "components/peek/PeekPanel.tsx",
    "components/hierarchy/HierarchyPanel.tsx",
    "components/graph/EgoGraph.tsx",
    "components/graph/LayeredDag.tsx",
  ],
  pickers: [
    "components/Omnibox.tsx",
    "components/StructurePopup.tsx",
    "components/RecentLocations.tsx",
    "components/LineHistoryPopup.tsx",
    "components/RefTypeahead.tsx",
    "components/bookmarks/MnemonicPopup.tsx",
  ],
  search: ["routes/Search.tsx", "routes/searchCommands.ts"],
};

/// A row that binds no key in any preset documents a passive behaviour, not
/// a keystroke — "does it have an executor" is not a meaningful question.
function hasNoKey(c: KbcCommand): boolean {
  return c.keys.vim.length === 0 && c.keys.plain.length === 0 && c.keys.helix.length === 0;
}

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(p);
  }
  return out;
}

/// Every source file that registers central-bus handlers at all,
/// concatenated — mirrors `deadRows.test.ts`'s own `registrationText`
/// exactly (narrowing to `useCommandHandlers` callers first is what keeps
/// `"<id>":` from matching an unrelated object literal elsewhere).
function registrationText(): string {
  return walk(SRC)
    .map((p) => readFileSync(p, "utf-8"))
    .filter((body) => body.includes("useCommandHandlers"))
    .join("\n");
}

function ownerText(owner: string): string {
  const files = OWNER_FILES[owner];
  if (!files) return "";
  return files.map((rel) => readFileSync(join(SRC, rel), "utf-8")).join("\n");
}

/// A debt ledger, not a config — see the header. Empty as of V73-K6.
const KNOWN_UNCLAIMED: readonly string[] = [];

function unclaimedSurfaceRows(): string[] {
  const central = registrationText();
  return KBC_ACTIVE_COMMANDS.filter((c) => {
    if (c.dispatch !== "surface" || c.lifecycle !== "shipped") return false;
    if (hasNoKey(c)) return false;
    if (c.vimKind !== undefined) return false;
    if (central.includes(`"${c.id}":`)) return false;
    if (c.owner && ownerText(c.owner).includes(`"${c.id}":`)) return false;
    return true;
  })
    .map((c) => c.id)
    .sort();
}

describe("shipped surface rows have an owner", () => {
  it("no shipped `dispatch: surface` row is unclaimed — the ledger is EMPTY (V73-K6)", () => {
    // The ledger closed to zero this unit; a future row that ships with no
    // executor fails HERE, and vitest's own array diff names it — no
    // separate "which row" step required.
    expect(unclaimedSurfaceRows()).toEqual([...KNOWN_UNCLAIMED].sort());
  });

  it("every `owner` the registry names is a real, known surface", () => {
    const unknown = new Set<string>();
    for (const c of KBC_ACTIVE_COMMANDS) {
      if (c.owner && !OWNER_FILES[c.owner]) unknown.add(c.owner);
    }
    expect([...unknown], "unknown owner(s) — add them to OWNER_FILES or fix the typo").toEqual([]);
  });

  it("`owner` is a `dispatch: surface` concept only", () => {
    // `dispatch: "central"` rows are registered on `commands/CommandRoot.tsx`'s
    // own bus by construction — an `owner` field on one would be a second,
    // unchecked claim nobody reads.
    for (const c of KBC_ACTIVE_COMMANDS) {
      if (c.owner) expect(c.dispatch, `${c.id} names an owner but is dispatch: ${c.dispatch}`).toBe("surface");
    }
  });

  it("the ledger names only rows that really are in the registry and really are surface rows", () => {
    const ids = new Set(KBC_ACTIVE_COMMANDS.filter((c) => c.dispatch === "surface").map((c) => c.id));
    expect(KNOWN_UNCLAIMED.filter((id) => !ids.has(id))).toEqual([]);
  });
});
