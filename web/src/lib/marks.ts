// W2.6b — cross-kb bookmark marks (`m` then a–z sets, backtick then a–z
// jumps — see `lib/keymap.ts` + `components/chrome/HotkeyRoot.tsx`).
//
// W3.P-c — THIS FILE IS NOW A FAÇADE. Marks used to own their own 26-slot
// letter-keyed localStorage blob (`kb:marks`). Provenance REGISTERS
// (`lib/registers.ts`) generalize exactly that store to a tagged `Ref`
// union, and a mark IS the `kind: "artifact"` case — so rather than ship a
// second 26-slot letter store beside the first (two homes for one action,
// the failure invariant #30 exists to prevent), marks were SUBSUMED:
//
//   * the bytes live in `kb:registers` (schema-versioned, validated on
//     read); marks saved by the shipped W2.6b build are migrated forward
//     on first read (`registers.ts`'s `loadStore()`),
//   * this module's exported API, semantics and tests are UNCHANGED — it
//     is a lens: artifact-kind registers, flattened back to the old `Mark`
//     shape that `HotkeyRoot`'s `m`/backtick handlers already speak,
//   * so `` ` a `` jumps to whatever is in slot `a`, whether `m a` or
//     `" a` put it there.
//
// Everything below the type is a projection. Read `lib/registers.ts` for
// the store itself, its storage justification, and the recorded CLI-parity
// exemption.
//
// Pure module — no React, no fetch — covered by plain vitest (colocated
// `marks.test.ts`, deliberately untouched by the subsumption).

import {
  clearRegister,
  getRegister,
  listRegisters,
  setRegister,
  type Register,
} from "./registers";

export type Mark = {
  v: 1;
  /// Single lowercase letter a–z — the slot this mark occupies.
  letter: string;
  kb: string;
  /// `doc.source_relative` — the artifact permalink is path-based (Track U).
  sourceRelative: string;
  /// Best-effort human label for the listing (falls back to
  /// `sourceRelative` when no cached doc title was available at save time).
  title: string;
  /// The active section heading id at save time (TocSpy/detail.tsx's
  /// `kb:section` state), or `null` when the reader has no headings / the
  /// mark was set from a non-reader route.
  sec: string | null;
  /// Unix milliseconds (`Date.now()`), used for newest-first sort.
  savedAt: number;
};

/// An artifact-kind register, flattened to the historical `Mark` shape.
/// `undefined` for every other kind — a session/commit register has no
/// artifact position to jump to, so it simply isn't a mark.
function toMark(r: Register): Mark | undefined {
  if (r.ref.kind !== "artifact") return undefined;
  return {
    v: 1,
    letter: r.letter,
    kb: r.ref.kb,
    sourceRelative: r.ref.sourceRelative,
    title: r.ref.title,
    sec: r.ref.sec,
    savedAt: r.savedAt,
  };
}

/// Every saved mark, tolerant of corrupt/foreign JSON — malformed entries
/// are silently dropped rather than throwing. Order is storage order;
/// callers sort for display (newest-first via `savedAt`, mirroring
/// `atlasCameras`'s convention of leaving sort to the caller).
export function listMarks(): Mark[] {
  const out: Mark[] = [];
  for (const r of listRegisters()) {
    const m = toMark(r);
    if (m) out.push(m);
  }
  return out;
}

/// One mark by its letter (case-insensitive — callers may pass a raw
/// `KeyboardEvent.key` straight through), or `undefined` when unset / the
/// letter is out of the a–z grammar / storage is corrupt/denied / the slot
/// holds a non-artifact reference (a session/commit register isn't a place
/// backtick can jump to).
export function getMark(letter: string): Mark | undefined {
  const r = getRegister(letter);
  return r ? toMark(r) : undefined;
}

/// Save (or overwrite, by letter) a mark and return the updated list.
/// `savedAt` is stamped by the store — callers pass everything else. A
/// no-op (list unchanged) when `letter` isn't a single a–z character — the
/// 26-slot grammar is enforced on the write side, not just at read, so a
/// bad caller can't wedge an unreachable slot into storage.
export function saveMark(
  letter: string,
  data: Omit<Mark, "v" | "letter" | "savedAt">,
): Mark[] {
  setRegister(letter, {
    kind: "artifact",
    kb: data.kb,
    sourceRelative: data.sourceRelative,
    title: data.title,
    sec: data.sec,
  });
  return listMarks();
}

/// Delete by letter and return the updated list (a no-op delete still
/// returns the unchanged list — callers don't need to special-case "not
/// found", mirroring `atlasCameras.deleteCamera`).
export function deleteMark(letter: string): Mark[] {
  clearRegister(letter);
  return listMarks();
}
