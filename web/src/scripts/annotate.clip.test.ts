// @vitest-environment jsdom
//
// OL P4 — a block-anchored highlight band ends where the block's VISIBLE part
// ends. A table taller than its `max-height; overflow:auto` wrapper used to
// get a band the height of the whole table, painted over the next heading.
// jsdom has no layout, so the rects are stubbed; the geometry rule under test
// is the annotator's own (paintAll → rectsFor → clip → addHighlight).
import { afterEach, describe, expect, it, vi } from "vitest";

const w = window as unknown as Record<string, unknown>;

function rect(left: number, top: number, right: number, bottom: number) {
  return {
    left,
    top,
    right,
    bottom,
    x: left,
    y: top,
    width: right - left,
    height: bottom - top,
    toJSON: () => ({}),
  } as DOMRect;
}

function mount(wrapperBottom: number) {
  document.body.innerHTML = `
    <div id="wrap" style="overflow-y:auto;max-height:240px">
      <table id="bench"><tbody><tr><td>x</td></tr></tbody></table>
    </div>
    <h2 id="next">Next heading</h2>`;
  // Table box is 320px tall; the wrapper only shows `wrapperBottom - 100`.
  const wrap = document.getElementById("wrap")!;
  const table = document.getElementById("bench")!;
  vi.spyOn(wrap, "getBoundingClientRect").mockReturnValue(
    rect(0, 100, 600, wrapperBottom),
  );
  vi.spyOn(table, "getBoundingClientRect").mockReturnValue(
    rect(0, 100, 600, 420),
  );
  const orig = window.getComputedStyle.bind(window);
  vi.spyOn(window, "getComputedStyle").mockImplementation((el: Element) => {
    const cs = orig(el);
    if (el === wrap) {
      return { overflowX: "visible", overflowY: "auto" } as CSSStyleDeclaration;
    }
    return cs;
  });
  w.__KB_COMMENTS = {
    v: 1,
    etag: null,
    file: {
      schema: "kb-comments/1",
      artifact: { id: "a", title: "", kb: "k" },
      generatedAt: "2026-01-01T00:00:00Z",
      comments: [
        {
          id: "c1",
          status: "open",
          file: "f",
          fileLabel: "f",
          anchor: { kind: "section", id: "bench" },
          author: "you",
          body: "b",
          createdAt: "2026-01-01T00:00:00Z",
          editedAt: null,
          replies: [],
        },
      ],
    },
  };
}

afterEach(() => {
  document.body.innerHTML = "";
  delete w.__KB_COMMENTS;
  vi.restoreAllMocks();
  vi.resetModules();
});

describe("annotator highlight bands", () => {
  it("stop at the clipping wrapper instead of covering the next heading", async () => {
    mount(340);
    await import("./annotate");
    const hl = document.querySelector<HTMLElement>(".kb-annot-hl")!;
    expect(hl).toBeTruthy();
    expect(hl.style.top).toBe("100px");
    expect(hl.style.height).toBe("240px"); // not the table's 320px
  });

  it("keep the block's own height when nothing clips it", async () => {
    mount(900);
    await import("./annotate");
    const hl = document.querySelector<HTMLElement>(".kb-annot-hl")!;
    expect(hl.style.height).toBe("320px");
  });
});
