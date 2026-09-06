import { test, expect } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { PORT } from "./helpers";

// RLc — the Claude Code flow: `kb list import` materializes a curated
// list from one Markdown document; an already-open /lists page shows it
// live over SSE (no reload); `kb list export` round-trips. Needs the
// fast-profile CLI (the ci-e2e recipe builds it next to kb-server).

const BASE = `http://127.0.0.1:${PORT}`;
const KB_BIN = resolve(__dirname, "..", "..", "target", "fast", "kb");

function kbCli(args: string[], input?: string): string {
  return execFileSync(KB_BIN, [...args, "--daemon", BASE], {
    input,
    encoding: "utf-8",
  });
}

test("CLI markdown import appears live on /lists; export round-trips", async ({
  page,
  request,
}) => {
  const title = `RT cli ${Date.now()}`;
  const doc = `# ${title}

> Two canon picks, in order.

1. [ ] [the field guide](multi-page.html)
   why: errors first
2. [x] [the sink](kitchen-sink.html)
`;
  const tmp = join(mkdtempSync(join(tmpdir(), "kb-rl-")), "list.md");
  writeFileSync(tmp, doc, "utf-8");

  // Watch the index BEFORE importing — the card must appear without a
  // reload (list.created/updated SSE → bridge).
  await page.goto(`${BASE}/lists`);
  const out = kbCli(["list", "import", tmp, "--kb", "canon"]);
  expect(out).toContain("✓ imported 2 entries");
  const card = page.locator(".kb-list-card", { hasText: title });
  await expect(card).toBeVisible({ timeout: 10_000 });
  await expect(card.locator(".kb-list-card__stats")).toContainText("1/2 read");

  // Export reproduces the document's substance (title, order, override
  // checkbox, note) and a dry-run reimport targets the same list.
  const exported = kbCli(["list", "export", title, "--kb", "canon"]);
  expect(exported.startsWith(`# ${title}\n`)).toBe(true);
  expect(exported).toContain("> Two canon picks, in order.");
  const lines = exported.split("\n");
  const first = lines.find((l) => l.startsWith("1. "));
  const second = lines.find((l) => l.startsWith("2. "));
  expect(first).toContain("[ ]");
  expect(first).toContain("multi-page.html");
  expect(second).toContain("[x]");
  expect(second).toContain("kitchen-sink.html");
  expect(exported).toContain("   why: errors first");
  expect(exported).toContain("<!-- kb-list ");

  const dry = kbCli(
    ["list", "import", "-", "--kb", "canon", "--dry-run"],
    exported,
  );
  expect(dry).toContain("would import 2 entries");
  expect(dry).toContain("round-trip into");

  // Cleanup via the CLI too.
  const lists = await request.get(`${BASE}/api/lists`);
  const body = (await lists.json()) as { lists: { id: string; title: string }[] };
  const mine = body.lists.find((l) => l.title === title);
  expect(mine).toBeTruthy();
  kbCli(["list", "delete", mine!.id, "--yes", "--kb", "canon"]);
  await expect(page.locator(".kb-list-card", { hasText: title })).toHaveCount(
    0,
    { timeout: 10_000 },
  );
});
