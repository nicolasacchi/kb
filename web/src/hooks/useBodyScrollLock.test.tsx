// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { useBodyScrollLock } from "./useBodyScrollLock";

afterEach(() => {
  cleanup();
  document.body.style.overflow = "";
});

function Locker({ active }: { active: boolean }) {
  useBodyScrollLock(active);
  return null;
}

describe("useBodyScrollLock", () => {
  it("locks document.body overflow while active", () => {
    render(<Locker active={true} />);
    expect(document.body.style.overflow).toBe("hidden");
  });

  it("is a no-op while inactive", () => {
    render(<Locker active={false} />);
    expect(document.body.style.overflow).toBe("");
  });

  it("restores the PRIOR overflow value (not just '') on unmount", () => {
    document.body.style.overflow = "scroll";
    const { unmount } = render(<Locker active={true} />);
    expect(document.body.style.overflow).toBe("hidden");
    unmount();
    expect(document.body.style.overflow).toBe("scroll");
  });

  it("nested locks: the first to unmount does NOT unlock while a second is still active", () => {
    const first = render(<Locker active={true} />);
    const second = render(<Locker active={true} />);
    expect(document.body.style.overflow).toBe("hidden");
    first.unmount();
    expect(document.body.style.overflow).toBe("hidden");
    second.unmount();
    expect(document.body.style.overflow).toBe("");
  });

  it("toggling active off then back on re-locks without double-counting", () => {
    const { rerender, unmount } = render(<Locker active={true} />);
    expect(document.body.style.overflow).toBe("hidden");
    rerender(<Locker active={false} />);
    expect(document.body.style.overflow).toBe("");
    rerender(<Locker active={true} />);
    expect(document.body.style.overflow).toBe("hidden");
    unmount();
    expect(document.body.style.overflow).toBe("");
  });
});
