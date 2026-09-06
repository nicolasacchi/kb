import { describe, it, expect } from "vitest";
import { anyDaemonConnected, daemonBannerVisible } from "./DaemonDownBanner";
import type { DaemonStatus } from "../../api/sse";

const d = (phase: DaemonStatus["phase"]): DaemonStatus =>
  ({ phase }) as DaemonStatus;

describe("E1 daemon-down gating (the banner-flash fix)", () => {
  // invariant:33
  it("anyDaemonConnected is false for the empty initial snapshot", () => {
    expect(anyDaemonConnected([])).toBe(false);
  });

  it("anyDaemonConnected is false while every daemon is still disconnected (connecting)", () => {
    expect(anyDaemonConnected([d("disconnected"), d("disconnected")])).toBe(
      false,
    );
  });

  it("anyDaemonConnected is true once any daemon reaches a live phase", () => {
    expect(anyDaemonConnected([d("disconnected"), d("idle")])).toBe(true);
    expect(anyDaemonConnected([d("indexing")])).toBe(true);
    expect(anyDaemonConnected([d("degraded")])).toBe(true);
  });

  it("banner stays hidden until we've actually been connected (no load flash)", () => {
    // The exact regression: aggregate phase can read disconnected on the first
    // render, but everConnected is still false → hidden.
    expect(daemonBannerVisible("disconnected", false)).toBe(false);
    expect(daemonBannerVisible("idle", false)).toBe(false);
  });

  it("banner shows only on a real drop after a connection", () => {
    expect(daemonBannerVisible("disconnected", true)).toBe(true);
  });

  it("banner is hidden whenever connected, regardless of the latch", () => {
    expect(daemonBannerVisible("idle", true)).toBe(false);
    expect(daemonBannerVisible("indexing", true)).toBe(false);
  });
});
