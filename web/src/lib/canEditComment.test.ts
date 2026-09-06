import { describe, it, expect } from "vitest";
import { canEditComment } from "./canEditComment";

describe("canEditComment", () => {
  it("mine: user matches me → editable", () => {
    expect(canEditComment({ user: "alice" }, "alice", "carol")).toBe(true);
  });

  it("theirs: user differs from me → not editable", () => {
    expect(canEditComment({ user: "bob" }, "alice", "carol")).toBe(false);
  });

  it("legacy-no-user: absent user + me is the CONFIGURED operator → editable", () => {
    // The operator name comes from /api/identity (server config), never a
    // hardcoded default — here the deploy configured operator = "carol".
    expect(canEditComment({}, "carol", "carol")).toBe(true);
    expect(canEditComment({ user: undefined }, "carol", "carol")).toBe(true);
    expect(canEditComment({ user: null }, "carol", "carol")).toBe(true);
    expect(canEditComment({ user: "" }, "carol", "carol")).toBe(true);
    expect(canEditComment({ user: "   " }, "carol", "carol")).toBe(true);
  });

  it("legacy-no-user: me is not the configured operator → not editable", () => {
    expect(canEditComment({}, "alice", "carol")).toBe(false);
    // A user literally named "operator" gets nothing when the deploy
    // configured a different operator (mirrors the server's 403 rule).
    expect(canEditComment({}, "operator", "carol")).toBe(false);
  });

  it("identity or operator not resolved → not editable (fail closed)", () => {
    expect(canEditComment({ user: "alice" }, null, "carol")).toBe(false);
    expect(canEditComment({ user: "alice" }, undefined, "carol")).toBe(false);
    expect(canEditComment({}, "carol", null)).toBe(false);
    expect(canEditComment({}, "carol", undefined)).toBe(false);
  });

  it("operator cannot edit a teammate's stamped comment", () => {
    expect(canEditComment({ user: "alice" }, "carol", "carol")).toBe(false);
  });
});
