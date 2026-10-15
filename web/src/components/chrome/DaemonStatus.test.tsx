// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import DaemonStatus from "./DaemonStatus";

type Tick = (payload: Record<string, unknown>) => void;
const h = vi.hoisted(() => ({ tick: null as null | Tick, unsub: vi.fn() }));

vi.mock("../../api/sse", () => ({
  sse: {
    subscribeEvent: (type: string, fn: Tick) => {
      if (type === "metrics.tick") h.tick = fn;
      return h.unsub;
    },
  },
}));
vi.mock("../../api/client", () => ({
  fetchCrossStats: vi.fn().mockResolvedValue({
    total_docs: 1234,
    kbs: [{ name: "a" }, { name: "b" }],
    daemon: { name: "kb-test" },
  }),
}));
vi.mock("../../hooks/useArtifactHost", () => ({ useIdentity: () => null }));

afterEach(() => {
  cleanup();
  h.tick = null;
});

describe("DaemonStatus (the footer bar folded into the Spine)", () => {
  it("shows a text label beside the dot and keeps the popover closed", async () => {
    render(<DaemonStatus warnings={[]} />);
    const btn = await screen.findByRole("button", { name: /idle/ });
    expect(btn.textContent).toContain("idle");
    expect(screen.queryByRole("dialog", { name: "daemon status" })).toBeNull();
  });

  it("opens a popover with name, counts and warnings; Escape closes it", async () => {
    render(<DaemonStatus warnings={["build stamp drift"]} />);
    // a warning flips the label off "idle" (never hue-only)
    fireEvent.click(await screen.findByRole("button", { name: /warn/ }));
    const pop = screen.getByRole("dialog", { name: "daemon status" });
    await waitFor(() => expect(pop.textContent).toContain("1,234 artifacts · 2 kbs"));
    expect(pop.textContent).toContain("kb-test");
    expect(pop.textContent).toContain("build stamp drift");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "daemon status" })).toBeNull();
  });

  it("still follows metrics.tick: request rate -> active, embedder degraded -> warn", async () => {
    render(<DaemonStatus warnings={[]} />);
    expect(h.tick).not.toBeNull();
    act(() => h.tick!({ requests_last_sec: 3 }));
    const btn = await screen.findByRole("button", { name: /active/ });
    fireEvent.click(btn);
    expect(screen.getByRole("dialog").textContent).toContain("active · 3/s");
    act(() => h.tick!({ requests_last_sec: 0, embedder_degraded: true }));
    expect(screen.getByRole("dialog").textContent).toContain("embedder degraded");
    expect(screen.getByRole("button", { name: /warn/ })).toBeTruthy();
  });

  it("says offline once the stats probe fails", async () => {
    const { fetchCrossStats } = await import("../../api/client");
    vi.mocked(fetchCrossStats).mockRejectedValueOnce(new Error("down"));
    render(<DaemonStatus warnings={[]} />);
    expect(await screen.findByRole("button", { name: /offline/ })).toBeTruthy();
  });

  it("unsubscribes from metrics.tick on unmount", () => {
    const { unmount } = render(<DaemonStatus warnings={[]} />);
    unmount();
    expect(h.unsub).toHaveBeenCalled();
  });
});
