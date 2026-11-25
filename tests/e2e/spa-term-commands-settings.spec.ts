import { test, expect, type Page, type Request } from "@playwright/test";
import { BASE } from "./helpers";

// CMDSETTINGS - Settings > Terminal commands. The default e2e daemon has no
// terminal tier, so the identity and `/api/terms/commands` are answered by a
// small stateful stand-in that lives for the whole page (it survives a
// reload, which is what the persistence assertion needs). The real route's
// round trip is covered by the Rust store tests. CHROMIUM desktop project.

interface OvItem {
  id: string;
  [k: string]: unknown;
}
interface OvGroup {
  id: string;
  label: string;
  items: OvItem[];
}

async function installCommandsApi(page: Page): Promise<{ puts: Request[] }> {
  const puts: Request[] = [];
  let rev = 0;
  let groups: OvGroup[] = [];
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
  const body = () => ({
    rev,
    harnesses: {
      claude: {
        groups: [
          {
            id: "slash",
            label: "Slash commands",
            origin: groups.length ? "overlay" : "default",
            items: [
              { id: "help", kind: "command", label: "/help", text: "/help", mode: "paste", origin: "default" },
              ...groups.flatMap((g) => g.items.map((i) => ({ ...i, origin: "overlay" }))),
            ],
          },
        ],
      },
      omp: { groups: [] },
    },
    overlay: { harnesses: groups.length ? { claude: { groups } } : {}, hidden: {} },
    hidden: {},
  });
  await page.route("**/api/terms/commands", async (route) => {
    const req = route.request();
    if (req.method() === "PUT") {
      puts.push(req);
      const next = JSON.parse(req.postData() ?? "{}") as { harnesses?: { claude?: { groups: OvGroup[] } } };
      groups = next.harnesses?.claude?.groups ?? [];
      rev += 1;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      headers: { etag: `"${rev}"` },
      body: JSON.stringify(body()),
    });
  });
  return { puts };
}

const SECTION = "#terminal-commands";

test.describe("CMDSETTINGS terminal commands editor", () => {
  test("add /zz test to claude, reload, it persists with the added badge", async ({ page }) => {
    const api = await installCommandsApi(page);
    await page.goto(`${BASE}/settings#terminal-commands`);
    await expect(page.locator(SECTION)).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId("tcs-item-help")).toContainText("default");

    await page.getByRole("button", { name: "Add item" }).click();
    await page.getByLabel("Label", { exact: true }).fill("/zz test");
    await page.getByLabel("Text", { exact: true }).fill("/zz test");
    await page.getByRole("button", { name: "Save item" }).click();
    await expect(page.getByTestId("tcs-item-u-zz-test")).toContainText("added");

    expect(api.puts).toHaveLength(1);
    const h = api.puts[0].headers();
    expect(h["if-match"]).toBe('"0"');
    expect(h["x-kb-intent"]).toBe("terminal");

    await page.reload();
    await expect(page.getByTestId("tcs-item-u-zz-test")).toContainText("added", { timeout: 20_000 });
    await expect(page.getByTestId("tcs-item-u-zz-test")).toContainText("/zz test");
  });

  test.describe("phone width", () => {
    test.use({ viewport: { width: 390, height: 844 } });

    test("single column, no horizontal scroll, every control at least 40px tall", async ({ page }) => {
      await installCommandsApi(page);
      await page.goto(`${BASE}/settings#terminal-commands`);
      await expect(page.locator(SECTION)).toBeVisible({ timeout: 20_000 });
      await page.getByRole("button", { name: "Add item" }).click();
      await expect(page.getByTestId("tcs-form")).toBeVisible();

      const tooSmall = await page.evaluate(() => {
        const bad: string[] = [];
        const q = '.tcs button, .tcs select, .tcs textarea, .tcs input:not([type="checkbox"]):not([type="file"]), .tcs label.tcs__check';
        for (const el of Array.from(document.querySelectorAll<HTMLElement>(q))) {
          if (el.closest(".tcs__sr") || el.classList.contains("tcs__sr")) continue;
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) continue;
          if (r.height < 40) bad.push(`${el.tagName}.${el.className}:${Math.round(r.height)}`);
        }
        return bad;
      });
      expect(tooSmall).toEqual([]);

      const wide = await page.evaluate(() => {
        const out: string[] = [];
        for (const el of Array.from(document.querySelectorAll<HTMLElement>(".tcs, .tcs *"))) {
          if (el.classList.contains("tcs__sr")) continue;
          const r = el.getBoundingClientRect();
          if (r.width > 0 && r.right > window.innerWidth + 1) out.push(`${el.tagName}.${el.className}:${Math.round(r.right)}`);
        }
        return out;
      });
      expect(wide).toEqual([]);
    });
  });
});
