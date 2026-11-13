// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import SlatesRoute from "./slates";
import { useSlates } from "../hooks/useSlates";
import type { SlateSummary } from "../api/slateTypes";

vi.mock("../hooks/useSlates", async (o) => ({
  ...(await o<typeof import("../hooks/useSlates")>()),
  useSlates: vi.fn(),
}));
vi.mock("../components/nav/SectionTabs", () => ({ WorklogTabs: () => null }));

function row(slug: string, ask_for_you?: number): SlateSummary {
  return {
    slug,
    head_seq: 3,
    generation: 1,
    updated_unix: 1,
    closed: false,
    topics: [],
    ask_for_you,
    counts: {
      now: 0, warn: 0, hand_unack: 0, ask_open: 2, take_live: 0, take_stale: 0,
      take_contested: 0, found: 0, idea: 0, tried: 0,
    },
  };
}

afterEach(cleanup);

describe("/slates questions for you", () => {
  it("hides at 0 or absent, shows and links when > 0", () => {
    vi.mocked(useSlates).mockReturnValue({
      slates: [row("a", 0), row("b"), row("c", 1), row("d", 3)],
      attention: 0,
      loading: false,
      error: null,
    } as never);
    render(
      <MemoryRouter>
        <SlatesRoute />
      </MemoryRouter>,
    );
    expect(screen.queryByTestId("slate-foryou-a")).toBeNull();
    expect(screen.queryByTestId("slate-foryou-b")).toBeNull();
    const c = screen.getByTestId("slate-foryou-c");
    expect(c.textContent).toBe("1 question for you");
    expect(c.getAttribute("href")).toBe("/slates/c");
    expect(screen.getByTestId("slate-foryou-d").textContent).toBe("3 questions for you");
  });

  it("hides the count on a closed slate", () => {
    vi.mocked(useSlates).mockReturnValue({
      slates: [{ ...row("z", 2), closed: true }],
      attention: 0,
      loading: false,
      error: null,
    } as never);
    render(
      <MemoryRouter>
        <SlatesRoute />
      </MemoryRouter>,
    );
    expect(screen.queryByTestId("slate-foryou-z")).toBeNull();
  });
});
