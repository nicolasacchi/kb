// W3.P-c — provenance REGISTERS: 26 letter-keyed slots holding a tagged
// reference (an artifact position, a selection, a session, a commit).
// `" <letter>` stores; `' <letter>` pastes it as a citation; the `?` sheet's
// "Your registers" panel offers the same paste plus add-to-list.
//
// ── THE SUBSUMPTION (read this before adding a second store) ──────────────
//
// W2.6b's MARKS (`lib/marks.ts` — `m <letter>` sets, `` ` <letter> `` jumps)
// were ALREADY a 26-slot letter-keyed localStorage store. Shipping registers
// beside them would have meant `m a` and `" a` both meaning "remember this
// place", with different payloads and different jump semantics — two homes
// for one action, exactly the failure invariant #30 exists to prevent. So
// this module is not a sibling of `marks.ts`, it is its GENERALIZATION:
//
//   * ONE store, ONE storage key (`kb:registers`), ONE 26-slot grammar.
//   * A register's payload is a TAGGED `Ref` union; a mark is precisely the
//     `kind: "artifact"` case (kb + source-relative path + active section).
//   * `lib/marks.ts` is now a thin, behaviour-preserving FAÇADE over this
//     module — same exported API, same semantics, same tests (unchanged).
//     `` ` <letter> `` jumps to any artifact-kind register, whether it was
//     stored by `m` or by `"`.
//   * Marks saved by the shipped W2.6b build (the old `kb:marks` array) are
//     migrated FORWARD on read — see `loadStore()` below; pinned by
//     `registers.test.ts` + the untouched `marks.test.ts`.
//
// ── STORAGE JUSTIFICATION (the one-container-primitive rule) ──────────────
//
// Registers are browser-local, ephemeral UI state in a schema-versioned,
// validated-on-read localStorage blob (`kb:registers`, `v: 1`) — the THIRD
// instance of a template already shipped twice (`lib/atlasCameras.ts` →
// `lib/marks.ts`). kb has exactly ONE durable collection primitive
// (kb-list/1), and a register is NOT collection-shaped: exactly one
// reference per letter slot, overwrite-by-letter, a hard cap of 26, no
// ordering, no membership semantics, no sharing, no export, and no server
// representation whatsoever. The moment someone wants to KEEP a SET of
// these, that is a reading list — and the registers panel offers exactly
// that (`AddToListButton`, reused verbatim), rather than growing a second
// durable collection here.
//
// ── CLI PARITY: RECORDED EXEMPTION ────────────────────────────────────────
//
// Registers never cross the network. There is no route, no table, no lance
// column, no SSE event — nothing for a `kb` verb to read or write. The
// server-noun→CLI-verb parity rule therefore does not apply; this is a
// recorded exemption, not an oversight (same shape as `atlasCameras.ts`'s).
//
// Pure module — no React, no fetch — covered by plain vitest (colocated
// `registers.test.ts`). The PAYLOAD grammar (what a register renders as
// when pasted) deliberately lives in `lib/quote.ts` instead, so there is
// ONE citation grammar shared with the `y p` provenance yank and the
// comment/selection cite paths.

/// An artifact position — the mark case. `sec` is the reader's active
/// heading id at capture time (null off a reader / when the artifact has
/// no headings). `id` is the artifact id when the doc was already in the
/// query cache at capture time; it is what the add-to-list paste needs, so
/// a register migrated forward from a W2.6b mark (which never stored one)
/// has `null` here and gets a cite-only row.
export type ArtifactRef = {
  kind: "artifact";
  kb: string;
  sourceRelative: string;
  title: string;
  sec: string | null;
  id?: string | null;
  /// `ArtifactSessionOut.authored` when the capture site knew it — same
  /// optional field `buildProvenanceBlock` already prints.
  sessionId?: string | null;
};

/// A highlight inside an artifact. The three fields mirror the wire
/// `Anchor`'s selection case (`css_path`/`offset`/`snippet`, invariant #25)
/// so `renderRegister` can hand them straight to `buildSelectionCite` and
/// the add-to-list paste can hand them straight to a list entry.
export type SelectionRef = {
  kind: "selection";
  kb: string;
  sourceRelative: string;
  title: string;
  sec: string | null;
  id?: string | null;
  cssPath: string;
  offset: number;
  snippet: string;
};

/// A captured Claude Code session (invariant #11's canonical session id).
export type SessionRef = {
  kind: "session";
  sessionId: string;
  title: string;
};

/// A git commit. Wire-ready: the union member, its validation and its
/// rendering are pinned here, but no SPA surface CAPTURES one yet — the
/// natural capture site is the session detail's resolved-commit rows.
// TODO(wave3): wire a capture site once the session detail exposes a
// per-commit action; until then a commit register can only arrive from a
// future writer, and reading one back already works.
export type CommitRef = {
  kind: "commit";
  sha: string;
  subject: string;
  repo?: string | null;
};

export type Ref = ArtifactRef | SelectionRef | SessionRef | CommitRef;
export type RefKind = Ref["kind"];

export type Register = {
  v: 1;
  /// Single lowercase letter a–z — the slot this register occupies.
  letter: string;
  /// Unix milliseconds (`Date.now()`), stamped by `setRegister`.
  savedAt: number;
  ref: Ref;
};

const STORAGE_KEY = "kb:registers";
/// W2.6b's marks blob. Read once (per read) and folded forward, then
/// removed — see `loadStore()`.
const LEGACY_MARKS_KEY = "kb:marks";

// ── storage primitives (every one of them fails soft) ─────────────────────

function readRaw(key: string): string | null {
  if (typeof localStorage === "undefined") return null;
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeRaw(key: string, value: string) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(key, value);
  } catch {
    // localStorage denied/full — the save doesn't persist past this
    // session; the caller's in-memory return value still reflects it for
    // the current tab (mirrors `atlasCameras.ts` / `marks.ts`).
  }
}

function removeRaw(key: string) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.removeItem(key);
  } catch {
    // Denied, or a test double without `removeItem` — either way the
    // migration below is idempotent, so a failed removal is harmless.
  }
}

// ── validation (a corrupt blob degrades to empty, never throws) ───────────

export function isRegisterLetter(x: unknown): x is string {
  return typeof x === "string" && /^[a-z]$/.test(x);
}

function isNonEmptyString(x: unknown): x is string {
  return typeof x === "string" && x.length > 0;
}

function isNullableString(x: unknown): x is string | null | undefined {
  return x === null || x === undefined || typeof x === "string";
}

function isValidRef(x: unknown): x is Ref {
  if (typeof x !== "object" || x === null) return false;
  const r = x as Record<string, unknown>;
  switch (r.kind) {
    case "artifact":
      return (
        isNonEmptyString(r.kb) &&
        typeof r.sourceRelative === "string" &&
        typeof r.title === "string" &&
        (r.sec === null || typeof r.sec === "string") &&
        isNullableString(r.id) &&
        isNullableString(r.sessionId)
      );
    case "selection":
      return (
        isNonEmptyString(r.kb) &&
        typeof r.sourceRelative === "string" &&
        typeof r.title === "string" &&
        (r.sec === null || typeof r.sec === "string") &&
        isNullableString(r.id) &&
        typeof r.cssPath === "string" &&
        typeof r.offset === "number" &&
        Number.isFinite(r.offset) &&
        typeof r.snippet === "string"
      );
    case "session":
      return isNonEmptyString(r.sessionId) && typeof r.title === "string";
    case "commit":
      return (
        isNonEmptyString(r.sha) &&
        typeof r.subject === "string" &&
        isNullableString(r.repo)
      );
    default:
      return false;
  }
}

function isValidRegister(x: unknown): x is Register {
  if (typeof x !== "object" || x === null) return false;
  const g = x as Record<string, unknown>;
  if (g.v !== 1) return false;
  if (!isRegisterLetter(g.letter)) return false;
  if (typeof g.savedAt !== "number" || !Number.isFinite(g.savedAt)) return false;
  return isValidRef(g.ref);
}

function parseRegisters(raw: string | null): Register[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isValidRegister);
  } catch {
    return [];
  }
}

// ── the forward migration (W2.6b `kb:marks` → `kb:registers`) ─────────────

/// One legacy mark row → an artifact-kind register, preserving `savedAt`
/// exactly (so a migrated mark keeps its original save time rather than
/// being re-stamped "now"). Returns `undefined` for a row that wouldn't
/// have been a valid mark either — the old `isValidMark` gate, verbatim.
function legacyMarkToRegister(x: unknown): Register | undefined {
  if (typeof x !== "object" || x === null) return undefined;
  const m = x as Record<string, unknown>;
  if (m.v !== 1) return undefined;
  if (!isRegisterLetter(m.letter)) return undefined;
  if (!isNonEmptyString(m.kb)) return undefined;
  if (typeof m.sourceRelative !== "string") return undefined;
  if (typeof m.title !== "string") return undefined;
  if (m.sec !== null && typeof m.sec !== "string") return undefined;
  if (typeof m.savedAt !== "number" || !Number.isFinite(m.savedAt)) return undefined;
  return {
    v: 1,
    letter: m.letter,
    savedAt: m.savedAt,
    ref: {
      kind: "artifact",
      kb: m.kb,
      sourceRelative: m.sourceRelative,
      title: m.title,
      sec: m.sec,
      // A W2.6b mark never carried the artifact id (jumping only needs the
      // path); the paste-to-list action degrades gracefully for these.
      id: null,
    },
  };
}

/// Every stored register, tolerant of corrupt/foreign JSON — malformed
/// entries are silently dropped rather than throwing.
///
/// This is also where the one-way W2.6b migration happens: if the legacy
/// `kb:marks` key is present AT ALL (even holding garbage) its valid rows
/// are folded in as artifact refs, the merged set is written to
/// `kb:registers`, and the legacy key is removed. Consuming the old key is
/// load-bearing, not tidiness — leaving it in place would let a
/// `clearRegister` be silently undone by the next read re-migrating the
/// same mark back in. A same-letter collision resolves in favour of the
/// EXISTING register (never clobber newer state with a stale mark).
function loadStore(): Register[] {
  const current = parseRegisters(readRaw(STORAGE_KEY));
  const legacyRaw = readRaw(LEGACY_MARKS_KEY);
  if (legacyRaw === null) return current;

  const byLetter = new Map<string, Register>();
  try {
    const parsed: unknown = JSON.parse(legacyRaw);
    if (Array.isArray(parsed)) {
      for (const row of parsed) {
        const migrated = legacyMarkToRegister(row);
        if (migrated) byLetter.set(migrated.letter, migrated);
      }
    }
  } catch {
    // Corrupt legacy blob — nothing to carry forward; still drop the key
    // below so the probe doesn't run on every single read forever.
  }
  for (const r of current) byLetter.set(r.letter, r);

  const merged = [...byLetter.values()];
  writeRaw(STORAGE_KEY, JSON.stringify(merged));
  removeRaw(LEGACY_MARKS_KEY);
  return merged;
}

function writeStore(registers: Register[]) {
  writeRaw(STORAGE_KEY, JSON.stringify(registers));
}

// ── the API ───────────────────────────────────────────────────────────────

/// Every set register. Order is storage order; callers sort for display
/// (the `?` sheet sorts by letter), mirroring `marks.ts`/`atlasCameras.ts`.
export function listRegisters(): Register[] {
  return loadStore();
}

/// One register by letter (case-insensitive — callers may pass a raw
/// `KeyboardEvent.key` straight through), or `undefined` when unset / the
/// letter is out of the a–z grammar / storage is corrupt/denied.
export function getRegister(letter: string): Register | undefined {
  const l = letter.toLowerCase();
  if (!isRegisterLetter(l)) return undefined;
  return loadStore().find((r) => r.letter === l);
}

/// Store (or overwrite, by letter) a reference and return the updated
/// list. `savedAt` is stamped here. A no-op (list unchanged) when `letter`
/// isn't a single a–z character or `ref` isn't a valid tagged reference —
/// the 26-slot grammar is enforced on the WRITE side too, so a bad caller
/// can't wedge an unreachable slot into storage.
export function setRegister(letter: string, ref: Ref): Register[] {
  const l = letter.toLowerCase();
  const existing = loadStore();
  if (!isRegisterLetter(l) || !isValidRef(ref)) return existing;
  const next: Register = { v: 1, letter: l, savedAt: Date.now(), ref };
  const updated = [...existing.filter((r) => r.letter !== l), next];
  writeStore(updated);
  return updated;
}

/// Clear one slot and return the updated list (a no-op clear still returns
/// the unchanged list — callers don't need to special-case "not set").
export function clearRegister(letter: string): Register[] {
  const l = letter.toLowerCase();
  const updated = loadStore().filter((r) => r.letter !== l);
  writeStore(updated);
  return updated;
}

// ── display helpers (labels, NOT the paste payload) ───────────────────────

/// The short human label for a register row in the `?` sheet. The PASTE
/// payload is `lib/quote.ts`'s `renderRegister` — deliberately a different
/// function in a different module, so the citation grammar has exactly one
/// home and this stays free to be chatty/lossy.
export function registerLabel(ref: Ref): string {
  switch (ref.kind) {
    case "artifact":
      return ref.title || ref.sourceRelative;
    case "selection":
      return `“${ref.snippet.slice(0, 40)}${ref.snippet.length > 40 ? "…" : ""}”`;
    case "session":
      return ref.title || ref.sessionId;
    case "commit":
      return ref.subject || ref.sha;
  }
}

/// The secondary "where it came from" line (kb name, session id, short
/// sha) — `null` when the label already says everything.
export function registerContext(ref: Ref): string | null {
  switch (ref.kind) {
    case "artifact":
    case "selection":
      return ref.kb;
    case "session":
      return ref.sessionId.slice(0, 12);
    case "commit":
      return ref.sha.slice(0, 8);
  }
}
