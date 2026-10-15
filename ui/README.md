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
- web-code's `lint:css-vars` / `lint:themes` do not scan `ui/src` yet; a later
  unit widens them.
- `ui/scripts/` holds node scripts (generators, ratchets) added by later units.
