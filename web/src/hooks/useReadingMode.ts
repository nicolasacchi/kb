// OL READ2 - the SPA half of reading mode (the runtime half is READ1/READ1b:
// the artifact origin's `kb:read` handler + `/_kb/reading.css`).
//
// Intent and fact are kept apart. `override` is what the reader asked for
// (null = "no opinion yet": the per-kb `readingDefault` pref decides);
// `state` is only ever what the iframe ACKNOWLEDGED, so the More-menu row's
// `pressed` and the key's next move follow the page, not our hope.
//
// Per pane: ArtifactPane owns one of these per artifact. Both pieces of state
// are keyed by the artifact id they were set for, so opening another artifact
// in the same pane drops them without an effect round trip (no render where
// the new artifact briefly inherits the old one's override).
import { useCallback, useEffect, useRef, useState } from "react";
import { loadReadingDefault } from "../api/prefs";
import { toast } from "../lib/toast";

export type ReadingState = "off" | "on" | "unsuitable" | "unknown";
export type ReadingWhy = "author" | "media" | "short" | "load" | "house";
export type ReadingScheme = "light" | "dark";

/// What the SPA posts to the artifact iframe. Enums and a boolean only: the
/// runtime never reads anything else from the message (no CSS ever crosses).
export interface ReadMessage {
  kind: "kb:read";
  on: boolean;
  scheme: ReadingScheme;
}

/// How long a toggle waits for the runtime's `kb:read-state` before the pane
/// admits reading mode is not answering (an old daemon, a blocked frame).
export const ACK_TIMEOUT_MS = 800;

export const READING_TIMEOUT_COPY =
  "Reading mode is unavailable here (reload the page)";

const WHY_COPY: Record<ReadingWhy, string> = {
  author: "This page opts out of reading mode",
  media: "Reading mode is off: this page is mostly images or media",
  short: "Reading mode is off: this page has too little text",
  load: "Reading mode could not load its stylesheet",
  house: "This page already uses kb's reading style",
};

/// One sentence per refusal reason; the toast and the menu row's caption share
/// it so the two never disagree.
export function readingWhyCopy(why: string | undefined | null): string {
  return (
    (why && (WHY_COPY as Record<string, string>)[why]) ||
    "Reading mode is unavailable for this page"
  );
}

/// The runtime's ack, validated. Anything off-shape is null (dropped).
export function parseReadAck(
  msg: unknown,
): { state: "on" | "off" | "unsuitable"; why?: string } | null {
  if (!msg || typeof msg !== "object") return null;
  const m = msg as { kind?: unknown; state?: unknown; why?: unknown };
  if (m.kind !== "kb:read-state") return null;
  if (m.state !== "on" && m.state !== "off" && m.state !== "unsuitable") return null;
  return typeof m.why === "string" ? { state: m.state, why: m.why } : { state: m.state };
}

/// The SPA's effective colour scheme: `data-theme` light/dark as set, `system`
/// resolved through the OS preference.
export function resolveScheme(
  root: Pick<HTMLElement, "getAttribute"> = document.documentElement,
  matchMedia: ((q: string) => { matches: boolean }) | undefined = typeof window !==
    "undefined"
    ? window.matchMedia?.bind(window)
    : undefined,
): ReadingScheme {
  const t = root.getAttribute("data-theme");
  if (t === "light" || t === "dark") return t;
  return matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

interface Keyed<T> {
  id: string;
  v: T;
}

export interface UseReadingModeOptions {
  kb: string;
  id: string;
  /// Deliver a message to THIS pane's iframe (exact artifact origin, never
  /// `'*'`). False when there is no frame to deliver to.
  post: (msg: ReadMessage) => boolean;
  /// Fired when the runtime acknowledges `on` or `off` (not `unsuitable`):
  /// the page just reflowed, so overlays pinned to its text must repaint.
  onApplied?: (state: "on" | "off") => void;
}

export interface ReadingMode {
  state: ReadingState;
  /// True only when the iframe last acknowledged `on`.
  pressed: boolean;
  /// `override ?? readingDefault[kb]`.
  effective: boolean;
  /// Why the runtime refused (`unsuitable`), for the row caption.
  why: string | null;
  /// The reader's `r`. No-op (with the reason toast) on an unsuitable page.
  toggle: () => void;
  /// Feed a validated `kb:read-state` ack.
  onAck: (ack: { state: "on" | "off" | "unsuitable"; why?: string }) => void;
  /// Send the standing intent. Unforced (the effective-setting effect) skips a
  /// message identical to the last one sent; `force` (iframe load,
  /// `kb:read-ready`) always sends, because the document is new.
  sync: (force?: boolean) => void;
  /// The SPA theme changed: refresh the scheme of an applied sheet.
  syncScheme: () => void;
  /// The iframe loaded a new document: the old ack describes a page that is gone.
  onFrameLoad: () => void;
}

export function useReadingMode({
  kb,
  id,
  post,
  onApplied,
}: UseReadingModeOptions): ReadingMode {
  const [override, setOverrideRaw] = useState<Keyed<boolean> | null>(null);
  const [ack, setAckRaw] = useState<Keyed<{
    state: ReadingState;
    why: string | null;
  }> | null>(null);

  const o = override && override.id === id ? override.v : null;
  const a = ack && ack.id === id ? ack.v : { state: "unknown" as ReadingState, why: null };
  const effective = o ?? loadReadingDefault(kb);

  // Latest values for callbacks handed to long-lived listeners.
  const live = useRef({ id, kb, o, effective, state: a.state, why: a.why, post, onApplied });
  live.current = { id, kb, o, effective, state: a.state, why: a.why, post, onApplied };
  const userPending = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
  }, []);
  // A different artifact (or unmount) abandons any ack still being waited on.
  useEffect(() => {
    userPending.current = false;
    clearTimer();
    return clearTimer;
  }, [id, clearTimer]);

  // What was last posted for which artifact, so the effective-setting effect
  // does not repeat a message `toggle` has just sent. A frame load / ready
  // (a new document that starts from "off") restates the intent regardless.
  const lastSent = useRef<{ id: string; on: boolean } | null>(null);
  const send = useCallback((on: boolean): boolean => {
    lastSent.current = { id: live.current.id, on };
    return live.current.post({ kind: "kb:read", on, scheme: resolveScheme() });
  }, []);

  const toggle = useCallback(() => {
    const cur = live.current;
    if (cur.state === "unsuitable") {
      toast.info(readingWhyCopy(cur.why));
      return;
    }
    const next = cur.state !== "on";
    setOverrideRaw({ id: cur.id, v: next });
    userPending.current = true;
    send(next);
    clearTimer();
    const forId = cur.id;
    timer.current = setTimeout(() => {
      timer.current = null;
      if (live.current.id !== forId || !userPending.current) return;
      userPending.current = false;
      toast.info(READING_TIMEOUT_COPY);
    }, ACK_TIMEOUT_MS);
  }, [send, clearTimer]);

  const onAck = useCallback(
    (m: { state: "on" | "off" | "unsuitable"; why?: string }) => {
      clearTimer();
      const forId = live.current.id;
      setAckRaw({ id: forId, v: { state: m.state, why: m.why ?? null } });
      const asked = userPending.current;
      userPending.current = false;
      if (m.state === "unsuitable") {
        // Only a refusal of something the reader asked for speaks; an
        // automatic default-on that a page declines stays quiet.
        if (asked) toast.info(readingWhyCopy(m.why));
        return;
      }
      live.current.onApplied?.(m.state);
    },
    [clearTimer],
  );

  const sync = useCallback(
    (force = false) => {
      const cur = live.current;
      // Nothing asked and no default: leave the page exactly as served.
      // (Posting `off` would also strip a sheet the daemon applied at serve
      // time.)
      if (cur.o === null && !cur.effective) return;
      const last = lastSent.current;
      if (!force && last && last.id === cur.id && last.on === cur.effective) return;
      send(cur.effective);
    },
    [send],
  );

  const syncScheme = useCallback(() => {
    if (live.current.state === "on") send(true);
  }, [send]);

  const onFrameLoad = useCallback(() => {
    lastSent.current = null;
    setAckRaw(null);
  }, []);

  return {
    state: a.state,
    pressed: a.state === "on",
    effective,
    why: a.why,
    toggle,
    onAck,
    sync,
    syncScheme,
    onFrameLoad,
  };
}
