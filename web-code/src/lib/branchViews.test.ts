import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  BRANCH_DENSITIES,
  BRANCH_VIEWS,
  BRANCH_VIEW_HINTS,
  BRANCH_VIEW_LABELS,
  branchesSearch,
  cycleBranchView,
  DEFAULT_BRANCH_VIEW,
  parseBranchDensity,
  parseBranchesSearch,
  parseBranchView,
} from "./branchViews";

// V75-M3 — the view vocabulary is a MIRROR of `facts::View`, and this file
// is the lock-step gate, in the shape `kbcq.golden.test.ts` already uses
// for the grammar: read the SERVER'S OWN SOURCE and compare.
//
// There is no separate JSON golden here on purpose. `facts::View::ALL` is
// itself pinned Rust-side by
// `facts::tests::every_view_has_a_stable_name_and_the_list_is_closed`, so a
// second hand-written JSON would be a third copy to keep in step — one more
// place for the three to disagree. Scanning the Rust means a ninth view
// fails HERE the moment it lands, naming itself.

const FACTS_RS = fileURLToPath(
  new URL("../../../crates/kb-code-server/src/history/facts.rs", import.meta.url),
);

/// `pub const ALL: &'static [View] = &[ View::Current, … ];` → the eight
/// snake-case names, in declaration order. Deliberately reads the `ALL`
/// TABLE rather than the enum body: `ALL` is what the route iterates, so it
/// is the list whose ORDER is observable.
function serverViews(): string[] {
  const src = readFileSync(FACTS_RS, "utf8");
  const m = src.match(/pub const ALL: &'static \[View\] = &\[([\s\S]*?)\];/);
  if (!m) throw new Error("facts.rs no longer declares `View::ALL` in the expected shape");
  return [...m[1].matchAll(/View::(\w+)/g)].map((x) =>
    // `ForkPoint` has no ALL entry, but a future multi-word view would —
    // lower-camel → the kebab/lower form `View::as_str` emits.
    x[1].replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase(),
  );
}

describe("the branch-view vocabulary mirrors the server's", () => {
  it("is `facts::View::ALL`, name for name and in the same ORDER", () => {
    expect([...BRANCH_VIEWS]).toEqual(serverViews());
  });

  it("every view has a label and a hint — no view renders as its own id", () => {
    for (const v of BRANCH_VIEWS) {
      expect(BRANCH_VIEW_LABELS[v], v).toBeTruthy();
      expect(BRANCH_VIEW_HINTS[v], v).toBeTruthy();
    }
    expect(Object.keys(BRANCH_VIEW_LABELS).sort()).toEqual([...BRANCH_VIEWS].sort());
    expect(Object.keys(BRANCH_VIEW_HINTS).sort()).toEqual([...BRANCH_VIEWS].sort());
  });

  it("`all` is the default on BOTH sides", () => {
    expect(DEFAULT_BRANCH_VIEW).toBe("all");
    const src = readFileSync(FACTS_RS, "utf8");
    // `#[default]` sits on the line ABOVE the variant in the enum.
    expect(src).toMatch(/#\[default\]\s*\n\s*All,/);
  });
});

describe("parseBranchView is TOTAL", () => {
  it("round-trips every known view", () => {
    for (const v of BRANCH_VIEWS) expect(parseBranchView(v)).toBe(v);
  });

  it("reads anything else as the default — a stale bookmark must not blank the page", () => {
    for (const junk of ["", "recent", "MERGED", "../../etc", null, undefined]) {
      expect(parseBranchView(junk)).toBe(DEFAULT_BRANCH_VIEW);
    }
  });
});

describe("cycleBranchView", () => {
  it("wraps in both directions and visits every view exactly once", () => {
    const seen: string[] = [];
    let v = BRANCH_VIEWS[0];
    for (let i = 0; i < BRANCH_VIEWS.length; i += 1) {
      seen.push(v);
      v = cycleBranchView(v, 1);
    }
    expect(seen).toEqual([...BRANCH_VIEWS]);
    expect(v).toBe(BRANCH_VIEWS[0]);
    expect(cycleBranchView(BRANCH_VIEWS[0], -1)).toBe(BRANCH_VIEWS[BRANCH_VIEWS.length - 1]);
  });

  it("is total for an unrecognised current value", () => {
    // @ts-expect-error — deliberately out of vocabulary.
    expect(BRANCH_VIEWS).toContain(cycleBranchView("nonsense", 1));
  });
});

describe("the URL grammar is omit-at-default and round-trips", () => {
  it("emits nothing at all for the default selection", () => {
    expect(branchesSearch({})).toBe("");
    expect(branchesSearch({ view: "all", q: "", prefix: "", fav: false, density: "comfortable" })).toBe("");
  });

  it("emits each atom once it leaves its default", () => {
    expect(branchesSearch({ view: "agent" })).toBe("?view=agent");
    expect(branchesSearch({ fav: true })).toBe("?fav=1");
    expect(branchesSearch({ density: "compact" })).toBe("?density=compact");
    expect(branchesSearch({ view: "stale", q: "agent:exact", prefix: "feature/" })).toBe(
      "?view=stale&q=agent%3Aexact&prefix=feature%2F",
    );
  });

  it("round-trips through the parser", () => {
    const state = {
      view: "merged" as const,
      q: "branch:auth by:ada",
      prefix: "agent/",
      fav: true,
      radar: "main",
      density: "compact" as const,
    };
    const parsed = parseBranchesSearch(branchesSearch(state));
    expect(parsed).toEqual(state);
  });

  it("the parser is total on garbage", () => {
    const parsed = parseBranchesSearch("?view=nope&fav=maybe&density=cosy");
    expect(parsed.view).toBe("all");
    expect(parsed.fav).toBe(false);
    expect(parsed.density).toBe("comfortable");
    expect(parsed.radar).toBeNull();
  });
});

describe("density", () => {
  it("is a two-value vocabulary defaulting to comfortable", () => {
    expect([...BRANCH_DENSITIES]).toEqual(["comfortable", "compact"]);
    expect(parseBranchDensity(null)).toBe("comfortable");
    expect(parseBranchDensity("compact")).toBe("compact");
    expect(parseBranchDensity("COMPACT")).toBe("comfortable");
  });
});
