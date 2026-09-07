// `lib/loopback.ts` — what to OFFER, from the daemon's own verdict (V74-L2).
import { describe, expect, it } from "vitest";
import { isLoopbackCaller } from "./loopback";

describe("isLoopbackCaller", () => {
  it("is true only when the daemon SAID so", () => {
    expect(isLoopbackCaller({ repos: [], loopback: true })).toBe(true);
    expect(isLoopbackCaller({ repos: [], loopback: false })).toBe(false);
  });

  it("degrades to false — an older daemon, a failed fetch, one in flight", () => {
    // Hiding an affordance that would have worked costs a caption; offering one
    // that cannot costs a 404 the operator has to interpret.
    expect(isLoopbackCaller({ repos: [] })).toBe(false);
    expect(isLoopbackCaller(undefined)).toBe(false);
  });
});
