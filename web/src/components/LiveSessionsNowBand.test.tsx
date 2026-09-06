// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import LiveSessionsNowBand from "./LiveSessionsNowBand";
import type { LiveStatusRow } from "../api/sessions";

const NOW_SECS = 1_800_000_000;
const NOW_MS = NOW_SECS * 1000;

function row(overrides: Partial<LiveStatusRow> & Pick<LiveStatusRow, "session_id">): LiveStatusRow {
  return {
    harness: "claude",
    holder: "agent",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: NOW_SECS,
    since_secs: 0,
    resume: `claude -r ${overrides.session_id}`,
    blocked: false,
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
});

describe("LiveSessionsNowBand — empty state", () => {
  it("renders nothing (no wrapper element) when there are no rows", () => {
    const { container } = render(<LiveSessionsNowBand rows={[]} now={NOW_MS} />);
    expect(container.firstChild).toBeNull();
  });
});

describe("LiveSessionsNowBand — lane rendering", () => {
  it("only renders the lanes that have rows", () => {
    render(
      <LiveSessionsNowBand
        rows={[row({ session_id: "a", state: "working" })]}
        now={NOW_MS}
      />,
    );
    expect(screen.getByTestId("now-lane-progress")).toBeTruthy();
    expect(screen.queryByTestId("now-lane-waiting")).toBeNull();
    expect(screen.queryByTestId("now-lane-finished")).toBeNull();
  });

  it("renders all three lanes with correct counts when every lane is populated", () => {
    render(
      <LiveSessionsNowBand
        rows={[
          row({ session_id: "a", state: "working" }),
          row({ session_id: "b", state: "waiting" }),
          row({ session_id: "c", state: "finished" }),
        ]}
        now={NOW_MS}
      />,
    );
    expect(screen.getByTestId("now-lane-progress").textContent).toContain("In progress");
    expect(screen.getByTestId("now-lane-waiting").textContent).toContain("Waiting on you");
    expect(screen.getByTestId("now-lane-finished").textContent).toContain("Finished");
    expect(screen.getAllByTestId("now-row")).toHaveLength(3);
  });

  it("orders the waiting lane longest-wait-first", () => {
    render(
      <LiveSessionsNowBand
        rows={[
          row({ session_id: "recent", state: "waiting", since_unix: NOW_SECS - 60 }),
          row({ session_id: "oldest", state: "waiting", since_unix: NOW_SECS - 40_000 }),
        ]}
        now={NOW_MS}
      />,
    );
    const rows = screen.getAllByTestId("now-row");
    expect(rows[0].getAttribute("data-live-state")).toBe("waiting");
    expect(rows[0].textContent).toContain("11h");
    expect(rows[1].textContent).toContain("1m");
  });

  it("orders the in-progress lane most-recently-active-first", () => {
    render(
      <LiveSessionsNowBand
        rows={[
          row({ session_id: "old", state: "working", since_unix: NOW_SECS - 600 }),
          row({ session_id: "new", state: "working", since_unix: NOW_SECS - 5 }),
        ]}
        now={NOW_MS}
      />,
    );
    const rows = screen.getAllByTestId("now-row");
    expect(rows[0].textContent).toContain("now");
    expect(rows[1].textContent).toContain("10m");
  });

  it("caps the finished lane and captions the cap", () => {
    const rows = Array.from({ length: 8 }, (_, i) =>
      row({ session_id: `f${i}`, state: "finished", since_unix: NOW_SECS - i }),
    );
    render(<LiveSessionsNowBand rows={rows} now={NOW_MS} />);
    expect(screen.getAllByTestId("now-row")).toHaveLength(5);
    expect(screen.getByTestId("now-lane-finished").textContent).toContain(
      "showing 5 most recent of 8",
    );
  });
});

describe("LiveSessionsNowBand — honesty markers", () => {
  it("does not show a honesty marker on an ordinary observed/hook row", () => {
    render(
      <LiveSessionsNowBand
        rows={[row({ session_id: "a", state: "working", source: "hook", confidence: "observed" })]}
        now={NOW_MS}
      />,
    );
    expect(screen.queryByTestId("now-row-honesty")).toBeNull();
  });

  it("shows the presumed marker for a presumed-confidence row", () => {
    render(
      <LiveSessionsNowBand
        rows={[row({ session_id: "a", state: "working", confidence: "presumed" })]}
        now={NOW_MS}
      />,
    );
    const badge = screen.getByTestId("now-row-honesty");
    expect(badge.textContent).toBe("presumed");
    expect(badge.getAttribute("data-kb-live-honesty")).toBe("presumed");
  });

  it("shows the capture-sourced marker for a capture-sourced row", () => {
    render(
      <LiveSessionsNowBand
        rows={[
          row({
            session_id: "a",
            state: "waiting",
            source: "capture",
            confidence: "inferred",
          }),
        ]}
        now={NOW_MS}
      />,
    );
    const badge = screen.getByTestId("now-row-honesty");
    expect(badge.textContent).toBe("as of last capture");
    expect(badge.getAttribute("data-kb-live-honesty")).toBe("capture");
  });

  it("a stalled row stays in the in-progress lane with a doubt marker", () => {
    render(
      <LiveSessionsNowBand
        rows={[row({ session_id: "a", state: "stalled" })]}
        now={NOW_MS}
      />,
    );
    expect(screen.getByTestId("now-lane-progress")).toBeTruthy();
    expect(screen.queryByTestId("now-lane-waiting")).toBeNull();
    expect(screen.getByTestId("now-row-stalled")).toBeTruthy();
  });

  it("presumed_ended renders in the finished lane, labelled as an inference", () => {
    render(
      <LiveSessionsNowBand
        rows={[
          row({
            session_id: "a",
            state: "presumed_ended",
            source: "capture",
            confidence: "presumed",
          }),
        ]}
        now={NOW_MS}
      />,
    );
    expect(screen.getByTestId("now-lane-finished")).toBeTruthy();
    expect(screen.getByTestId("now-row-presumed-ended").textContent).toBe("presumed ended");
  });
});

describe("LiveSessionsNowBand — resume action", () => {
  it("copies the row's own precomputed resume string, harness-correct per row", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    render(
      <LiveSessionsNowBand
        rows={[
          row({
            session_id: "codex-sid",
            harness: "codex",
            state: "waiting",
            resume: "codex resume codex-sid",
          }),
        ]}
        now={NOW_MS}
      />,
    );
    fireEvent.click(screen.getByTestId("now-row-resume-copy"));
    expect(writeText).toHaveBeenCalledWith("codex resume codex-sid");
  });
});
