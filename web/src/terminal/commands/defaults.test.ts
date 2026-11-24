// CMDSCHEMA — the shipped defaults validate clean, and nothing that is not
// read from this repo's own sources claims to be verified.

import { describe, expect, it } from "vitest";
import { DEFAULT_CATALOGUE } from "./defaults";
import { validateCatalogue } from "./schema";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);

/** The omp extension commands registered by the kb-memory plugin's hook. */
const VERIFIED = ["kb-context", "kb-desk", "kb-slate", "kb-distill"];

describe("shipped defaults", () => {
  // Twin: `shipped_defaults_validate_clean` (kb-server `terminal/commands.rs`).
  it("validate clean", () => {
    expect(validateCatalogue(DEFAULT_CATALOGUE)).toEqual([]);
  });

  it("ship claude and omp only", () => {
    expect(Object.keys(DEFAULT_CATALOGUE.harnesses).sort()).toEqual(["claude", "omp"]);
  });

  it("flag every command and chord unverified except the four omp extension commands", () => {
    const hook = nodeFs.readFileSync(here("../../../../plugins/kb-memory/hooks/kb-omp.ts"), "utf8");
    for (const name of VERIFIED) expect(hook.includes(`cmd("${name}"`), name).toBe(true);
    const seen: string[] = [];
    for (const [h, cat] of Object.entries(DEFAULT_CATALOGUE.harnesses)) {
      for (const g of cat.groups) {
        for (const item of g.items) {
          if (item.kind === "reply") continue;
          const verified = h === "omp" && item.kind === "command" && VERIFIED.includes(item.id);
          expect(item.unverified === true, `${h}/${item.id}`).toBe(!verified);
          if (verified) seen.push(item.id);
        }
      }
    }
    expect(seen.sort()).toEqual([...VERIFIED].sort());
  });

  it("every command is a paste (slash text goes in as a paste, never a run) with a description", () => {
    for (const cat of Object.values(DEFAULT_CATALOGUE.harnesses)) {
      for (const g of cat.groups) {
        for (const item of g.items) {
          expect(item.mode === undefined || item.mode === "paste", item.id).toBe(true);
          if (item.kind !== "reply") expect((item.description ?? "").length, item.id).toBeGreaterThan(0);
          if (item.kind === "command") expect(item.text?.startsWith("/") ?? false, item.id).toBe(true);
        }
      }
    }
  });

  it("each harness carries the migrated quick replies", () => {
    for (const cat of Object.values(DEFAULT_CATALOGUE.harnesses)) {
      const g = cat.groups.find((x) => x.id === "replies");
      expect(g?.items.map((i) => i.text)).toEqual(["yes, proceed", "continue", "run the tests", "commit and push"]);
    }
  });
});
