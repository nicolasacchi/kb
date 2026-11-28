// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const toastErr = vi.hoisted(() => vi.fn());
vi.mock("../../../lib/toast", () => ({ toast: { err: toastErr, ok: vi.fn(), info: vi.fn() } }));
vi.mock("../../../hooks/useArtifactHost", () => ({ useIdentity: () => ({ terminal_admitted: true }) }));
vi.mock("../../../hooks/useIsMobile", () => ({ usePhonePanel: () => false }));

import { TermRailContext } from "../../rail/context";
import { railTabs } from "../../rail/registry";
import "../../rail/registerTabs";
import { CommandsRail } from "./CommandsRail";
import { rigFetch, wrap, type Rig } from "./paletteTestkit";

let rig: Rig;
beforeEach(() => {
  toastErr.mockReset();
  rig = rigFetch();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function mountRail(harness = "claude") {
  return render(
    wrap(
      <div data-testid="kb-terminal">
        <textarea className="xterm-helper-textarea" />
        <TermRailContext.Provider value={{ termId: "t9", harness }}>
          <CommandsRail />
        </TermRailContext.Provider>
      </div>,
    ),
  );
}

describe("CommandsRail", () => {
  it("renders the harness's list inline (no dialog) with 40 px rows", async () => {
    mountRail();
    await waitFor(() => expect(screen.queryByText("Showing shipped commands")).toBeNull());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.querySelector(".kb-cmdp--rail")).not.toBeNull();
    const rows = screen.getAllByTestId("kb-cmdp-row");
    expect(rows.every((r) => r.classList.contains("kb-cmdp__row"))).toBe(true);
    expect(screen.queryAllByText("/shake")).toEqual([]);
  });

  it("sends through the same paths, for this terminal, and focuses the pane", async () => {
    mountRail();
    await waitFor(() => expect(screen.queryByText("Showing shipped commands")).toBeNull());
    fireEvent.click(screen.getAllByText("/compact")[0]);
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.calls.find((c) => c.url.endsWith("/paste"))?.url).toContain("/api/terms/t9/paste");
    expect(rig.drive()[0]).toEqual({ op: "paste", body: { text: "/compact" } });
    await waitFor(() => expect(document.activeElement).toBe(document.querySelector(".xterm-helper-textarea")));
  });

  it("registerTabs replaces the commands placeholder", () => {
    const tab = railTabs().find((t) => t.id === "commands");
    expect(tab?.label).toBe("Commands");
    expect((tab?.node as { type: unknown }).type).toBe(CommandsRail);
  });
});
