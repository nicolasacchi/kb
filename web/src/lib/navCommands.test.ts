import { describe, expect, it, vi } from "vitest";
import { navCommands } from "./navCommands";
import { NAV_ITEMS, resolveNavItems, type NavItem } from "./navItems";

const G = (() => null) as unknown as NavItem["Glyph"];
const item = (id: string, extra: Partial<NavItem> = {}): NavItem => ({
  id: id as NavItem["id"],
  label: id,
  Glyph: G,
  href: ({ kb }) => `/${id}${kb ? `?kb=${kb}` : ""}`,
  isActive: () => false,
  ...extra,
});
const S = { terminalAdmitted: true, blockedTerms: 0 };

describe("navCommands", () => {
  it("emits a go-nav-<id> row only for resolved items with cmdk", () => {
    const items = resolveNavItems(
      [
        item("a", { cmdk: { label: "Go A", hint: "/a" } }),
        item("b"),
        item("c", { cmdk: { label: "Go C", hint: "/c" }, visible: false }),
      ],
      S,
    );
    const cmds = navCommands(items);
    expect(cmds.map((c) => c.id)).toEqual(["go-nav-a"]);
    const navigate = vi.fn();
    const onClose = vi.fn();
    cmds[0].run({ navigate, onClose, kb: "k" });
    expect(navigate).toHaveBeenCalledWith("/a?kb=k");
    expect(onClose).toHaveBeenCalled();
  });

  it("Terminals has a Cmd+K row only for an admitted caller", () => {
    const rows = (terminalAdmitted: boolean) =>
      navCommands(resolveNavItems(NAV_ITEMS, { terminalAdmitted, blockedTerms: 0 })).map((c) => c.id);
    expect(rows(true)).toContain("go-nav-terminals");
    expect(rows(false)).not.toContain("go-nav-terminals");
    const navigate = vi.fn();
    const cmd = navCommands(resolveNavItems(NAV_ITEMS, { terminalAdmitted: true, blockedTerms: 0 })).find(
      (c) => c.id === "go-nav-terminals",
    )!;
    cmd.run({ navigate, onClose: vi.fn(), kb: "k" });
    expect(navigate).toHaveBeenCalledWith("/terminals");
  });
});
