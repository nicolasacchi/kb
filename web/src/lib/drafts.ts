// A-SPA — durable comment-composer drafts. Every composer in
// CommentsPanel/CommentModal held its draft as bare React state: a reload,
// a crashed tab, or a stray `discardCompose` mis-click threw the text away
// with no recovery. This is a tiny localStorage-backed store plus a React
// hook that hydrates/persists/clears it, so an in-progress draft survives
// the things bare `useState` can't.
//
// Key grammar: `kb-draft/1:{kb}:{artifactId}:{slot}`, slot one of
//   `file`              — the file-scope composer
//   `compose:<anchor>`  — the routed-anchor composer, `<anchor>` a stable
//                         serialization of the clicked Anchor (below)
//   `reply:<commentId>` — a comment's reply composer. Deliberately the
//                         SAME slot for CommentRow's inline reply box and
//                         CommentModal's reply box: both are "the draft
//                         for replying to this comment", not two drafts.
// Colon-joined, unescaped — the same low-ceremony style as the existing
// `kb:lid:<url>` SSE cursor keys (sse/transport.ts); kb names and artifact
// ids never contain colons elsewhere in this codebase, so the join is
// unambiguous in practice.
//
// Every localStorage touch is try/catch-wrapped: private browsing / a full
// quota throws on write (sometimes on read), and a draft is a nice-to-have
// — never worth crashing a composer over.

import { useCallback, useEffect, useRef, useState } from "react";
import type { Anchor } from "../api/client";

const KEY_PREFIX = "kb-draft/1:";
const TTL_MS = 7 * 24 * 60 * 60 * 1000; // 7 days
const DEBOUNCE_MS = 400;

type StoredDraft = { text: string; savedAt: number };

function draftKey(kb: string, artifactId: string, slot: string): string {
  return `${KEY_PREFIX}${kb}:${artifactId}:${slot}`;
}

/// Anchor has no stable-id kind for `selection` (matching lists'
/// `AddToListButton.targetMatches`, invariant #25): equality is structural
/// over the fields that round-trip through the server's `anchor_to_json`.
/// No JS equivalent of that Rust serializer exists, so this derives a
/// stable string client-side from the same field set, in the server's
/// field order — deterministic and collision-free across anchor kinds
/// (each branch is tagged by its own `kind` prefix).
export function stableAnchorKey(anchor: Anchor): string {
  switch (anchor.kind) {
    case "file":
      return "file";
    case "chapter":
      return `chapter:${anchor.path}`;
    case "section":
      return `section:${anchor.id}:${anchor.tag ?? ""}:${anchor.snippet ?? ""}`;
    case "selection":
      return `selection:${anchor.css_path}:${anchor.offset}:${anchor.snippet}`;
  }
}

let swept = false;

/// Sweep every expired `kb-draft/1:*` key out of localStorage. Runs at
/// most once per page load (module-level latch), lazily on the first
/// `readDraft` call — draft cleanup rides ordinary use, it isn't a
/// background job.
function sweepExpired(): void {
  if (swept) return;
  swept = true;
  try {
    const now = Date.now();
    const stale: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (!k || !k.startsWith(KEY_PREFIX)) continue;
      const raw = localStorage.getItem(k);
      if (raw === null) continue;
      if (!isFresh(raw, now)) stale.push(k);
    }
    for (const k of stale) localStorage.removeItem(k);
  } catch {
    // private mode / no localStorage — nothing to sweep
  }
}

function isFresh(raw: string, now: number): boolean {
  try {
    const parsed = JSON.parse(raw) as Partial<StoredDraft>;
    return (
      typeof parsed.savedAt === "number" && now - parsed.savedAt <= TTL_MS
    );
  } catch {
    return false; // unparseable — treat as expired
  }
}

/// Read a slot's persisted text, or "" if none / expired / unavailable.
export function readDraft(kb: string, artifactId: string, slot: string): string {
  sweepExpired();
  try {
    const raw = localStorage.getItem(draftKey(kb, artifactId, slot));
    if (raw === null) return "";
    const parsed = JSON.parse(raw) as Partial<StoredDraft>;
    if (typeof parsed.text !== "string") return "";
    if (!isFresh(raw, Date.now())) return "";
    return parsed.text;
  } catch {
    return "";
  }
}

/// Persist a slot's text. An empty string clears it (a blank draft is the
/// same as no draft — no point keeping a `{text:"",savedAt:…}` tombstone
/// around for 7 days).
export function writeDraft(
  kb: string,
  artifactId: string,
  slot: string,
  text: string,
): void {
  try {
    if (!text) {
      localStorage.removeItem(draftKey(kb, artifactId, slot));
      return;
    }
    const value: StoredDraft = { text, savedAt: Date.now() };
    localStorage.setItem(draftKey(kb, artifactId, slot), JSON.stringify(value));
  } catch {
    // private mode / quota — silent no-op
  }
}

/// Clear a slot outright — a successful submit or a user-confirmed
/// discard, neither of which should leave anything to restore.
export function clearDraft(kb: string, artifactId: string, slot: string): void {
  try {
    localStorage.removeItem(draftKey(kb, artifactId, slot));
  } catch {
    // ignore
  }
}

/// Test-only: reset the lazy-sweep latch so a fresh stubbed localStorage
/// gets swept again within the same test file (mirrors toast.ts's
/// `__resetToasts`).
export function __resetDraftsForTests(): void {
  swept = false;
}

// --- React binding -----------------------------------------------------

export type DraftState = {
  /// Current text — hydrated from storage on mount and on every slot
  /// change (kb/artifactId/slot), otherwise driven by `setText`.
  text: string;
  /// Update the text; schedules a debounced (~400ms) persist. Switching
  /// to a new slot (or unmounting) flushes any pending write first, so a
  /// fast anchor-switch never drops the last few keystrokes.
  setText: (v: string) => void;
  /// Reset to "" and drop the persisted copy immediately — call on a
  /// successful submit or a confirmed discard.
  clear: () => void;
};

/// `slot === null` disables persistence entirely (e.g. a composer with no
/// addressable target yet) — `text`/`setText` still work as plain state,
/// `clear` just resets it, and storage is never touched.
export function useDraft(
  kb: string,
  artifactId: string,
  slot: string | null,
): DraftState {
  const [text, setTextState] = useState("");
  // The (kb, artifactId, slot) tuple this component currently reflects;
  // `undefined` before the first hydration. Compared against the CURRENT
  // tuple each render so a slot change (not just kb/artifactId) is caught
  // even though slot is often a derived string, not a prop.
  const hydratedKeyRef = useRef<string | undefined>(undefined);
  const pendingRef = useRef<{
    kb: string;
    artifactId: string;
    slot: string;
    text: string;
  } | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const flush = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    const p = pendingRef.current;
    if (p) {
      pendingRef.current = null;
      writeDraft(p.kb, p.artifactId, p.slot, p.text);
    }
  }, []);

  useEffect(() => {
    const key = slot === null ? undefined : `${kb}:${artifactId}:${slot}`;
    if (hydratedKeyRef.current === key) return;
    // Persist whatever was pending under the OUTGOING key before swapping
    // — the debounce timer alone wouldn't fire in time for a fast switch.
    flush();
    hydratedKeyRef.current = key;
    setTextState(slot === null ? "" : readDraft(kb, artifactId, slot));
  }, [kb, artifactId, slot, flush]);

  // Flush on unmount — `flush` is referentially stable, so this effect's
  // cleanup only ever runs on the real unmount, not on every render.
  useEffect(() => flush, [flush]);

  const setText = useCallback(
    (v: string) => {
      setTextState(v);
      if (slot === null) return;
      pendingRef.current = { kb, artifactId, slot, text: v };
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        flush();
      }, DEBOUNCE_MS);
    },
    [kb, artifactId, slot, flush],
  );

  const clear = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    pendingRef.current = null;
    setTextState("");
    if (slot !== null) clearDraft(kb, artifactId, slot);
  }, [kb, artifactId, slot]);

  return { text, setText, clear };
}
