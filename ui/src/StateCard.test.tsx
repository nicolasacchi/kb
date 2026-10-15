import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { StateCard, type StateKind } from "./StateCard";

const kinds: StateKind[] = ["loading", "empty", "refused", "degraded", "mismatch"];
const html = (props: Parameters<typeof StateCard>[0]) => renderToString(createElement(StateCard, props));

describe("StateCard", () => {
  it.each(kinds)("renders %s with its kind marker", (kind) => {
    const out = html({ kind, title: "T", reason: "why", cli: "kb x" });
    expect(out).toContain(`data-kb-state-kind="${kind}"`);
    expect(out).toContain("why");
    expect(out).toContain("<code>kb x</code>");
    expect(out).toMatchSnapshot();
  });

  it("loading is a polite status; the others are labelled regions with an h2", () => {
    const l = html({ kind: "loading", title: "Loading" });
    expect(l).toContain('role="status"');
    expect(l).toContain('aria-live="polite"');
    expect(l).not.toContain("<h2");
    for (const kind of kinds.filter((k) => k !== "loading")) {
      const out = html({ kind, title: "Nothing" });
      expect(out).toContain('role="region"');
      expect(out).toMatch(/aria-labelledby="([^"]+)"/);
      const id = /aria-labelledby="([^"]+)"/.exec(out)![1];
      expect(out).toContain(`<h2 class="kb-statecard__title" id="${id}">Nothing</h2>`);
    }
  });

  it("honours headingLevel", () => {
    expect(html({ kind: "empty", title: "T", headingLevel: 3 })).toContain("<h3 ");
  });

  it("icon overrides the default", () => {
    const out = html({ kind: "empty", title: "T", icon: createElement("i", { id: "mine" }) });
    expect(out).toContain('<i id="mine">');
  });

  it("progress with total", () => {
    const out = html({ kind: "loading", title: "T", progress: { done: 3, total: 10, unit: "files" } });
    expect(out).toContain('max="10"');
    expect(out).toContain('value="3"');
    expect(out).toContain("3 / 10 files");
  });

  it("progress without total is indeterminate", () => {
    const out = html({ kind: "loading", title: "T", progress: { done: 7, unit: "files" } });
    const tag = /<progress[^>]*>/.exec(out)![0];
    expect(tag).not.toContain("value=");
    expect(tag).not.toContain("max=");
    expect(out).toContain("7 files");
    expect(out).not.toContain("/ ");
  });

  it("cli block is absent without cli and carries a copy button with it", () => {
    expect(html({ kind: "empty", title: "T" })).not.toContain("<code");
    const out = html({ kind: "empty", title: "T", cli: "kb reindex" });
    expect(out).toContain("kb-statecard__copy");
    expect(out).toContain('aria-label="Copy command"');
  });

  it("actions: href -> <a>, onClick -> <button>", () => {
    const out = html({
      kind: "empty",
      title: "T",
      actions: [
        { label: "Go", href: "/x", key: "go" },
        { label: "Do", onClick: () => {}, key: "do" },
      ],
    });
    expect(out).toContain('<a class="kb-statecard__action" href="/x" data-kb-action="go">Go</a>');
    expect(out).toContain('<button type="button" class="kb-statecard__action" data-kb-action="do">Do</button>');
  });

  it("renderLink is used for hrefs", () => {
    const out = html({
      kind: "empty",
      title: "T",
      actions: [{ label: "Go", href: "/x" }],
      renderLink: (href, children) => createElement("a", { "data-router": href }, children),
    });
    expect(out).toContain('<a data-router="/x">Go</a>');
    expect(out).not.toContain('href="/x"');
  });
});
