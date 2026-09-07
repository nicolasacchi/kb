// The kbc-canvas/1 VOCABULARY lock-step — the SPA half (V74-L2).
//
// `lib/boards.ts`'s six closed sets are MIRRORS of the daemon's own consts, and
// neither side generates the other. What keeps them from drifting is this file:
// it reads `crates/kb-code-server/src/boards/{mod,resolve,lint}.rs` and asserts
// the arrays match, in order, naming the value that moved. Same discipline
// `kbcq.golden.test.ts` and `kbcRefs.golden.test.ts` apply to a grammar,
// applied here to a vocabulary — a `pub const` array is a fixture as much as a
// JSON file is.
//
// Reading across the crate boundary is fine in a TEST (vitest runs from the
// repo, exactly as `commands/registry.gen.test.ts` does); the BUNDLE may never
// do it, and `lib/boards.ts` itself imports nothing outside `src/`.
//
// Why not a golden JSON fixture instead: one would need a Rust-side writer that
// has to be RUN to refresh, and a vocabulary is exactly the case where reading
// the declaration is both simpler and stricter — the source IS the fixture, so
// there is no third artifact to fall out of date.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  BOARD_EDGE_KINDS,
  BOARD_EDGE_PROVENANCE,
  BOARD_NODE_KINDS,
  BOARD_NODE_REASONS,
  BOARD_NODE_STATES,
  BOARD_STATUSES,
  boardReasonLabel,
  boardStateLabel,
} from "./boards";
import { COORDINATE_KEYS, MAX_ID_LEN } from "./boardDoc";

const MOD_RS = fileURLToPath(
  new URL("../../../crates/kb-code-server/src/boards/mod.rs", import.meta.url),
);
const RESOLVE_RS = fileURLToPath(
  new URL("../../../crates/kb-code-server/src/boards/resolve.rs", import.meta.url),
);
const LINT_RS = fileURLToPath(
  new URL("../../../crates/kb-code-server/src/boards/lint.rs", import.meta.url),
);

/// The string literals of a `pub const NAME: [&str; N] = [ … ];` declaration,
/// in declaration order. Elements are either bare literals or the `KIND_*`
/// consts declared above them; the latter are resolved through their own
/// `pub const NAME: &str = "…";`.
function constArray(source: string, name: string): string[] {
  const at = source.indexOf(`pub const ${name}:`);
  expect(at, `${name} must be declared`).toBeGreaterThanOrEqual(0);
  const open = source.indexOf("[", source.indexOf("=", at));
  const close = source.indexOf("];", open);
  const body = source.slice(open + 1, close);
  const out: string[] = [];
  for (const raw of body.split(",")) {
    const t = raw.trim();
    if (!t) continue;
    const quoted = t.match(/^"([^"]*)"$/);
    if (quoted) {
      out.push(quoted[1]);
      continue;
    }
    const alias = source.match(new RegExp(`pub const ${t}: &str = "([^"]*)";`));
    expect(alias, `${t} must resolve to a string const`).not.toBeNull();
    out.push((alias as RegExpMatchArray)[1]);
  }
  return out;
}

describe("kbc-canvas/1 vocabularies stay in lock-step with the daemon", () => {
  const mod = readFileSync(MOD_RS, "utf-8");
  const resolve = readFileSync(RESOLVE_RS, "utf-8");
  const lint = readFileSync(LINT_RS, "utf-8");

  it("node kinds match, in the daemon's own order", () => {
    expect([...BOARD_NODE_KINDS]).toEqual(constArray(mod, "NODE_KINDS"));
  });

  it("edge kinds match", () => {
    expect([...BOARD_EDGE_KINDS]).toEqual(constArray(mod, "EDGE_KINDS"));
  });

  it("statuses match", () => {
    expect([...BOARD_STATUSES]).toEqual(constArray(mod, "STATUSES"));
  });

  it("node states match", () => {
    expect([...BOARD_NODE_STATES]).toEqual(constArray(resolve, "NODE_STATES"));
  });

  it("node reasons match", () => {
    expect([...BOARD_NODE_REASONS]).toEqual(constArray(resolve, "NODE_REASONS"));
  });

  it("the coordinate refusal list matches the lint's", () => {
    expect([...COORDINATE_KEYS]).toEqual(constArray(lint, "COORDINATE_KEYS"));
  });

  it("the id length cap matches", () => {
    const m = mod.match(/pub const MAX_ID_LEN: usize = (\d+);/);
    expect(m).not.toBeNull();
    expect(MAX_ID_LEN).toBe(Number((m as RegExpMatchArray)[1]));
  });

  it("the two provenance values match", () => {
    const authored = mod.match(/pub const PROVENANCE_AUTHORED: &str = "([^"]*)";/);
    const derived = mod.match(/pub const PROVENANCE_DERIVED: &str = "([^"]*)";/);
    expect([
      (authored as RegExpMatchArray)[1],
      (derived as RegExpMatchArray)[1],
    ]).toEqual([...BOARD_EDGE_PROVENANCE]);
  });

  it("every reason has a sentence of its own — none falls through", () => {
    for (const r of BOARD_NODE_REASONS) {
      expect(boardReasonLabel(r), r).not.toBe(r);
    }
  });

  it("an UNKNOWN state or reason renders verbatim rather than being folded", () => {
    // The honest degrade: a daemon that grows a sixth state must not have it
    // silently read as `inert` on screen.
    expect(boardStateLabel("quantum")).toBe("quantum");
    expect(boardReasonLabel("newly-invented")).toBe("newly-invented");
  });
});
