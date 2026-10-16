// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import NavList from "./NavList";

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
});
