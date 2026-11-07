// Seeded fuzz: no throw, no APC G leak, stats identity, store integrity (KBT-GFX).
import { test, expect, afterAll } from 'bun:test';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { ImageStore } from '../src/imagestore.ts';
import { GraphicsExtractor } from '../src/graphics.ts';
import { makePng, kittyApc, transmitApcs, placeApc, deleteApc, enc, cat } from './helpers-graphics.ts';
import { mulberry32 } from './helpers.ts';

const parents: string[] = [];

afterAll(() => {
  for (const d of parents) rmSync(d, { recursive: true, force: true });
});

const CAN = 0x18;
const SUB = 0x1a;
const ESC = 0x1b;

function genFuzzStream(rand: () => number): Uint8Array {
  const pieces: Uint8Array[] = [];
  const n = 1 + Math.floor(rand() * 12);
  for (let i = 0; i < n; i++) {
    const r = rand();
    if (r < 0.3) {
      // byte soup
      const len = 1 + Math.floor(rand() * 40);
      const buf = new Uint8Array(len);
      for (let k = 0; k < len; k++) buf[k] = Math.floor(rand() * 256);
      pieces.push(buf);
    } else if (r < 0.5) {
      pieces.push(enc('hello world'));
    } else if (r < 0.62) {
      // valid/invalid kitty APCs
      const id = 1 + Math.floor(rand() * 20);
      const kind = rand();
      if (kind < 0.3) {
        const png = makePng(1 + Math.floor(rand() * 6), 1 + Math.floor(rand() * 6), Math.floor(rand() * 255));
        for (const a of transmitApcs(id, png, 16)) pieces.push(a);
      } else if (kind < 0.5) {
        pieces.push(placeApc(id, 1 + Math.floor(rand() * 10), 1 + Math.floor(rand() * 4)));
      } else if (kind < 0.65) {
        pieces.push(deleteApc(rand() < 0.5 ? 'A' : 'I', id));
      } else if (kind < 0.8) {
        pieces.push(kittyApc('a=t,f=24,i=1', 'AAAA'));
      } else {
        pieces.push(kittyApc('a=q,i=1'));
      }
    } else if (r < 0.72) {
      // oversize body
      pieces.push(kittyApc('a=t,f=100,i=99', 'B'.repeat(9000)));
    } else if (r < 0.8) {
      // IIP / sixel
      if (rand() < 0.5) pieces.push(enc('\x1b]1337;File=;size=8:QQ==\x07'));
      else pieces.push(enc('\x1bP0q\x01\x02\x1b\\'));
    } else if (r < 0.88) {
      // DCS-tmux fragment
      pieces.push(enc('\x1bPtmux;\x1b\x1b_Ga=d\x1b\\'));
    } else if (r < 0.94) {
      // aborted APC: CAN/SUB/ESC mid-body
      const abort = [CAN, SUB, ESC][Math.floor(rand() * 3)]!;
      const head = enc('\x1b_Ga=t,f=100,i=5,m=1;partial');
      const tail = new Uint8Array([abort, 0x61, 0x62]);
      pieces.push(cat([head, tail]));
    } else {
      pieces.push(enc('\x1b[1mtext\x1b]0;t\x07'));
    }
  }
  return cat(pieces);
}

function randomChunkCuts(rand: () => number, len: number): number[] {
  const cuts: number[] = [];
  let p = 0;
  while (p < len) {
    p += 1 + Math.floor(rand() * 64);
    if (p < len) cuts.push(p);
  }
  return cuts;
}

test('fuzz_never_throws_and_never_leaks_apc_g', () => {
  let streams = 0;
  let bytesIn = 0;
  let bytesOut = 0;
  for (let seed = 1; seed <= 2000; seed++) {
    const rand = mulberry32(seed);
    const stream = genFuzzStream(rand);
    const parent = mkdtempSync(join(tmpdir(), 'kbt-t4-fuzz-'));
    parents.push(parent);
    const store = new ImageStore({ sessionDir: join(parent, 'sess'), now: () => 1_000 });
    const ex = new GraphicsExtractor(store);
    const cuts = randomChunkCuts(rand, stream.length);
    const out: Uint8Array[] = [];
    let p = 0;
    for (const c of [...cuts, stream.length]) {
      for (const it of ex.push(stream.subarray(p, c))) {
        if (it.kind === 'bytes') out.push(it.data);
      }
      p = c;
    }
    for (const it of ex.flush()) {
      if (it.kind === 'bytes') out.push(it.data);
    }
    const outBytes = cat(out);
    streams++;
    bytesIn += stream.length;
    bytesOut += outBytes.length;
    // No exception (implicit) + no APC G leak.
    if ((outBytes as Buffer).includes(Buffer.from('\x1b_G'))) {
      throw new Error(`seed=${seed} output contains ESC _ G; in=${stream.length}`);
    }
    // Stats identity.
    const s = ex.stats;
    const drops = s.unsupportedFormat + s.unsupportedMedium + s.compressed
      + s.directPlacement + s.nonVirtualPlacement + s.queryAction
      + s.imageNumberUnsupported + s.orphanChunk + s.badBase64 + s.badPng
      + s.oversize + s.otherAction;
    if (s.apcs !== s.transmits + s.places + s.resets + s.chunks + drops) {
      throw new Error(`seed=${seed} stats identity: apcs=${s.apcs} transmits=${s.transmits} places=${s.places} resets=${s.resets} chunks=${s.chunks} drops=${drops}`);
    }
    // Store dir has no .tmp files; every img/seq filename is the sha of its content.
    for (const sub of ['img', 'seq']) {
      const dir = join(parent, 'sess', sub);
      for (const name of readdirSync(dir)) {
        if (name.startsWith('.tmp-')) throw new Error(`seed=${seed} leftover tmp ${name}`);
        const content = readFileSync(join(dir, name));
        const sha = createHash('sha256').update(content).digest('hex');
        if (sha !== name) throw new Error(`seed=${seed} bad filename ${name} != ${sha}`);
      }
    }
    if (seed % 500 === 0) {
      for (const d of parents.splice(0, parents.length)) rmSync(d, { recursive: true, force: true });
    }
  }
  console.log(`fuzz: streams=${streams} bytesIn=${bytesIn} bytesOut=${bytesOut}`);
  expect(streams).toBe(2000);
});
