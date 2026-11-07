// kb-term tap: the headless xterm twin. Zero workspace dependencies beyond the
// pinned @xterm/* packages.
//
// The twin is the authoritative screen model of one pane: the post-transform
// stream is fed here, and T6/T7 seed late joiners from it (never from
// serialize() alone). onData is DISCARDED: the twin never answers a terminal
// query; the strip pipeline/responder own replies. `feed` resolves when xterm
// has parsed the chunk; chunking is 64 KiB like the spike's writeChunked.
//
// The unicode width table is pinned the way the spikes validated: the
// graphemes addon is loaded and `unicode.activeVersion` is set EXPLICITLY to
// the version key the addon registered matching /grapheme/i; if none exists
// makeTwin throws (spike T2: without activation xterm 6.0.0 splits 207 of 297
// kitty row/column diacritics into separate cells at width 1 instead of
// combining into the placeholder cell).
//
// The section at the bottom is an INTERNALS section: the only other place in
// the tap allowed to touch _core/_extendedAttrs/_oscLinkService is
// test/support/compare.ts.


import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const headless = require('@xterm/headless') as { Terminal: TerminalCtor };
const serializePkg = require('@xterm/addon-serialize') as { SerializeAddon: SerializeCtor };
const graphemesPkg = require('@xterm/addon-unicode-graphemes') as { UnicodeGraphemesAddon: GraphemesCtor };

export const TWIN_OPTS = { scrollback: 50000, scrollOnEraseInDisplay: true, allowProposedApi: true } as const;
/** Viewer-side scrollback value (carried in the Profile later, T7). The twin itself always uses 50000. */
export const PHONE_SCROLLBACK = 20000;
const FEED_CHUNK = 64 * 1024;

// Structural surface of the xterm Terminal we rely on. Kept minimal and
// pinned by tests; no @xterm TypeScript types are imported.
type TerminalCtor = new (opts: Record<string, unknown>) => TerminalLike;
type SerializeCtor = new () => SerializeLike;
type GraphemesCtor = new () => unknown;
type U11Ctor = new () => unknown;

interface TerminalLike {
  cols: number;
  rows: number;
  unicode: { versions: string[] | undefined; activeVersion: string };
  loadAddon(addon: unknown): void;
  onData(cb: () => void): void;
  write(data: Uint8Array | string, cb?: () => void): void;
  reset(): void;
  resize(cols: number, rows: number): void;
  dispose(): void;
  _core: unknown;
  buffer: unknown;
}

interface SerializeLike {
  serialize(): string;
}

export interface Twin {
  term: TerminalLike;
  serialize: SerializeLike;
  cols: number;
  rows: number;
  feed(b: Uint8Array | string): Promise<void>;
  reset(): void;
  resize(c: number, r: number): void;
  dispose(): void;
  repliesDropped(): number;
  /** The width-table pin of the ACTIVE variant: production is always
   * unicode-graphemes 0.4.0; the unicode11 test variant pins itself. */
  widthPin: { addon: 'unicode-graphemes' | 'unicode11'; version: string; activeVersion: string };
  /** Live line count of one buffer (scrollback rows + viewport rows). */
  lineCount(which: 'normal' | 'alt'): number;
}

export interface MakeTwinOpts {
  variant?: 'graphemes' | 'unicode11';
  /** Test seam: replace the graphemes addon constructor (default: the pinned addon). */
  graphemesAddonForTest?: new () => unknown;
}

const encoder = new TextEncoder();

function writeOnce(term: TerminalLike, piece: Uint8Array): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  term.write(piece, resolve);
  return promise;
}

export function makeTwin(cols: number, rows: number, opts?: MakeTwinOpts): Twin {
  const variant = opts?.variant ?? 'graphemes';
  const term = new headless.Terminal({ cols, rows, ...TWIN_OPTS });
  let dropped = 0;
  let activeVersion: string;
  // Error-path dispose must never mask the real throw: a stub/test addon
  // may lack dispose(), so clean up best-effort and always throw the pin error.
  const disposeBestEffort = (): void => {
    try {
      term.dispose();
    } catch {
      // ignore: the pinned throw below is the error that must surface
    }
  };
  if (variant === 'graphemes') {
    const Ctor = opts?.graphemesAddonForTest ?? graphemesPkg.UnicodeGraphemesAddon;
    term.loadAddon(new Ctor());
    const versions = (term.unicode.versions ?? []) as string[];
    const found = versions.find((k) => /grapheme/i.test(String(k)));
    if (!found) {
      disposeBestEffort();
      throw new Error(
        `graphemes addon registered no version; versions=${JSON.stringify(term.unicode.versions)}`,
      );
    }
    term.unicode.activeVersion = found;
    activeVersion = found;
  } else if (variant === 'unicode11') {
    // Test-only variant (width comparison): loaded lazily so the production
    // graphemes path never depends on the devDependency.
    const u11 = require('@xterm/addon-unicode11') as { Unicode11Addon: U11Ctor };
    term.loadAddon(new u11.Unicode11Addon());
    term.unicode.activeVersion = '11';
    activeVersion = '11';
  } else {
    disposeBestEffort();
    throw new Error(`bad variant ${String(variant)}`);
  }
  term.onData(() => {
    dropped++;
  });
  const ser = new serializePkg.SerializeAddon();
  term.loadAddon(ser);
  const twin: Twin = {
    term,
    serialize: ser,
    cols,
    rows,
    async feed(b) {
      const all = typeof b === 'string' ? encoder.encode(b) : b;
      for (let offset = 0; offset < all.length; offset += FEED_CHUNK) {
        await writeOnce(term, all.subarray(offset, Math.min(all.length, offset + FEED_CHUNK)));
      }
    },
    reset() {
      term.reset();
    },
    resize(c, r) {
      twin.cols = c;
      twin.rows = r;
      term.resize(c, r);
    },
    dispose() {
      term.dispose();
    },
    repliesDropped() {
      return dropped;
    },
    widthPin:
      variant === 'unicode11'
        ? { addon: 'unicode11', version: '0.9.0', activeVersion }
        : { addon: 'unicode-graphemes', version: '0.4.0', activeVersion },
    lineCount(which) {
      return bufferLineCount(term, which);
    },
  };
  return twin;
}

// ---------------------------------------------------------------- internals
// xterm internals access. Only this section (and test/support/compare.ts) may
// touch _core / _extendedAttrs / _oscLinkService.

interface BufferLines {
  lines: { length: number };
}

interface BufferServiceShape {
  _bufferService: { buffers: Record<string, BufferLines> };
}

export function bufferLineCount(term: TerminalLike, which: 'normal' | 'alt'): number {
  // One named cast at the boundary: the buffer-service shape is asserted once
  // here, not trusted inline at every read. A missing shape is an Error.
  const core: BufferServiceShape = term._core as BufferServiceShape;
  const b: BufferLines | undefined = core._bufferService.buffers[which];
  if (!b) throw new Error(`unknown buffer ${which}`);
  return b.lines.length;
}
