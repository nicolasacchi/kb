import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// MI-W4 — the memory-instrument visualization suite: health-timeline
// sparklines (W4.1), the agent's-eye simulator + quadrant scatter (W4.2),
// the lineage viewer (W4.3), and the hygiene queue (W4.4). Uses the SAME
// seeded corpus as spa-memory.spec.ts ("Prefers tabs" salience 0.9,
// "Deploy pipeline" salience 0.6, both slow-decay) — assertions here are
// deliberately tolerant of that corpus's exact salience/recall numbers
// (this suite runs after spa-memory.spec.ts's own salience-edit/forget
// tests may have already mutated it) and instead verify the SURFACES
// render and behave, not a specific triage/rollup membership.
test.describe("memory view — MI-W4 visualization suite", () => {
  test("each row renders a decay-projection sparkline in the health column", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const row = page.getByTestId("memory-item").first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row.getByTestId("decay-sparkline")).toBeVisible();
  });

  test("the DecayRail shows a 'fastest-decaying scores' rollup panel", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    await expect(page.getByTestId("memory-view")).toBeVisible();
    await expect(page.getByTestId("decay-rollup")).toBeVisible();
    await expect(page.getByTestId("decay-rollup")).toContainText(/Fastest-decaying scores/);
  });

  test("the salience × recall quadrant scatter renders points for the seeded memories", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const scatter = page.getByTestId("recall-quadrant");
    await expect(scatter).toBeVisible();
    await expect(scatter.getByTestId("recall-quadrant-point").first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test("clicking a quadrant point focuses it and shows a link to the memory", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const scatter = page.getByTestId("recall-quadrant");
    const point = scatter.getByTestId("recall-quadrant-point").first();
    await expect(point).toBeVisible({ timeout: 10_000 });
    await point.click();
    await expect(scatter.getByTestId("recall-quadrant-focus")).toBeVisible();
  });

  test("the hygiene queue section renders (empty or populated) without erroring", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const hygiene = page.getByTestId("hygiene-queue");
    await expect(hygiene).toBeVisible();
    // Either the empty state or at least one queued item — never both
    // absent, which would mean the fetch silently hung.
    await expect(
      hygiene.getByTestId("hygiene-queue-empty").or(hygiene.getByTestId("hygiene-queue-item")),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("the agent's-eye simulator previews the exact kb-recall injection block", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const sim = page.getByTestId("agent-eye-simulator");
    await expect(sim).toBeVisible();
    await expect(sim.getByTestId("agent-eye-simulator-idle")).toBeVisible();

    await sim.getByTestId("agent-eye-simulator-input").fill("quetzal pipeline deploys");

    // Debounced (300ms) then a real recall round-trip — either a rendered
    // injection block or an honest "no hits" message, never a stuck
    // loading spinner.
    await expect(
      sim
        .getByTestId("agent-eye-simulator-block")
        .or(sim.getByTestId("agent-eye-simulator-nohits")),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("opens the lineage viewer for a memory and shows it as the start node", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const row = page.getByTestId("memory-item").filter({ hasText: "Prefers tabs" });
    await expect(row).toBeVisible({ timeout: 10_000 });

    await row.getByTitle("view supersede lineage").click();

    const dialog = page.getByTestId("lineage-viewer");
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    await expect(dialog.getByTestId("lineage-start")).toContainText("Prefers tabs");
    // A never-superseded, never-superseding fact says so on both sides
    // rather than rendering an empty chain silently.
    await expect(dialog).toContainText(/supersedes nothing|nothing supersedes/);

    await dialog.getByRole("button", { name: "close" }).click();
    await expect(dialog).toBeHidden();
  });
});
