import { describe, expect, it } from "vitest";
import { normalizeScrollKey } from "./useScrollRestoration";

// W3.D/S2 — the #31 `ephemeralParams` amendment: declared params are
// stripped from the scroll-restoration STORAGE key only. Pure-fn unit tests
// (the hook itself needs a DOM + jsdom timer harness for the retry loop,
// out of scope here — this pins the load-bearing key-normalization logic).
describe("normalizeScrollKey (invariant #31 ephemeralParams amendment)", () => {
  it("returns the key unchanged when no ephemeral params are declared", () => {
    expect(normalizeScrollKey("/sessions?focus=sid-1", [])).toBe(
      "/sessions?focus=sid-1",
    );
  });

  it("returns the key unchanged when there is no query string", () => {
    expect(normalizeScrollKey("/sessions", ["focus"])).toBe("/sessions");
  });

  it("strips a declared param from the key, keeping other params", () => {
    expect(
      normalizeScrollKey("/sessions?project=kb&focus=sid-1", ["focus"]),
    ).toBe("/sessions?project=kb");
  });

  it("drops the query string entirely when the only param was ephemeral", () => {
    expect(normalizeScrollKey("/sessions?focus=sid-1", ["focus"])).toBe(
      "/sessions",
    );
  });

  it("is a no-op (byte-identical) when the declared param is absent", () => {
    expect(normalizeScrollKey("/sessions?project=kb", ["focus"])).toBe(
      "/sessions?project=kb",
    );
  });

  it("strips multiple declared params", () => {
    expect(
      normalizeScrollKey("/sessions?a=1&b=2&c=3", ["a", "c"]),
    ).toBe("/sessions?b=2");
  });

  it("two URLs differing only in an ephemeral param normalize to the SAME key", () => {
    const withFocus = normalizeScrollKey("/sessions?project=kb&focus=sid-1", [
      "focus",
    ]);
    const withoutFocus = normalizeScrollKey("/sessions?project=kb", [
      "focus",
    ]);
    expect(withFocus).toBe(withoutFocus);
  });

  // W7 (R15/LF-2) — `?follow=1` joins the ephemeralParams set on the
  // sessions.tsx call site (`["focus", "follow"]`).
  it("strips ?follow= alongside ?focus= (the sessions.tsx declared set)", () => {
    expect(
      normalizeScrollKey("/sessions?focus=sid-1&follow=1", [
        "focus",
        "follow",
      ]),
    ).toBe("/sessions");
  });

  it("a follow toggle alone normalizes to the same key as without it", () => {
    const withFollow = normalizeScrollKey("/sessions?project=kb&follow=1", [
      "focus",
      "follow",
    ]);
    const withoutFollow = normalizeScrollKey("/sessions?project=kb", [
      "focus",
      "follow",
    ]);
    expect(withFollow).toBe(withoutFollow);
  });
});
