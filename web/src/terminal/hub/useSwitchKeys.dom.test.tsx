// @vitest-environment jsdom
//
// HUBSWITCH — the `terminals` key scope's dispatcher.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { useSwitchKeys } from "./useSwitchKeys";
import { fixtureTerms, ordered } from "./switchTestKit";

const ROWS = ordered(fixtureTerms()); // e a b c | d ; from b: a b e c

function Probe({ current }: { current: string | null }) {
  useSwitchKeys(ROWS, current);
  const l = useLocation();
  return (
    <>
      <div data-testid="where">{l.pathname}</div>
      <input data-testid="field" />
      <textarea data-testid="area" />
      <div data-testid="ce" contentEditable suppressContentEditableWarning>
        x
      </div>
      <div className="xterm" data-testid="xterm">
        <textarea className="xterm-helper-textarea" data-testid="xterm-input" />
      </div>
      <div role="dialog" data-testid="dlg">
        <button type="button" data-testid="dlg-btn">
          ok
        </button>
      </div>
      <button type="button" data-testid="plain">
        plain
      </button>
    </>
  );
}

function mount(current: string | null = "b") {
  return render(
    <MemoryRouter initialEntries={[current ? `/terminals/${current}` : "/terminals"]}>
      <Routes>
        <Route path="*" element={<Probe current={current} />} />
      </Routes>
    </MemoryRouter>,
  );
}
const where = (): string => screen.getByTestId("where").textContent ?? "";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("useSwitchKeys", () => {
  it("] goes to the next live terminal and [ to the previous", () => {
    mount("b");
    fireEvent.keyDown(screen.getByTestId("plain"), { key: "]" });
    expect(where()).toBe("/terminals/e");
  });
  it("[ goes to the previous live terminal", () => {
    mount("b");
    fireEvent.keyDown(document.body, { key: "[" });
    expect(where()).toBe("/terminals/a");
  });
  it("n jumps to the oldest needs terminal, N likewise from a non-needs row", () => {
    mount("b");
    fireEvent.keyDown(document.body, { key: "n" });
    expect(where()).toBe("/terminals/e");
  });
  it("n then steps among needs rows", () => {
    mount("e");
    fireEvent.keyDown(document.body, { key: "n" });
    expect(where()).toBe("/terminals/a");
    cleanup();
    mount("a");
    fireEvent.keyDown(document.body, { key: "N" });
    expect(where()).toBe("/terminals/e");
  });
  it("calls preventDefault only when it navigates", () => {
    mount("b");
    const acted = new KeyboardEvent("keydown", { key: "]", bubbles: true, cancelable: true });
    document.body.dispatchEvent(acted);
    expect(acted.defaultPrevented).toBe(true);
    const other = new KeyboardEvent("keydown", { key: "x", bubbles: true, cancelable: true });
    document.body.dispatchEvent(other);
    expect(other.defaultPrevented).toBe(false);
  });

  it.each([
    ["an input", "field"],
    ["a textarea", "area"],
    ["a contenteditable", "ce"],
    ["the xterm helper textarea", "xterm-input"],
    ["the xterm container", "xterm"],
    ["a dialog", "dlg-btn"],
  ])("does nothing with focus in %s", (_n, id) => {
    mount("b");
    const ev = new KeyboardEvent("keydown", { key: "]", bubbles: true, cancelable: true });
    screen.getByTestId(id).dispatchEvent(ev);
    expect(where()).toBe("/terminals/b");
    expect(ev.defaultPrevented).toBe(false);
  });

  it.each([
    ["ctrl", { ctrlKey: true }],
    ["alt", { altKey: true }],
    ["meta", { metaKey: true }],
    ["repeat", { repeat: true }],
    ["composition", { isComposing: true }],
  ])("ignores %s", (_n, init) => {
    mount("b");
    fireEvent.keyDown(document.body, { key: "]", ...init });
    expect(where()).toBe("/terminals/b");
  });

  it("ignores an event another handler already consumed", () => {
    mount("b");
    const ev = new KeyboardEvent("keydown", { key: "]", bubbles: true, cancelable: true });
    ev.preventDefault();
    document.body.dispatchEvent(ev);
    expect(where()).toBe("/terminals/b");
  });

  it("does nothing when there is nowhere to go", () => {
    mount("b");
    fireEvent.keyDown(document.body, { key: "x" });
    expect(where()).toBe("/terminals/b");
  });

  it("removes its listener on unmount", () => {
    const add = vi.spyOn(document, "addEventListener");
    const remove = vi.spyOn(document, "removeEventListener");
    const { unmount } = mount("b");
    const added = add.mock.calls.filter((c) => c[0] === "keydown");
    expect(added.length).toBeGreaterThan(0);
    unmount();
    const removed = remove.mock.calls.filter((c) => c[0] === "keydown").map((c) => c[1]);
    for (const [, fn] of added) expect(removed).toContain(fn);
  });
});
