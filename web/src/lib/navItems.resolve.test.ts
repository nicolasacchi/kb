import { describe, expect, it } from "vitest";
import {
  NAV_ITEMS,
  matchActiveNavItem,
  resolveNavItems,
  type NavItem,
  type NavState,
} from "./navItems";

const S: NavState = { terminalAdmitted: true, blockedTerms: 4 };
const G = (() => null) as unknown as NavItem["Glyph"];

function fx(id: string, extra: Partial<NavItem> = {}): NavItem {
  return {
    id: id as NavItem["id"],
    label: id,
    Glyph: G,
    href: () => `/${id}`,
    isActive: (p) => p === `/${id}`,
    ...extra,
  };
}

describe("resolveNavItems", () => {
  it("drops items whose visible is false or a false predicate; absent = visible", () => {
    const out = resolveNavItems(
      [
        fx("a"),
        fx("b", { visible: false }),
        fx("c", { visible: (s) => s.terminalAdmitted }),
        fx("d", { visible: () => false }),
      ],
      S,
    );
    expect(out.map((i) => i.id)).toEqual(["a", "c"]);
    expect(
      resolveNavItems([fx("c", { visible: (s) => s.terminalAdmitted })], {
        ...S,
        terminalAdmitted: false,
      }),
    ).toEqual([]);
  });

  it("normalises badges to integers >= 0", () => {
    const badges = [undefined, 0, NaN, -1, 2.7, Infinity, (s: NavState) => s.blockedTerms].map(
      (b) => resolveNavItems([fx("x", { badge: b })], S)[0].badge,
    );
    expect(badges).toEqual([0, 0, 0, 0, 2, 0, 4]);
  });

  it("preserves order and does not mutate the input", () => {
    const input = [fx("a", { badge: 1 }), fx("b"), fx("c", { badge: 2 })];
    const snap = JSON.stringify(input.map((i) => [i.id, i.badge]));
    const out = resolveNavItems(input, S);
    expect(out.map((i) => i.id)).toEqual(["a", "b", "c"]);
    expect(JSON.stringify(input.map((i) => [i.id, i.badge]))).toBe(snap);
    expect("badge" in input[1]).toBe(false);
  });

  it("leaves the real doors unchanged: five, no badges", () => {
    const out = resolveNavItems(NAV_ITEMS, S);
    expect(out.map((i) => i.id)).toEqual(NAV_ITEMS.map((i) => i.id));
    expect(out).toHaveLength(5);
    expect(out.every((i) => i.badge === 0)).toBe(true);
  });

  it("matchActiveNavItem never returns a hidden item", () => {
    const shown = resolveNavItems(
      [fx("a"), fx("b", { visible: false })],
      S,
    );
    expect(matchActiveNavItem("/b", shown)).toBeNull();
    expect(matchActiveNavItem("/a", shown)?.id).toBe("a");
    expect(matchActiveNavItem("/sessions")?.id).toBe("worklog");
  });
});
