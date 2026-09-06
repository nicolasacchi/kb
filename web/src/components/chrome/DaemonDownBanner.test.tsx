// @vitest-environment jsdom
//
// CT — the grace-period gate (useGracePeriod) and the authSuspect variant
// copy. DaemonDownBanner.test.ts (no DOM) still covers the pure
// anyDaemonConnected/daemonBannerVisible predicates untouched by this file.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import DaemonDownBanner, { useGracePeriod, BANNER_GRACE_MS } from "./DaemonDownBanner";
import { useDaemonStatus } from "../../hooks/useDaemonStatus";
import type { AggregatedStatus, DaemonStatus } from "../../api/sse";

vi.mock("../../hooks/useDaemonStatus", () => ({
  useDaemonStatus: vi.fn(),
}));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.clearAllMocks();
});

function daemon(overrides: Partial<DaemonStatus> = {}): DaemonStatus {
  return {
    url: "http://d1",
    phase: "idle",
    inFlight: 0,
    openErrors: 0,
    openComments: 0,
    lastEventAt: null,
    authSuspect: false,
    ...overrides,
  };
}

function status(overrides: Partial<AggregatedStatus> = {}): AggregatedStatus {
  return {
    phase: "idle",
    inFlight: 0,
    openErrors: 0,
    openComments: 0,
    authSuspect: false,
    daemons: [daemon()],
    ...overrides,
  };
}

describe("useGracePeriod — the timer mechanics in isolation", () => {
  function Harness({ active, graceMs }: { active: boolean; graceMs: number }) {
    const shown = useGracePeriod(active, graceMs);
    return <div data-testid="out">{String(shown)}</div>;
  }
  const out = () => screen.getByTestId("out").textContent;

  it("stays false while inactive", () => {
    render(<Harness active={false} graceMs={3000} />);
    expect(out()).toBe("false");
  });

  it("does not flip true before graceMs elapses", () => {
    vi.useFakeTimers();
    render(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(out()).toBe("false");
  });

  it("flips true exactly at graceMs", () => {
    vi.useFakeTimers();
    render(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(out()).toBe("true");
  });

  it("a blip (active flips false before graceMs) cancels the timer — never flips true", () => {
    vi.useFakeTimers();
    const { rerender } = render(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    rerender(<Harness active={false} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(5000); // well past the original deadline
    });
    expect(out()).toBe("false");
  });

  it("recovery (active → false) after already showing hides IMMEDIATELY, no grace on the way down", () => {
    vi.useFakeTimers();
    const { rerender } = render(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(out()).toBe("true");
    rerender(<Harness active={false} graceMs={3000} />);
    expect(out()).toBe("false"); // no act() timer advance needed
  });

  it("re-arms cleanly on a second active transition after a recovery", () => {
    vi.useFakeTimers();
    const { rerender } = render(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    rerender(<Harness active={false} graceMs={3000} />);
    rerender(<Harness active={true} graceMs={3000} />);
    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(out()).toBe("false");
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(out()).toBe("true");
  });
});

describe("DaemonDownBanner — grace-gated rendering + authSuspect copy", () => {
  it("a blip under 3s never renders the banner", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);
    expect(screen.queryByRole("alert")).toBeNull();

    // Connect, then drop.
    rerender(<DaemonDownBanner />);
    vi.mocked(useDaemonStatus).mockReturnValue(
      status({ phase: "disconnected", daemons: [daemon({ phase: "disconnected" })] }),
    );
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(2000); // under the 3s grace
    });
    // Recovers before the grace period elapses.
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("a sustained disconnect (>= 3s) shows the banner", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);

    vi.mocked(useDaemonStatus).mockReturnValue(
      status({ phase: "disconnected", daemons: [daemon({ phase: "disconnected" })] }),
    );
    rerender(<DaemonDownBanner />);
    expect(screen.queryByRole("alert")).toBeNull(); // not yet — still in grace
    act(() => {
      vi.advanceTimersByTime(BANNER_GRACE_MS);
    });
    expect(screen.getByRole("alert")).toBeTruthy();
  });

  it("recovery after the banner is showing hides it immediately", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);
    vi.mocked(useDaemonStatus).mockReturnValue(
      status({ phase: "disconnected", daemons: [daemon({ phase: "disconnected" })] }),
    );
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(BANNER_GRACE_MS);
    });
    expect(screen.getByRole("alert")).toBeTruthy();

    vi.mocked(useDaemonStatus).mockReturnValue(
      status({ phase: "idle", daemons: [daemon({ phase: "idle" })] }),
    );
    rerender(<DaemonDownBanner />);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("adds the authSuspect variant copy with a real reload link once shown", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);
    vi.mocked(useDaemonStatus).mockReturnValue(
      status({
        phase: "disconnected",
        authSuspect: true,
        daemons: [daemon({ phase: "disconnected", authSuspect: true })],
      }),
    );
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(BANNER_GRACE_MS);
    });

    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("session may have expired");
    const link = screen.getByText("reload to sign in") as HTMLAnchorElement;
    expect(link.getAttribute("href")).toBe(window.location.href);
  });

  it("omits the authSuspect copy for a plain disconnect", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);
    vi.mocked(useDaemonStatus).mockReturnValue(
      status({ phase: "disconnected", daemons: [daemon({ phase: "disconnected" })] }),
    );
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(BANNER_GRACE_MS);
    });

    const alert = screen.getByRole("alert");
    expect(alert.textContent).not.toContain("session may have expired");
    expect(screen.queryByText("reload to sign in")).toBeNull();
  });

  it("never calls location.reload — the link is a plain navigable <a>, never automatic", () => {
    vi.useFakeTimers();
    vi.mocked(useDaemonStatus).mockReturnValue(status());
    const { rerender } = render(<DaemonDownBanner />);
    vi.mocked(useDaemonStatus).mockReturnValue(
      status({
        phase: "disconnected",
        authSuspect: true,
        daemons: [daemon({ phase: "disconnected", authSuspect: true })],
      }),
    );
    rerender(<DaemonDownBanner />);
    act(() => {
      vi.advanceTimersByTime(BANNER_GRACE_MS);
    });
    const link = screen.getByText("reload to sign in");
    expect(link.tagName).toBe("A");
  });
});
