import { expect, test } from "@playwright/test";
import { CALLER_FILE, KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V70-A10 ("Workspaces v0", D26) end to end: open two files, save a
/// workspace (`data-cmd="desk.save-workspace"`, `kind: "workspace"`,
/// `desk_json` capturing the exact pane + line), confirm `~workspaces`
/// lists it grouped under its ref, open it from a FRESH page (no leftover
/// `sessionStorage` — proves the restore comes from the server's own
/// `desk_json`/`spans`, not browser state left over from the save), confirm
/// both files land in the working-set strip in the saved order with the
/// saved pane's line, then add a general note and a code-anchored note and
/// confirm both survive a reload.
test.describe("workspaces", () => {
  test("save, list under the branch, open + restore, notes persist", async ({ page, context }) => {
    // --- open two files, in order ------------------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });

    await page.goto(`${BASE}/r/${REPO_NAME}/${CALLER_FILE}?line=5`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-ws-chip]")).toHaveCount(2);

    // --- save workspace ------------------------------------------------------
    await page.locator('[data-cmd="desk.save-workspace"]').click();
    await expect(page.locator("[data-kbc-save-workspace]")).toBeVisible();
    await page.locator("[data-kbc-save-workspace-name]").fill("the caller path");
    await page.locator("[data-kbc-save-workspace-ref]").fill("main");
    await page.locator("[data-kbc-save-workspace-submit]").click();
    await expect(page.locator("[data-kbc-save-workspace]")).toHaveCount(0);
    await expect(page.locator(".kbc-toast--ok")).toBeVisible();

    // --- a FRESH page/tab: no leftover sessionStorage -------------------------
    const fresh = await context.newPage();
    await fresh.goto(`${BASE}/r/${REPO_NAME}/~workspaces`);
    await expect(fresh.locator('[data-kbc-workspace-group="main"]')).toBeVisible();
    const openBtn = fresh.locator("[data-kbc-workspace-open]", { hasText: "the caller path" });
    await expect(openBtn).toBeVisible();
    await expect(
      fresh
        .locator("[data-kbc-workspace-row]")
        .filter({ has: fresh.locator("[data-kbc-workspace-open]", { hasText: "the caller path" }) })
        .locator("[data-kbc-workspace-meta]"),
    ).toContainText("2 files");

    // --- open → restore ----------------------------------------------------
    await openBtn.click();
    await expect(fresh).toHaveURL(/\?workspace=set_/);
    await expect(fresh.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });

    // Both files restored into the working set, in the saved order.
    await expect(fresh.locator("[data-kbc-ws-chip]")).toHaveCount(2);
    await expect(fresh.locator("[data-kbc-ws-chip]").nth(0)).toHaveAttribute("data-kbc-ws-chip", KNOWN_FILE);
    await expect(fresh.locator("[data-kbc-ws-chip]").nth(1)).toHaveAttribute("data-kbc-ws-chip", CALLER_FILE);

    // The active-workspace chip names the workspace.
    await expect(fresh.locator("[data-kbc-ws-workspace-chip]")).toContainText("the caller path");

    // The saved pane (caller.rs, focused at save time) landed with its own
    // saved line.
    await expect(fresh).toHaveURL(new RegExp(`${CALLER_FILE.replace(".", "\\.")}.*line=5`));

    // --- notes: a general note + a code-anchored one --------------------------
    await fresh.locator('[data-kbc-itab="notes"]').click();
    await expect(fresh.locator("[data-kbc-ws-notes]")).toBeVisible();

    await fresh.locator("[data-kbc-ws-note-body]").fill("why this workspace exists");
    await fresh.locator("[data-kbc-ws-note-save]").click();
    await expect(fresh.locator("[data-kbc-ws-notes-general] [data-kbc-ws-note-id]")).toHaveCount(1);

    await fresh.locator("[data-kbc-ws-note-body]").fill("this line matters");
    await fresh.locator("[data-kbc-ws-note-attach]").check();
    await fresh.locator("[data-kbc-ws-note-save]").click();
    await expect(
      fresh.locator(`[data-kbc-ws-notes-file="${CALLER_FILE}"] [data-kbc-ws-note-id]`),
    ).toHaveCount(1);

    // --- persists across reload -----------------------------------------------
    await fresh.reload();
    await expect(fresh.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
    await fresh.locator('[data-kbc-itab="notes"]').click();
    await expect(fresh.locator("[data-kbc-ws-notes-general] [data-kbc-ws-note-id]")).toHaveCount(1);
    await expect(
      fresh.locator(`[data-kbc-ws-notes-file="${CALLER_FILE}"] [data-kbc-ws-note-id]`),
    ).toHaveCount(1);

    await fresh.close();
  });
});
