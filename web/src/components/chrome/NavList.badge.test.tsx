// @vitest-environment jsdom
// HUBDOOR — the mobile sheet's Terminals row: hidden unless admitted, pill hidden
// from AT, count in the row's name.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import NavList from "./NavList";
import { NAV_ITEMS, resolveNavItems } from "../../lib/navItems";

const st = vi.hoisted(() => ({ admitted: true, n: 3 }));
vi.mock("../../hooks/useNavState", () => ({
  useNavItems: () =>
    resolveNavItems(NAV_ITEMS, { terminalAdmitted: st.admitted, blockedTerms: st.n }),
}));
vi.mock("../../hooks/useInbox", () => ({ useInbox: () => ({ totalOpen: 0 }) }));
vi.mock("../../hooks/useAnchors", () => ({ useAnchors: () => ({ count: 0 }) }));
vi.mock("../../hooks/useSlates", () => ({ useSlatesAttention: () => 0 }));

afterEach(cleanup);

const mount = () =>
  render(
    <MemoryRouter initialEntries={["/"]}>
      <NavList onNavigate={() => {}} />
    </MemoryRouter>,
  );

describe("NavList Terminals row (HUBDOOR)", () => {
  it("admitted: 'Terminals, 3 need attention' with an aria-hidden pill", () => {
    st.admitted = true;
    st.n = 3;
    mount();
    expect(screen.getByRole("link", { name: "Terminals, 3 need attention" })).toBeTruthy();
    expect(screen.getByTestId("navlist-badge-terminals").getAttribute("aria-hidden")).toBe("true");
  });

  it("not admitted: no Terminals row", () => {
    st.admitted = false;
    mount();
    expect(screen.queryByRole("link", { name: /Terminals/ })).toBeNull();
  });
});
