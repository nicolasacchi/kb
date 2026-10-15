// @vitest-environment jsdom
// OL-C11 — a route's crumbs reach the top bar's scope slot through a portal,
// and leave with the route.
import { describe, expect, it } from "vitest";
import { createElement, Fragment } from "react";
import { createRoot } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { renderToString } from "react-dom/server";
import { ScopePortal, ScopeSlot, SCOPE_SLOT_ID } from "./ScopeSlot";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("ScopeSlot / ScopePortal", () => {
  it("renders an empty host the top bar can hide", () => {
    const html = renderToString(createElement(ScopeSlot));
    expect(html).toContain(`id="${SCOPE_SLOT_ID}"`);
    expect(html).toContain("data-kbc-scope-slot");
  });

  it("portals children into the host, and removes them on unmount", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    const crumbs = createElement("nav", { "aria-label": "Breadcrumb" }, "a / b");
    act(() => {
      root.render(createElement(Fragment, null, createElement(ScopeSlot), createElement(ScopePortal, null, crumbs)));
    });
    const slot = document.getElementById(SCOPE_SLOT_ID)!;
    expect(slot.querySelector("nav[aria-label=Breadcrumb]")?.textContent).toBe("a / b");
    act(() => {
      root.render(createElement(ScopeSlot));
    });
    expect(slot.children.length).toBe(0);
    act(() => root.unmount());
    host.remove();
  });
});
