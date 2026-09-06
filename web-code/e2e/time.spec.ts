import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_COMMIT_SUBJECT, FEATURE_FILE, KNOWN_FILE, KNOWN_SYMBOL } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// Phase C-SPA ("kb-code v2 — The Operable Reader", Wave C) end to end: the
/// commit page hub, the branches page's ahead/behind → compare click-
/// through, the reader's History inspector tab, and `[c`/`]c` time-
/// stepping. Runs against the SAME fixture repo every other e2e spec
/// shares, extended additively by `feature-x` (`fixture-repo.ts`'s own
/// doc) — `main`'s own commit/file set is untouched, so this suite makes
/// no ordering assumption against the others.
///
/// Phase C7 note: `main`'s HEAD is NO LONGER the initial fixture commit —
/// `fixture-repo.ts` now adds two more commits on `main` for `story.spec.ts`
/// (see that file's own doc on why they can't live on a side branch). This
/// spec never read "main's HEAD" for its own sake, only as a stand-in for
/// "the initial commit" — `initialCommitSha` below gets there a more
/// durable way (via a file `story.rs`'s commits never touch), and the
/// branches-page test's `behind` assertion is updated to account for the
/// two extra main-only commits.

interface FileHistoryBody {
  entries: Array<{ sha: string }>;
}

/// The fixture's initial commit sha — discovered via the live API rather
/// than hardcoded, so this spec never has to duplicate `fixture-repo.ts`'s
/// git plumbing. Resolved through `KNOWN_FILE`'s OWN file-history (it has
/// exactly one entry, the initial commit, and nothing later ever touches
/// it — including the C7 `story.rs` commits) rather than `main`'s live ref,
/// which no longer equals the initial commit once those additive commits
/// land.
async function initialCommitSha(): Promise<string> {
  const res = await fetch(`${BASE}/api/file-history?repo=${REPO_NAME}&path=${KNOWN_FILE}`);
  const body = (await res.json()) as FileHistoryBody;
  const entry = body.entries[body.entries.length - 1];
  if (!entry) throw new Error(`fixture bug: ${KNOWN_FILE} has no file-history entries`);
  return entry.sha;
}

test.describe("commit page", () => {
  test("hard-navigated for the fixture's initial sha renders subject + file list", async ({ page }) => {
    const sha = await initialCommitSha();
    await page.goto(`${BASE}/r/${REPO_NAME}/~commit/${sha}`);

    await expect(page.locator(".kbc-commit__subject")).toHaveText("initial fixture commit");
    await expect(page.locator("[data-kbc-commit-sha]")).toContainText(sha.slice(0, 7));
    // The fixture's initial commit adds all 5 root files (lib.rs, README.md,
    // caller.rs, resolver.rs, todos_fixture.rs — see `fixture-repo.ts`).
    await expect(page.locator("[data-kbc-filechange]")).toHaveCount(5);
    await expect(page.locator(`[data-kbc-filechange="${KNOWN_FILE}"]`)).toBeVisible();
  });
});

test.describe("branches page", () => {
  test("lists main (HEAD, 0/0) + feature-x (ahead 1); ahead count click-through compares 1 commit + 1 file", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);

    const mainRow = page.locator('[data-kbc-branch-row="main"]');
    await expect(mainRow).toBeVisible();
    await expect(mainRow.locator("[data-kbc-branch-head]")).toBeVisible();
    await expect(mainRow.locator("[data-kbc-branch-ahead]")).toHaveText("0");
    await expect(mainRow.locator("[data-kbc-branch-behind]")).toHaveText("0");

    const featureRow = page.locator(`[data-kbc-branch-row="${FEATURE_BRANCH}"]`);
    await expect(featureRow).toBeVisible();
    await expect(featureRow.locator("[data-kbc-branch-head]")).toHaveCount(0);
    await expect(featureRow.locator("[data-kbc-branch-ahead]")).toHaveText("1");
    // `main` has moved 9 commits ahead of the `feature-x` fork point since
    // (C7 `story.rs`, the todos fixture, v3.1's H3a trait/impl + H3b
    // `impact_extra.rs` additions, DCB W2.B's additive rev_remap demo —
    // TWO commits creating then shifting `doclens_remap_fixture.rs`,
    // seeded by `doclens-fixture.ts`'s `seedRemapDemo` — DCB-W2.B.R fix
    // 9's ONE additional commit seeding the ambiguity-demo fixture files
    // (`seedAmbiguityDemo`), and DCB-W3.B's ONE additional commit seeding
    // the cited-by demo fixture file (`seedCitedByDemo`) — all committed
    // directly on `main`, see `fixture-repo.ts`) — `feature-x` is
    // unaffected in its OWN 1 commit (still `ahead: 1`), but is now behind
    // `main` by those 9 — V72-I2's Rails fixture app is the ninth. This
    // number moves whenever a lane extends the fixture on main; that is
    // expected.
    await expect(featureRow.locator("[data-kbc-branch-behind]")).toHaveText("9");

    await featureRow.locator("[data-kbc-branch-ahead] a").click();
    await expect(page).toHaveURL(new RegExp(`~compare\\?from=main&to=${FEATURE_BRANCH}`));

    await expect(page.locator(".kbc-compare__commit")).toHaveCount(1);
    await expect(page.locator(".kbc-compare__commit-subject")).toHaveText(FEATURE_COMMIT_SUBJECT);
    await expect(page.locator(`[data-kbc-filechange="${FEATURE_FILE}"]`)).toBeVisible();
  });
});

test.describe("History inspector tab", () => {
  test("lists the initial commit; clicking it time-travels to ?ref=<sha>", async ({ page }) => {
    const sha = await initialCommitSha();
    await page.goto(`${BASE}/r/${REPO_NAME}`);

    // Set up the wait BEFORE the click that triggers the fetch (`useFileHistory`
    // fires as soon as a file opens, NOT gated on the tab being open) —
    // same "both waits set up before clicking" discipline
    // `blame-gutter.spec.ts` already uses for its own provenance fetch.
    const historyPromise = page.waitForResponse((res) => res.url().includes("/api/file-history?"));
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await historyPromise;

    await page.locator('[data-kbc-itab="history"]').click();
    const entry = page.locator(`[data-kbc-history-entry="${sha}"]`);
    await expect(entry).toBeVisible();
    // The virtual "working tree" row is the current position on first open
    // (no `?ref=` yet).
    await expect(page.locator('[data-kbc-history-entry="working-tree"]')).toHaveClass(/is-current/);

    await entry.locator(`[data-kbc-history-goto="${sha}"]`).click();
    await expect(page).toHaveURL(new RegExp(`[?&]ref=${sha}(&|$)`));
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
  });
});

test.describe("[c ]c history stepping", () => {
  test("steps between the working tree and the one commit touching lib.rs", async ({ page }) => {
    const sha = await initialCommitSha();
    await page.goto(`${BASE}/r/${REPO_NAME}`);

    const historyPromise = page.waitForResponse((res) => res.url().includes("/api/file-history?"));
    await page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });
    await historyPromise;
    // Deterministic focus, same idiom `reader-vim.spec.ts` uses.
    await page.locator(".kbc-codeview .cm-line").first().click();

    // A `[c`/`]c` step re-fetches the file at the new `?ref=` — even though
    // THIS fixture's content is byte-identical at every position (one
    // commit, never edited since), `CodeView` still unmounts/remounts
    // across that fetch's loading flicker (`Reader.tsx`'s Wave C focus-
    // follows-ref fix), so the buffer only regains keyboard focus once the
    // remount's `.focus()` call has actually landed. Waiting for that
    // (rather than firing the next keypress immediately) is what makes a
    // CHAINED `[c [c` sequence deterministic here.
    async function waitForBufferFocus() {
      await page.waitForFunction(() => document.activeElement?.closest(".kbc-codeview") != null);
    }

    // [c — older: working tree -> the one (newest AND oldest) commit.
    await page.keyboard.press("[");
    await page.keyboard.press("c");
    await expect(page).toHaveURL(new RegExp(`[?&]ref=${sha}(&|$)`));
    await waitForBufferFocus();

    // [c again — already at the oldest commit (there's only one) — warns,
    // the URL is unchanged. Filtered by its own text (not just `[data-
    // kbc-toast="warn"]`) since the earlier toast(s) from this same test
    // may still be on-screen (4.5s TTL) — a bare kind-selector would be a
    // Playwright strict-mode violation once more than one warn toast is
    // simultaneously visible.
    await page.keyboard.press("[");
    await page.keyboard.press("c");
    await expect(page.locator('[data-kbc-toast="warn"]').filter({ hasText: "oldest commit" })).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`[?&]ref=${sha}(&|$)`));

    // ]c — newer: back to the working tree (no ?ref=).
    await page.keyboard.press("]");
    await page.keyboard.press("c");
    await expect(page).not.toHaveURL(/[?&]ref=/);
    await waitForBufferFocus();

    // ]c again — already at the working tree — warns, the URL is unchanged.
    await page.keyboard.press("]");
    await page.keyboard.press("c");
    await expect(page.locator('[data-kbc-toast="warn"]').filter({ hasText: "back to working tree" })).toBeVisible();
    await expect(page).not.toHaveURL(/[?&]ref=/);
  });
});
