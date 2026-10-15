import { QueryClientProvider } from "@tanstack/react-query";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter, Route, Routes } from "react-router";
import { describe, expect, it } from "vitest";
import { queryClient } from "../api/queryClient";
import Collections from "./Collections";

function render(): string {
  return renderToStaticMarkup(
    createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(
        MemoryRouter,
        { initialEntries: ["/r/kb/~collections"] },
        createElement(
          Routes,
          null,
          createElement(Route, { path: "/r/:repo/~collections", element: createElement(Collections) }),
        ),
      ),
    ),
  );
}

describe("Collections page", () => {
  const html = render();

  it("renders the three labelled groups in order", () => {
    const groups = [...html.matchAll(/data-kbc-collections-group="([a-z]+)"/g)].map((m) => m[1]);
    expect(groups).toEqual(["sets", "boards", "instruments"]);
    expect(html).toContain(">Sets</h2>");
    expect(html).toContain(">Boards</h2>");
    expect(html).toContain(">Instruments</h2>");
  });

  it("links one card per destination to its existing route", () => {
    const tags = [...html.matchAll(/<a\b[^>]*>/g)].map((m) => m[0]);
    const cards = tags
      .filter((t) => t.includes("data-kbc-collection="))
      .map((t) => [/data-kbc-collection="([a-z]+)"/.exec(t)?.[1], /href="([^"]+)"/.exec(t)?.[1]]);
    expect(Object.fromEntries(cards)).toEqual({
      sets: "/r/kb/~sets",
      stacks: "/r/kb/~stacks",
      workspaces: "/r/kb/~workspaces",
      tours: "/r/kb/~tours",
      boards: "/r/kb/~boards",
      canvas: "/r/kb/~canvas",
      rails: "/r/kb/~rails",
      lanes: "/r/kb/~lanes",
      recipes: "/r/kb/~recipes",
      todos: "/r/kb/~todos",
      hotspots: "/r/kb/~hotspots",
      comments: "/r/kb/~comments",
    });
  });

  it("shows no count before kbc-seq/1 has answered — never an invented 0", () => {
    expect(html).not.toContain("data-kbc-collection-count");
  });
});
