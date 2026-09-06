#!/usr/bin/env node
// V70-A7 — regenerate the two checked-in theme artefacts from the registry.
//
//   crates/kb-code-server/themes/registry.json   (the SOURCE, kbc-theme/1)
//        ↓
//   web-code/src/themes/registry.gen.ts          (metadata for the picker)
//   web-code/src/themes/themes.gen.css           (one block per theme)
//
// The registry lives in the SERVER crate (same ruling as the command
// registry): the daemon serves it verbatim at `GET /api/themes` via
// `include_str!`, so a CLI/agent caller and the SPA read the same bytes.
// The SPA must NEVER import from `../crates` at build time — the Docker SPA
// stage only copies `web-code/`, so a build-time reach across the crate
// boundary would work locally and break in the image. Hence: a dev-time
// script writes checked-in artefacts, and `registry.gen.test.ts` regenerates
// them in memory and asserts byte equality so the copies cannot go stale.
//
// Run: `npm run gen:themes` (from web-code/).
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { generateRegistryTs, generateThemesCss } from "../src/themes/derive.ts";

export const REGISTRY_PATH = fileURLToPath(
  new URL("../../crates/kb-code-server/themes/registry.json", import.meta.url),
);
export const REGISTRY_TS_PATH = fileURLToPath(
  new URL("../src/themes/registry.gen.ts", import.meta.url),
);
export const THEMES_CSS_PATH = fileURLToPath(
  new URL("../src/themes/themes.gen.css", import.meta.url),
);

export function loadRegistry(path = REGISTRY_PATH) {
  return JSON.parse(readFileSync(path, "utf8"));
}

const invokedDirectly =
  process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];

if (invokedDirectly) {
  const registry = loadRegistry();
  writeFileSync(REGISTRY_TS_PATH, generateRegistryTs(registry));
  writeFileSync(THEMES_CSS_PATH, generateThemesCss(registry));
  console.log(
    `gen-themes: ${registry.themes.length} themes → registry.gen.ts + themes.gen.css`,
  );
}
