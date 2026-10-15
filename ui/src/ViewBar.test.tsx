import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ViewBar } from "./ViewBar";
import type { Cluster, Control } from "./ViewBar";

const btn = (id: string, extra: Partial<Control> = {}): Control => ({ id, label: `L-${id}`, kind: "button", ...extra });
const twelve = (): Cluster[] => [
  { label: "Alpha", controls: ["a1", "a2", "a3", "a4", "a5", "a6"].map((i) => btn(i)) },
  { label: "Beta", controls: ["b1", "b2", "b3", "b4", "b5", "b6"].map((i) => btn(i)) },
];
const count = (html: string, needle: string) => html.split(needle).length - 1;

describe("ViewBar (server render)", () => {
  it("renders exactly maxVisible controls plus the more button", () => {
    const html = renderToString(<ViewBar clusters={twelve()} maxVisible={9} />);
    expect(count(html, "data-kb-viewbar-control=")).toBe(9);
    expect(count(html, "data-kb-viewbar-more")).toBe(1);
    expect(html).toContain('role="toolbar"');
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("L-b6");
  });

  it("renders no more button when everything fits", () => {
    const html = renderToString(<ViewBar clusters={twelve()} maxVisible={12} />);
    expect(count(html, "data-kb-viewbar-control=")).toBe(12);
    expect(html).not.toContain("data-kb-viewbar-more");
  });

  it("keeps a disabled control's reason visible", () => {
    const html = renderToString(
      <ViewBar clusters={[{ label: "A", controls: [btn("x", { disabled: true, reason: "Needs a patchset" })] }]} />,
    );
    expect(html).toContain("Needs a patchset");
    expect(html).toContain("aria-describedby");
    expect(html).toContain("disabled");
  });

  it("counts a segment as one control", () => {
    const seg: Control = {
      id: "seg",
      label: "Layout",
      kind: "segment",
      value: "a",
      options: [
        { value: "a", label: "Unified" },
        { value: "b", label: "Split" },
        { value: "c", label: "Raw" },
      ],
    };
    const html = renderToString(<ViewBar clusters={[{ label: "A", controls: [seg, btn("y")] }]} maxVisible={2} />);
    expect(html).not.toContain("data-kb-viewbar-more");
    expect(html).toContain('role="group"');
    expect(html).toContain('aria-pressed="true"');
  });

  it("spreads attrs onto the control, labels the toolbar and renders cluster legends", () => {
    const html = renderToString(
      <ViewBar
        title="Files"
        count={3}
        clusters={[{ label: "Wrap", controls: [btn("w", { kind: "toggle", active: true, key: "w", attrs: { "data-kbc-wrap-toggle": "1" } })] }]}
      />,
    );
    expect(html).toContain('data-kbc-wrap-toggle="1"');
    expect(html).toContain('aria-label="Files"');
    expect(html).toContain("<legend");
    expect(html).toContain("<kbd");
    expect(html).toContain('aria-pressed="true"');
  });
});
