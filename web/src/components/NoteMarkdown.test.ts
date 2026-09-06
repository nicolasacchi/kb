import { describe, it, expect } from "vitest";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import NoteMarkdown from "./NoteMarkdown";
import type { ResolvedLink } from "../api/notes";

// Regression test for the native note render going through the REAL
// react-markdown + unified pipeline (not just the isolated hast passes that
// wikilink.test.ts/callout.test.ts pin). A prior bug passed the pre-applied
// `rehypeWikilinks(resolve)` transformer directly as a rehype plugin; unified
// then invoked that transformer as an attacher at freeze with no tree, so
// `walk(undefined)` threw "Cannot read properties of undefined (reading
// 'children')" and crashed EVERY note view. The fix wires it as the
// `[plugin, options]` form; these renders exercise that integration.
function render(body: string, links?: ResolvedLink[]): string {
  return renderToStaticMarkup(
    React.createElement(
      MemoryRouter,
      null,
      React.createElement(NoteMarkdown, { body, links }),
    ),
  );
}

describe("NoteMarkdown — react-markdown integration", () => {
  it("renders plain markdown without crashing", () => {
    const html = render("# Hello\n\nsome text\n");
    expect(html).toContain("<h1>Hello</h1>");
    expect(html).toContain("some text");
  });

  it("renders GFM task lists (stamps interactive checkboxes)", () => {
    const html = render("- [ ] first task\n- [x] second task\n");
    expect(html).toContain('type="checkbox"');
    // two task items → two checkboxes
    expect(html.match(/type="checkbox"/g)?.length).toBe(2);
  });

  it("renders an Obsidian callout natively", () => {
    const html = render("> [!note] Heads up\n> body line\n");
    expect(html).toContain("kb-callout");
    expect(html).toContain("Heads up");
  });

  it("renders an unresolved wikilink as a muted span (no map)", () => {
    const html = render("see [[Other Note]] here\n");
    expect(html).toContain("kb-wikilink");
    expect(html).toContain("Other Note");
  });

  it("renders a resolved wikilink as an internal link", () => {
    const links: ResolvedLink[] = [
      {
        target: "Other Note",
        state: "resolved",
        kb: "canon",
        source_relative: "other-note.md",
        title: "Other Note",
      } as ResolvedLink,
    ];
    const html = render("see [[Other Note]] here\n", links);
    expect(html).toContain("kb-wikilink");
    expect(html).toContain('href="/a/canon/other-note.md"');
  });
});
