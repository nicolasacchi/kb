// Differential test (T3, spec 6): src/strip.ts (typed arrays + byte fast path) and
// test/reference-strip.ts (frozen spike, array-per-byte) must produce byte-identical
// output AND identical stripped/aborted/capPasses/kinds counters and the same
// pending() after every chunk. Both are driven through the same helper
// (runChunks) with a factory. This is the safety net for the fast path.
import { test, expect } from 'bun:test';
import { E, enc, cat, eq, hexs, mulberry32, wrap, makeProd, makeRef, runChunks, FRAGMENTS, TAILS, SOUP, genStream, soup, harness, inject, randomCuts } from './helpers.ts';
import { goldens } from './goldens.ts';

const bytes = (x: string | Uint8Array) => (typeof x === 'string' ? enc(x) : x);

function compare(tag: string, data: Uint8Array, cutsList: number[][]): string[] {
  const fails: string[] = [];
  for (const cuts of cutsList) {
    const a = runChunks(data, cuts, makeProd);
    const b = runChunks(data, cuts, makeRef);
    if (!eq(a.out, b.out)) { fails.push(`${tag} OUTPUT cuts=${cuts.length > 3 ? 'bytewise' : JSON.stringify(cuts)} in=${hexs(data)} prod=${hexs(a.out)} ref=${hexs(b.out)}`); return fails; }
    for (let i = 0; i < a.steps.length; i++) {
      const sa = a.steps[i], sb = b.steps[i];
      if (!eq(sa.out, sb.out) || sa.pending !== sb.pending || sa.stripped !== sb.stripped ||
          sa.aborted !== sb.aborted || sa.capPasses !== sb.capPasses ||
          JSON.stringify(sa.kinds) !== JSON.stringify(sb.kinds)) {
        fails.push(`${tag} STEP ${i}/${a.steps.length} cuts=${JSON.stringify(cuts)} in=${hexs(data)} prod=${JSON.stringify(sa)} ref=${JSON.stringify(sb)}`);
        return fails;
      }
    }
    if (a.s.stripped !== b.s.stripped || a.s.aborted !== b.s.aborted || a.s.capPasses !== b.s.capPasses) {
      fails.push(`${tag} COUNTERS in=${hexs(data)}`);
      return fails;
    }
  }
  return fails;
}



test('differential: every golden, whole + byte-wise + sampled 2-way + 3-way', () => {
  const fails: string[] = [];
  const r = mulberry32(0xd1ff);
  for (const [name, input] of goldens) {
    const data = bytes(input);
    const cutsList: number[][] = [[]];
    cutsList.push(Array.from({ length: Math.max(0, data.length - 1) }, (_, i) => i + 1));
    for (let k = 0; k < 6 && data.length > 2; k++) cutsList.push(randomCuts(r, data.length));
    if (data.length <= 36) for (let i = 1; i < data.length; i++) for (let j = i + 1; j < data.length; j++) cutsList.push([i, j]);
    fails.push(...compare(`golden:${name}`, data, cutsList));
    if (fails.length > 8) break;
  }
  expect(fails).toEqual([]);
});

test('differential: fake_harness seeds 1..5 with queries injected at random offsets', () => {
  const fails: string[] = [];
  const r = mulberry32(0x7eed);
  for (let seed = 1; seed <= 5; seed++) {
    const base = harness(seed, 60);
    for (let k = 0; k < 4; k++) {
      const data = inject(r, base, 1 + Math.floor(r() * 12));
      const cutsList: number[][] = [[], randomCuts(r, data.length), randomCuts(r, data.length)];
      fails.push(...compare(`harness:${seed}.${k}`, data, cutsList));
    }
    if (fails.length > 8) break;
  }
  expect(fails).toEqual([]);
});

test('differential: token soup', () => {
  const fails: string[] = [];
  const r = mulberry32(0x5010);
  for (let i = 0; i < 300; i++) {
    const data = soup(r);
    const cutsList: number[][] = [[], randomCuts(r, data.length), randomCuts(r, data.length)];
    if (data.length <= 48) cutsList.push(Array.from({ length: Math.max(0, data.length - 1) }, (_, k) => k + 1));
    fails.push(...compare(`soup:${i}`, data, cutsList));
    if (fails.length > 8) break;
  }
  // SOUP tokens alone must be covered by the stream above
  const soupJoined = enc(SOUP.join(''));
  fails.push(...compare('soup:all', soupJoined, [[], Array.from({ length: soupJoined.length - 1 }, (_, k) => k + 1)]));
  expect(fails).toEqual([]);
});

// chunk boundaries inside multibyte UTF-8 and inside ESC [ / ESC P tmux; / ESC _ G openers
test('differential: chunk boundaries inside openers and UTF-8', () => {
  const r = mulberry32(0xb0d);
  const fails: string[] = [];
  const seqs = [
    `${E}[?2026h`, `${E}[>q`, `${E}[6n`, `${E}]11;?${BEL}`,
    `${E}Ptmux;${E}${E}[c${E}\\`, `${E}_Gi=31337,a=q${E}\\\\`, `π─日é\u{10EEEE}😀`,
    `a${E}[cb${E}[6nc`, `${E}P+q4d${E}\\\\`,
  ];
  for (const s of seqs) {
    const data = enc(s);
    const cutsList: number[][] = [[]];
    // every single-cut position (covers boundaries inside multibyte chars and openers)
    for (let i = 1; i < data.length; i++) cutsList.push([i]);
    cutsList.push(Array.from({ length: Math.max(0, data.length - 1) }, (_, i) => i + 1));
    cutsList.push(randomCuts(r, data.length));
    fails.push(...compare(`opener:${hexs(data)}`, data, cutsList));
    if (fails.length > 8) break;
  }
  expect(fails).toEqual([]);
});

test('differential: generated fuzz streams byte-wise (short ones)', () => {
  const fails: string[] = [];
  const r = mulberry32(0xf022);
  let bytewise = 0;
  for (let i = 0; i < 400; i++) {
    const data = genStream(r);
    const cutsList: number[][] = [[], randomCuts(r, data.length)];
    if (data.length <= 48) {
      cutsList.push(Array.from({ length: Math.max(0, data.length - 1) }, (_, k) => k + 1));
      bytewise++;
    }
    fails.push(...compare(`gen:${i}`, data, cutsList));
    if (fails.length > 8) break;
  }
  expect(bytewise).toBeGreaterThan(0);
  expect(fails).toEqual([]);
});


