# web-code/e2e

Playwright suite for kb-code's SPA (`web-code/`) against a real
`kb-code-server` fast-profile binary + a freshly generated git fixture repo
(`fixture-repo.ts`). Self-contained: its own `package.json`, its own
`playwright.config.ts`, its own port (`helpers.ts`'s `PORT`), booted/torn
down by `global-setup.ts`/`global-teardown.ts`. See the root
`justfile`'s `ci-code-e2e` recipe for the canonical from-scratch sequence.

```bash
just ci-code-spa                              # builds web-code/dist/
cargo build --profile fast -p kb-code-server  # nice -n 20 ionice -c 3 prefix for a cold build
cd web-code/e2e && npm ci && npx playwright test
```

## Regenerating the landmark snapshots (`regions.spec.ts`)

`regions.spec.ts` gates the `data-region` shell (topbar / dock / main /
pane-1 / pane-2 / drawer / rail / stripe-left / stripe-right /
review-side) added in V70-A0 and re-cut by the Desk in V70-A4. Its expected snapshots are **hand-authored**, not
Playwright's own auto-generated `toMatchAriaSnapshot({name})` files — see
that spec's own header comment for why (auto-generated snapshots would
capture review ids, patchset counts, file lists — exactly the "data rows"
the deliverable says must never gate this check). Snapshot text lives at
`__snapshots__/regions.spec.ts/<route>.aria.yml`, one file per route in
`regions-routes.ts`'s route table, and is **not** managed by
`--update-snapshots`.

If a route's landmark shell changes on purpose (e.g. a Desk-shell unit adds
or removes a region), update the matching `.aria.yml` by hand: run the spec,
read the actual-vs-expected diff Playwright prints on a `toMatchAriaSnapshot`
failure (it shows the full pruned accessibility tree — trim it back down to
role-only lines for the regions that changed, keeping non-landmark content
out), and commit the result.

## Regenerating the visual baselines (`visual.spec.ts`)

Opt-in — skipped unless `KBC_VISUAL=1`, and **not** run in CI (`just
ci-code-e2e`). Full-page 1280×720 screenshots are the most machine-sensitive
check in this suite (font rasterization, GPU compositing, scrollbar
rendering all drift by platform/driver in ways the ARIA snapshots above
never do); the milestone's before/after screenshot review is a human pass
over these baselines run locally, not a CI gate.

```bash
cd web-code/e2e
KBC_VISUAL=1 npx playwright test visual --update-snapshots=all
```

Baselines land under `visual/` (this directory's own `visual/`, set via
`playwright.config.ts`'s `expect.toHaveScreenshot.pathTemplate`), committed
to git. Regenerate them after any INTENTIONAL visual change and review the
diff before committing — an accidental diff here is exactly what the check
exists to catch.

**`=all`, not the bare flag** (V70-A4 finding). The default mode is
`changed`, which only rewrites a baseline whose comparison FAILED — and the
comparison runs at `maxDiffPixelRatio: 0.02`, so a real-but-small change
passes and the baseline silently keeps depicting the OLD ui. The Desk
refactor measured 1.4% drift on `reader-file` (12,932 px of 921,600): under
tolerance, invisible to the bare flag. Use `=all` and then review
`git status` — which is also how you find the routes that drift on every
run regardless (`branches`, `commit`, `compare`, `inbox`, the two
`lens-entry` ramps all render fixture shas/timestamps, so they re-write on
any run; restore those with `git checkout` unless you actually changed
them).

## The Desk's own specs (V70-A4)

Three files, split by what they promise:

| spec | promise |
|---|---|
| `desk-landmarks.spec.ts` | the LANDMARK golden — region identity, order and `data-region` addressing are identical across center modes; a mode may collapse a region, never move or rename one. Iterates a `MODES` list that has one entry today (`reader`) and gains one per center mode. |
| `desk-viewport.spec.ts` | the VIEWPORT golden — at 1280×720 with the default (Read) desk the code region measures ≥80 columns × 28 lines, computed from CodeMirror's REAL char advance and line height, not an assumed ratio. Also pins the overlay branch: opening the drawer never pushes the code under that floor. |
| `desk-shell.spec.ts` | everything else the unit shipped — separator drag + persistence, the `Ctrl-w r` resize submode, double-click reset, collapse-to-stripe, `Ctrl-w m` zoom, F11/Shift-F11, the re-cut rail (All first, passport-card order, caret-follow + pin), the drawer's "Keep in drawer" tenant, and the `?shell=legacy` escape hatch. |

## Firefox (opt-in, `KBC_E2E_FIREFOX=1`)

`playwright.config.ts` declares a `firefox` project, enabled only when
`KBC_E2E_FIREFOX=1` is set — `npx playwright install firefox` was run on
this box (browser cached under `~/.cache/ms-playwright/`) so a developer can
spot-check cross-browser drift locally:

```bash
KBC_E2E_FIREFOX=1 npx playwright test --project=firefox
```

**Browser floor: chromium** — CI and `just ci-code-e2e` stay chromium-only
regardless of this flag. V70-A6 (the Location Contract, §P7) landed its two
router adapters and states the matrix this section used to defer: the
Navigation API (Chrome/Edge 102+, Firefox 145+, "Baseline: newly available"
as of January 2026) drives per-entry scroll/caret restoration keyed on
`entry.key`; everywhere else (older Firefox, any other engine) the History
adapter is used and scroll restoration keys on the normalised URL instead —
nothing else about navigation changes. The opt-in `firefox` project here is
what actually exercises the History-adapter fallback path (chromium only
ever exercises the Navigation-API path), so it is still worth running after
touching `web-code/src/nav/history.ts` even though it is not a CI gate.

## The `?desk=` URL contract (`web-code/src/lib/deskParam.ts`)

V70-A0 also pins the `?desk=read|review|explore|present|legacy` query-param
grammar one wave before its consumer (the Desk shell itself, unit A3) —
mirrors `lib/codeUrl.ts`'s `pane2` precedent (shipped before its first
reader). `parseDeskParam` is unit-tested in `web-code/src/lib/
deskParam.test.ts` (vitest); this e2e package's own `helpers.ts` exports a
`gotoWithDesk(page, url, preset)` convenience so specs written from now on
can navigate with a desk preset pinned in the URL without hand-rolling the
query-string merge. No route reads `?desk=` yet — adding a consumer is out
of scope here.
