import { test, expect, type Page } from "@playwright/test";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// HK1 - the kb -> kb-code -> Cited by -> kb round trip against BOTH real
// daemons. Every other crossing spec mocks one side (tests/e2e/spa-coderef-
// doors.spec.ts and spa-sibling-chip.spec.ts stub kb-code with page.route();
// doc-lens.spec.ts mocks kb with doclens-fixture.ts), so a wire mismatch
// between the two (a field name, the doclens origin allowlist, the `?sec=`
// grammar, the by_target path form) passes every one of them. This spec boots
// its OWN real kb-server and real kb-code-server (separate ports, temp dirs,
// config files; the suite's shared kb-code daemon and its kb mock are not
// touched) and walks the whole loop:
//   a. kb's reader shows the citation as a door (artifact runtime decoration);
//   b. the door opens kb-code at <file>?line=42 in a new tab;
//   c. kb-code's Cited by lists the kb artifact, read live from kb
//      (`source: kb-live`, nothing pinned);
//   d. the single citing document's chip opens kb's reader with ?sec=<h2 id>
//      and the section is what the iframe scrolled to.
// The spec prints one ROUNDTRIP-WITNESS line for the job log.

const REPO_ROOT = resolve(__dirname, "..", "..");
const KB_BIN = resolve(REPO_ROOT, "target", "fast", "kb-server");
const KBC_BIN = resolve(REPO_ROOT, "target", "fast", "kb-code-server");
const KB_DIST = resolve(REPO_ROOT, "web", "dist");
const KBC_DIST = resolve(REPO_ROOT, "web-code", "dist");

// Distinct from 4737 (kb e2e), 4747/4748 (dev), 4757 (kb-code e2e), 4758 (mock).
const KB_PORT = 4771;
const KBC_PORT = 4772;
const KB_ORIGIN = `http://127.0.0.1:${KB_PORT}`;
const KBC_ORIGIN = `http://127.0.0.1:${KBC_PORT}`;

const KB_NAME = "rt";
const REPO = "rtrepo";
const SRC = "src/roundtrip_target.rs";
const DOC = "roundtrip-demo.html";
const SECTION = "rt-section";
const LINE = 42;

const BINARIES_PRESENT = [KB_BIN, KBC_BIN, join(KB_DIST, "index.html"), join(KBC_DIST, "index.html")].every(
  existsSync,
);

const procs: ChildProcess[] = [];
let tmpDir = "";

async function waitUntil(predicate: () => Promise<boolean>, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate().catch(() => false)) return true;
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

function git(dir: string, args: string[]) {
  execFileSync("git", ["-C", dir, "-c", "commit.gpgsign=false", ...args], {
    stdio: "inherit",
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
  });
}

function seedRepo(dir: string) {
  mkdirSync(join(dir, "src"), { recursive: true });
  // 60 lines; line 42 is the declaration the artifact cites.
  const lines: string[] = [];
  for (let i = 1; i <= 60; i++) {
    lines.push(i === LINE ? "pub fn roundtrip_handler() -> u32 {" : `// roundtrip fixture line ${i}`);
  }
  writeFileSync(join(dir, SRC), lines.join("\n") + "\n", "utf-8");
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "user.email", "e2e@example.invalid"]);
  git(dir, ["config", "user.name", "e2e"]);
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", "roundtrip fixture"]);
}

function seedCorpus(dir: string) {
  mkdirSync(dir, { recursive: true });
  // A second <code> names `roundtrip_handler`, the identifier on the cited line:
  // kb extracts context tokens from <code> elements only, kb-code confirms a line
  // only from a token found there, and an unconfirmed line gives a door with no
  // `?line=`.
  // A tall spacer puts the cited section below the fold, so "the section is the
  // scroll target" is a real assertion rather than a first-paint accident.
  writeFileSync(
    join(dir, DOC),
    `<!doctype html><html><head><meta charset="utf-8"><title>Round trip demo</title><meta name="kb-category" content="reference"></head><body>
<h1>Round trip demo</h1>
<div style="height:2400px"></div>
<h2 id="${SECTION}">Where the handler lives</h2>
<p>The <code>roundtrip_handler</code> function is declared at <code>${SRC}:${LINE}</code> and nowhere else.</p>
<div style="height:1200px"></div>
</body></html>`,
    "utf-8",
  );
}

function start(bin: string, configPath: string, env: Record<string, string>): ChildProcess {
  const p = spawn(bin, ["--config", configPath], {
    env: { ...process.env, ...env, RUST_LOG: "warn" },
    stdio: "inherit",
  });
  p.on("error", (e) => console.error(`spawn ${bin}:`, e));
  procs.push(p);
  return p;
}

function frame(page: Page) {
  return page.frameLocator(".detail__frame");
}

test.describe("kb -> kb-code -> Cited by -> kb round trip (real daemons)", () => {
  test.skip(
    !BINARIES_PRESENT && !process.env.CI,
    "needs target/fast/{kb-server,kb-code-server} and web/dist + web-code/dist (just ci-code-e2e builds them)",
  );

  test.beforeAll(async () => {
    test.setTimeout(180_000);
    if (!BINARIES_PRESENT) throw new Error("round-trip prerequisites missing (binaries or SPA bundles)");
    tmpDir = mkdtempSync(join(tmpdir(), "kb-roundtrip-"));
    const repoDir = join(tmpDir, "repo");
    const corpusDir = join(tmpDir, "corpus");
    seedRepo(repoDir);
    seedCorpus(corpusDir);

    const dirs = (name: string) => {
      const base = join(tmpDir, name);
      for (const d of ["state", "cache", "config"]) mkdirSync(join(base, d), { recursive: true });
      return {
        XDG_STATE_HOME: join(base, "state"),
        XDG_CACHE_HOME: join(base, "cache"),
        XDG_CONFIG_HOME: join(base, "config"),
      };
    };

    // kb: one corpus, code_url -> the real kb-code. No embedder (lexical only).
    const kbToml = join(tmpDir, "kb.toml");
    writeFileSync(
      kbToml,
      `
[daemon]
name = "roundtrip"

[server]
addr = "127.0.0.1:${KB_PORT}"

[defaults]
disable_embedder_fallback = true

[kb.${KB_NAME}]
path = "${corpusDir}"
code_url = "${KBC_ORIGIN}"
`,
      "utf-8",
    );
    start(KB_BIN, kbToml, { ...dirs("kb"), KB_SPA_DIST: KB_DIST });

    // kb-code: the repo, [kb_daemon] -> the real kb, [doclens] naming that kb.
    // NOT exercised: the [doclens] origins allowlist. Both daemons are on
    // 127.0.0.1 and loopback origins are always allowed, so listing kb's origin
    // would be a no-op here; the spec deliberately leaves it out.
    const kbcToml = join(tmpDir, "kb-code.toml");
    writeFileSync(
      kbcToml,
      `
[server]
addr = "127.0.0.1:${KBC_PORT}"

[[repos]]
name = "${REPO}"
path = "${repoDir}"

[kb_daemon]
enabled = true
url = "${KB_ORIGIN}"
public_url = "${KB_ORIGIN}"

[doclens]
kbs = ["${KB_NAME}"]

[transcripts]
enabled = false
`,
      "utf-8",
    );
    start(KBC_BIN, kbcToml, { ...dirs("kbc"), KB_CODE_SPA_DIST: KBC_DIST });

    const kbUp = await waitUntil(async () => (await fetch(`${KB_ORIGIN}/api/identity`)).ok, 60_000);
    expect(kbUp, "kb-server reachable").toBe(true);
    const kbcUp = await waitUntil(async () => (await fetch(`${KBC_ORIGIN}/api/repos`)).ok, 60_000);
    expect(kbcUp, "kb-code-server reachable").toBe(true);

    // kb has indexed the artifact AND extracted its citation; kb-code has walked the repo.
    const docId = await (async () => {
      let id = "";
      const ok = await waitUntil(async () => {
        const r = await fetch(`${KB_ORIGIN}/api/kb/${KB_NAME}/docs?limit=50`);
        const docs = (await r.json()) as { id: string; source_relative: string }[];
        id = docs.find((d) => d.source_relative === DOC)?.id ?? "";
        return id !== "";
      }, 60_000);
      expect(ok, `${DOC} indexed by kb`).toBe(true);
      return id;
    })();
    expect(
      await waitUntil(async () => {
        const r = await fetch(`${KB_ORIGIN}/api/kb/${KB_NAME}/docs/${docId}/code-refs`);
        return r.ok && ((await r.json()) as { ref_count: number }).ref_count > 0;
      }, 60_000),
      "kb extracted the citation",
    ).toBe(true);
    expect(
      await waitUntil(async () => {
        const r = await fetch(`${KBC_ORIGIN}/api/repos`);
        const body = (await r.json()) as { repos: { file_count: number }[] };
        return (body.repos[0]?.file_count ?? 0) >= 1;
      }, 60_000),
      "kb-code indexed the repo",
    ).toBe(true);
  });

  test.afterAll(async () => {
    // Wait for each daemon to exit (SIGKILL after 10 s) before deleting the
    // dirs they may still be flushing sqlite/lance files into.
    await Promise.all(
      procs.map(
        (p) =>
          new Promise<void>((done) => {
            if (p.exitCode !== null || p.signalCode !== null) return done();
            const t = setTimeout(() => {
              try {
                p.kill("SIGKILL");
              } catch {
                // already gone
              }
            }, 10_000);
            p.once("exit", () => {
              clearTimeout(t);
              done();
            });
            try {
              p.kill("SIGTERM");
            } catch {
              clearTimeout(t);
              done();
            }
          }),
      ),
    );
    if (tmpDir) rmSync(tmpDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  });

  test("the citation opens kb-code, Cited by lists the artifact live, and the chip returns to its section", async ({
    page,
    context,
  }) => {
    test.setTimeout(150_000);

    // a. kb's reader. `?repo=` names the checkout (a checkout is never auto-selected).
    await page.goto(`${KB_ORIGIN}/a/${KB_NAME}/${DOC}?repo=${REPO}`);
    const door = frame(page).locator("[data-kb-coderef]");
    await expect(door).toHaveCount(1, { timeout: 30_000 });
    // kb-code answered the lens and confirmed line 42 against the file: `exact`.
    // A drop to likely/candidate/hint is a regression this must catch.
    await expect(door).toHaveAttribute("data-kb-coderef-cls", "exact", { timeout: 30_000 });
    const doorCls = (await door.getAttribute("data-kb-coderef-cls")) ?? "?";

    // b. the click opens kb-code at the file and line, in a new tab.
    const kbcTabP = context.waitForEvent("page");
    await door.click();
    const kbcTab = await kbcTabP;
    await kbcTab.waitForURL((u) => u.origin === KBC_ORIGIN, { timeout: 20_000 });
    const doorHref = kbcTab.url();
    expect(doorHref).toBe(`${KBC_ORIGIN}/r/${REPO}/${SRC}?line=${LINE}`);

    // c. kb-code's Cited by lists the artifact, as an unpinned live claim.
    const citedBy = kbcTab.locator("[data-kbc-citedby]");
    await expect(citedBy).toBeVisible({ timeout: 30_000 });
    await expect(citedBy.locator("[data-kbc-citedby-label]")).toHaveText(/Cited by 1 doc/);
    await citedBy.locator("[data-kbc-citedby-toggle]").click();
    const row = kbcTab.locator("[data-kbc-citedby-row]");
    await expect(row).toHaveCount(1);
    await expect(row).toContainText("Round trip demo");
    const source = kbcTab.locator('[data-kbc-citedby-source="kb-live"]');
    await expect(source).toHaveText(/unpinned/);
    const citedbySource = (await source.getAttribute("data-kbc-citedby-source")) ?? "?";

    // d. the single citing document's chip opens kb's reader at the section.
    const chip = kbcTab.locator("[data-kb-spine-slot=crossing] [data-testid=sibling-doc-chip]");
    await expect(chip).toBeVisible({ timeout: 10_000 });
    await expect(chip).toHaveAttribute("href", `${KB_ORIGIN}/a/${KB_NAME}/${DOC}?sec=${SECTION}`);
    const backTabP = context.waitForEvent("page");
    await chip.click();
    const backTab = await backTabP;
    await backTab.waitForURL((u) => u.origin === KB_ORIGIN, { timeout: 20_000 });
    const back = new URL(backTab.url());
    expect(back.pathname).toBe(`/a/${KB_NAME}/${DOC}`);
    expect(back.searchParams.get("sec")).toBe(SECTION);
    // The section is what the artifact frame scrolled to.
    await expect(frame(backTab).locator(`#${SECTION}`)).toBeInViewport({ timeout: 20_000 });
    await expect.poll(() => frame(backTab).locator("body").evaluate(() => window.scrollY)).toBeGreaterThan(500);

    console.log(
      `ROUNDTRIP-WITNESS door=${doorHref} class=${doorCls} citedby=${citedbySource} back=${backTab.url()}`,
    );
  });
});
