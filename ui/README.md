# ui/ — shared design foundation

Tokens, themes, icons and shell primitives shared by the two SPAs: `web/` (kb,
React 19.3) and `web-code/` (kb-code, React 18.3).

`ui/` is a **source directory**, not a package. There is no `package.json`, no
`node_modules` and no lockfile here, and there never will be. The only
dependencies are `react` and `react-dom`, resolved from whichever app imports
the file.

## Wiring (done once, per app)

- `tsconfig.json`: `include` gains `../ui/src`; `paths` maps `@kb/ui/*` to
  `../ui/src/*`, and maps `react`, `react/jsx-runtime`, `react-dom`,
  `react-dom/*`, `vitest` and `vitest/*` to the app's own `node_modules`
  (for react, the app's `@types/*` is tried first), so a file under `ui/src`
  type-checks against that app's React and vitest.
- `vite.config.ts` / `vitest.config.ts`: `resolve.alias` maps `@kb/ui` to
  `ui/src`; `resolve.dedupe` lists `react` and `react-dom`, so ui files resolve
  them from the app root. `server.fs.allow` admits `../ui` for the dev server.
- Apps import with `import { … } from "@kb/ui/<module>"`.

## Rules

- Code here runs under React 18.3 AND 19.3. Use only APIs common to both.
  This is currently exercised by the Probe tests, which run in both apps' vitest.
- Do not add a `package.json` or any dependency beyond `react` / `react-dom`.
- Tests live beside the source as `*.test.ts(x)` and are run by BOTH apps'
  vitest (each app's `test.include` has `../ui/src/**`). Component tests render
  with `react-dom/server` `renderToString` in the node environment.
- Both apps' vitest default to the node environment; a ui/ test that needs the
  DOM must start with a `// @vitest-environment jsdom` docblock.
- `ui/scripts/css-undefined-vars.mjs <appdir>` lints every `var(--x)` in
  `<appdir>/src` AND `ui/src` against the names declared in either (OL F10).
- `ui/scripts/` holds node scripts: `gen-themes.mjs` + `theme-lint.mjs` (the
  kbc-theme/1 generator and contrast oracle, OL F2; run them with plain `node`),
  the type codemod, `css-undefined-vars.mjs` and `ratchet.mjs` (the F10 gates).
- Gates (OL F10): `<app>/ratchet.json` (literal counts), `ui/chrome-budget.json`
  and `ui/nav-budget.json`. Both directions fail; lower a number in the PR that
  earns it. See `docs/web-internals.md` "Frontend gates".

## Themes (kbc-theme/1)

`crates/kb-code-server/themes/registry.json` is the single source (26 OKLCH
anchors per theme). `node ui/scripts/gen-themes.mjs` writes
`ui/src/themes/registry.gen.ts` + `themes.gen.css`, both committed and pinned
byte-for-byte by `registry.gen.test.ts` (run under both apps). `derive.ts` is
the pure derivation, `apply.ts` the DOM half (`applyTheme(appearance, family,
{ builtinFamily })` sets `data-theme`, `data-kbc-theme`, `color-scheme`), and
`ThemePicker.tsx` the shared picker (storage is the host's, via a controller).
kb's default family is `kb-classic` (registry `lead` flag); kb-code keeps its
built-in `kbc` family painted from its own tokens.css.

## Primitives

**Icons** (`@kb/ui/icons`): one set of 75 names for both apps. `Icon.Name`
keeps the component-map shape; `createIconSet(overrides)` builds a set with
per-app default sizes (kb-code overrides Search, Folder and X). Glyphs share one
16x16 viewBox, 1.4 stroke, `currentColor`.

**Spine.** The one bar both apps share: 44px, sticky, four slots: `switcher` (left), `scope` (breadcrumb; the only slot that shrinks), `search` (centred, max 40rem), `right` (status/identity). At <=860px `scope` and `right` hide and the menu button (enabled by passing `onMenu`) appears. View controls never live in the Spine; they belong to the ViewBar. Apps pass their own router link via `SpineCrumbs`' `renderLink`; `ui/` imports no router.

**ViewBar** (`@kb/ui/ViewBar`): `<ViewBar title count clusters maxVisible={9} moreLabel renderControl />`. Control kinds: button, toggle (`aria-pressed`), select, segment (one control, `aria-pressed` per option). Labels and keys come from the caller's registry row; the component invents none. A disabled control keeps `reason` visible. Controls beyond `maxVisible` (counted across clusters in order) move to a More menu with cluster label, label and key. `attrs` are spread as data-* attributes so apps keep their e2e selectors.

**StateCard** (`@kb/ui/StateCard`): one surface for loading, empty, refused, degraded and version-mismatch states. `kind` picks the semantics (`loading` is a polite `role=status` div; the rest are labelled regions with a heading, default h2) and a default icon; every kind carries `data-kb-state-kind`. Optional `reason`, `progress` (`<progress>` plus "done / total unit"), `actions` (`href` renders `<a>`, otherwise `<button>`) and `cli` (the CLI twin in a `<code>` with a copy button). ui/ imports no router: pass `renderLink={(href, children) => <Link to={href}>{children}</Link>}`. No skeletons or shimmer by design.

**Sheet** (`@kb/ui/Sheet`): a bottom sheet with closed/peek/full detents. The detent machine (`sheet/detents.ts`) is pure; the component adds the scrim, dialog role, grab handle (pointer drag and arrow keys), Esc to close, focus move and restore, and a body scroll lock that restores the previous inline value. `asSheet={false}` renders a plain div so one component serves both a desktop panel and a mobile sheet. Height transitions run only under `prefers-reduced-motion: no-preference`.

**keys/** — `WhichKey` (next-key overlay for a pending chord) and `KeyboardHelp` (`?` sheet) take generic `KeyRow`/`KeyScopeView` data; an app adapter maps its own registry onto them. They own markup, Esc-to-close and CSS, never key dispatch. Import from `@kb/ui/keys/WhichKey`, `@kb/ui/keys/KeyboardHelp`, `@kb/ui/keys/types`.
