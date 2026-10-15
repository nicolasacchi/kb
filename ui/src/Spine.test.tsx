import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Spine, SpineCrumbs } from "./Spine";

const slots = (
  <Spine switcher={<i>SW</i>} scope={<i>SC</i>} search={<i>SE</i>} right={<i>RI</i>} />
);

describe("Spine", () => {
  it("renders a banner header with the four slots in order", () => {
    const html = renderToString(slots);
    expect(html).toContain("data-kb-spine");
    expect(html).toContain('role="banner"');
    const order = ["switcher", "scope", "search", "right"].map((s) =>
      html.indexOf(`data-kb-spine-slot="${s}"`),
    );
    expect(order.every((i) => i >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(html.indexOf("SW")).toBeLessThan(html.indexOf("SC"));
    expect(html.indexOf("SC")).toBeLessThan(html.indexOf("SE"));
    expect(html.indexOf("SE")).toBeLessThan(html.indexOf("RI"));
  });

  it("omits the menu button without onMenu and renders it with onMenu", () => {
    expect(renderToString(slots)).not.toContain("data-kb-spine-menu");
    const html = renderToString(<Spine onMenu={() => {}} menuControls="nav-sheet" />);
    expect(html).toContain("data-kb-spine-menu");
    expect(html).toContain('aria-controls="nav-sheet"');
    expect(html).toContain('aria-label="Menu"');
    expect(html).toContain("☰");
  });

  it("mirrors menuOpen into aria-expanded and honours menuLabel/menuIcon", () => {
    const open = renderToString(<Spine onMenu={() => {}} menuOpen />);
    expect(open).toContain('aria-expanded="true"');
    const closed = renderToString(
      <Spine onMenu={() => {}} menuOpen={false} menuLabel="Navigation" menuIcon={<b>ICON</b>} />,
    );
    expect(closed).toContain('aria-expanded="false"');
    expect(closed).toContain('aria-label="Navigation"');
    expect(closed).toContain("ICON");
    expect(closed).not.toContain("☰");
  });
});

describe("SpineCrumbs", () => {
  const crumbs = [
    { label: "Home", href: "/" },
    { label: "Docs", href: "/docs" },
    { label: "Page", current: true },
  ];

  it("links earlier crumbs and marks the last aria-current", () => {
    const html = renderToString(<SpineCrumbs crumbs={crumbs} />);
    expect(html).toContain('<a href="/">Home</a>');
    expect(html).toContain('<a href="/docs">Docs</a>');
    expect(html).toContain('<span aria-current="page">Page</span>');
    expect(html.match(/aria-current/g)).toHaveLength(1);
  });

  it("uses renderLink when given", () => {
    const html = renderToString(
      <SpineCrumbs
        crumbs={crumbs}
        renderLink={(href, children) => (
          <a data-router-link href={href}>
            {children}
          </a>
        )}
      />,
    );
    expect(html).toContain("data-router-link");
    expect(html.match(/data-router-link/g)).toHaveLength(2);
  });

  it("tags only middle crumbs as collapsible", () => {
    const five = ["a", "b", "c", "d", "e"].map((l, i) => ({ label: l, href: `/${l}`, current: i === 4 }));
    const html = renderToString(<SpineCrumbs crumbs={five} />);
    expect(html.match(/data-kb-crumb-middle/g)).toHaveLength(3);
  });
});
