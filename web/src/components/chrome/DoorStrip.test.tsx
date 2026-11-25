import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import DoorStrip from "./DoorStrip";
import {
  NAV_ITEMS,
  resolveNavItems,
  type NavItem,
} from "../../lib/navItems";

const S = { terminalAdmitted: true, blockedTerms: 0 };
const G = (() => null) as unknown as NavItem["Glyph"];
const extra = (extra: Partial<NavItem>): NavItem => ({
  id: "hub" as NavItem["id"],
  label: "Hub",
  Glyph: G,
  href: () => "/hub",
  isActive: () => false,
  ...extra,
});

function html(items: NavItem[], path = "/") {
  return renderToString(
    <MemoryRouter initialEntries={[path]}>
      <DoorStrip items={resolveNavItems(items, S)} pathname={path} kb={null} />
    </MemoryRouter>,
  );
}

describe("DoorStrip", () => {
  it("renders the badge at 3 and omits it at 0", () => {
    const on = html([extra({ badge: 3 })]);
    expect(on).toContain("kb-door__badge");
    expect(on).toContain('data-testid="door-badge-hub"');
    expect(on).toContain(">3<");
    expect(html([extra({ badge: 0 })])).not.toContain("kb-door__badge");
  });

  it("omits an invisible item", () => {
    const out = html([extra({ visible: false, label: "Hidden" }), ...NAV_ITEMS]);
    expect(out).not.toContain("Hidden");
    expect(out).toContain("Library");
  });

  it("keeps the five real doors' markup", () => {
    const out = html(NAV_ITEMS, "/sessions");
    for (const l of ["Library", "Search", "Memory", "Worklog", "Lists"]) {
      expect(out).toContain(`title="${l}"`);
    }
    expect(out).toContain('aria-current="page"');
    expect(out).toContain('data-testid="header-sessions-tab"');
    expect(out).not.toContain("kb-door__badge");
  });
});

describe("DoorStrip attention pill (HUBDOOR)", () => {
  it("hides the pill from AT and names the link with the count", () => {
    const on = html([extra({ badge: 3 })]);
    expect(on).toMatch(/<span class="kb-door__badge"[^>]*aria-hidden="true"/);
    expect(on).not.toContain(`aria-label="3`);
    expect(on).toContain('<span class="kb-door__sr">, 3 need attention</span>');
    expect(html([extra({ badge: 1 })])).toContain(", 1 needs attention");
    expect(html([extra({ badge: 0 })])).not.toContain("kb-door__sr");
  });

  it("Terminals: absent when not admitted, present with the count when admitted", () => {
    const strip = (s: { terminalAdmitted: boolean; blockedTerms: number }) =>
      renderToString(
        <MemoryRouter initialEntries={["/"]}>
          <DoorStrip items={resolveNavItems(NAV_ITEMS, s)} pathname="/" kb={null} />
        </MemoryRouter>,
      );
    expect(strip({ terminalAdmitted: false, blockedTerms: 5 })).not.toContain("Terminals");
    const out = strip({ terminalAdmitted: true, blockedTerms: 3 });
    expect(out).toContain('data-testid="header-terminals-tab"');
    expect(out).toContain('data-testid="door-badge-terminals"');
    expect(out).toContain(", 3 need attention");
  });
});
