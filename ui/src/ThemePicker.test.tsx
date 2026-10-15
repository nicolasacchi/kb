// OL F2 — the shared ThemePicker under both React majors (renderToString, node
// env): ordering, the inline/floating split, the receipt and the slots.
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import ThemePicker, { type ThemePickerController } from "./ThemePicker";
import { KBC_THEME_FAMILIES } from "./themes/registry.gen";

function controller(family = "kb-classic", appearance: "light" | "dark" | "system" = "dark") {
  return {
    appearance: () => appearance,
    family: () => family,
    setAppearance: vi.fn(),
    setFamily: vi.fn(),
    preview: vi.fn(),
    revert: vi.fn(),
  } satisfies ThemePickerController;
}

function rowOrder(html: string): string[] {
  return [...html.matchAll(/data-kbc-theme-family="([^"]+)"/g)].map((m) => m[1]);
}

describe("ThemePicker", () => {
  it("lists every registry family in registry order by default", () => {
    const html = renderToString(<ThemePicker controller={controller()} />);
    expect(rowOrder(html)).toEqual(KBC_THEME_FAMILIES.map((f) => f.family));
  });

  it("leadFirst lists the lead family (kb-classic) first, the rest in registry order", () => {
    const html = renderToString(<ThemePicker controller={controller()} leadFirst />);
    const order = rowOrder(html);
    expect(order[0]).toBe("kb-classic");
    expect(order.slice(1)).toEqual(
      KBC_THEME_FAMILIES.map((f) => f.family).filter((f) => f !== "kb-classic"),
    );
  });

  it("marks the committed family selected and states the receipt for it", () => {
    const html = renderToString(<ThemePicker controller={controller("kb-classic")} />);
    expect(html).toMatch(/data-kbc-theme-family="kb-classic"[^>]*aria-selected="true"/);
    expect(html).toContain("kb classic Dark");
    expect(html).toContain("nudged for AA");
  });

  it("floating: dialog role, close button and the key hint; inline: none of them", () => {
    const floating = renderToString(<ThemePicker controller={controller()} />);
    expect(floating).toContain('role="dialog"');
    expect(floating).toContain("close theme picker");
    expect(floating).toContain("Esc revert");
    const inline = renderToString(<ThemePicker controller={controller()} inline />);
    expect(inline).toContain("kbc-themepicker--inline");
    expect(inline).not.toContain('role="dialog"');
    expect(inline).not.toContain("close theme picker");
    expect(inline).not.toContain("Esc revert");
  });

  it("renders the three appearances, pressing the persisted one", () => {
    const html = renderToString(<ThemePicker controller={controller("kb-classic", "light")} />);
    expect(html).toMatch(/data-kbc-theme-appearance="light"[^>]*aria-pressed="true"/);
    expect(html).toMatch(/data-kbc-theme-appearance="dark"[^>]*aria-pressed="false"/);
    expect(html).toContain('data-kbc-theme-appearance="system"');
  });

  it("renders host slots: sample, extras (with the staged family) and the builtin receipt", () => {
    const html = renderToString(
      <ThemePicker
        controller={controller("kbc")}
        host={{ builtinFamily: "kbc" }}
        builtinReceipt={<em>built-in receipt</em>}
        sample={<div data-sample />}
        extras={({ staged, appearance }) => <div data-extras={`${staged}/${appearance}`} />}
      />,
    );
    expect(html).toContain("data-sample");
    expect(html).toContain('data-extras="kbc/dark"');
    expect(html).toContain("<em>built-in receipt</em>");
  });
});
