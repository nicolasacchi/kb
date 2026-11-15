// T14 — the ["terms"] query pin: finite staleTime, no polling. The
// source-literal assertions fail if a refetchInterval (or a changed key /
// staleTime) is introduced: terminal BYTES never enter TanStack and
// /api/terms must not be polled (#23 exception: 15 s staleTime plus
// direct invalidation on term.created|term.ended|term.state).

import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);

describe("useTerms query options", () => {
  const src = fs.readFileSync(here("./useTerms.ts"), "utf8");

  it("uses the ['terms'] query key", () => {
    expect(src).toContain('queryKey: ["terms"]');
  });

  it("uses a finite 15 s staleTime", () => {
    expect(src).toContain("staleTime: 15_000");
  });

  it("never polls: no refetchInterval anywhere in the file", () => {
    expect(src).not.toContain("refetchInterval");
  });

  it("queries through listTerms", () => {
    expect(src).toContain("listTerms(signal)");
  });
});
