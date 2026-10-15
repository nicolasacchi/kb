import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { RepoListEntry } from "../api/types";
import {
  AppearanceSection,
  KeyboardSection,
  RepositoriesSection,
  ReviewStoreBlock,
  SiblingSection,
  uptimeLabel,
} from "./Settings";
import NavSheet from "../components/NavSheet";
import { settingsUrl } from "../lib/codeUrl";

const repo: RepoListEntry = {
  name: "fixture",
  path: "/repos/fixture",
  file_count: 3,
  symbol_count: 9,
  head: { detached: false, unborn: false, branch: "main", sha: "0123456789abcdef" },
  watcher: "watching",
  intel: null,
  intel_providers: [{ provider: "ruby-lsp", langs: ["ruby"], alive: true, server_version: null }],
  catching_up: true,
  settled_at: null,
};

function markup(el: ReturnType<typeof h>): string {
  const client = new QueryClient();
  try {
    return renderToStaticMarkup(h(QueryClientProvider, { client }, el));
  } finally {
    client.clear();
  }
}

describe("Settings (OL-C8)", () => {
  it("every section carries its plain <h2> heading", () => {
    const html = [
      markup(h(AppearanceSection)),
      markup(h(KeyboardSection, {})),
      markup(h(RepositoriesSection, { repos: [repo] })),
      markup(h(ReviewStoreBlock, { repos: [], remote: true })),
      markup(h(SiblingSection, { identity: undefined })),
    ].join("\n");
    for (const t of ["Appearance", "Keyboard", "Repositories", "Review store", "Sibling"]) {
      expect(html).toContain(`<h2 class="kbc-settings__h2">${t}</h2>`);
    }
  });

  it("the theme picker is rendered inline, not as a popover", () => {
    const html = markup(h(AppearanceSection));
    expect(html).toContain("kbc-themepicker--inline");
    expect(html).toContain('role="group"');
    expect(html).not.toContain("close theme picker");
  });

  it("the remote-mutations line keeps its attribute, and is absent when the daemon is silent", () => {
    const on = markup(h(ReviewStoreBlock, { repos: [], remote: true }));
    expect(on).toContain('data-kbc-home-remote-mutations="true"');
    expect(on).toContain("Remote review mutations: on");
    expect(markup(h(ReviewStoreBlock, { repos: [], remote: false }))).toContain(
      "Remote review mutations: off",
    );
    expect(markup(h(ReviewStoreBlock, { repos: [], remote: undefined }))).not.toContain(
      "Remote review mutations",
    );
  });

  it("repositories show facts as rows, intel providers, and catching_up as an inline degraded card", () => {
    const html = markup(h(RepositoriesSection, { repos: [repo] }));
    expect(html).toContain("3 files · 9 symbols");
    expect(html).toContain("main @ 01234567");
    expect(html).toContain("ruby-lsp (ruby) · running");
    expect(html).toContain("Indexing the mirror");
  });

  it("sibling is honest when the daemon reports nothing", () => {
    expect(markup(h(SiblingSection, { identity: { name: "kb-code", version: "1" } }))).toContain(
      "not reported by this daemon",
    );
    const off = markup(
      h(SiblingSection, {
        identity: { name: "kb-code", version: "1", kb_daemon_enabled: false, kb_public_url: "" },
      }),
    );
    expect(off).toContain("not configured");
    expect(off).toContain("none");
  });

  it("uptime is null for an absent or unparseable start", () => {
    expect(uptimeLabel(undefined)).toBeNull();
    expect(uptimeLabel("not a date")).toBeNull();
    expect(uptimeLabel("2026-01-01T00:00:00Z", Date.parse("2026-01-02T03:00:00Z"))).toBe("1d 3h");
  });

  it("the mobile NavSheet carries a Settings row pointing at the builder's URL", () => {
    const html = markup(
      h(MemoryRouter, null, h(NavSheet, { repo: null, open: true, onClose: () => {} })),
    );
    expect(html).toContain('data-kbc-nav-row="settings"');
    expect(html).toContain(`href="${settingsUrl()}"`);
  });
});
