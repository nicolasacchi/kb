import { describe, expect, it } from "vitest";
import { initialInputText } from "./initialInput";

describe("initialInputText", () => {
  it("strips one trailing newline", () => {
    expect(initialInputText("hello\n")).toBe("hello");
    expect(initialInputText("hello\r\n")).toBe("hello");
  });
  it("flattens interior line-break runs to one space, so no Enter-acting newline survives", () => {
    expect(initialInputText("a\nb")).toBe("a b");
    expect(initialInputText("a\r\n\r\nb")).toBe("a b");
    expect(initialInputText("a \n\t b")).toBe("a b");
    expect(initialInputText("a\rb")).toBe("a b");
    expect(/[\r\n]/.test(initialInputText("x\ny\r\nz\n"))).toBe(false);
  });
});
