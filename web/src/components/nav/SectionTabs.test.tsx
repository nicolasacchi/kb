// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { ListsTabs, WorklogTabs } from "./SectionTabs";

afterEach(cleanup);

describe("SectionTabs", () => {
  it("Worklog row marks the current tab with aria-current and carries the kb", () => {
    render(
      <MemoryRouter initialEntries={["/slates"]}>
        <WorklogTabs />
      </MemoryRouter>,
    );
    expect(screen.getByRole("navigation", { name: "Worklog" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Slates" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Sessions" }).getAttribute("aria-current")).toBeNull();
  });

  it("Lists row lights Notes on /notes and keeps ?kb= on the Lists link", () => {
    render(
      <MemoryRouter initialEntries={["/notes?kb=mem"]}>
        <ListsTabs />
      </MemoryRouter>,
    );
    expect(screen.getByRole("link", { name: "Notes" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Lists" }).getAttribute("href")).toBe("/lists?kb=mem");
  });
});
