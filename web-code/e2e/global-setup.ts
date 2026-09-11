import { spawn, ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { Server } from "node:http";
import { createFixtureRepo, KNOWN_FILE } from "./fixture-repo";
import { DOCLENS_FIXTURE_PORT, DOCLENS_KB, BASE, PORT, REPO_NAME } from "./helpers";
import { startDoclensFixture } from "./doclens-fixture";

/**
 * Boots a real `kb-code-server` fast-profile binary against a small, freshly-
 * created git fixture repo (`fixture-repo.ts`) on a fixed, uncommon port
 * (distinct from the dev daemon's 4747 + its SPA dev server's 4748), and
 * waits for the initial index walk to finish before handing control to the
 * specs. Mirrors the root `tests/e2e/global-setup.ts`'s shape (spawn +
 * XDG-scoped tmp dirs + poll-until-ready), swapped to kb-code-server's own
 * config schema (`[server] addr` + `[[repos]]`, see `crates/kb-code-server/
 * src/config.rs`) and its own SPA-dist env var (`KB_CODE_SPA_DIST`).
 */
declare global {
  // eslint-disable-next-line no-var
  var __KB_CODE_DAEMON__: { process: ChildProcess; tmpDir: string } | undefined;
  // DCB W2.B — the doc-lens mock's own handle, torn down alongside the
  // daemon in global-teardown.ts.
  // eslint-disable-next-line no-var
  var __DOCLENS_FIXTURE__: Server | undefined;
}

const REPO_ROOT = resolve(__dirname, "..", "..");
const KB_CODE_SERVER_BIN = resolve(REPO_ROOT, "target", "fast", "kb-code-server");
const SPA_DIST = resolve(REPO_ROOT, "web-code", "dist");

function writeConfig(path: string, repoPath: string) {
  // DCB W2.B — `[kb_daemon]` flips from the harness's original `enabled =
  // false` (this section's own history below) to `true`, pointed at
  // `doclens-fixture.ts`'s mock instead of a real `kb` daemon — the lens
  // page fundamentally needs SOME live `coderef/1` source to pull from,
  // and this harness spawns exactly ONE shared `kb-code-server` for the
  // whole worker run (`playwright.config.ts`: `workers: 1`), so every spec
  // shares this one config. This is deliberately SAFE for the join
  // ladder's own sessions/attribution specs (`blame-gutter.spec.ts`,
  // `review.spec.ts`'s Compare-grouped-toggle case): `join::ladder`
  // (`crates/kb-code-server/src/join/ladder.rs`) treats EVERY
  // `KbClientError` variant uniformly as "no session match" (`let
  // Ok(matches) = kb_client.by_commit(...).await else { … }`) — the mock
  // implements none of the sessions/commit-map/why routes those specs'
  // commits would hit, so they 404 (`KbClientError::BadStatus`) exactly
  // the way `KbClientError::Disabled`'s short-circuit already did:
  // `confidence: none` either way, no observable DOM difference. Verified
  // empirically too — the full existing e2e suite stays green alongside
  // this change (see the W2.B commit's own verification notes).
  //
  // `[transcripts] enabled = false` — this lane defaults ON, walking
  // `~/.claude/projects` (`TranscriptsSection::default_root`) — REAL
  // Claude Code session transcripts on whatever machine runs the suite,
  // not the harness's own fixture. An e2e run must not depend on (or
  // scan) a developer's actual session history — unaffected by the
  // `kb_daemon` change above (a completely separate config section).
  const body = `
[server]
addr = "127.0.0.1:${PORT}"

[[repos]]
name = "${REPO_NAME}"
path = "${repoPath}"

[kb_daemon]
enabled = true
url = "http://127.0.0.1:${DOCLENS_FIXTURE_PORT}"

[doclens]
kbs = ["${DOCLENS_KB}"]

[transcripts]
enabled = false

# V74-L3b: kbc-trail/1 is OFF by default (design D17). This key PERMITS the
# feature; the runtime opt-in (the trails_state row) is still off on a fresh
# volume, so the harness reproduces first-boot exactly. Without it the whole
# surface would be untestable: every write would refuse with "disabled" and
# the indicator would never render at all.
[trails]
enabled = true
# A 5-second floor, so trails.spec.ts's 12s and 3s hops prove the
# QUANTISATION (12 -> 10, 3 -> 0) rather than passing through unchanged.
step_granularity_secs = 5

# V76-R3a — enable rubocop so lanes.spec.ts can ingest one diagnostic
# via the loopback route and show it on the Facts tab + diagnostics gutter.
[lanes]
enabled = ["rubocop"]
`;
  writeFileSync(path, body, "utf-8");
}

async function waitUntil(predicate: () => Promise<boolean>, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate().catch(() => false)) return true;
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

async function reposBody(): Promise<{ repos: Array<{ file_count: number }> }> {
  const resp = await fetch(`${BASE}/api/repos`);
  if (!resp.ok) throw new Error(`GET /api/repos -> ${resp.status}`);
  return resp.json();
}

export default async function globalSetup() {
  const tmpDir = mkdtempSync(join(tmpdir(), "kb-code-e2e-"));
  const repoDir = join(tmpDir, "repo");
  createFixtureRepo(repoDir);
  // Spec files run in separate WORKER processes (no `globalThis` sharing
  // with this root process — see the module doc on why `tmpDir`/`process`
  // are instead stashed there for `global-teardown.ts`, which DOES share
  // this process). Workers are spawned only after `globalSetup` returns and
  // inherit `process.env` at that point, so an env var is the one channel
  // that reaches them — `checkout-dirty.spec.ts` reads this to dirty the
  // fixture's working tree directly on disk.
  process.env.KB_CODE_E2E_REPO_DIR = repoDir;

  // DCB W2.B — start the doc-lens mock BEFORE the daemon spawns (so it's
  // already listening when the daemon's `[kb_daemon]`-backed doc-lens
  // route makes its first request); `startDoclensFixture` also seeds the
  // rev_remap demo's two additive commits onto the SAME fixture repo, so
  // they're on disk before the daemon's own initial boot walk runs.
  const doclensFixture = await startDoclensFixture(repoDir);
  globalThis.__DOCLENS_FIXTURE__ = doclensFixture;

  const configPath = join(tmpDir, "kb-code.toml");
  writeConfig(configPath, repoDir);

  const stateDir = join(tmpDir, "state");
  const cacheDir = join(tmpDir, "cache");
  const cfgDir = join(tmpDir, "config");
  mkdirSync(stateDir);
  mkdirSync(cacheDir);
  mkdirSync(cfgDir);

  const proc = spawn(KB_CODE_SERVER_BIN, ["--config", configPath], {
    env: {
      ...process.env,
      XDG_STATE_HOME: stateDir,
      XDG_CONFIG_HOME: cfgDir,
      XDG_CACHE_HOME: cacheDir,
      KB_CODE_SPA_DIST: SPA_DIST,
      RUST_LOG: "warn,kb_code_server=info",
    },
    stdio: "inherit",
  });
  proc.on("error", (err) => {
    console.error("kb-code-server spawn error:", err);
  });

  const up = await waitUntil(async () => (await fetch(`${BASE}/api/repos`)).ok, 30_000);
  if (!up) {
    proc.kill("SIGTERM");
    doclensFixture.close();
    throw new Error(
      `kb-code-server at ${BASE} never became reachable — did you run ` +
        "`cargo build --profile fast -p kb-code-server` and `cd web-code && npm run build` first? " +
        "(`just ci-code-e2e` does both.)",
    );
  }

  // Wait for the initial boot walk to index every fixture file (lib.rs +
  // README.md + caller.rs [B1] + resolver.rs [B3] + story.rs [C7] + …,
  // bumped to 9 by DCB W2.B's additive `doclens_remap_fixture.rs`, seeded
  // by `startDoclensFixture` above) before any spec searches — otherwise
  // an early query races the indexer and flakes.
  const indexed = await waitUntil(async () => (await reposBody()).repos[0]?.file_count >= 9, 30_000);
  if (!indexed) {
    proc.kill("SIGTERM");
    doclensFixture.close();
    throw new Error("kb-code-server never finished indexing the fixture repo");
  }

  // V76-R3a — one rubocop diagnostic on KNOWN_FILE so lanes.spec.ts has a
  // fact to list and a diagnostics-gutter variant to show. Loopback ingest
  // (this daemon binds 127.0.0.1). Distinctive cop name, not a fixture path.
  const ingest = await fetch(`${BASE}/api/lanes/rubocop/ingest?repo=${encodeURIComponent(REPO_NAME)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json", "X-Kbc-Request": "1" },
    body: JSON.stringify({
      schema: "lane-ingest/1",
      run: { tool: "rubocop", tool_version: "1.66.1", argv_redacted: "rubocop --format json" },
      facts: [
        {
          path: KNOWN_FILE,
          range: 3,
          kind: "diagnostic",
          value: { cop: "Style/FactsLane", message: "facts-lane e2e cop" },
          severity: "warning",
        },
      ],
    }),
  });
  if (!ingest.ok) {
    const text = await ingest.text();
    proc.kill("SIGTERM");
    doclensFixture.close();
    throw new Error(`rubocop ingest for lanes.spec.ts failed ${ingest.status}: ${text}`);
  }

  globalThis.__KB_CODE_DAEMON__ = { process: proc, tmpDir };
}
