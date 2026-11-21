import { describe, expect, it } from "vitest";
import { CROPPED_DESKTOP_HINT, CROPPED_LABEL, CROPPED_PHONE_HINT, isCroppedX } from "./cropHint";

describe("cropHint", () => {
  it("one label for both surfaces, a hint per surface that says how to see the rest", () => {
    expect(CROPPED_LABEL).toBe("Cropped");
    expect(CROPPED_DESKTOP_HINT).toMatch(/scroll/);
    expect(CROPPED_PHONE_HINT).toMatch(/pan/);
    expect(CROPPED_PHONE_HINT).toMatch(/Fit to phone/);
  });
  it("isCroppedX: content wider than the box by more than a pixel; an unlaid-out box is not cropped", () => {
    expect(isCroppedX({ scrollWidth: 1000, clientWidth: 400 })).toBe(true);
    expect(isCroppedX({ scrollWidth: 401, clientWidth: 400 })).toBe(false);
    expect(isCroppedX({ scrollWidth: 400, clientWidth: 400 })).toBe(false);
    expect(isCroppedX({ scrollWidth: 1000, clientWidth: 0 })).toBe(false);
  });
});
