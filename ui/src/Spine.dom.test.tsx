// @vitest-environment jsdom
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vitest";
import { Spine } from "./Spine";

describe("Spine menu button (jsdom)", () => {
  it("calls onMenu exactly once per click", () => {
    const onMenu = vi.fn();
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    flushSync(() => root.render(<Spine onMenu={onMenu} menuOpen={false} />));
    const btn = host.querySelector<HTMLButtonElement>("[data-kb-spine-menu]");
    expect(btn).not.toBeNull();
    btn!.click();
    expect(onMenu).toHaveBeenCalledTimes(1);
    flushSync(() => root.unmount());
    host.remove();
  });
});
