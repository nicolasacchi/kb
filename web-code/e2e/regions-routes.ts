// V70-A0 — the ONE route list shared by `regions.spec.ts` (ARIA landmark
// snapshots) and `visual.spec.ts` (opt-in full-page screenshots), so the two
// specs can never drift onto different route sets (same "one deep-link
// builder" discipline `lib/codeUrl.ts` documents for the SPA itself).
//
// Derived from `web-code/src/app.tsx`'s route table crossed with
// `fixture-repo.ts`'s fixture data (`docs/research/kb-code-v7-evidence/
// recon/navigation-history.md` §2.1 has the full route table this mirrors).
// Every route in that table is represented here — either with a URL the
// fixture can actually reach, or with an explicit `skip` reason (never a
// silent omission).
//
// The couple of routes that need daemon-side STATE (a review, a reading
// set) create it via the same `POST /api/reviews` / `POST /api/sets` calls
// `reviews.spec.ts`/`sets.spec.ts` already use. UNLIKE those specs, this
// module's caller MUST clean its fixtures up (the returned `cleanup`)
// rather than leaving them for the rest of the worker run: `sets.spec.ts`'s
// own "reading sets" test locates its just-created row via an unscoped
// `[data-kbc-sets-row-link]` locator and asserts an exact span COUNT
// afterward, so a second pre-existing set on the `~sets` list (this
// module's, alphabetically created first since `regions.spec.ts` sorts
// before `sets.spec.ts`) makes that test click the wrong row entirely —
// caught empirically running the full suite in this unit (V70-A0). Reviews
// don't have this problem (every review-touching spec already tolerates an
// accumulating list), but the review is cleaned up too for symmetry and so
// this module never becomes the next such landmine.

import { execFileSync } from "node:child_process";
import type { APIRequestContext } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE, KNOWN_FILE } from "./fixture-repo";
import {
  BASE,
  DOCLENS_DOC_ID,
  DOCLENS_DOC_PATH,
  DOCLENS_KB,
  REPO_DIR,
  REPO_NAME,
} from "./helpers";

export interface RegionRoute {
  /// Stable, filesystem-safe id — becomes the snapshot file's basename.
  name: string;
  url: string;
  /// When set, the route is not visited; `regions.spec.ts`/`visual.spec.ts`
  /// record it as an explicit, reasoned skip.
  skip?: string;
}

export interface RegionRoutesResult {
  routes: RegionRoute[];
  /// Deletes the set/review this call created (best-effort — a failed
  /// delete is logged, never thrown, so teardown can't mask the real test
  /// result). Call from a `finally` so it runs even on assertion failure.
  cleanup(): Promise<void>;
}

export async function buildRegionRoutes(request: APIRequestContext): Promise<RegionRoutesResult> {
  const cleanupFns: Array<() => Promise<void>> = [];
  async function cleanup(): Promise<void> {
    for (const fn of cleanupFns.splice(0)) {
      await fn().catch((err) => console.warn(`regions-routes cleanup failed: ${err}`));
    }
  }

  const routes: RegionRoute[] = [
    { name: "home", url: `${BASE}/` },
    { name: "search", url: `${BASE}/search` },
    { name: "inbox", url: `${BASE}/~inbox` },
  ];

  if (!REPO_DIR) {
    routes.push({
      name: "repo-scoped",
      url: `${BASE}/r/${REPO_NAME}/${KNOWN_FILE}`,
      skip: "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run",
    });
    return { routes, cleanup };
  }

  // --- DCB doc-lens entry ramps (repo-less) ---------------------------------
  routes.push(
    { name: "lens-entry", url: `${BASE}/~lens/${DOCLENS_KB}/${DOCLENS_DOC_ID}` },
    {
      name: "lens-entry-by-path",
      url: `${BASE}/~lens/${DOCLENS_KB}/by-path/${DOCLENS_DOC_PATH}`,
    },
  );

  // --- the reader, three shapes: empty / one file / split -------------------
  routes.push(
    { name: "reader-empty", url: `${BASE}/r/${REPO_NAME}` },
    { name: "reader-file", url: `${BASE}/r/${REPO_NAME}/${KNOWN_FILE}` },
    {
      // `pane2`'s grammar is `path@ref:line` (`lib/codeUrl.ts`'s
      // `formatPane2`/`parsePane2`) — the trailing `:` is REQUIRED even
      // with no line (its absence makes the whole value fail to parse,
      // per `parsePane2`'s own doc), so this is `<path>@<ref>:` with an
      // empty line segment, not a bare `<path>@<ref>`.
      name: "reader-split",
      url: `${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?pane2=${encodeURIComponent(`${FEATURE_FILE}@${FEATURE_BRANCH}:`)}`,
    },
  );

  const headSha = execFileSync("git", ["-C", REPO_DIR, "rev-parse", "main"], {
    encoding: "utf-8",
  }).trim();

  // --- repo-scoped, non-file sentinels --------------------------------------
  routes.push(
    { name: "commit", url: `${BASE}/r/${REPO_NAME}/~commit/${headSha}` },
    { name: "compare", url: `${BASE}/r/${REPO_NAME}/~compare?from=main&to=${FEATURE_BRANCH}` },
    { name: "branches", url: `${BASE}/r/${REPO_NAME}/~branches` },
    {
      // A self-compare (old === new) rather than growing/cleaning up a
      // fresh rebase-amend fixture on the SHARED repo like `review.spec
      // .ts`'s own `~range-diff` test does — this spec only needs the
      // page's landmark shell to render, never a specific pair count.
      name: "range-diff",
      url: `${BASE}/r/${REPO_NAME}/~range-diff?old=main..${FEATURE_BRANCH}&new=main..${FEATURE_BRANCH}`,
    },
    { name: "prs", url: `${BASE}/r/${REPO_NAME}/~prs` },
    { name: "sets", url: `${BASE}/r/${REPO_NAME}/~sets` },
    // V70-A10 ("Workspaces v0") — the list page IS the only workspace
    // surface (`lib/setsUrl.ts`'s `workspacesUrl` doc: "unlike `~sets`,
    // there is no separate detail route"), so unlike `set-detail` below this
    // needs no fixture at all — a bare visit is the whole route.
    { name: "workspaces", url: `${BASE}/r/${REPO_NAME}/~workspaces` },
    { name: "todos", url: `${BASE}/r/${REPO_NAME}/~todos` },
    { name: "comments", url: `${BASE}/r/${REPO_NAME}/~comments` },
    // V72-I2 — `~rails`. Reachable in this fixture because `fixture-repo.ts`
    // writes a synthetic Rails app into the repo (`config/routes.rb` + a
    // `Gemfile` declaring `gem "rails"` is exactly what `detect_is_rails`
    // needs); the page's chrome is identical whether or not the app is
    // detected, so this snapshot does not depend on the lens having settled.
    { name: "rails", url: `${BASE}/r/${REPO_NAME}/~rails` },
    { name: "hotspots", url: `${BASE}/r/${REPO_NAME}/~hotspots` },
    { name: "reviews", url: `${BASE}/r/${REPO_NAME}/~reviews` },
    { name: "recipes", url: `${BASE}/r/${REPO_NAME}/~recipes` },
    { name: "stacks", url: `${BASE}/r/${REPO_NAME}/~stacks` },
    { name: "canvas", url: `${BASE}/r/${REPO_NAME}/~canvas` },
    { name: "boards", url: `${BASE}/r/${REPO_NAME}/~boards` },
    { name: "browser", url: `${BASE}/r/${REPO_NAME}/~browser` },
    { name: "lens-repo-scoped", url: `${BASE}/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_DOC_ID}` },
  );

  // `/session/:sid/diff` — SessionDiff. Same reasoning `sets.spec.ts`
  // already documents for its own from-session case: the e2e daemon
  // disables `[transcripts]` entirely (`global-setup.ts`'s `writeConfig`),
  // so no session id exists anywhere for this route to resolve — reachable
  // in the route TABLE, unreachable in THIS fixture.
  routes.push({
    name: "session-diff",
    url: `${BASE}/session/none/diff?repo=${REPO_NAME}`,
    skip: "[transcripts] enabled = false in the e2e daemon config — no session id exists to visit",
  });

  // --- a reading set (backs ~sets/:id and ~sets/:id/~tour) ------------------
  const setRes = await request.post(`${BASE}/api/sets`, {
    data: {
      repo: REPO_NAME,
      name: "regions e2e set",
      spans: [{ path: KNOWN_FILE }, { path: FEATURE_FILE, ref: FEATURE_BRANCH }],
    },
  });
  if (setRes.ok()) {
    const set = (await setRes.json()) as { id: string };
    routes.push(
      { name: "set-detail", url: `${BASE}/r/${REPO_NAME}/~sets/${set.id}` },
      { name: "set-tour", url: `${BASE}/r/${REPO_NAME}/~sets/${set.id}/~tour?step=1` },
    );
    cleanupFns.push(async () => {
      const res = await request.delete(`${BASE}/api/sets/${set.id}`);
      if (!res.ok()) throw new Error(`DELETE /api/sets/${set.id} -> ${res.status()}`);
    });
  } else {
    routes.push({
      name: "set-detail",
      url: `${BASE}/r/${REPO_NAME}/~sets`,
      skip: `POST /api/sets failed: ${setRes.status()} ${await setRes.text()}`,
    });
    routes.push({
      name: "set-tour",
      url: `${BASE}/r/${REPO_NAME}/~sets`,
      skip: `POST /api/sets failed: ${setRes.status()} ${await setRes.text()}`,
    });
  }

  // --- a review (backs ~reviews/:id — the "review page" — + review diff) ---
  const reviewRes = await request.post(`${BASE}/api/reviews`, {
    data: {
      repo: REPO_NAME,
      head_ref: FEATURE_BRANCH,
      base_ref: "main",
      title: "regions e2e review",
    },
  });
  if (reviewRes.ok()) {
    const review = (await reviewRes.json()) as { id: number };
    routes.push(
      { name: "review-detail", url: `${BASE}/r/${REPO_NAME}/~reviews/${review.id}` },
      { name: "review-diff", url: `${BASE}/r/${REPO_NAME}/~reviews/${review.id}/diff` },
      {
        name: "review-diff-file",
        url: `${BASE}/r/${REPO_NAME}/~reviews/${review.id}/diff/${FEATURE_FILE}`,
      },
    );
    cleanupFns.push(async () => {
      const res = await request.delete(`${BASE}/api/reviews/${review.id}`);
      if (!res.ok()) throw new Error(`DELETE /api/reviews/${review.id} -> ${res.status()}`);
    });
  } else {
    const reason = `POST /api/reviews failed: ${reviewRes.status()} ${await reviewRes.text()}`;
    routes.push(
      { name: "review-detail", url: `${BASE}/r/${REPO_NAME}/~reviews`, skip: reason },
      { name: "review-diff", url: `${BASE}/r/${REPO_NAME}/~reviews`, skip: reason },
      { name: "review-diff-file", url: `${BASE}/r/${REPO_NAME}/~reviews`, skip: reason },
    );
  }


  // V74-L2 — one kbc-canvas/1 board, so `~boards/{slug}` is a route this
  // module can actually reach. Applied through the loopback-only route the
  // harness can use (it hits 127.0.0.1), and DELETED by `cleanup` for the
  // reason this file's header records for the reading set: a row left behind
  // changes what another spec's list assertions see.
  const boardSlug = `regions-e2e-board`;
  const boardRes = await request.post(`${BASE}/api/boards/apply`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: {
      schema: "kbc-canvas/1",
      repo: REPO_NAME,
      slug: boardSlug,
      title: "regions e2e board",
      nodes: [{ id: "n1", kind: "note", title: "n1", body_md: "a landmark fixture" }],
    },
  });
  if (boardRes.ok()) {
    cleanupFns.push(async () => {
      await request.delete(`${BASE}/api/boards/${boardSlug}?repo=${REPO_NAME}`, {
        headers: { "X-Kbc-Request": "1" },
      });
    });
    routes.push({
      name: "board-detail",
      url: `${BASE}/r/${REPO_NAME}/~boards/${boardSlug}`,
    });
  } else {
    routes.push({
      name: "board-detail",
      url: `${BASE}/r/${REPO_NAME}/~boards`,
      skip: `POST /api/boards/apply -> ${boardRes.status()} (loopback-only)`,
    });
  }

  return { routes, cleanup };
}
