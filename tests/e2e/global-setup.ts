import { spawn, ChildProcess } from "node:child_process";
import {
  mkdtempSync,
  copyFileSync,
  cpSync,
  mkdirSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

/**
 * Starts the daemon for the duration of the test run and persists its
 * connection details on `globalThis.__KB_DAEMON__`. The daemon binds
 * 127.0.0.1:4737 (uncommon port to avoid clashes with a real daemon
 * bound to the topic-11 default 4000).
 */
declare global {
  // eslint-disable-next-line no-var
  var __KB_DAEMON__: { process: ChildProcess; port: number; tmpDir: string } | undefined;
}

const PORT = 4737;
const REPO_ROOT = resolve(__dirname, "..", "..");
const KB_SERVER_BIN = resolve(REPO_ROOT, "target", "fast", "kb-server");
const CANON_DIR = resolve(REPO_ROOT, "corpus", "canon");
const SPA_DIST = resolve(REPO_ROOT, "web", "dist");

const CANON_FILES = [
  "fullscreen-viz.html",
  "kitchen-sink.html",
  "multi-page.html",
  "cost-of-abstraction.html",
];

function copyCanon(target: string) {
  mkdirSync(target, { recursive: true });
  for (const name of CANON_FILES) {
    copyFileSync(join(CANON_DIR, name), join(target, name));
  }
  // The pm/ subfolder is a real nested artifact set (4 linked HTML
  // files + style.css). spa-multipage.spec.ts deep-links into it, and
  // the folder/sort/group specs in spa-views.spec.ts use it as
  // folder-tree material — the rest of the canon corpus is flat.
  cpSync(join(CANON_DIR, "pm"), join(target, "pm"), { recursive: true });
  // G4 — seed one nested file under pm/extra/ so the sibling popover's
  // subfolder toggle (off by default → direct siblings only; on → all
  // descendants) has something to reveal. Kept minimal: one tiny HTML
  // with a stable title so spa-siblings.spec.ts can locate it.
  const extraDir = join(target, "pm", "extra");
  mkdirSync(extraDir, { recursive: true });
  writeFileSync(
    join(extraDir, "note.html"),
    `<!doctype html><html><head><title>Extra note</title></head><body><h1>Extra note</h1><p>Nested file used by G4 subfolder-toggle tests.</p></body></html>`,
    "utf-8",
  );
  // DCB W1.D — spa-coderefs.spec.ts's same-origin half. Deliberately relies
  // primarily on the DECLARED ref (`data-kb-ref`, Decision 2's "skip
  // inference, emit as declared" — grammar-independent and stable against
  // upstream extractor tuning) plus ONE bare `path:line` inferred ref (the
  // "100%-precision" production every W1.A grammar amendment agrees on, per
  // the base plan's own corpus measurement). No comma-lists, no
  // `Namespace::Class`, no ambiguous basenames are exercised via REAL
  // extraction here — those come from spa-coderefs.spec.ts's own canned
  // `page.route()` JSON instead. No heading `id=` — matches the real
  // corpus's actual shape (`group_anchor` derives automatically via
  // `kb_core::headings::heading_id`).
  writeFileSync(
    join(target, "code-refs-demo.html"),
    `<!doctype html><html><head><meta charset="utf-8"><title>Code refs demo</title><meta name="kb-category" content="reference"></head><body>
<h1>Code refs demo</h1>
<h2>Extraction</h2>
<p>The grammar lives in <code data-kb-ref="crates/kb-core/src/coderefs.rs#L42">coderefs.rs</code>.</p>
<p>See <code>crates/kb-core/src/config.rs:120</code> for the resolved settings.</p>
</body></html>`,
    "utf-8",
  );
  // W1.D.R #2 — a SECOND code-refs-bearing doc, a root-level sibling of
  // code-refs-demo.html above, so spa-coderefs.spec.ts can exercise an
  // IN-APP doc→doc navigation (a Folder-tab sibling Link, never
  // `page.goto`) and assert `pinned_repo` re-seeds for the SECOND doc too
  // — PreviewInspector stays mounted across reader→reader nav (no `key=`
  // at either detail.tsx call site), so a seed latch that only ever
  // re-arms once would silently stop working past the first doc viewed.
  writeFileSync(
    join(target, "code-refs-demo-2.html"),
    `<!doctype html><html><head><meta charset="utf-8"><title>Code refs demo two</title><meta name="kb-category" content="reference"></head><body>
<h1>Code refs demo two</h1>
<p>The wire types live in <code data-kb-ref="crates/kb-code-server/src/doclens/wire.rs#L88">wire.rs</code>.</p>
</body></html>`,
    "utf-8",
  );
}

// DCB W1.D — the "kb with no code_url configured" degrade-state fixture
// (13-w1d-kb-spa.md §6 state 1). Cites a code path in prose so the doc
// carries a real coderef/1 row (ref_count > 0), so the Code section renders
// its "not linked to a code repo" hint rather than being hidden entirely.
function seedNoCode(target: string) {
  mkdirSync(target, { recursive: true });
  writeFileSync(
    join(target, "no-code-url.html"),
    `<!doctype html><html><head><meta charset="utf-8"><title>No code url</title><meta name="kb-category" content="reference"></head><body><h1>No code url</h1><p>Cites <code>crates/kb-core/src/config.rs:120</code> but this kb has no code_url configured.</p></body></html>`,
    "utf-8",
  );
  seedRootIndex(target, "Nocode root", NOCODE_ROOT_SENTINEL);
}

function writeConfig(
  path: string,
  sourcePath: string,
  memPath: string,
  liveTranscriptsPath: string,
  nocodePath: string,
) {
  // `disable_embedder_fallback = true` pins the suite to a NO-embedder
  // daemon: neither kb sets `embedding_model`, so without this the daemon
  // auto-selects the registry default (bge-small) and tries to spawn the
  // `kb-embedder` subprocess. In clean CI that spawn ENOENTs (only
  // `kb-server` is built) so the embedder ends up absent — but on a dev box
  // that built the full workspace (kb-embedder present + models cached) it
  // succeeds, which silently flips `spa-cmdk`'s "hybrid without an embedder
  // surfaces the 400 inline" test from pass to fail. This flag makes the
  // intended state (no embedder; lexical-only search) deterministic in BOTH
  // environments. Memory recall (spa-memory) uses an empty query → recency
  // timeline, and the atlas uses hash-placement, so neither needs vectors.
  const body = `
[daemon]
name = "e2e"

[server]
addr = "127.0.0.1:${PORT}"

[defaults]
disable_embedder_fallback = true

[kb.canon]
path = "${sourcePath}"
# DCB W1.D — spa-coderefs.spec.ts's page.route() interception owns every
# request to this origin; nothing needs to actually be listening on 4747
# (the SPA reads the base URL exclusively from this config value, never a
# hardcoded literal in application code).
code_url = "http://127.0.0.1:4747"

[kb.mem]
path = "${memPath}"
memory_scope = "global"

# DCB W1.D — the code_url-LESS kb for the "not linked to a code repo"
# degrade state (13-w1d-kb-spa.md §6 state 1, §12.3 test 5).
[kb.nocode]
path = "${nocodePath}"

[sessions]
live_transcripts_dir = "${liveTranscriptsPath}"
`;
  writeFileSync(path, body, "utf-8");
}

// v0.9 M7 — a global-scope memory corpus seeded with a couple of memory
// artifacts so spa-memory.spec.ts has something to render + forget.
function memoryHtml(title: string, body: string, salience: number): string {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title><meta name="kb-category" content="memory-user"><meta name="kb-salience" content="${salience}"><meta name="kb-decay" content="slow"></head><body><h1>${title}</h1><p>${body}</p></body></html>`;
}

function seedMemory(target: string) {
  mkdirSync(target, { recursive: true });
  writeFileSync(
    join(target, "pref-tabs.html"),
    memoryHtml("Prefers tabs", "The user prefers tabs over spaces in editors.", 0.9),
    "utf-8",
  );
  writeFileSync(
    join(target, "deploy-quetzal.html"),
    memoryHtml("Deploy pipeline", "Deploys run through the quetzal pipeline nightly.", 0.6),
    "utf-8",
  );
  seedRootIndex(target, "Mem root", MEM_ROOT_SENTINEL);
}

// ARTIFACT HOST GRAMMAR v2 (docs/architecture-invariants.md #7) —
// `ArtifactId::from_path` hashes only the source-relative path (invariant
// #27), so a root `index.html` in BOTH the `mem` and `nocode` corpora
// hashes to the SAME 12-hex artifact id regardless of content. Pre-v2
// this was the exact "hermes-wins" collision the qualified `{kb_enc}--
// {id}` label exists to fix; artifact-kb-collision.spec.ts asserts the
// two corpora now resolve to distinct qualified iframe origins despite
// sharing an id. Each gets a distinct sentinel string so the spec can
// tell the two iframes' CONTENT apart, not just their origin.
const MEM_ROOT_SENTINEL = "MEM-ROOT-SENTINEL-f3a1c8";
const NOCODE_ROOT_SENTINEL = "NOCODE-ROOT-SENTINEL-9d2e47";

function seedRootIndex(target: string, title: string, sentinel: string) {
  writeFileSync(
    join(target, "index.html"),
    `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body><h1>${title}</h1><p>${sentinel}</p></body></html>`,
    "utf-8",
  );
}

async function waitForDaemon(port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(`http://127.0.0.1:${port}/api/identity`);
      if (resp.ok) return true;
    } catch {
      // not yet up
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

export default async function globalSetup() {
  const tmpDir = mkdtempSync(join(tmpdir(), "kb-e2e-"));
  const sourceDir = join(tmpDir, "corpus");
  copyCanon(sourceDir);
  const memDir = join(tmpDir, "memory");
  seedMemory(memDir);
  const nocodeDir = join(tmpDir, "nocode");
  seedNoCode(nocodeDir);
  const liveTranscriptsDir = join(tmpDir, "live-transcripts");
  mkdirSync(liveTranscriptsDir, { recursive: true });

  const configPath = join(tmpDir, "kb.toml");
  writeConfig(configPath, sourceDir, memDir, liveTranscriptsDir, nocodeDir);

  const stateDir = join(tmpDir, "state");
  const cacheDir = join(tmpDir, "cache");
  const cfgDir = join(tmpDir, "config");
  mkdirSync(stateDir);
  mkdirSync(cacheDir);
  mkdirSync(cfgDir);

  // KB_SPA_DIST points the daemon at the prebuilt SPA bundle so the
  // SPA-load + view-switch + cmd+k specs can exercise the full route
  // dispatch. CI runs `npm run build` in web/ before Playwright; a local
  // `npx playwright test` works the same way as long as web/dist exists.
  const proc = spawn(KB_SERVER_BIN, ["--config", configPath], {
    env: {
      ...process.env,
      XDG_STATE_HOME: stateDir,
      XDG_CONFIG_HOME: cfgDir,
      XDG_CACHE_HOME: cacheDir,
      KB_SPA_DIST: SPA_DIST,
      RUST_LOG: "warn,kb=info",
    },
    stdio: "inherit",
  });
  proc.on("error", (err) => {
    console.error("daemon spawn error:", err);
  });

  const ok = await waitForDaemon(PORT, 30_000);
  if (!ok) {
    proc.kill("SIGTERM");
    throw new Error(`daemon at 127.0.0.1:${PORT} never became reachable`);
  }
  // Give the indexer a beat to process the initial walk.
  await new Promise((r) => setTimeout(r, 1500));

  // Workers run in separate processes — globalThis here doesn't reach
  // them. Stash the port + the writable corpus dir in env vars that they
  // read on import (the live-fs spec writes/removes artifacts in it).
  process.env.KB_E2E_PORT = String(PORT);
  process.env.KB_E2E_CORPUS = sourceDir;
  process.env.KB_E2E_MEM_CORPUS = memDir;
  process.env.KB_E2E_LIVE_TRANSCRIPTS = liveTranscriptsDir;
  process.env.KB_E2E_NOCODE_CORPUS = nocodeDir;
  globalThis.__KB_DAEMON__ = { process: proc, port: PORT, tmpDir };
}
