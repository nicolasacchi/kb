// @vitest-environment jsdom
//
// OL R1 — the ContextBar keeps four actions visible (annotate, add to list,
// share, More); anchor / copy link / split / folio / fullscreen / bare live in
// the More menu, each row keeping its `data-kb-act` and showing its key.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import ContextBar from "./ContextBar";

vi.mock("../../hooks/useAnchors", () => ({
  useAnchors: () => ({ isAnchored: () => false, toggle: vi.fn() }),
}));
vi.mock("../lists/AddToListButton", () => ({
  default: () => <button data-kb-act="add-to-list" type="button" />,
}));
vi.mock("../ShareModal", () => ({ default: () => null }));
const confirmMock = vi.hoisted(() => vi.fn());
const excludeMock = vi.hoisted(() => vi.fn());
vi.mock("../ConfirmProvider", () => ({ useConfirm: () => confirmMock }));
vi.mock("../../api/client", () => ({ excludeArtifact: excludeMock }));

afterEach(cleanup);

const acts = (root: ParentNode) =>
  Array.from(root.querySelectorAll("[data-kb-act]")).map((n) =>
    n.getAttribute("data-kb-act"),
  );

function bar(extra: Partial<React.ComponentProps<typeof ContextBar>> = {}) {
  return render(
    <MemoryRouter>
      <ContextBar
        kb="k"
        id="a1"
        title="T"
        filename="f.html"
        isIndex={false}
        sourceRelative="f.html"
        reviewActive
        onToggleAnnotate={() => {}}
        onFullscreen={() => {}}
        onSplit={() => {}}
        onToggleFolio={() => {}}
        bareUrl="https://example.test/f.html"
        {...extra}
      />
    </MemoryRouter>,
  );
}

describe("ContextBar More menu", () => {
  it("shows only annotate, add-to-list, share and More until opened", () => {
    const { container } = bar();
    expect(acts(container)).toEqual(["annotate", "add-to-list", "share", "more"]);
  });

  it("moves the other verbs into the menu with their data-kb-act and keys", () => {
    const { container } = bar();
    fireEvent.click(container.querySelector('[data-kb-act="more"]')!);
    const menu = container.querySelector('[role="menu"]')!;
    expect(acts(menu)).toEqual([
      "anchor",
      "copy-link",
      "split",
      "folio",
      "fullscreen",
      "bare",
      "exclude",
    ]);
    const key = (a: string) =>
      menu.querySelector(`[data-kb-act="${a}"] .kb-ctxbar__mi-key`)?.textContent;
    expect(key("anchor")).toBe("a");
    expect(key("fullscreen")).toBe("o");
    expect(key("bare")).toBe("b");
  });

  it("runs the action and closes; Escape closes too", () => {
    const onFullscreen = vi.fn();
    const { container } = bar({ onFullscreen });
    const more = () => container.querySelector('[data-kb-act="more"]')!;
    fireEvent.click(more());
    fireEvent.click(container.querySelector('[data-kb-act="fullscreen"]')!);
    expect(onFullscreen).toHaveBeenCalledTimes(1);
    expect(container.querySelector('[role="menu"]')).toBeNull();
    fireEvent.click(more());
    expect(container.querySelector('[role="menu"]')).not.toBeNull();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(container.querySelector('[role="menu"]')).toBeNull();
  });

  it("omits rows whose handler is absent", () => {
    const { container } = bar({
      onSplit: undefined,
      onToggleFolio: undefined,
      bareUrl: undefined,
    });
    fireEvent.click(container.querySelector('[data-kb-act="more"]')!);
    expect(acts(container.querySelector('[role="menu"]')!)).toEqual([
      "anchor",
      "copy-link",
      "fullscreen",
      "exclude",
    ]);
  });

  it("Exclude from index asks via useConfirm first and only then calls the API", async () => {
    confirmMock.mockResolvedValueOnce(false);
    const { container } = bar();
    fireEvent.click(container.querySelector('[data-kb-act="more"]')!);
    fireEvent.click(container.querySelector('[data-kb-act="exclude"]')!);
    await Promise.resolve();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(excludeMock).not.toHaveBeenCalled();

    confirmMock.mockResolvedValueOnce(true);
    excludeMock.mockResolvedValueOnce(undefined);
    fireEvent.click(container.querySelector('[data-kb-act="more"]')!);
    fireEvent.click(container.querySelector('[data-kb-act="exclude"]')!);
    await vi.waitFor(() => expect(excludeMock).toHaveBeenCalledWith("k", "f.html"));
  });
});
