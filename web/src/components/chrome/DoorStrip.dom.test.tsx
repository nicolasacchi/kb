// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import DoorStrip from "./DoorStrip";
import { NAV_ITEMS, resolveNavItems } from "../../lib/navItems";

afterEach(cleanup);

const strip = (blockedTerms: number, admitted = true) =>
  render(
    <MemoryRouter initialEntries={["/terminals"]}>
      <DoorStrip
        items={resolveNavItems(NAV_ITEMS, { terminalAdmitted: admitted, blockedTerms })}
        pathname="/terminals"
        kb={null}
      />
    </MemoryRouter>,
  );

describe("DoorStrip accessible name (HUBDOOR)", () => {
  it("the Terminals link reads 'Terminals, 3 need attention'", () => {
    strip(3);
    const link = screen.getByRole("link", { name: "Terminals, 3 need attention" });
    expect(link.getAttribute("href")).toBe("/terminals");
    expect(link.getAttribute("aria-current")).toBe("page");
    expect(screen.getByTestId("door-badge-terminals").getAttribute("aria-hidden")).toBe("true");
  });

  it("singular, and a bare name at zero", () => {
    strip(1);
    expect(screen.getByRole("link", { name: "Terminals, 1 needs attention" })).toBeTruthy();
    cleanup();
    strip(0);
    expect(screen.getByRole("link", { name: "Terminals" })).toBeTruthy();
    expect(screen.queryByTestId("door-badge-terminals")).toBeNull();
  });

  it("not admitted: no Terminals link at all", () => {
    strip(4, false);
    expect(screen.queryByRole("link", { name: /Terminals/ })).toBeNull();
  });
});
