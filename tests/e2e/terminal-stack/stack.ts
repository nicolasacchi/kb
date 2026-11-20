// T25 — helpers the terminal end-to-end spec shares: the stack's addresses
// (handed over by global-setup-terminal through env), REST calls with the
// gate's headers, scenario launching, session binding, WebSocket recording
// and the scrollback read. No assertions about product behaviour live here.

import { expect, type APIRequestContext, type Page } from "@playwright/test";
import { createHash, randomBytes } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import net from "node:net";
import { join } from "node:path";

export const PORT_MAIN = 4737;
export const PORT_TTL = 4738;
export const PORT_OFF = 4739;
export const MAIN = `http://127.0.0.1:${PORT_MAIN}`;
export const TTL = `http://127.0.0.1:${PORT_TTL}`;
export const OFF = `http://127.0.0.1:${PORT_OFF}`;

function env(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`terminal e2e: ${name} is not set (global-setup-terminal did not run?)`);
  return v;
}
export const root = (): string => env("KB_E2E_TS_ROOT");
export const repoRoot = (): string => env("KB_E2E_REPO_ROOT");
export const fixtures = (): string => join(repoRoot(), "tests", "e2e", "terminal-stack", "fixtures");
export const tmuxBin = (): string => env("KB_E2E_TMUX_BIN");
export const holderDir = (): string => env("KB_E2E_TMUX_SOCK_DIR");

// ------------------------------------------------------------------ REST

export interface CallOpts {
  /** Origin header: a string, or null for ABSENT. Default: the server's own origin. */
  origin?: string | null;
  /** Send `X-Kb-Intent: terminal` (default true). */
  intent?: boolean;
  data?: unknown;
  /** Extra request headers (e.g. a Content-Type), merged over the gate's. */
  headers?: Record<string, string>;
}

/** A REST client for one terminal-enabled (or disabled) server. */
export class Api {
  constructor(
    readonly req: APIRequestContext,
    readonly base: string = MAIN,
  ) {}

  headers(o: CallOpts = {}): Record<string, string> {
    const h: Record<string, string> = {};
    const origin = o.origin === undefined ? this.base : o.origin;
    if (origin) h["Origin"] = origin;
    if (o.intent ?? true) h["X-Kb-Intent"] = "terminal";
    return { ...h, ...(o.headers ?? {}) };
  }

  async call(method: string, path: string, o: CallOpts = {}) {
    return this.req.fetch(`${this.base}${path}`, {
      method,
      headers: this.headers(o),
      data: o.data,
      failOnStatusCode: false,
    });
  }

  async json<T = unknown>(method: string, path: string, o: CallOpts = {}): Promise<{ status: number; body: T }> {
    const r = await this.call(method, path, o);
    let body: unknown = null;
    try {
      body = await r.json();
    } catch {
      body = null;
    }
    return { status: r.status(), body: body as T };
  }
}

export interface TermRow {
  id: string;
  pane_pid?: number;
  pane_dead?: boolean;
  stale?: boolean;
  session_id?: string | null;
  state?: string | null;
  blocked?: boolean | null;
  cols?: number;
  rows?: number;
}

export async function listTerms(api: Api): Promise<TermRow[]> {
  const { status, body } = await api.json<{ terms: TermRow[] }>("GET", "/api/terms");
  expect(status, "GET /api/terms").toBe(200);
  return body.terms;
}

// -------------------------------------------------------------- scenarios

export interface NumberedOpts {
  seed?: number;
  live?: number;
  rebuildEvery?: number;
  rate?: number;
  queries?: boolean;
  gate?: string;
  leadMs?: number;
  slowRebuildMs?: number;
  floodGate?: string;
  floodBytes?: number;
  floodTail?: number;
}

export interface Harness {
  /** Per-run output directory (expected.json, stdin.log, soon-N). */
  out: string;
  /** Shell body the fake `omp` runs (`scenario.sh`). */
  shell: string;
}

/** A unique, empty directory under the stack root. */
export function scratch(prefix = "run"): string {
  return mkdtempSync(join(root(), `${prefix}-`));
}

const quote = (s: string): string => `'${s.replace(/'/g, `'\\''`)}'`;

export function numberedHarness(
  o: NumberedOpts = {},
  grid: { cols: number; rows: number } = { cols: 120, rows: 40 },
): Harness {
  const out = scratch("num");
  const args = [
    "--seed", String(o.seed ?? 3000),
    "--live", String(o.live ?? 400),
    "--rebuild-every", String(o.rebuildEvery ?? 100),
    "--cols", String(grid.cols),
    "--rows", String(grid.rows),
    "--rate", String(o.rate ?? 200),
    "--lead-ms", String(o.leadMs ?? 1500),
    "--slow-rebuild-ms", String(o.slowRebuildMs ?? 0),
    "--out", out,
  ];
  if (o.queries) args.push("--queries");
  if (o.gate) args.push("--gate", o.gate);
  if (o.floodGate) args.push("--flood-gate", o.floodGate);
  if (o.floodBytes) args.push("--flood-bytes", String(o.floodBytes));
  if (o.floodTail) args.push("--flood-tail", String(o.floodTail));
  // `exec` keeps the pane pid equal to the harness pid.
  return {
    out,
    shell: `exec python3 -I ${quote(join(fixtures(), "numbered_harness.py"))} ${args.map(quote).join(" ")}\n`,
  };
}

/**
 * The ported kitty-picture generator, then idle so the pane stays alive. With
 * `gate` the pane waits for that file first, so the picture arrives LIVE
 * (after the viewer joined) instead of through the join seed.
 */
export function kittyHarness(
  o: { cols?: number; rows?: number; px?: string; resizeReplay?: boolean; gate?: string } = {},
): Harness {
  const out = scratch("kitty");
  const args = ["--cols", String(o.cols ?? 40), "--rows", String(o.rows ?? 8), "--px", o.px ?? "240x96"];
  if (o.resizeReplay) args.push("--resize-replay");
  const wait = o.gate ? `until [ -e ${quote(o.gate)} ]; do sleep 0.1; done\n` : "";
  return {
    out,
    shell: `${wait}python3 -I ${quote(join(fixtures(), "synth_omp_kitty_stream.py"))} ${args.map(quote).join(" ")}\nexec sleep 3600\n`,
  };
}

/**
 * A pane that waits for `gate`, then prints ONE line of exactly `width`
 * characters (`W`, dashes, `Z`: both ends are visible and distinguishable), a
 * `WIDE-DONE` line, and writes `<out>/printed`. Used to prove a wide line
 * drawn AFTER a grid change reaches the viewer intact (last column present,
 * no wrap).
 */
export function wideHarness(gate: string, width: number): Harness {
  const out = scratch("wide");
  const line = `W${"-".repeat(width - 2)}Z`;
  return {
    out,
    shell: `until [ -e ${quote(gate)} ]; do sleep 0.1; done\nprintf '%s\\n' ${quote(line)}\nprintf 'WIDE-DONE\\n'\n: > ${quote(join(out, "printed"))}\nexec sleep 3600\n`,
  };
}

export const idleHarness = (): Harness => ({ out: scratch("idle"), shell: "exec sleep 3600\n" });

const launched: Launched[] = [];

export interface Launched {
  id: string;
  panePid: number;
}

/**
 * Launch through the SPA's own REST route. Every launch gets its OWN directory
 * holding its OWN `scenario.sh`, and the pane starts in that directory (the
 * fake `omp` runs `./scenario.sh`), so a pane that outlives its test can never
 * read a script a later test rewrote.
 */
export async function launch(api: Api, h: Harness, grid?: { cols: number; rows: number }): Promise<Launched> {
  const dir = scratch("pane");
  writeFileSync(join(dir, "scenario.sh"), h.shell);
  const data: Record<string, unknown> = { harness: "omp", cwd: dir };
  if (grid) {
    data.cols = grid.cols;
    data.rows = grid.rows;
  }
  const { status, body } = await api.json<{ id: string; pane_pid: number }>("POST", "/api/terms", { data });
  expect(status, `launch: ${JSON.stringify(body)}`).toBe(201);
  const t = { id: body.id, panePid: body.pane_pid };
  launched.push(t);
  return t;
}

/**
 * The state letter of a `/proc/<pid>/stat` line (field 3). The command name is
 * parenthesised and may itself contain spaces and parentheses, so the state is
 * the first token after the LAST `)`.
 */
export function procStateOf(stat: string): string | null {
  const i = stat.lastIndexOf(")");
  if (i < 0) return null;
  const m = /^\s*([A-Za-z])/.exec(stat.slice(i + 1));
  return m ? m[1] : null;
}

/** `kill(pid, 0)` succeeds for a zombie (state Z, or X when dying), which is dead for every purpose here. */
export const alive = (pid: number): boolean => {
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  try {
    const state = procStateOf(readFileSync(`/proc/${pid}/stat`, "latin1"));
    if (state === "Z" || state === "X") return false;
  } catch {
    // no /proc (not Linux) or the process vanished between the two calls: kill(0) already said alive
  }
  return true;
};

/**
 * Kill every session this test launched (never touches anything else). A
 * kill that fails, or a pane that is still alive afterwards, is REPORTED (the
 * afterEach throws it): a surviving pane would leak into later tests. A 404
 * means the test already removed the terminal itself.
 */
export async function killLaunched(api: Api): Promise<void> {
  const ts = launched.splice(0);
  const failures: string[] = [];
  for (const t of ts) {
    try {
      const r = await api.call("POST", `/api/terms/${t.id}/kill`, { data: {} });
      if (r.status() !== 200 && r.status() !== 404) failures.push(`kill ${t.id}: HTTP ${r.status()} ${await r.text()}`);
    } catch (e) {
      failures.push(`kill ${t.id}: ${String(e)}`);
    }
  }
  for (const t of ts) {
    const deadline = Date.now() + 10_000;
    while (alive(t.panePid) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
    if (alive(t.panePid)) failures.push(`pane pid ${t.panePid} of ${t.id} is still alive 10 s after the kill`);
  }
  if (failures.length > 0) throw new Error(`killLaunched: ${failures.join("; ")}`);
}

// ------------------------------------------------------- session + beats

export function newSid(): string {
  return `t25${randomBytes(5).toString("hex")}`;
}

/** Seed a captured session transcript (synthetic) so /sessions/:sid resolves. */
export async function seedSession(req: APIRequestContext, sid: string): Promise<void> {
  const lines = [
    `{"type":"user","timestamp":"2026-07-01T09:00:00Z","cwd":"/proj/t25","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"synthetic prompt ${sid}"}}`,
    `{"type":"assistant","timestamp":"2026-07-01T09:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":"synthetic reply"}]}}`,
  ];
  const r = await req.post(`${MAIN}/api/kb/mem/artifacts`, {
    data: {
      title: `Session transcript 20260701T0900${sid.slice(-4)}Z`,
      body_html: `<pre>${lines.join("\n")}</pre>`,
      category: "memory-session",
      session_id: sid,
    },
  });
  expect(r.ok(), "seed session artifact").toBeTruthy();
  await expect
    .poll(async () => (await req.get(`${MAIN}/api/sessions/${sid}/card`)).status(), { timeout: 30_000 })
    .toBe(200);
}

export async function beat(
  req: APIRequestContext,
  o: { sid: string; event: string; pid?: number; term?: string; base?: string },
): Promise<number> {
  const r = await req.post(`${o.base ?? MAIN}/api/sessions/beat`, {
    data: {
      v: 1,
      session_id: o.sid,
      harness: "omp",
      event: o.event,
      at: new Date().toISOString(),
      lease_secs: 3600,
      ...(o.pid !== undefined ? { pid: o.pid } : {}),
      ...(o.term !== undefined ? { term: o.term } : {}),
    },
    failOnStatusCode: false,
  });
  return r.status();
}

interface CardLane {
  terminal?: { available?: boolean; term_id?: string; admitted?: boolean };
}

/** Bind `sid` to the terminal the way the kb-beat hook does: a verified `term` + `pid` claim. */
export async function bindSession(api: Api, sid: string, t: Launched): Promise<void> {
  await seedSession(api.req, sid);
  await expect
    .poll(
      async () => {
        // A successful list refreshes kb-server's directory; the beat then verifies against it.
        await listTerms(api);
        await beat(api.req, { sid, event: "start", pid: t.panePid, term: t.id });
        const r = await api.req.get(`${MAIN}/api/sessions/${sid}/card`);
        if (!r.ok()) return false;
        const card = (await r.json()) as CardLane;
        return card.terminal?.available === true && card.terminal.term_id === t.id && card.terminal.admitted === true;
      },
      { timeout: 30_000, intervals: [500, 1000, 1000] },
    )
    .toBe(true);
}

/** Keep the beat lease fresh (and re-bind after a daemon restart) until stopped. */
export function keepBound(api: Api, sid: string, t: Launched, everyMs = 250): () => void {
  let stop = false;
  void (async () => {
    while (!stop) {
      try {
        await listTerms(api);
        await beat(api.req, { sid, event: "prompt", pid: t.panePid, term: t.id });
      } catch {
        // the daemon may be mid-restart
      }
      await new Promise((r) => setTimeout(r, everyMs));
    }
  })();
  return () => {
    stop = true;
  };
}

// --------------------------------------------------------------- the page

export interface Sock {
  url: string;
  rx: Array<string | Buffer>;
  /** Date.now() at which each `rx` frame arrived (parallel to `rx`). */
  rxAt: number[];
  tx: Array<string | Buffer>;
  closed: boolean;
  /** Date.now() when the page created the socket (the join request). */
  openedAt: number;
}

/** Record every terminal WebSocket the page opens (passive: no routing). */
export function recordSockets(page: Page): Sock[] {
  const socks: Sock[] = [];
  page.on("websocket", (ws) => {
    if (!/\/api\/terms\/[^/]+\/ws/.test(ws.url())) return;
    const s: Sock = { url: ws.url(), rx: [], rxAt: [], tx: [], closed: false, openedAt: Date.now() };
    socks.push(s);
    ws.on("framereceived", (e) => {
      s.rx.push(e.payload);
      s.rxAt.push(Date.now());
    });
    ws.on("framesent", (e) => s.tx.push(e.payload));
    ws.on("close", () => {
      s.closed = true;
    });
  });
  return socks;
}

export function collectPageErrors(page: Page): string[] {
  const errs: string[] = [];
  page.on("pageerror", (e) => errs.push(`pageerror: ${String(e).slice(0, 400)}`));
  return errs;
}

/** Open /sessions/:sid, click Open terminal, wait for the panel to be live. */
export async function openPanel(page: Page, sid: string, phase = "live"): Promise<void> {
  await openPanelWhen(page, sid, async () => {}, phase);
}

/**
 * Like `openPanel`, but the page is loaded and the Open button is ready FIRST;
 * `before` then runs (wait for a harness mark) and the click, hence the
 * WebSocket join, follows immediately. This is how a test lands its join at a
 * known point of the harness's timeline instead of racing a page load.
 */
export async function openPanelWhen(page: Page, sid: string, before: () => Promise<void>, phase = "live"): Promise<void> {
  await page.goto(`${MAIN}/sessions/${sid}`);
  const open = page.locator('[data-testid="kb-term-open"]');
  await expect(open).toBeVisible({ timeout: 30_000 });
  await before();
  await open.click();
  await expect(page.locator("[data-kb-term]")).toBeVisible({ timeout: 30_000 });
  await waitPhase(page, phase);
}

export async function waitPhase(page: Page, phase: string, timeout = 45_000): Promise<void> {
  await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", phase, { timeout });
}

export interface PanelAttrs {
  phase: string;
  offset: number;
  resets: number;
  gaps: number;
}

export async function panelAttrs(page: Page): Promise<PanelAttrs> {
  return page.evaluate(() => {
    const el = document.querySelector("[data-kb-term]");
    const n = (a: string): number => Number(el?.getAttribute(a) ?? "NaN");
    return {
      phase: el?.getAttribute("data-kb-term-phase") ?? "?",
      offset: n("data-kb-term-offset"),
      resets: n("data-kb-term-resets"),
      gaps: n("data-kb-term-gaps"),
    };
  });
}

/** The whole NORMAL buffer (scrollback + viewport) as text; wrapped rows are re-joined. */
export async function readNormalBuffer(page: Page): Promise<string | null> {
  return page.evaluate(() => {
    const host = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm?: {
        buffer: {
          normal: {
            length: number;
            getLine(y: number): { isWrapped: boolean; translateToString(trim: boolean): string } | undefined;
          };
        };
      };
    } | null;
    const term = host?.__kbTerm;
    if (!term) return null;
    const buf = term.buffer.normal;
    const rows: string[] = [];
    for (let i = 0; i < buf.length; i += 1) {
      const line = buf.getLine(i);
      const text = line ? line.translateToString(true) : "";
      if (line?.isWrapped && rows.length > 0) rows[rows.length - 1] += text;
      else rows.push(text);
    }
    return rows.join("\n");
  });
}

/** Poll until the harness's marker line is in the viewer's buffer; returns the text. */
export async function waitDone(page: Page, marker = "DONE", timeout = 120_000): Promise<string> {
  let last = "";
  await expect
    .poll(
      async () => {
        last = (await readNormalBuffer(page)) ?? "";
        return last.split("\n").some((l) => l === marker);
      },
      { timeout, intervals: [500, 1000] },
    )
    .toBe(true);
  return last;
}

export interface Expected {
  total: number;
  seed: number;
  live: number;
  queries_sent: number;
  rebuilds: number;
}

export function readExpected(out: string): Expected {
  const p = join(out, "expected.json");
  expect(existsSync(p), "harness wrote expected.json").toBeTruthy();
  return JSON.parse(readFileSync(p, "utf-8")) as Expected;
}

export async function waitFile(path: string, timeout = 120_000): Promise<void> {
  await expect.poll(() => existsSync(path), { timeout, intervals: [100, 250, 500] }).toBe(true);
}

/** The epoch-ms timestamp the harness wrote into mark file `name` (soon-N, frame-start-N, frame-end-N), or null if absent. */
export function readMark(out: string, name: string): number | null {
  const p = join(out, name);
  if (!existsSync(p)) return null;
  const v = Number(readFileSync(p, "utf-8").trim());
  return Number.isFinite(v) && v > 0 ? v : null;
}

/** Date.now() at which socket `s` received its first text frame of type `t`, or null. */
export function firstTextAt(s: Sock, t: string): number | null {
  for (let i = 0; i < s.rx.length; i += 1) {
    const f = s.rx[i];
    if (typeof f !== "string") continue;
    try {
      if ((JSON.parse(f) as { t?: unknown }).t === t) return s.rxAt[i] ?? null;
    } catch {
      // not JSON
    }
  }
  return null;
}

export function touch(path: string): void {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, "1");
}

// ------------------------------------------------------- stream decoding

export interface DecodedStream {
  texts: Array<Record<string, unknown>>;
  /** The seed payload (empty on a resume). */
  seed: Buffer;
  /** Data frames after the seed, in arrival order. */
  data: Array<{ offset: number; payload: Buffer }>;
  /** seed ++ data payloads: every byte the browser was handed. */
  all: Buffer;
  /** data payloads only. */
  live: Buffer;
}

/** Split one socket's received frames into texts, the seed payload and data frames. */
export function decodeStream(frames: Array<string | Buffer>): DecodedStream {
  const texts: Array<Record<string, unknown>> = [];
  const seed: Buffer[] = [];
  const data: Array<{ offset: number; payload: Buffer }> = [];
  let seedLeft = 0;
  for (const f of frames) {
    if (typeof f === "string") {
      let o: Record<string, unknown>;
      try {
        o = JSON.parse(f) as Record<string, unknown>;
      } catch {
        continue;
      }
      texts.push(o);
      if (o.t === "seed") seedLeft = Number(o.len ?? 0);
      continue;
    }
    if (f.length < 8) continue;
    const offset = Number(f.readBigUInt64BE(0));
    let payload = f.subarray(8);
    if (seedLeft > 0) {
      const take = Math.min(seedLeft, payload.length);
      seed.push(payload.subarray(0, take));
      seedLeft -= take;
      payload = payload.subarray(take);
      if (payload.length === 0) continue;
    }
    data.push({ offset, payload });
  }
  const seedBuf = Buffer.concat(seed);
  const live = Buffer.concat(data.map((d) => d.payload));
  return { texts, seed: seedBuf, data, all: Buffer.concat([seedBuf, live]), live };
}

export const sha256 = (b: Buffer | Uint8Array): string => createHash("sha256").update(b).digest("hex");

// ------------------------------------------------- raw WebSocket upgrade

/**
 * One hand-written HTTP/1.1 upgrade request: the only way to send NO Origin
 * (browsers always send one) and to see the handler's refusal status. A 101
 * means the upgrade was accepted; the socket is closed right away.
 */
export function rawUpgrade(
  port: number,
  path: string,
  headers: Record<string, string> = {},
): Promise<{ status: number }> {
  return new Promise((resolve, reject) => {
    const sock = net.connect(port, "127.0.0.1");
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error(`rawUpgrade ${path}: no response in 8 s`));
    }, 8000);
    sock.on("data", (d) => {
      buf += d.toString("latin1");
      const m = buf.match(/^HTTP\/1\.1 (\d{3})/);
      if (m) {
        clearTimeout(timer);
        sock.destroy();
        resolve({ status: Number(m[1]) });
      }
    });
    sock.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    const extra = Object.entries(headers).map(([k, v]) => `${k}: ${v}\r\n`).join("");
    sock.write(
      `GET ${path} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
        `Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n${extra}\r\n`,
    );
  });
}

export async function mintTicket(api: Api, id: string): Promise<{ ticket: string; expires_in: number }> {
  const { status, body } = await api.json<{ ticket: string; expires_in: number }>("POST", `/api/terms/${id}/ticket`, {
    data: {},
  });
  expect(status, "ticket mint").toBe(200);
  return body;
}

// -------------------------------------------------------- runner RPC

export interface RunnerReply {
  status: string;
  code?: string;
  message?: string;
  data?: { cols?: number; rows?: number; owner?: string | null };
}

/**
 * One request over the runner's own socket (`{"v":1,"op":...}` lines), the way
 * `kb-term attach` talks to it. The harness uses it to play a local attach:
 * seed a `local:<pid>` grid owner, or attempt a forced local take.
 */
export function runnerRpc(op: string, fields: Record<string, unknown>): Promise<RunnerReply> {
  const sock = env("KB_E2E_RUNNER_SOCK");
  return new Promise((resolve, reject) => {
    const c = net.connect(sock);
    let buf = "";
    const timer = setTimeout(() => {
      c.destroy();
      reject(new Error(`runner rpc ${op}: no reply in 10 s`));
    }, 10_000);
    c.on("connect", () => c.write(`${JSON.stringify({ v: 1, op, ...fields })}\n`));
    c.on("data", (d) => {
      buf += d.toString("utf8");
      const i = buf.indexOf("\n");
      if (i >= 0) {
        clearTimeout(timer);
        c.end();
        try {
          resolve(JSON.parse(buf.slice(0, i)) as RunnerReply);
        } catch (e) {
          reject(e);
        }
      }
    });
    c.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
  });
}
