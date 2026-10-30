import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Spine, SpineCrumbs } from "./Spine";

// Node's fs via getBuiltinModule so `tsc -b` needs no Node typings (see tokens.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};
// React 18 and 19 serialise the boolean `data-kb-spine` differently.
const norm = (html: string) => html.replace('data-kb-spine="true"', 'data-kb-spine=""');

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

// OL-SIBL1 - the optional `crossing` slot.
describe("Spine crossing slot", () => {
  // Byte-for-byte what main renders for these slots (taken before the slot existed).
  const MAIN_SNAPSHOT =
    '<header data-kb-spine="" role="banner" class="kb-spine">' +
    '<div data-kb-spine-slot="switcher" class="kb-spine__slot kb-spine__switcher"><i>SW</i></div>' +
    '<div data-kb-spine-slot="scope" class="kb-spine__slot kb-spine__scope"><i>SC</i></div>' +
    '<div data-kb-spine-slot="search" class="kb-spine__slot kb-spine__search"><i>SE</i></div>' +
    '<div data-kb-spine-slot="right" class="kb-spine__slot kb-spine__right"><i>RI</i></div>' +
    "</header>";

  it("leaves the DOM identical to main when crossing is undefined", () => {
    expect(norm(renderToString(slots))).toBe(MAIN_SNAPSHOT);
    expect(norm(renderToString(<Spine {...{ crossing: undefined }} switcher={<i>SW</i>} scope={<i>SC</i>} search={<i>SE</i>} right={<i>RI</i>} />))).toBe(MAIN_SNAPSHOT);
  });

  it("renders the slot between search and right when defined", () => {
    const html = renderToString(
      <Spine switcher={<i>SW</i>} scope={<i>SC</i>} search={<i>SE</i>} crossing={<i>CR</i>} right={<i>RI</i>} />,
    );
    const order = ["switcher", "scope", "search", "crossing", "right"].map((s) =>
      html.indexOf(`data-kb-spine-slot="${s}"`),
    );
    expect(order.every((i) => i >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(html).toContain('class="kb-spine__slot kb-spine__crossing"');
  });

  it("never hides the crossing slot at the mobile breakpoint", () => {
    const css = fs.readFileSync(new URL("./spine.css", import.meta.url), "utf8");
    const start = css.indexOf("@media (max-width: 860px)");
    expect(start).toBeGreaterThan(-1);
    const block = css.slice(start, css.indexOf("\n}\n", start));
    expect(block).not.toContain("crossing");
    expect(css).not.toMatch(/crossing[^{]*\{[^}]*display:\s*none/);
  });
});
