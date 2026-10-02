import { describe, expect, it } from "vitest";
import { isReviewIdString, judgeCurrentReview } from "./currentReviewValidity";

describe("isReviewIdString", () => {
  it("accepts plain positive integers only", () => {
    expect(isReviewIdString("7")).toBe(true);
    expect(isReviewIdString("999")).toBe(true);
    for (const bad of ["7.5", "1e3", "0x10", " 7", "-1", "0", "", "abc", "99999999999999999999"]) {
      expect(isReviewIdString(bad)).toBe(false);
    }
  });
});

describe("judgeCurrentReview", () => {
  it("a 404 means gone", () => {
    expect(judgeCurrentReview("r", { isPending: false, errorStatus: 404 })).toBe("gone");
  });
  it("a review of another repo is gone", () => {
    expect(judgeCurrentReview("r", { isPending: false, reviewRepo: "other" })).toBe("gone");
  });
  it("a review of this repo is ok", () => {
    expect(judgeCurrentReview("r", { isPending: false, reviewRepo: "r" })).toBe("ok");
  });
  it("pending stays pending; a non-404 failure keeps the marker", () => {
    expect(judgeCurrentReview("r", { isPending: true })).toBe("pending");
    expect(judgeCurrentReview("r", { isPending: false, errorStatus: 500 })).toBe("ok");
  });
});
