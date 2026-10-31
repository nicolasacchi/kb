// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  SinceVisitBannerView,
  sinceVisitDismissKey,
  sinceVisitVisible,
} from "./SinceVisitBanner";
import type { SinceVisitResponse, ChangedSection } from "../../api/versions";

const sec = (id: string | undefined, title: string | undefined, change: ChangedSection["change"] = "changed"): ChangedSection => ({
  id,
  title,
  level: id ? 2 : undefined,
  change,
  added: 1,
  removed: 0,
});

function changed(over: Partial<SinceVisitResponse> = {}): SinceVisitResponse {
  return {
    state: "changed",
    mode: "index",
    baseline: "previous_visit",
    last_visit_unix: 1_700_000_000,
    working_ts_unix: 1_700_100_000,
    memento: {
      at_unix: 1_700_000_000,
      relation: "nearest-prior",
      found: true,
      exact: false,
      version: {
        ref: "index:7",
        source: "index",
        label: "",
        author: "",
        ts_unix: 1_699_999_000,
        short: "#7",
      },
    },
    sections: [sec("kb-h-risks", "Risks")],
    added_lines: 1,
    removed_lines: 0,
    ...over,
  };
}

const view = (data: SinceVisitResponse | null, extra: Record<string, unknown> = {}) =>
  render(<SinceVisitBannerView kb="docs" id="abc123def456" data={data} {...extra} />);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  sessionStorage.clear();
});

describe("SinceVisitBanner", () => {
  it("previous-visit headline says 'last visit' and carries the full date-time in title", () => {
    view(changed());
    const banner = screen.getByTestId("since-visit-banner");
    expect(banner.getAttribute("role")).toBe("status");
    const msg = banner.querySelector(".kb-since-visit__msg")!;
    expect(msg.textContent).toMatch(/^Changed since your last visit, /);
    expect(msg.getAttribute("title")).toBe(new Date(1_700_000_000 * 1000).toLocaleString());
  });

  it("current-visit headline says 'you opened it at'", () => {
    view(changed({ baseline: "current_visit" }));
    expect(screen.getByTestId("since-visit-banner").textContent).toMatch(
      /Changed since you opened it at \d\d:\d\d/,
    );
  });

  it("shows 3 chips plus '+N more'", () => {
    const sections = ["a", "b", "c", "d", "e"].map((x) => sec(`kb-h-${x}`, x.toUpperCase()));
    view(changed({ sections }), { onJumpSection: vi.fn() });
    expect(screen.getAllByRole("button").filter((b) => b.className.includes("chip"))).toHaveLength(3);
    expect(screen.getByText("+2 more")).toBeTruthy();
    expect(screen.getByText(/5 sections:/)).toBeTruthy();
  });

  it("a chip click calls onJumpSection with the section id", () => {
    const onJump = vi.fn();
    view(changed(), { onJumpSection: onJump });
    fireEvent.click(screen.getByText("Risks"));
    expect(onJump).toHaveBeenCalledWith("kb-h-risks");
  });

  it("a removed section is struck text, not a button; the preamble reads 'Opening'", () => {
    view(
      changed({
        sections: [sec("kb-h-old", "Old bit", "removed"), sec(undefined, undefined)],
      }),
      { onJumpSection: vi.fn() },
    );
    expect(screen.queryByRole("button", { name: "Old bit" })).toBeNull();
    expect(screen.getByText("Old bit").tagName).toBe("S");
    expect(screen.queryByRole("button", { name: "Opening" })).toBeNull();
    expect(screen.getByText("Opening")).toBeTruthy();
  });

  it("'View changes' calls onViewChanges with the resolved version ref", () => {
    const onView = vi.fn();
    view(changed(), { onViewChanges: onView });
    fireEvent.click(screen.getByText(/View changes/));
    expect(onView).toHaveBeenCalledWith("index:7");
  });

  it("no_version names the floor and History opens the dock with its default (null)", () => {
    const onView = vi.fn();
    view(
      changed({
        state: "changed_no_delta",
        reason: "no_version",
        sections: undefined,
        memento: {
          at_unix: 1_700_000_000,
          relation: "nearest-prior",
          found: false,
          exact: false,
          oldest_ts_unix: 1_700_050_000,
        },
      }),
      { onViewChanges: onView },
    );
    expect(screen.getByTestId("since-visit-banner").textContent).toMatch(
      /the version you saw is no longer kept \(oldest kept /,
    );
    fireEvent.click(screen.getByText("History"));
    expect(onView).toHaveBeenCalledWith(null);
  });

  it("mode_off says the kb keeps no history and offers no action", () => {
    view(changed({ state: "changed_no_delta", reason: "mode_off", sections: undefined }), {
      onViewChanges: vi.fn(),
    });
    expect(screen.getByTestId("since-visit-banner").textContent).toContain(
      "this kb keeps no version history",
    );
    expect(screen.queryByText("History")).toBeNull();
    expect(screen.queryByText(/View changes/)).toBeNull();
  });

  it("is hidden for no_visit, unchanged, unavailable, null and when asOf", () => {
    for (const state of ["no_visit", "unchanged", "unavailable"] as const) {
      view({ state, mode: "index", working_ts_unix: 5 });
      expect(screen.queryByTestId("since-visit-banner")).toBeNull();
      cleanup();
    }
    view(null);
    expect(screen.queryByTestId("since-visit-banner")).toBeNull();
    cleanup();
    view(changed(), { asOf: true });
    expect(screen.queryByTestId("since-visit-banner")).toBeNull();
    expect(sinceVisitVisible(changed(), false, true)).toBe(false);
    expect(sinceVisitVisible(changed(), false, false)).toBe(true);
  });

  it("dismiss persists for that working_ts_unix; a newer one shows the banner again", () => {
    const { unmount } = view(changed());
    fireEvent.click(screen.getByLabelText("dismiss"));
    expect(screen.queryByTestId("since-visit-banner")).toBeNull();
    expect(sessionStorage.getItem(sinceVisitDismissKey("docs", "abc123def456", 1_700_100_000))).toBe("1");
    unmount();
    view(changed());
    expect(screen.queryByTestId("since-visit-banner")).toBeNull();
    cleanup();
    view(changed({ working_ts_unix: 1_700_200_000 }));
    expect(screen.getByTestId("since-visit-banner")).toBeTruthy();
  });
});
