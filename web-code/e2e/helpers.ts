import type { Page } from "@playwright/test";

// Shared constants for e2e specs + global-setup.ts. A plain compile-time
// constant re-evaluated once per process (the main Playwright process and
// each test worker) — safe to import from both, unlike the daemon's live
// `ChildProcess` handle itself (which only exists in the main process; see
// global-setup.ts's own doc for why THAT is stashed on `globalThis`
// instead).
export const PORT = 4757; // distinct from the dev daemon's 4747 + its SPA's 4748
export const BASE = `http://127.0.0.1:${PORT}`;
export const REPO_NAME = "fixture";
/// The fixture repo's absolute filesystem path — set by `global-setup.ts`
/// (`process.env.KB_CODE_E2E_REPO_DIR`, inherited by worker processes at
/// spawn time). Only `checkout-dirty.spec.ts` needs real FS access (to
/// dirty the working tree directly, bypassing the SPA); every other spec
/// stays HTTP/DOM-only.
export const REPO_DIR = process.env.KB_CODE_E2E_REPO_DIR ?? "";

// DCB W2.B — the doc-lens fixture's own port + kb/doc identifiers. Re-
// exported here (rather than importing straight from `doclens-fixture.ts`
// everywhere) purely so every e2e constant has ONE home, matching this
// file's existing role.
export {
  DOCLENS_FIXTURE_PORT,
  DOCLENS_KB,
  DOCLENS_DOC_ID,
  DOCLENS_DOC_PATH,
} from "./doclens-fixture";

// V70-A0 — the `?desk=` override contract (`web-code/src/lib/deskParam.ts`,
// golden-pinned there). Deliberately a LOCAL copy of the preset union rather
// than a cross-package import of `DeskPreset` — this e2e package has its own
// `package.json`/`tsconfig.json` and runs standalone (`cd web-code/e2e &&
// npm ci && npx playwright test`), so it never reaches into `../src`
// (grep the existing specs: none of them do). Keep this union in lock-step
// with `deskParam.ts`'s `DESK_PRESETS` set by hand — both are small and
// change together, same posture kb-code already accepts for its several
// hand-mirrored grammars (`codeUrl.ts`'s `sym=`/`lang.rs`, per the
// navigation-history recon).
export type DeskPreset = "read" | "review" | "explore" | "present" | "legacy";

/// Navigates to `url` with `?desk=<preset>` appended (merged onto any
/// existing query string, never clobbering it) — so every spec written from
/// now on can pin a desk preset without hand-rolling the query-string
/// merge. No consumer reads `?desk=` yet (the shell lands in unit A4); this
/// only pins the URL shape early, mirroring `codeUrl.ts`'s `pane2`
/// precedent (shipped a wave before its first reader).
export async function gotoWithDesk(page: Page, url: string, preset: DeskPreset): Promise<void> {
  const withDesk = new URL(url);
  withDesk.searchParams.set("desk", preset);
  await page.goto(withDesk.toString());
}
