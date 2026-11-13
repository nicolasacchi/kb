// T15 - fetch, decode and cache the picture bytes the tap stored.
//
// Bytes come from `GET /api/terms/media/{sha256}` (the T15 media route).
// The request is a script `fetch` carrying the terminal intent header: an
// `<img src="/api/terms/media/...">` would carry neither the header nor an
// Origin and the route refuses it. The bytes become a `blob:` object URL
// (decoded once up front, so the first paint is never a blank box), kept in
// a byte-budgeted LRU. Nothing here is persisted, and nothing enters the
// TanStack query cache (invariant #23): URLs and blobs live only in this
// object, which the panel owns.

import { currentDaemonBase } from "../../api/base";
import { TERMINAL_INTENT_HEADER, TERMINAL_INTENT_VALUE } from "../../api/terms";

/** At most this many media fetches in flight at once. Unmeasured on hundreds of defs. */
export const MAX_CONCURRENT_FETCHES = 4;
/** LRU byte budget of decoded pictures. */
export const MEDIA_BUDGET_BYTES = 96 * 1024 * 1024;
/** The smaller budget for a compact profile (`scrollback <= 20000`). */
export const MEDIA_BUDGET_COMPACT_BYTES = 32 * 1024 * 1024;
/** A transient failure is retried after each of these waits, then gives up. */
export const NETWORK_RETRY_DELAYS_MS: readonly number[] = [200, 400, 800];
/** The prefetch window: this many screens above the viewport, ... */
export const PREFETCH_SCREENS_ABOVE = 1;
/** ... and this many screens from the viewport top downwards (viewport + one below). */
export const PREFETCH_SCREENS_BELOW = 2;

const SHA256_RE = /^[0-9a-f]{64}$/;

export interface LoadedMedia {
  url: string;
  blob: Blob;
}

export type MediaResult = LoadedMedia | "missing";

export interface MediaCacheOptions {
  /** The 32 MiB budget instead of 96 MiB. */
  compact?: boolean;
  fetchImpl?: typeof fetch;
  createObjectURL?: (blob: Blob) => string;
  revokeObjectURL?: (url: string) => void;
  /** Decode `url` as an image; rejects when the bytes are not a picture. */
  decode?: (url: string) => Promise<void>;
  /** Backoff wait (injectable so tests never sleep). */
  sleep?: (ms: number) => Promise<void>;
}

interface Entry {
  url: string;
  blob: Blob;
  bytes: number;
}

type Attempt =
  | { kind: "ok"; blob: Blob }
  /** 404 / 415: the store has no (servable) picture under this key. */
  | { kind: "gone" }
  /** Another 4xx (gate refusal, bad key): not retried, not sticky. */
  | { kind: "denied" }
  /** Network error or 5xx: retried with backoff. */
  | { kind: "retry" };

async function decodeImage(url: string): Promise<void> {
  const img = new Image();
  img.src = url;
  await img.decode();
}

export class MediaCache {
  private readonly budget: number;
  private readonly doFetch: typeof fetch;
  private readonly mkUrl: (blob: Blob) => string;
  private readonly rmUrl: (url: string) => void;
  private readonly decode: (url: string) => Promise<void>;
  private readonly sleep: (ms: number) => Promise<void>;

  /** Loaded pictures, least recently used first (Map insertion order). */
  private readonly entries = new Map<string, Entry>();
  /** Keys the store answered 404/415 for: not asked again until `forget`. */
  private readonly gone = new Set<string>();
  private readonly inflight = new Map<string, Promise<MediaResult>>();
  private readonly pins = new Map<string, number>();
  private readonly listeners = new Set<() => void>();
  private totalBytes = 0;
  private active = 0;
  private readonly waiting: Array<() => void> = [];

  constructor(opts: MediaCacheOptions = {}) {
    this.budget = opts.compact === true ? MEDIA_BUDGET_COMPACT_BYTES : MEDIA_BUDGET_BYTES;
    this.doFetch = opts.fetchImpl ??
      ((input: RequestInfo | URL, init?: RequestInit) => globalThis.fetch(input, init));
    this.mkUrl = opts.createObjectURL ?? ((b: Blob) => URL.createObjectURL(b));
    this.rmUrl = opts.revokeObjectURL ?? ((u: string) => URL.revokeObjectURL(u));
    this.decode = opts.decode ?? decodeImage;
    this.sleep = opts.sleep ?? ((ms: number) => new Promise<void>((res) => setTimeout(res, ms)));
  }

  /** The byte budget in force (tests and diagnostics). */
  get budgetBytes(): number {
    return this.budget;
  }

  /** Bytes currently held. */
  get usedBytes(): number {
    return this.totalBytes;
  }

  /** Number of loaded pictures. */
  get size(): number {
    return this.entries.size;
  }

  /** Synchronous view: the loaded picture, `"missing"` (sticky), or undefined (not known yet). */
  peek(sha: string): MediaResult | undefined {
    const hit = this.entries.get(sha);
    if (hit !== undefined) {
      this.touch(sha, hit);
      return { url: hit.url, blob: hit.blob };
    }
    return this.gone.has(sha) ? "missing" : undefined;
  }

  /** Whether a fetch for `sha` is running or queued. */
  isLoading(sha: string): boolean {
    return this.inflight.has(sha);
  }

  load(sha: string): Promise<MediaResult> {
    const hit = this.entries.get(sha);
    if (hit !== undefined) {
      this.touch(sha, hit);
      return Promise.resolve({ url: hit.url, blob: hit.blob });
    }
    if (this.gone.has(sha) || !SHA256_RE.test(sha)) return Promise.resolve("missing");
    const flying = this.inflight.get(sha);
    if (flying !== undefined) return flying;
    const p = this.run(sha).finally(() => {
      this.inflight.delete(sha);
    });
    this.inflight.set(sha, p);
    return p;
  }

  /** Pin `sha` (refcounted): a pinned picture is never evicted or revoked. */
  pin(sha: string): void {
    this.pins.set(sha, (this.pins.get(sha) ?? 0) + 1);
  }

  unpin(sha: string): void {
    const n = this.pins.get(sha) ?? 0;
    if (n <= 1) this.pins.delete(sha);
    else this.pins.set(sha, n - 1);
    // Eviction was blocked while pinned: settle the budget now.
    this.evict(undefined);
  }

  /** Clear a sticky `missing` so the next `load` asks the store again. */
  forget(sha: string): void {
    this.gone.delete(sha);
  }

  /** Revoke every unpinned picture and clear every sticky `missing` (full join). */
  forgetAll(): void {
    this.gone.clear();
    for (const [sha, entry] of [...this.entries]) {
      if ((this.pins.get(sha) ?? 0) > 0) continue;
      this.drop(sha, entry);
    }
  }

  /** Panel teardown: revoke EVERYTHING (pins included) and stop notifying. */
  dispose(): void {
    this.gone.clear();
    this.pins.clear();
    this.listeners.clear();
    for (const [sha, entry] of [...this.entries]) this.drop(sha, entry);
  }

  /** Fires after a load settles or an entry is dropped. */
  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private emit(): void {
    for (const fn of [...this.listeners]) fn();
  }

  private touch(sha: string, entry: Entry): void {
    this.entries.delete(sha);
    this.entries.set(sha, entry);
  }

  private drop(sha: string, entry: Entry): void {
    this.entries.delete(sha);
    this.totalBytes -= entry.bytes;
    this.rmUrl(entry.url);
  }

  /** Evict least-recently-used unpinned pictures until within budget. */
  private evict(keep: string | undefined): void {
    if (this.totalBytes <= this.budget) return;
    for (const [sha, entry] of [...this.entries]) {
      if (this.totalBytes <= this.budget) break;
      if (sha === keep || (this.pins.get(sha) ?? 0) > 0) continue;
      this.drop(sha, entry);
    }
  }

  private async run(sha: string): Promise<MediaResult> {
    let got: Blob | undefined;
    for (let attempt = 0; ; attempt += 1) {
      const out = await this.attemptFetch(sha);
      if (out.kind === "ok") {
        got = out.blob;
        break;
      }
      if (out.kind === "gone") {
        this.gone.add(sha);
        this.emit();
        return "missing";
      }
      if (out.kind === "denied") return "missing";
      // Network error / 5xx: wait and retry, then give up WITHOUT caching
      // the failure (a later `load` asks again).
      if (attempt >= NETWORK_RETRY_DELAYS_MS.length) return "missing";
      await this.sleep(NETWORK_RETRY_DELAYS_MS[attempt]);
    }
    if (got === undefined) return "missing";
    const blob = got;
    const url = this.mkUrl(blob);
    try {
      await this.decode(url);
    } catch {
      // Bytes that are not a decodable picture: the same sha never will be.
      this.rmUrl(url);
      this.gone.add(sha);
      this.emit();
      return "missing";
    }
    const entry: Entry = { url, blob, bytes: blob.size };
    this.entries.set(sha, entry);
    this.totalBytes += entry.bytes;
    this.evict(sha);
    this.emit();
    return { url, blob };
  }

  private async attemptFetch(sha: string): Promise<Attempt> {
    await this.acquire();
    try {
      const res = await this.doFetch(`${currentDaemonBase()}/api/terms/media/${sha}`, {
        headers: { [TERMINAL_INTENT_HEADER]: TERMINAL_INTENT_VALUE },
        credentials: "same-origin",
      });
      if (res.status === 404 || res.status === 415) return { kind: "gone" };
      if (res.status >= 500) return { kind: "retry" };
      if (!res.ok) return { kind: "denied" };
      return { kind: "ok", blob: await res.blob() };
    } catch {
      return { kind: "retry" };
    } finally {
      this.release();
    }
  }

  private acquire(): Promise<void> {
    if (this.active < MAX_CONCURRENT_FETCHES) {
      this.active += 1;
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      this.waiting.push(() => {
        this.active += 1;
        resolve();
      });
    });
  }

  private release(): void {
    this.active -= 1;
    const next = this.waiting.shift();
    if (next !== undefined) next();
  }
}

/** The buffer rows scanned for seed-batch pictures: one screen above the viewport to one below it. */
export function prefetchWindow(
  viewportY: number,
  rows: number,
  bufferLength: number,
): { start: number; count: number } {
  const start = Math.max(0, viewportY - PREFETCH_SCREENS_ABOVE * rows);
  const end = Math.min(bufferLength, viewportY + PREFETCH_SCREENS_BELOW * rows);
  return { start, count: Math.max(0, end - start) };
}

/**
 * Which picture ids to start loading: every id with a placeholder cell in
 * the prefetch window (`cells` is the scan of that window), plus the live
 * defs (those that arrived after the seed completed). Seed-batch defs with
 * no cell in the window are NOT in the result: hundreds of pictures from a
 * long scrollback must not all load at join. Unique, cells first (in order
 * of appearance), then live ids ascending.
 */
export function idsToPrefetch(
  cells: readonly { id: number }[],
  liveDefIds: Iterable<number>,
): number[] {
  const out: number[] = [];
  const seen = new Set<number>();
  for (const c of cells) {
    if (!seen.has(c.id)) {
      seen.add(c.id);
      out.push(c.id);
    }
  }
  for (const id of [...liveDefIds].sort((a, b) => a - b)) {
    if (!seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}
