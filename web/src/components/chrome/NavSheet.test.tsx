// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import NavSheet from "./NavSheet";

afterEach(cleanup);

describe("NavSheet (mobile nav on @kb/ui/Sheet)", () => {
  it("renders nothing while closed", () => {
    render(
      <NavSheet open={false} onClose={() => {}}>
        <a href="/x">row</a>
      </NavSheet>,
    );
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
  });

  it("opens as a dialog at the peek detent holding its children, outside any wrapper", () => {
    render(
      <div className="kb-head">
        <NavSheet open onClose={() => {}}>
          <a className="kb-navlist__row" href="/x">
            row
          </a>
        </NavSheet>
      </div>,
    );
    const sheet = document.querySelector("[data-kb-sheet]");
    expect(sheet?.getAttribute("role")).toBe("dialog");
    expect(sheet?.getAttribute("id")).toBe("kb-navsheet");
    expect(sheet?.getAttribute("data-detent")).toBe("peek");
    expect(sheet?.querySelector(".kb-navlist__row")).not.toBeNull();
  });

  it("closes on Escape and on a scrim tap", () => {
    const onClose = vi.fn();
    render(
      <NavSheet open onClose={onClose}>
        <span>x</span>
      </NavSheet>,
    );
    fireEvent.keyDown(document.querySelector("[data-kb-sheet]")!, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(document.querySelector("[data-kb-scrim]")!);
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("dialog")).toBeTruthy();
  });
});
