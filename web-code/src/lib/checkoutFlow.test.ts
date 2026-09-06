import { describe, expect, it } from "vitest";
import {
  checkoutFlowReducer,
  initialCheckoutFlowState,
  type CheckoutFlowState,
} from "./checkoutFlow";

describe("checkoutFlowReducer", () => {
  it("starts idle", () => {
    expect(initialCheckoutFlowState).toEqual({ phase: "idle" });
  });

  it("open moves idle -> confirming with the target ref", () => {
    const next = checkoutFlowReducer(initialCheckoutFlowState, {
      type: "open",
      repo: "fixture",
      target: "main",
    });
    expect(next).toEqual({ phase: "confirming", repo: "fixture", target: "main" });
  });

  it("submit moves confirming -> submitting, carrying repo/target forward", () => {
    const confirming: CheckoutFlowState = { phase: "confirming", repo: "fixture", target: "main" };
    expect(checkoutFlowReducer(confirming, { type: "submit" })).toEqual({
      phase: "submitting",
      repo: "fixture",
      target: "main",
    });
  });

  it("submit is a no-op from idle (nothing to submit)", () => {
    expect(checkoutFlowReducer(initialCheckoutFlowState, { type: "submit" })).toBe(
      initialCheckoutFlowState,
    );
  });

  it("submit is a no-op while already submitting (no double-fire)", () => {
    const submitting: CheckoutFlowState = { phase: "submitting", repo: "fixture", target: "main" };
    expect(checkoutFlowReducer(submitting, { type: "submit" })).toBe(submitting);
  });

  it("succeeded resets to idle", () => {
    const submitting: CheckoutFlowState = { phase: "submitting", repo: "fixture", target: "main" };
    expect(checkoutFlowReducer(submitting, { type: "succeeded" })).toEqual({ phase: "idle" });
  });

  it("dirty moves submitting -> dirty, carrying the path list", () => {
    const submitting: CheckoutFlowState = { phase: "submitting", repo: "fixture", target: "main" };
    const next = checkoutFlowReducer(submitting, { type: "dirty", dirtyPaths: ["a.rs", "b.rs"] });
    expect(next).toEqual({ phase: "dirty", repo: "fixture", target: "main", dirtyPaths: ["a.rs", "b.rs"] });
  });

  it("dirty is ignored outside of submitting (a stale response after cancel)", () => {
    expect(
      checkoutFlowReducer(initialCheckoutFlowState, { type: "dirty", dirtyPaths: ["a.rs"] }),
    ).toBe(initialCheckoutFlowState);
  });

  it("failed moves submitting -> error with the message", () => {
    const submitting: CheckoutFlowState = { phase: "submitting", repo: "fixture", target: "main" };
    expect(checkoutFlowReducer(submitting, { type: "failed", message: "boom" })).toEqual({
      phase: "error",
      repo: "fixture",
      target: "main",
      message: "boom",
    });
  });

  it("cancel from any non-idle phase returns to idle", () => {
    const dirty: CheckoutFlowState = {
      phase: "dirty",
      repo: "fixture",
      target: "main",
      dirtyPaths: ["a.rs"],
    };
    expect(checkoutFlowReducer(dirty, { type: "cancel" })).toEqual({ phase: "idle" });
  });

  it("a retry submit from dirty carries the same repo/target", () => {
    const dirty: CheckoutFlowState = {
      phase: "dirty",
      repo: "fixture",
      target: "main",
      dirtyPaths: ["a.rs"],
    };
    expect(checkoutFlowReducer(dirty, { type: "submit" })).toEqual({
      phase: "submitting",
      repo: "fixture",
      target: "main",
    });
  });
});
