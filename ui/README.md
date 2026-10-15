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
- web-code's `lint:css-vars` counts the names `ui/src/**/*.css` declares as
  defined (incl. the generated `themes.gen.css`) but does not scan `ui/src` for
  references yet; a later unit widens that.
- `ui/scripts/` holds node scripts: `gen-themes.mjs` + `theme-lint.mjs` (the
  kbc-theme/1 generator and contrast oracle, OL F2; run them with plain `node`)
  and the type codemod.

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
