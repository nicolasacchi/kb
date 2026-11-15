// T15 - helpers for spa-terminal-gfx.spec.ts: a scripted tap (WebSocket +
// media route), synthetic kitty streams, and a pixel oracle that is
// independent of the renderer's own placeholder decoding.
//
// Everything here is synthetic: generated PNGs (flat quadrants and
// checkers), invented ids. The oracle decodes the page SCREENSHOT in node
// and compares pixels to where the scenario says each picture must be.

import { createHash } from "node:crypto";
import zlib from "node:zlib";
import type { Page } from "@playwright/test";
import { BASE } from "./helpers";
import { fixtureCard, helloFrame, profileFrame, binary, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

export { SID, TERM_ID };

// The first 297 kitty rowcolumn-diacritics (an independent copy of the table).
export const DIAC: readonly number[] = [773, 781, 782, 784, 786, 829, 830, 831, 838, 842, 843, 844, 848, 849, 850, 855, 859, 867,
  868, 869, 870, 871, 872, 873, 874, 875, 876, 877, 878, 879, 1155, 1156, 1157, 1158, 1159,
  1426, 1427, 1428, 1429, 1431, 1432, 1433, 1436, 1437, 1438, 1439, 1440, 1441, 1448, 1449,
  1451, 1452, 1455, 1476, 1552, 1553, 1554, 1555, 1556, 1557, 1558, 1559, 1623, 1624, 1625,
  1626, 1627, 1629, 1630, 1750, 1751, 1752, 1753, 1754, 1755, 1756, 1759, 1760, 1761, 1762,
  1764, 1767, 1768, 1771, 1772, 1840, 1842, 1843, 1845, 1846, 1850, 1853, 1855, 1856, 1857,
  1859, 1861, 1863, 1865, 1866, 2027, 2028, 2029, 2030, 2031, 2032, 2033, 2035, 2070, 2071,
  2072, 2073, 2075, 2076, 2077, 2078, 2079, 2080, 2081, 2082, 2083, 2085, 2086, 2087, 2089,
  2090, 2091, 2092, 2093, 2385, 2387, 2388, 3970, 3971, 3974, 3975, 4957, 4958, 4959, 6109,
  6458, 6679, 6773, 6774, 6775, 6776, 6777, 6778, 6779, 6780, 7019, 7021, 7022, 7023, 7024,
  7025, 7026, 7027, 7376, 7377, 7378, 7386, 7387, 7392, 7616, 7617, 7619, 7620, 7621, 7622,
  7623, 7624, 7625, 7627, 7628, 7633, 7634, 7635, 7636, 7637, 7638, 7639, 7640, 7641, 7642,
  7643, 7644, 7645, 7646, 7647, 7648, 7649, 7650, 7651, 7652, 7653, 7654, 7678, 8400, 8401,
  8404, 8405, 8406, 8407, 8411, 8412, 8417, 8423, 8425, 8432, 11503, 11504, 11505, 11744,
  11745, 11746, 11747, 11748, 11749, 11750, 11751, 11752, 11753, 11754, 11755, 11756, 11757,
  11758, 11759, 11760, 11761, 11762, 11763, 11764, 11765, 11766, 11767, 11768, 11769, 11770,
  11771, 11772, 11773, 11774, 11775, 42607, 42620, 42621, 42736, 42737, 43232, 43233, 43234,
  43235, 43236, 43237, 43238, 43239, 43240, 43241, 43242, 43243, 43244, 43245, 43246, 43247,
  43248, 43249, 43696, 43698, 43699, 43703, 43704, 43710, 43711, 43713, 65056, 65057, 65058,
  65059, 65060, 65061, 65062, 68111, 68152, 119173, 119174, 119175, 119176, 119177, 119210,
  119211, 119212, 119213, 119362, 119363, 119364,
];

export const ESC = "\x1b";
export const PH = String.fromCodePoint(0x10eeee);

// ---- PNG encode / decode (RGBA8, no deps) -----------------------------------

const crcTable = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i += 1) c = crcTable[(c ^ buf[i]) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

export type Rgb = [number, number, number];

export function png(w: number, h: number, f: (x: number, y: number) => Rgb): Buffer {
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y += 1) {
    const o = y * (w * 4 + 1);
    raw[o] = 0;
    for (let x = 0; x < w; x += 1) {
      const c = f(x, y);
      const p = o + 1 + x * 4;
      raw[p] = c[0];
      raw[p + 1] = c[1];
      raw[p + 2] = c[2];
      raw[p + 3] = 255;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 3 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

export interface Img {
  w: number;
  h: number;
  /** RGBA, row-major. */
  data: Uint8Array;
}

/** Decode an 8-bit RGB / RGBA non-interlaced PNG (what a Playwright screenshot is). */
export function decodePng(buf: Buffer): Img {
  let off = 8;
  let w = 0;
  let h = 0;
  let colorType = 6;
  const idat: Buffer[] = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    off += 12 + len;
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      if (data[8] !== 8 || data[12] !== 0) throw new Error("unsupported PNG (depth/interlace)");
      colorType = data[9];
    } else if (type === "IDAT") idat.push(data);
    else if (type === "IEND") break;
  }
  if (colorType !== 6 && colorType !== 2) throw new Error(`unsupported PNG colour type ${colorType}`);
  const bpp = colorType === 6 ? 4 : 3;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = w * bpp;
  const out = new Uint8Array(w * h * 4);
  let prev = new Uint8Array(stride);
  for (let y = 0; y < h; y += 1) {
    const ft = raw[y * (stride + 1)];
    const line = new Uint8Array(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let i = 0; i < stride; i += 1) {
      const a = i >= bpp ? line[i - bpp] : 0;
      const b = prev[i];
      const c = i >= bpp ? prev[i - bpp] : 0;
      let v = line[i];
      if (ft === 1) v += a;
      else if (ft === 2) v += b;
      else if (ft === 3) v += (a + b) >> 1;
      else if (ft === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      line[i] = v & 255;
    }
    for (let x = 0; x < w; x += 1) {
      const o = (y * w + x) * 4;
      out[o] = line[x * bpp];
      out[o + 1] = line[x * bpp + 1];
      out[o + 2] = line[x * bpp + 2];
      out[o + 3] = bpp === 4 ? line[x * bpp + 3] : 255;
    }
    prev = line;
  }
  return { w, h, data: out };
}

export function pixel(img: Img, x: number, y: number): Rgb | null {
  const px = Math.round(x);
  const py = Math.round(y);
  if (px < 0 || py < 0 || px >= img.w || py >= img.h) return null;
  const o = (py * img.w + px) * 4;
  return [img.data[o], img.data[o + 1], img.data[o + 2]];
}

// ---- the known test pattern: four quadrants + a centre square ---------------

export const PALETTE: Rgb[] = [
  [230, 30, 30],
  [30, 200, 30],
  [30, 30, 230],
  [240, 220, 20],
];
export const MAGENTA: Rgb = [255, 0, 255];

export function quadColor(u: number, v: number, k = 0): Rgb {
  if (Math.abs(u - 0.5) < 0.1 && Math.abs(v - 0.5) < 0.1) return MAGENTA;
  const q = (v < 0.5 ? 0 : 2) + (u < 0.5 ? 0 : 1);
  return PALETTE[(q + k) % 4];
}

/** Expected colour at (u,v), or null when a boundary is too close to call. */
export function expectedAt(u: number, v: number, k = 0): Rgb | null {
  const c = quadColor(u, v, k);
  const e = 0.015;
  for (const [du, dv] of [[e, 0], [-e, 0], [0, e], [0, -e], [e, e], [-e, -e]]) {
    const o = quadColor(u + du, v + dv, k);
    if (o[0] !== c[0] || o[1] !== c[1] || o[2] !== c[2]) return null;
  }
  return c;
}

export const quadPng = (w: number, h: number, k = 0): Buffer =>
  png(w, h, (x, y) => quadColor((x + 0.5) / w, (y + 0.5) / h, k));

export const checkerPng = (w: number, h: number, sq: number): Buffer =>
  png(w, h, (x, y) => ((Math.floor(x / sq) + Math.floor(y / sq)) & 1 ? [255, 255, 255] : [0, 0, 0]));

export const sha256 = (b: Buffer): string => createHash("sha256").update(b).digest("hex");

// ---- the scripted tap ---------------------------------------------------------

export interface DefSpec {
  id: number;
  png: Buffer;
  cols: number;
  rows: number;
  gen?: number;
}

interface FakeSocket {
  send(data: string | Buffer): void;
  closed: boolean;
}

export interface GfxFakeOptions {
  cols?: number;
  rows?: number;
  scrollback?: number;
  /** Seed a late joiner with another fake's stream and defs. */
  from?: GfxFake;
  /** Delay (ms) before the media route answers. */
  mediaDelayMs?: number;
}

/**
 * One scripted terminal: the card/list/ticket routes, a WebSocket that
 * serves `stream` as the seed on join and every `push()` live, imagedef
 * frames, and the media route backed by `media` (sha -> PNG bytes).
 */
export class GfxFake {
  readonly cols: number;
  readonly rows: number;
  readonly scrollback: number;
  chunks: Buffer[] = [];
  total = 0;
  defs: Array<Record<string, unknown>> = [];
  readonly media = new Map<string, Buffer>();
  readonly mediaRequests: Array<{ sha: string; headers: Record<string, string> }> = [];
  readonly sockets: FakeSocket[] = [];
  private seq = 1;
  private readonly mediaDelayMs: number;

  constructor(opts: GfxFakeOptions = {}) {
    this.cols = opts.cols ?? 100;
    this.rows = opts.rows ?? 24;
    this.scrollback = opts.scrollback ?? 5000;
    this.mediaDelayMs = opts.mediaDelayMs ?? 0;
    if (opts.from) {
      this.chunks = [Buffer.concat(opts.from.chunks)];
      this.total = opts.from.total;
      this.defs = opts.from.defs.map((d) => ({ ...d }));
      for (const [k, v] of opts.from.media) this.media.set(k, v);
      this.seq = opts.from.seq;
    }
  }

  async install(page: Page): Promise<void> {
    const card = fixtureCard();
    await page.route("**/api/sessions/*/card", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(card) }),
    );
    await page.route("**/api/terms", (route) => {
      if (route.request().method() !== "GET") return route.continue();
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          terms: [{
            id: TERM_ID, harness: "omp", cwd: "/synthetic", started_unix: 1760000000, resume_sid: null,
            pane_pid: 1, cols: this.cols, rows: this.rows, bracket_paste_flag: false, pane_dead: false,
            exit_code: null, grid_owner: null, conf_version: 1, anchor_pid: null, stale: false,
            session_id: SID, state: "working", blocked: false,
          }],
        }),
      });
    });
    await page.route("**/api/terms/*/ticket", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ ticket: "a".repeat(32), expires_in: 30, term: TERM_ID }),
      }),
    );
    await page.route("**/api/terms/media/*", async (route) => {
      const sha = new URL(route.request().url()).pathname.split("/").pop() ?? "";
      this.mediaRequests.push({ sha, headers: route.request().headers() });
      if (this.mediaDelayMs > 0) await new Promise((r) => setTimeout(r, this.mediaDelayMs));
      const bytes = this.media.get(sha);
      if (!bytes) return route.fulfill({ status: 404, body: "" });
      return route.fulfill({
        status: 200,
        contentType: "image/png",
        headers: { "x-content-type-options": "nosniff", "cache-control": "private, max-age=31536000, immutable" },
        body: bytes,
      });
    });
    await page.routeWebSocket("**/api/terms/*/ws*", (ws) => {
      const sock: FakeSocket = { closed: false, send: (d) => { if (!sock.closed) ws.send(d); } };
      ws.onClose(() => { sock.closed = true; });
      this.sockets.push(sock);
      sock.send(helloFrame("E1"));
      sock.send(profileFrame({ cols: this.cols, rows: this.rows, scrollback: this.scrollback }));
      for (const d of this.defs) sock.send(JSON.stringify(d));
      const all = Buffer.concat(this.chunks);
      sock.send(JSON.stringify({ t: "seed", offset: 0, len: all.length }));
      for (let i = 0; i < all.length; i += 4096) sock.send(binary(0, all.subarray(i, Math.min(all.length, i + 4096))));
    });
  }

  private broadcast(d: string | Buffer): void {
    for (const s of this.sockets) s.send(d);
  }

  /** Register `png` under its hash and announce it (live: after the seed). */
  def(spec: DefSpec, opts: { announce?: boolean } = {}): string {
    const sha = sha256(spec.png);
    this.media.set(sha, spec.png);
    const frame = {
      t: "imagedef", seq: this.seq, id: spec.id, gen: spec.gen ?? 1, sha256: sha, mime: "image/png",
      w: pngSize(spec.png).w, h: pngSize(spec.png).h, cols: spec.cols, rows: spec.rows, offset: this.total,
    };
    this.seq += 1;
    this.defs = this.defs.filter((d) => d.id !== spec.id);
    this.defs.push(frame);
    if (opts.announce !== false) this.broadcast(JSON.stringify(frame));
    return sha;
  }

  /** Append stream bytes: sent live at their stream offset. */
  push(bytes: Buffer | string): void {
    const buf = typeof bytes === "string" ? Buffer.from(bytes, "utf8") : bytes;
    for (let i = 0; i < buf.length; i += 16384) {
      const part = buf.subarray(i, Math.min(buf.length, i + 16384));
      this.chunks.push(part);
      this.broadcast(binary(this.total, part));
      this.total += part.length;
    }
  }

  text(frame: Record<string, unknown>): void {
    this.broadcast(JSON.stringify({ ...frame, seq: this.seq, offset: this.total }));
    this.seq += 1;
  }
}

export function pngSize(b: Buffer): { w: number; h: number } {
  return { w: b.readUInt32BE(16), h: b.readUInt32BE(20) };
}

// ---- driving the page --------------------------------------------------------

/** Make xterm's WebGL context creation fail so the DOM renderer is used. */
export async function forceDom(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const orig = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, type: string, ...rest: unknown[]) {
      if (type === "webgl" || type === "webgl2" || type === "experimental-webgl") return null;
      return (orig as (...a: unknown[]) => unknown).call(this, type, ...rest);
    } as typeof orig;
  });
}

export interface Opened {
  errors: string[];
}

/**
 * Open the session page and the terminal panel, wait for `live`. The T14
 * panel's view box is `--spine-h` tall, so its screen overflows; the style
 * tag below only lets the box grow with its content for pixel probing.
 */
export async function openGfxTerminal(page: Page): Promise<Opened> {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(`pageerror: ${String(e)}`));
  page.on("console", (m) => {
    if (m.type() === "error" && !/WebGL|GL Driver|GPU stall|Failed to load resource/i.test(m.text())) {
      errors.push(`console: ${m.text()}`);
    }
  });
  await page.goto(`${BASE}/sessions/${SID}`);
  await page.addStyleTag({ content: ".kb-term__view{height:auto !important;overflow:visible !important;}" });
  await page.locator('[data-testid="kb-terminal-slot"]').waitFor({ timeout: 15_000 });
  await page.locator('[data-testid="kb-term-open"]').click();
  await page.locator("[data-kb-term]").waitFor({ timeout: 15_000 });
  await page.waitForFunction(
    () => document.querySelector("[data-kb-term]")?.getAttribute("data-kb-term-phase") === "live",
    null,
    { timeout: 20_000 },
  );
  return { errors };
}

export interface Metrics {
  dpr: number;
  cols: number;
  rows: number;
  cw: number;
  ch: number;
  devCW: number;
  devCH: number;
  screen: { x: number; y: number; w: number; h: number };
  viewportY: number;
  baseY: number;
  length: number;
  canvases: number;
}

/** Scroll the terminal into view (screenshots are viewport-only) and measure it. */
export async function metrics(page: Page): Promise<Metrics> {
  await page.evaluate(() => {
    const host = document.querySelector("[data-kb-term]") as HTMLElement;
    host.scrollIntoView({ block: "start" });
  });
  return page.evaluate(() => {
    const host = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm: { cols: number; rows: number; buffer: { active: { viewportY: number; baseY: number; length: number } } };
      querySelector(s: string): HTMLElement | null;
      querySelectorAll(s: string): ArrayLike<unknown>;
    };
    const term = host.__kbTerm;
    const screen = host.querySelector(".xterm-screen") as HTMLElement;
    const r = screen.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const cw = r.width / term.cols;
    const ch = r.height / term.rows;
    return {
      dpr, cols: term.cols, rows: term.rows, cw, ch,
      devCW: Math.round(cw * dpr), devCH: Math.round(ch * dpr),
      screen: { x: r.left, y: r.top, w: r.width, h: r.height },
      viewportY: term.buffer.active.viewportY, baseY: term.buffer.active.baseY, length: term.buffer.active.length,
      canvases: host.querySelectorAll(".xterm-screen canvas").length,
    };
  });
}

/** Wait until `n` pictures are drawn (decoded, no neutral boxes left) and two frames have passed. */
export async function waitPictures(page: Page, n: number, timeout = 20_000): Promise<void> {
  await page.waitForFunction(
    (want) => {
      const imgs = Array.from(document.querySelectorAll<HTMLImageElement>(".kbgfx-img"));
      const pending = document.querySelectorAll(".kbgfx-pending, .kbgfx-missing").length;
      return imgs.length === want && pending === 0 && imgs.every((i) => i.complete && i.naturalWidth > 0);
    },
    n,
    { timeout },
  );
  await frames(page);
}

export async function frames(page: Page, n = 3): Promise<void> {
  await page.evaluate(async (count) => {
    for (let i = 0; i < count; i += 1) await new Promise<void>((r) => requestAnimationFrame(() => r()));
  }, n);
}

export async function overlayInfo(page: Page) {
  return page.evaluate(() => {
    const ov = document.querySelector<HTMLElement>(".kbgfx-overlay");
    if (!ov) return null;
    return {
      clips: ov.querySelectorAll(".kbgfx-clip").length,
      imgs: ov.querySelectorAll(".kbgfx-img").length,
      missing: ov.querySelectorAll(".kbgfx-missing").length,
      pending: ov.querySelectorAll(".kbgfx-pending").length,
      texts: Array.from(ov.querySelectorAll(".kbgfx-missing")).map((e) => e.textContent ?? ""),
      versions: Array.from(ov.querySelectorAll<HTMLElement>(".kbgfx-clip")).map((e) => e.dataset.kbgfxVer ?? ""),
      created: Number(ov.dataset.kbgfxDecosCreated ?? "-1"),
      disposed: Number(ov.dataset.kbgfxDecosDisposed ?? "-1"),
      live: Number(ov.dataset.kbgfxDecosLive ?? "-1"),
    };
  });
}

/** An independent scan of the whole buffer for placeholder cells: `[abs, x, fg]`. */
export async function scanAll(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const host = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm: {
        cols: number;
        buffer: { active: { length: number; getLine(y: number): { getCell(x: number): { getChars(): string; getFgColor(): number } | undefined } | undefined } };
      };
    };
    const t = host.__kbTerm;
    const out: string[] = [];
    for (let y = 0; y < t.buffer.active.length; y += 1) {
      const line = t.buffer.active.getLine(y);
      if (!line) continue;
      for (let x = 0; x < t.cols; x += 1) {
        const c = line.getCell(x);
        if (!c) break;
        const ch = c.getChars();
        if (ch.length >= 2 && ch.codePointAt(0) === 0x10eeee) out.push(`${y},${x},${c.getFgColor()}`);
      }
    }
    return out;
  });
}

export async function scrollToLine(page: Page, line: number): Promise<number> {
  const vy = await page.evaluate((n) => {
    const host = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm: { scrollToLine(n: number): void; buffer: { active: { viewportY: number } } };
    };
    host.__kbTerm.scrollToLine(n);
    return host.__kbTerm.buffer.active.viewportY;
  }, line);
  await frames(page);
  return vy;
}

// ---- scenarios --------------------------------------------------------------

export const fgFor = (id: number, mode: "rgb" | "pal" = "rgb"): string =>
  mode === "pal"
    ? `${ESC}[38;5;${id}m`
    : `${ESC}[38;2;${(id >> 16) & 255};${(id >> 8) & 255};${id & 255}m`;

export const diac = (i: number): string => String.fromCodePoint(DIAC[i]);

/** `r` rows of placeholder cells, each row starting at 1-based column `indent`. */
export function placeholderRows(
  id: number, c: number, r: number, indent: number,
  mode: "rgb" | "pal" = "rgb", implicit = false,
): string {
  const rows: string[] = [];
  for (let y = 0; y < r; y += 1) {
    let line = fgFor(id, mode);
    for (let x = 0; x < c; x += 1) {
      if (implicit && x > 0) line += PH;
      else line += PH + diac(y) + diac(x);
    }
    rows.push(`${ESC}[${indent}G${line}${ESC}[39m`);
  }
  return `${rows.join("\r\n")}\r\n`;
}

export const textLine = (n: number, tag = "line"): string =>
  `${tag} ${String(n).padStart(6, "0")} the quick brown fox jumps over the lazy dog ${"#".repeat(n % 17)}`;

export interface Truth {
  id: number;
  line0: number;
  col0: number;
  c: number;
  r: number;
  variant: number;
}

/** Builds a byte stream and the truth table (absolute line numbers, no wrapping, no scroll regions). */
export class Scenario {
  parts: string[] = [];
  line = 0;
  truth: Truth[] = [];
  constructor(readonly fake: GfxFake, readonly m: Metrics) {}
  raw(s: string): this {
    this.parts.push(s);
    return this;
  }
  head(): this {
    return this.raw(`${ESC}[?25l${ESC}[2J${ESC}[H`);
  }
  textLines(n: number, tag = "line", start = 0): this {
    for (let i = 0; i < n; i += 1) {
      this.raw(`${textLine(start + i, tag)}\r\n`);
      this.line += 1;
    }
    return this;
  }
  blank(n = 1): this {
    for (let i = 0; i < n; i += 1) {
      this.raw("\r\n");
      this.line += 1;
    }
    return this;
  }
  /** Announce the def and add the placeholder rows. `reuseSha` skips announcing (same id, same bytes). */
  image(o: {
    id: number; c: number; r: number; indent?: number; variant?: number; mode?: "rgb" | "pal";
    implicit?: boolean; pngBuf?: Buffer; announce?: boolean;
  }): this {
    const indent = o.indent ?? 11;
    const variant = o.variant ?? 0;
    const buf = o.pngBuf ?? quadPng(Math.round(o.c * this.m.devCW), Math.round(o.r * this.m.devCH), variant);
    if (o.announce !== false) this.fake.def({ id: o.id, png: buf, cols: o.c, rows: o.r });
    this.truth.push({ id: o.id, line0: this.line, col0: indent - 1, c: o.c, r: o.r, variant });
    this.raw(placeholderRows(o.id, o.c, o.r, indent, o.mode ?? "rgb", o.implicit ?? false));
    this.line += o.r;
    return this;
  }
  build(): string {
    return this.parts.join("");
  }
}

export const BIG_IDS = [0x3a7f21, 0x1b4c9d, 0x6e2f80, 0x9c5a13];

export function basicScenario(fake: GfxFake, m: Metrics): Scenario {
  const s = new Scenario(fake, m).head().textLines(2, "header").blank();
  s.image({ id: 0x3a7f21, c: 40, r: 10, indent: 11, variant: 0 });
  s.blank().textLines(1, "middle").blank();
  s.image({ id: 0x1b4c9d, c: 20, r: 6, indent: 51, variant: 1 });
  return s;
}

export function bigScenario(fake: GfxFake, m: Metrics, total = 2040): Scenario {
  const at = [300, 800, 1300, 1800];
  const s = new Scenario(fake, m).head();
  let k = 0;
  for (let i = 0; i < total; i += 1) {
    if (k < at.length && i === at[k]) {
      s.blank();
      s.image({ id: BIG_IDS[k], c: 30, r: 8, indent: 11, variant: k });
      s.blank();
      k += 1;
    }
    s.textLines(1, "line", i);
  }
  return s;
}

// ---- the oracle -------------------------------------------------------------

const near = (a: Rgb | null, b: Rgb | null, tol: number): boolean =>
  a !== null && b !== null && Math.abs(a[0] - b[0]) <= tol && Math.abs(a[1] - b[1]) <= tol && Math.abs(a[2] - b[2]) <= tol;

export interface CheckResult {
  total: number;
  ok: number;
  badCount: number;
  bad: Array<{ kind: string; id: number; got: Rgb | null; exp: Rgb | null }>;
}

/** The terminal background as the screenshot shows it (top-right corner of the screen, away from text). */
export function backgroundOf(img: Img, m: Metrics): Rgb {
  const p = pixel(img, (m.screen.x + m.screen.w - 3) * m.dpr, (m.screen.y + 3) * m.dpr);
  return p ?? [0, 0, 0];
}

/**
 * Where must each picture be on screen? Probe the screenshot at the corners
 * (one device pixel inside), a 5x5 interior grid, and just outside each
 * edge. `edgeDev` is the placement tolerance in device pixels (the DOM
 * renderer's cells are fractional in device pixels).
 */
export function checkPictures(
  img: Img, m: Metrics, truths: Truth[], vy: number,
  o: { edgeDev?: number; tol?: number } = {},
): CheckResult {
  const edgeDev = o.edgeDev ?? 2;
  const tol = o.tol ?? 12;
  const D = edgeDev / m.dpr;
  const bg = backgroundOf(img, m);
  const pts: Array<{ x: number; y: number; kind: string; exp: Rgb | null; id: number }> = [];
  for (const t of truths) {
    const top = t.line0 - vy;
    const bottom = t.line0 + t.r - vy;
    const vTop = Math.max(0, top);
    const vBot = Math.min(m.rows, bottom);
    if (vBot <= vTop) continue;
    const L = t.col0;
    const R = t.col0 + t.c;
    const x0 = m.screen.x + L * m.cw;
    const x1 = m.screen.x + R * m.cw;
    const y0 = m.screen.y + vTop * m.ch;
    const y1 = m.screen.y + vBot * m.ch;
    const addIn = (px: number, py: number, kind: string): void => {
      const u = (px - (m.screen.x + L * m.cw)) / (t.c * m.cw);
      const v = (py - (m.screen.y + top * m.ch)) / (t.r * m.ch);
      const exp = expectedAt(u, v, t.variant);
      if (!exp) return;
      pts.push({ x: px * m.dpr, y: py * m.dpr, kind, exp, id: t.id });
    };
    addIn(x0 + D, y0 + D, "corner-tl");
    addIn(x1 - 2 * D, y0 + D, "corner-tr");
    addIn(x0 + D, y1 - 2 * D, "corner-bl");
    addIn(x1 - 2 * D, y1 - 2 * D, "corner-br");
    for (let i = 1; i <= 5; i += 1) for (let j = 1; j <= 5; j += 1) addIn(x0 + ((x1 - x0) * i) / 6, y0 + ((y1 - y0) * j) / 6, "grid");
    const my = (y0 + y1) / 2;
    pts.push({ x: (x0 - 2 * D) * m.dpr, y: my * m.dpr, kind: "outside-left", exp: null, id: t.id });
    pts.push({ x: (x1 + D) * m.dpr, y: my * m.dpr, kind: "outside-right", exp: null, id: t.id });
    if (top >= 0) pts.push({ x: ((x0 + x1) / 2) * m.dpr, y: (y0 - 2 * D) * m.dpr, kind: "outside-above", exp: null, id: t.id });
    if (bottom <= m.rows) pts.push({ x: ((x0 + x1) / 2) * m.dpr, y: (y1 + D) * m.dpr, kind: "outside-below", exp: null, id: t.id });
  }
  const bad: CheckResult["bad"] = [];
  let ok = 0;
  for (const p of pts) {
    const got = pixel(img, p.x, p.y);
    const good = p.exp ? near(got, p.exp, tol) : near(got, bg, tol);
    if (good) ok += 1;
    else bad.push({ kind: p.kind, id: p.id, got, exp: p.exp ?? bg });
  }
  return { total: pts.length, ok, badCount: pts.length - ok, bad: bad.slice(0, 8) };
}

// ---- sharpness --------------------------------------------------------------

/** Mean squared 4-neighbour Laplacian of the luminance over a rect. */
export function laplacianEnergy(img: Img, rect: { x: number; y: number; w: number; h: number }): number {
  const { x, y, w, h } = rect;
  const lum = new Float32Array(w * h);
  for (let j = 0; j < h; j += 1) {
    for (let i = 0; i < w; i += 1) {
      const o = ((y + j) * img.w + (x + i)) * 4;
      lum[j * w + i] = 0.299 * img.data[o] + 0.587 * img.data[o + 1] + 0.114 * img.data[o + 2];
    }
  }
  let s = 0;
  let n = 0;
  for (let j = 1; j < h - 1; j += 1) {
    for (let i = 1; i < w - 1; i += 1) {
      const k = j * w + i;
      const l = 4 * lum[k] - lum[k - 1] - lum[k + 1] - lum[k - w] - lum[k + w];
      s += l * l;
      n += 1;
    }
  }
  return n ? s / n : 0;
}

/** Area-average (box) downscale of a decoded image to `dw x dh`. */
export function boxDownscale(src: Img, dw: number, dh: number): Img {
  const out = new Uint8Array(dw * dh * 4);
  for (let j = 0; j < dh; j += 1) {
    const y0 = (j * src.h) / dh;
    const y1 = ((j + 1) * src.h) / dh;
    for (let i = 0; i < dw; i += 1) {
      const x0 = (i * src.w) / dw;
      const x1 = ((i + 1) * src.w) / dw;
      let r = 0;
      let g = 0;
      let b = 0;
      let wsum = 0;
      for (let yy = Math.floor(y0); yy < Math.ceil(y1); yy += 1) {
        const wy = Math.min(y1, yy + 1) - Math.max(y0, yy);
        for (let xx = Math.floor(x0); xx < Math.ceil(x1); xx += 1) {
          const wx = Math.min(x1, xx + 1) - Math.max(x0, xx);
          const wgt = wx * wy;
          const o = (Math.min(src.h - 1, yy) * src.w + Math.min(src.w - 1, xx)) * 4;
          r += src.data[o] * wgt;
          g += src.data[o + 1] * wgt;
          b += src.data[o + 2] * wgt;
          wsum += wgt;
        }
      }
      const o = (j * dw + i) * 4;
      out[o] = r / wsum;
      out[o + 1] = g / wsum;
      out[o + 2] = b / wsum;
      out[o + 3] = 255;
    }
  }
  return { w: dw, h: dh, data: out };
}
