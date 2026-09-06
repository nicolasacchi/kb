// CT-F6 — Memento (RFC 7089) resolution over an artifact's version timeline:
// given an instant, WHICH version stood at that moment.
//
// LOCK-STEP with `kb_core::versions::resolve_memento` (crates/kb-core/src/
// versions.rs) — the same pick rule, the same metadata, the same goldens
// (see memento.test.ts, which mirrors the Rust `resolve_as_of_tests` cases
// one-for-one). Keep the two in step; the repo's precedent for a deliberate
// SPA-side mirror of a Rust rule is invariant #29 (wikilink) and #25
// (kb-list grammar).
//
// Why a mirror rather than the wire: the SPA already holds the full
// timeline in the `["versions", kb, id]` query cache (useVersions), and
// resolution is a pure scan over it — a `?at=` round-trip would be a second
// fetch for an answer already in hand. The daemon's `?at=` query exists for
// the API/CLI consumers that DON'T hold the list (`kb versions --at`).
//
// Boundaries, same as the Rust side: this is PER-ARTIFACT resolution. It is
// not a corpus timeline, and it touches no ranking whatsoever.

import type { Version } from "../api/versions";

export type Memento = {
  /** Echo of the requested instant (unix seconds). */
  atUnix: number;
  /**
   * The newest version at or before `atUnix`; `null` when every known
   * version is NEWER than that. Never silently the oldest version — a miss
   * stays a miss.
   */
  version: Version | null;
  /**
   * True only when the resolved version's own `ts_unix` equals `atUnix`, so
   * an approximate answer can never be rendered as an exact one.
   */
  exact: boolean;
  /** Oldest known version's timestamp — the floor a miss ran past. */
  oldestTsUnix: number | null;
};

/**
 * Resolve a Memento coordinate.
 *
 * Precondition (same as the Rust `resolve_as_of`): `versions` is in the
 * façade's own newest-first order — exactly what `GET .../versions` returns
 * and `useVersions` hands over. The function trusts that order rather than
 * re-deriving it, which would be a second, drift-prone copy of the server's
 * sort. `oldestTsUnix` uses a min() scan so it stays right regardless.
 */
export function resolveMemento(versions: Version[], atUnix: number): Memento {
  const version = versions.find((v) => v.ts_unix <= atUnix) ?? null;
  const oldest = versions.reduce<number | null>(
    (min, v) => (min === null || v.ts_unix < min ? v.ts_unix : min),
    null,
  );
  return {
    atUnix,
    version,
    exact: version !== null && version.ts_unix === atUnix,
    oldestTsUnix: oldest,
  };
}

/**
 * One-line human copy for a resolution — the SPA mirror of the Rust
 * `Memento::miss_note` + the CLI's `memento_line`. A hit ALWAYS names the
 * relation, so "nearest prior" is never dropped on the way to the reader.
 */
export function mementoSummary(m: Memento, fmt: (ts: number) => string): string {
  if (!m.version) {
    return m.oldestTsUnix === null
      ? `no version of this artifact is old enough — it has no recorded versions`
      : `no version of this artifact is that old — the oldest is ${fmt(m.oldestTsUnix)}`;
  }
  const relation = m.exact ? "exact match" : "nearest prior";
  return `${m.version.short} @ ${fmt(m.version.ts_unix)} (${relation})`;
}

/**
 * Parse a `?at=` URL param — unix SECONDS, matching the daemon's wire
 * grammar exactly. TOTAL: anything that isn't a finite integer is `null`
 * (a malformed deep link degrades to the ordinary reader, never to a
 * fabricated instant). Mirrors `paneUrl.ts`'s parse-is-total discipline.
 */
export function parseAtParam(raw: string | null): number | null {
  if (raw === null) return null;
  const t = raw.trim();
  if (!/^-?\d+$/.test(t)) return null;
  const n = Number(t);
  return Number.isSafeInteger(n) ? n : null;
}
