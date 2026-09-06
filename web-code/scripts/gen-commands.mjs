#!/usr/bin/env node
// V70-A5 — regenerate the checked-in SPA copy of the kbc-cmd/1 registry.
//
//   crates/kb-code-server/commands/registry.json   (the SOURCE, kbc-cmd/1)
//        ↓
//   web-code/src/commands/registry.gen.ts          (what the SPA imports)
//
// Same ruling and same shape as `gen-themes.mjs`: the registry lives in the
// SERVER crate because the daemon serves it verbatim at `GET /api/commands`
// via `include_str!` and the CLI embeds the SAME file, so an agent, the CLI
// and the SPA all read one set of bytes. The SPA must NEVER import from
// `../crates` at build time — the Docker SPA stage copies only `web-code/`,
// so a build-time reach across the crate boundary would work locally and
// break in the image. Hence: a dev-time script writes a checked-in artefact,
// and `registry.gen.test.ts` regenerates it in memory and asserts byte
// equality so the copy cannot go stale.
//
// Run: `npm run gen:commands` (from web-code/).
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { generateRegistryTs } from "../src/commands/derive.ts";

export const REGISTRY_PATH = fileURLToPath(
  new URL("../../crates/kb-code-server/commands/registry.json", import.meta.url),
);
export const REGISTRY_TS_PATH = fileURLToPath(
  new URL("../src/commands/registry.gen.ts", import.meta.url),
);

export function loadRegistry(path = REGISTRY_PATH) {
  return JSON.parse(readFileSync(path, "utf8"));
}

const invokedDirectly =
  process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];

if (invokedDirectly) {
  const registry = loadRegistry();
  writeFileSync(REGISTRY_TS_PATH, generateRegistryTs(registry));
  console.log(
    `gen-commands: ${registry.commands.length} commands across ${registry.scopes.length} scopes → registry.gen.ts`,
  );
}
