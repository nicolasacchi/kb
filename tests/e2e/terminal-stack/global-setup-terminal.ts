// T25 — bring-up of the REAL stack for the terminal end-to-end lane:
// pinned tmux 3.7c + the compiled tap + kb-termd (direct supervisor) + three
// kb-server processes (4737 enabled, 4738 enabled with a 2 s ticket TTL, 4739
// with no [terminal] section at all). Opt-in: tests/e2e/global-setup.ts
// delegates here only when KB_E2E_TERMINAL=1; the default e2e lane never
// loads this file.
//
// Everything lives under ONE unique temp root. HOME and every XDG_* of every
// child point inside it, so nothing reads or writes a real home, a real
// /run/kb-term or the shared tmux namespace. Teardown kills only what the
// root's path names (tmux holders by their own socket, then `pkill -f <root>`).

import { spawn, execFileSync, spawnSync, type ChildProcess } from "node:child_process";
import {
  closeSync,
  copyFileSync,
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const REPO_ROOT = resolve(__dirname, "..", "..", "..");
const KB_SERVER_BIN = resolve(REPO_ROOT, "target", "fast", "kb-server");
const KB_TERMD_BIN = resolve(REPO_ROOT, "target", "fast", "kb-termd");
const KB_TERM_CONF = resolve(REPO_ROOT, "crates", "kb-term", "share", "kb-term.conf");
const SPA_DIST = resolve(REPO_ROOT, "web", "dist");

export const PORT_MAIN = 4737;
export const PORT_TTL = 4738;
export const PORT_OFF = 4739;
/** The short-TTL server's ticket lifetime (seconds). */
export const SHORT_TTL_SECS = 2;

interface Launched {
  name: string;
  proc: ChildProcess;
  logFd: number;
}

function need(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`terminal e2e: ${name} must be set (the term-e2e job exports it)`);
  return v;
}

async function waitFor(what: string, ok: () => Promise<boolean> | boolean, ms: number): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    try {
      if (await ok()) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`terminal e2e: ${what} not ready after ${ms} ms`);
}

async function identityOk(port: number): Promise<boolean> {
  const r = await fetch(`http://127.0.0.1:${port}/api/identity`);
  return r.ok;
}

function serverToml(opts: {
  name: string;
  port: number;
  kbDir: string;
  runnerSock?: string;
  ttlSecs?: number;
}): string {
  // `[terminal]` is ABSENT on the disabled server on purpose: that proves the
  // default-off path, not an explicit `enabled = false`.
  const terminal = opts.runnerSock
    ? `
[terminal]
enabled = true
runner_socket = "${opts.runnerSock}"
${opts.ttlSecs ? `ticket_ttl_secs = ${opts.ttlSecs}\n` : ""}`
    : "";
  return `[daemon]
name = "${opts.name}"

[server]
addr = "127.0.0.1:${opts.port}"

[defaults]
disable_embedder_fallback = true

[kb.mem]
path = "${opts.kbDir}"
memory_scope = "global"
${terminal}`;
}

function strayPids(root: string): string[] {
  const r = spawnSync("pgrep", ["-f", root], { encoding: "utf-8" });
  return (r.stdout ?? "").split("\n").map((s) => s.trim()).filter(Boolean);
}

export default async function setupTerminalStack(): Promise<() => Promise<void>> {
  const tmuxBin = need("KB_TMUX_BIN");
  const tapBin = need("KB_TAP_BIN");
  const tmuxVersion = execFileSync(tmuxBin, ["-V"], { encoding: "utf-8" }).trim();
  if (tmuxVersion !== "tmux 3.7c") {
    throw new Error(`terminal e2e: KB_TMUX_BIN reports "${tmuxVersion}", need "tmux 3.7c"`);
  }
  for (const [what, p] of [
    ["kb-server", KB_SERVER_BIN],
    ["kb-termd", KB_TERMD_BIN],
    ["tap", tapBin],
    ["SPA bundle", join(SPA_DIST, "index.html")],
  ] as const) {
    if (!existsSync(p)) throw new Error(`terminal e2e: ${what} not found at ${p}`);
  }

  // A short, unique root: unix socket paths are capped near 108 bytes.
  const root = mkdtempSync(join(tmpdir(), "kbte-"));
  const runDir = join(root, "run");
  const home = join(root, "home");
  const binDir = join(root, "bin");
  const logDir = join(root, "logs");
  for (const d of [runDir, home, binDir, logDir]) mkdirSync(d, { recursive: true });
  const runnerSock = join(runDir, "runner", "runner.sock");
  const holderDir = join(runDir, "s");

  // The fake `omp` the runner's launch table resolves on the pane PATH. The
  // scenario (what the pane runs) is `$HOME/scenario.sh`, rewritten by each
  // test before it launches; the default idles.
  writeFileSync(join(home, "scenario.sh"), "exec sleep 3600\n");
  writeFileSync(join(binDir, "omp"), '#!/bin/sh\nexec /bin/sh "$HOME/scenario.sh"\n');
  chmodSync(join(binDir, "omp"), 0o755);
  // Tap wrapper: same binary, stderr kept for the failure artifacts.
  writeFileSync(
    join(binDir, "kb-tap-wrapper.sh"),
    `#!/bin/sh\nexec '${tapBin}' "$@" 2>>'${join(logDir, "tap.err")}'\n`,
  );
  chmodSync(join(binDir, "kb-tap-wrapper.sh"), 0o755);

  const childEnv = (extra: Record<string, string>): NodeJS.ProcessEnv => ({
    ...process.env,
    HOME: home,
    XDG_CONFIG_HOME: join(root, "xdg-config"),
    XDG_DATA_HOME: join(root, "xdg-data"),
    XDG_STATE_HOME: join(root, "xdg-state"),
    XDG_CACHE_HOME: join(root, "xdg-cache"),
    XDG_RUNTIME_DIR: join(root, "xdg-run"),
    ...extra,
  });
  for (const d of ["xdg-config", "xdg-data", "xdg-state", "xdg-cache", "xdg-run"]) {
    mkdirSync(join(root, d), { recursive: true, mode: 0o700 });
  }

  const launched: Launched[] = [];
  const start = (name: string, bin: string, args: string[], env: NodeJS.ProcessEnv): ChildProcess => {
    const logFd = openSync(join(logDir, `${name}.log`), "a");
    const proc = spawn(bin, args, { env, stdio: ["ignore", logFd, logFd] });
    proc.on("error", (err) => console.error(`terminal e2e: ${name} spawn error:`, err));
    launched.push({ name, proc, logFd });
    return proc;
  };

  const termd = start(
    "kb-termd",
    KB_TERMD_BIN,
    [
      "--run-dir", runDir,
      "--tmux-bin", tmuxBin,
      "--conf", KB_TERM_CONF,
      "--supervisor", "direct",
      "--tap-bin", join(binDir, "kb-tap-wrapper.sh"),
      "--tap-ready-secs", "30",
    ],
    childEnv({ PATH: `${binDir}:${process.env.PATH ?? "/usr/bin:/bin"}`, RUST_LOG: "info" }),
  );
  await waitFor("kb-termd runner.sock", () => {
    if (termd.exitCode !== null) throw new Error(`kb-termd exited with ${termd.exitCode}`);
    return existsSync(runnerSock);
  }, 30_000);

  const servers: Array<{ name: string; port: number; ttl?: number; enabled: boolean }> = [
    { name: "main", port: PORT_MAIN, enabled: true },
    { name: "ttl", port: PORT_TTL, ttl: SHORT_TTL_SECS, enabled: true },
    { name: "off", port: PORT_OFF, enabled: false },
  ];
  const procs: Record<string, ChildProcess> = {};
  for (const s of servers) {
    const kbDir = join(root, `kb-${s.name}`);
    mkdirSync(kbDir, { recursive: true });
    const cfg = join(root, `kb-${s.name}.toml`);
    writeFileSync(
      cfg,
      serverToml({
        name: `e2eterm-${s.name}`,
        port: s.port,
        kbDir,
        runnerSock: s.enabled ? runnerSock : undefined,
        ttlSecs: s.ttl,
      }),
    );
    // Per-server state/cache dirs: the daemon name pins the state dir.
    const stateDir = join(root, `state-${s.name}`);
    const cacheDir = join(root, `cache-${s.name}`);
    const cfgDir = join(root, `cfgdir-${s.name}`);
    for (const d of [stateDir, cacheDir, cfgDir]) mkdirSync(d, { recursive: true });
    procs[s.name] = start(
      `kb-server-${s.port}`,
      KB_SERVER_BIN,
      ["--config", cfg],
      childEnv({
        XDG_STATE_HOME: stateDir,
        XDG_CACHE_HOME: cacheDir,
        XDG_CONFIG_HOME: cfgDir,
        KB_SPA_DIST: SPA_DIST,
        RUST_LOG: "warn,kb=info",
      }),
    );
  }
  for (const s of servers) {
    await waitFor(`kb-server :${s.port}`, () => {
      const p = procs[s.name];
      if (p.exitCode !== null) throw new Error(`kb-server :${s.port} exited with ${p.exitCode}`);
      return identityOk(s.port);
    }, 60_000);
  }

  // Workers are separate processes: hand them everything through env.
  process.env.KB_E2E_PORT = String(PORT_MAIN);
  process.env.KB_E2E_TS_ROOT = root;
  process.env.KB_E2E_RUNNER_SOCK = runnerSock;
  process.env.KB_E2E_TMUX_BIN = tmuxBin;
  process.env.KB_E2E_TMUX_SOCK_DIR = holderDir;
  process.env.KB_E2E_REPO_ROOT = REPO_ROOT;
  // The default teardown SIGTERMs this; ours clears it so it never double-kills.
  (globalThis as { __KB_DAEMON__?: unknown }).__KB_DAEMON__ = {
    process: procs.main,
    port: PORT_MAIN,
    tmpDir: root,
  };

  let tornDown = false;
  return async function teardown(): Promise<void> {
    if (tornDown) return;
    tornDown = true;
    (globalThis as { __KB_DAEMON__?: unknown }).__KB_DAEMON__ = undefined;
    // 1. Tmux holders by their OWN socket (never a shared name, never kill-server -a).
    if (existsSync(holderDir)) {
      for (const f of readdirSync(holderDir)) {
        if (!f.endsWith(".sock")) continue;
        spawnSync(tmuxBin, ["-S", join(holderDir, f), "kill-server"], { timeout: 5000 });
      }
    }
    // 2. The servers and the runner, politely then hard.
    for (const l of [...launched].reverse()) {
      if (l.proc.exitCode === null) l.proc.kill("SIGTERM");
    }
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline && launched.some((l) => l.proc.exitCode === null && l.proc.signalCode === null)) {
      await new Promise((r) => setTimeout(r, 200));
    }
    for (const l of launched) {
      if (l.proc.exitCode === null && l.proc.signalCode === null) l.proc.kill("SIGKILL");
      try {
        closeSync(l.logFd);
      } catch {
        // already closed
      }
    }
    // 3. Strays that name the unique root (tap, tmux, harness), then assert none remain.
    let strays = strayPids(root);
    if (strays.length > 0) {
      spawnSync("pkill", ["-f", root]);
      await new Promise((r) => setTimeout(r, 1500));
      strays = strayPids(root);
    }
    // 4. Keep the logs for the CI artifact before deleting the root.
    const keep = process.env.KB_TERM_E2E_LOG_DIR ?? resolve(REPO_ROOT, "tests", "e2e", "test-results", "term-e2e-logs");
    try {
      mkdirSync(keep, { recursive: true });
      for (const f of readdirSync(logDir)) copyFileSync(join(logDir, f), join(keep, f));
    } catch (e) {
      console.error("terminal e2e: could not copy logs:", e);
    }
    if (strays.length === 0) rmSync(root, { recursive: true, force: true });
    if (strays.length > 0) {
      throw new Error(`terminal e2e: processes naming ${root} survived teardown: ${strays.join(",")}`);
    }
  };
}
