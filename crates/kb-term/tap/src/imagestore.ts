// kb-term tap: sha256 image store + id map (KBT-GFX). Zero npm dependencies.
//
// PNGs under `img/<sha256>`, IIP/SIXEL originals under `seq/<sha256>`, and an
// append-only `idmap.jsonl` of full-record JSON lines (later lines override).
// File names are only the computed 64-hex sha256; ids appear only in idmap.jsonl.
// Eviction is LRU by least-recently-seen mtime and never edits the id map.

import {
  appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync,
  statSync, unlinkSync, writeFileSync, utimesSync,
} from 'node:fs';
import { dirname, join } from 'node:path';

export const SESSION_CAP_BYTES = 256 * 1024 * 1024;
export const HOST_CAP_BYTES = 1024 * 1024 * 1024;

export interface IdRecord {
  id: number;
  gen: number;
  sha256: string;
  mime: 'image/png';
  w: number;
  h: number;
  cols: number | null;
  rows: number | null;
}

const HEX64 = /^[0-9a-f]{64}$/;

export class ImageStore {
  badIdmapLines = 0;
  private readonly sessionDir: string;
  private readonly imgDir: string;
  private readonly seqDir: string;
  private readonly idmapPath: string;
  private readonly sessionCapBytes: number;
  private readonly hostCapBytes: number;
  private readonly hostRoot: string;
  private readonly now: () => number;
  private readonly map = new Map<number, IdRecord>();
  private tmpCounter = 0;
  private lastHostScan = -Infinity;

  constructor(opts: {
    sessionDir: string;
    sessionCapBytes?: number;
    hostCapBytes?: number;
    hostRoot?: string;
    now?: () => number;
  }) {
    this.sessionDir = opts.sessionDir;
    this.sessionCapBytes = opts.sessionCapBytes ?? SESSION_CAP_BYTES;
    this.hostCapBytes = opts.hostCapBytes ?? HOST_CAP_BYTES;
    this.imgDir = join(this.sessionDir, 'img');
    this.seqDir = join(this.sessionDir, 'seq');
    this.idmapPath = join(this.sessionDir, 'idmap.jsonl');
    this.hostRoot = opts.hostRoot ?? dirname(dirname(this.imgDir));
    this.now = opts.now ?? Date.now;
    mkdirSync(this.imgDir, { recursive: true });
    mkdirSync(this.seqDir, { recursive: true });
    for (const dir of [this.imgDir, this.seqDir]) {
      for (const name of readdirSync(dir)) {
        if (name.startsWith('.tmp-')) {
          try { unlinkSync(join(dir, name)); } catch { /* ignore */ }
        }
      }
    }
    this.replayIdmap();
  }

  putPng(sha: string, bytes: Uint8Array): { written: boolean } {
    assertHex(sha);
    return this.put(this.imgDir, sha, bytes);
  }

  putSeq(sha: string, bytes: Uint8Array): { written: boolean } {
    assertHex(sha);
    return this.put(this.seqDir, sha, bytes);
  }

  has(sha: string): boolean {
    if (!HEX64.test(sha)) return false;
    return existsSync(join(this.imgDir, sha));
  }

  hasSeq(sha: string): boolean {
    if (!HEX64.test(sha)) return false;
    return existsSync(join(this.seqDir, sha));
  }

  readPng(sha: string): Uint8Array | null {
    if (!HEX64.test(sha)) return null;
    try { return new Uint8Array(readFileSync(join(this.imgDir, sha))); }
    catch { return null; }
  }

  readSeq(sha: string): Uint8Array | null {
    if (!HEX64.test(sha)) return null;
    try { return new Uint8Array(readFileSync(join(this.seqDir, sha))); }
    catch { return null; }
  }

  bind(id: number, v: { sha256: string; w: number; h: number }): IdRecord {
    const existing = this.map.get(id);
    if (existing === undefined) {
      const rec: IdRecord = { id, gen: 1, sha256: v.sha256, mime: 'image/png', w: v.w, h: v.h, cols: null, rows: null };
      this.map.set(id, rec);
      this.append(rec);
      return { ...rec };
    }
    if (existing.sha256 === v.sha256 && existing.w === v.w && existing.h === v.h) {
      return { ...existing };
    }
    const rec: IdRecord = {
      ...existing,
      sha256: v.sha256,
      w: v.w,
      h: v.h,
      gen: existing.sha256 === v.sha256 ? existing.gen : existing.gen + 1,
    };
    this.map.set(id, rec);
    this.append(rec);
    return { ...rec };
  }

  place(id: number, cols: number, rows: number): boolean {
    const existing = this.map.get(id);
    if (existing === undefined) return false;
    if (existing.cols === cols && existing.rows === rows) return true;
    const rec: IdRecord = { ...existing, cols, rows };
    this.map.set(id, rec);
    this.append(rec);
    return true;
  }

  get(id: number): IdRecord | null {
    const r = this.map.get(id);
    return r === undefined ? null : { ...r };
  }

  entries(): IdRecord[] {
    return [...this.map.values()].map((r) => ({ ...r })).sort((a, b) => a.id - b.id);
  }

  // -- internals --

  private put(dir: string, sha: string, bytes: Uint8Array): { written: boolean } {
    const dest = join(dir, sha);
    const t = this.now();
    const when = new Date(t);
    if (existsSync(dest)) {
      try { utimesSync(dest, when, when); } catch { /* gone: ignored */ }
      this.enforceSession(dest);
      this.enforceHost(dest);
      return { written: false };
    }
    const tmp = join(dir, `.tmp-${process.pid}-${++this.tmpCounter}`);
    writeFileSync(tmp, bytes);
    renameSync(tmp, dest);
    try { utimesSync(dest, when, when); } catch { /* ignore */ }
    this.enforceSession(dest);
    this.enforceHost(dest);
    return { written: true };
  }

  private enforceSession(protectPath: string): void {
    const files = listFiles([this.imgDir, this.seqDir]);
    evictLru(files, this.sessionCapBytes, protectPath);
  }

  private enforceHost(protectPath: string): void {
    const t = this.now();
    if (t - this.lastHostScan < 5000) return;
    this.lastHostScan = t;
    const dirs: string[] = [];
    let names: string[];
    try { names = readdirSync(this.hostRoot); } catch { return; }
    for (const name of names) {
      const sess = join(this.hostRoot, name);
      dirs.push(join(sess, 'img'), join(sess, 'seq'));
    }
    const files = listFiles(dirs);
    evictLru(files, this.hostCapBytes, protectPath);
  }

  private append(rec: IdRecord): void {
    appendFileSync(this.idmapPath, JSON.stringify(rec) + '\n');
  }

  private replayIdmap(): void {
    let raw: string;
    try { raw = readFileSync(this.idmapPath, 'utf8'); }
    catch { return; }
    for (const line of raw.split('\n')) {
      if (line === '') continue;
      try {
        const v = JSON.parse(line) as IdRecord;
        if (typeof v.id !== 'number' || !Number.isSafeInteger(v.id)
          || typeof v.sha256 !== 'string' || !HEX64.test(v.sha256)
          || typeof v.gen !== 'number' || typeof v.w !== 'number' || typeof v.h !== 'number') {
          this.badIdmapLines++;
          continue;
        }
        const rec: IdRecord = {
          id: v.id, gen: v.gen, sha256: v.sha256, mime: 'image/png',
          w: v.w, h: v.h,
          cols: typeof v.cols === 'number' ? v.cols : null,
          rows: typeof v.rows === 'number' ? v.rows : null,
        };
        this.map.set(rec.id, rec);
      } catch {
        this.badIdmapLines++;
      }
    }
  }
}

function assertHex(sha: string): void {
  if (!HEX64.test(sha)) throw new Error(`invalid sha256: ${sha}`);
}

interface FileEntry { path: string; name: string; size: number; mtimeMs: number }

function listFiles(dirs: string[]): FileEntry[] {
  const out: FileEntry[] = [];
  for (const dir of dirs) {
    let names: string[];
    try { names = readdirSync(dir); } catch { continue; }
    for (const name of names) {
      if (name.startsWith('.tmp-')) continue;
      const p = join(dir, name);
      try {
        const s = statSync(p);
        if (!s.isFile()) continue;
        out.push({ path: p, name, size: s.size, mtimeMs: s.mtimeMs });
      } catch { /* ignore */ }
    }
  }
  return out;
}

function evictLru(files: FileEntry[], capBytes: number, protectPath: string): void {
  let total = 0;
  for (const f of files) total += f.size;
  if (total <= capBytes) return;
  files.sort((a, b) => a.mtimeMs - b.mtimeMs || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0) || (a.path < b.path ? -1 : 1));
  for (const f of files) {
    if (total <= capBytes) break;
    if (f.path === protectPath) continue;
    try { unlinkSync(f.path); total -= f.size; }
    catch (e) {
      if ((e as NodeJS.ErrnoException).code !== 'ENOENT') throw e;
    }
  }
}
