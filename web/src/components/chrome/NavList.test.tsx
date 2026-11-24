// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import NavList from "./NavList";
import {
  NAV_ITEMS,
  resolveNavItems,
  type NavItem,
  type ResolvedNavItem,
} from "../../lib/navItems";

const navState = vi.hoisted(() => ({ extra: null as null | (() => ResolvedNavItem[]) }));
vi.mock("../../hooks/useNavState", () => ({
  useNavItems: () =>
    navState.extra
      ? navState.extra()
      : resolveNavItems(NAV_ITEMS, { terminalAdmitted: false, blockedTerms: 0 }),
}));

vi.mock("../../hooks/useInbox", () => ({ useInbox: () => ({ totalOpen: 0 }) }));
vi.mock("../../hooks/useAnchors", () => ({ useAnchors: () => ({ count: 0 }) }));
vi.mock("../../hooks/useSlates", () => ({ useSlatesAttention: () => 0 }));

afterEach(cleanup);

describe("NavList (mobile sheet)", () => {
  it("lists the five doors then Inbox, Anchors, Settings and the theme toggle; lights the active door", () => {
    render(
      <MemoryRouter initialEntries={["/slates"]}>
        <NavList onNavigate={() => {}} />
      </MemoryRouter>,
    );
    const labels = Array.from(
      document.querySelectorAll(".kb-navlist__label"),
    ).map((e) => e.textContent);
    expect(labels).toEqual([
      "Library",
      "Search",
      "Memory",
      "Worklog",
      "Lists",
      "Inbox",
      "Anchors",
      "Settings",
      "Toggle theme",
    ]);
    expect(screen.getByRole("link", { name: "Worklog" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Library" }).getAttribute("aria-current")).toBeNull();
  });

  it("shows a door badge at n>0, hides it at 0, and omits a hidden item", () => {
    const G = (() => null) as unknown as NavItem["Glyph"];
    const mk = (id: string, extra: Partial<NavItem>): NavItem => ({
      id: id as NavItem["id"],
      label: id,
      Glyph: G,
      href: () => `/${id}`,
      isActive: () => false,
      ...extra,
    });
    const run = (n: number) => {
      navState.extra = () =>
        resolveNavItems(
          [mk("hubdoor", { badge: n }), mk("ghost", { visible: false, badge: 9 })],
          { terminalAdmitted: true, blockedTerms: 0 },
        );
      cleanup();
      render(
        <MemoryRouter>
          <NavList onNavigate={() => {}} />
        </MemoryRouter>,
      );
    };
    run(3);
    expect(screen.getByTestId("navlist-badge-hubdoor").textContent).toBe("3");
    expect(screen.queryByText("ghost")).toBeNull();
    run(0);
    expect(screen.queryByTestId("navlist-badge-hubdoor")).toBeNull();
    expect(screen.getByText("hubdoor")).toBeTruthy();
    navState.extra = null;
  });
});
