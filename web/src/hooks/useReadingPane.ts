// OL READ2 - ArtifactPane's reading-mode transport and key, kept out of the
// 1900-line component so each piece can be driven by a test with a fake frame.
//
// Trust: every message from the iframe is accepted only when its origin is
// THIS pane's exact artifact origin (`isOriginOfArtifact`, invariant #7 v2 -
// the suffix-only check passes for every artifact frame) AND `event.source`
// is this pane's own `contentWindow`. Every message TO the iframe goes to that
// exact origin, never '*'. A then|now split puts two frames on one origin;
// the source check is what tells them apart.
import { useCallback, useEffect, useRef, type RefObject } from "react";
import { THEME_APPLIED_EVENT } from "@kb/ui/themes/apply";
import type { ReviewFile } from "../api/client";
import type { BridgeApi } from "../components/AnnotatorBridge";
import { artifactOrigin, isOriginOfArtifact } from "../lib/artifactHost";
import {
  parseReadAck,
  useReadingMode,
  type ReadMessage,
  type ReadingMode,
} from "./useReadingMode";

/// Annotator repaint cadence after the page reflowed: one frame, then once
/// fonts and the layout around them have settled.
export const REPAINT_SETTLE_MS = 400;

export interface UseReadingPaneOptions {
  kb: string;
  id: string;
  hostSuffix: string;
  iframeRef: RefObject<HTMLIFrameElement | null>;
  /// The annotator is mounted on this pane (`?cm=on`; not a time-travelled pane).
  annotateActive: boolean;
  reviewFile: ReviewFile | null;
  bridgeRef: { current: BridgeApi | null };
  /// Dismiss the held selection chooser (its rect is pinned to the old layout).
  onSelectionChange: (selection: null) => void;
}

export function useReadingPane(o: UseReadingPaneOptions): ReadingMode {
  const { kb, id, hostSuffix, iframeRef } = o;

  const post = useCallback(
    (msg: ReadMessage): boolean => {
      const frame = iframeRef.current?.contentWindow;
      if (!frame || !id) return false;
      try {
        frame.postMessage(msg, artifactOrigin(id, kb, hostSuffix));
        return true;
      } catch {
        return false;
      }
    },
    [iframeRef, id, kb, hostSuffix],
  );

  // After the runtime applies or removes the sheet the page has reflowed, so
  // the annotator's highlights and the held selection are pinned to the old
  // layout. Dismiss the chooser, then repaint on the next frame and again
  // after the layout settles. Nothing when no annotator is mounted.
  const annot = useRef(o);
  annot.current = o;
  const timers = useRef<{ raf: number | null; t: ReturnType<typeof setTimeout> | null }>({
    raf: null,
    t: null,
  });
  const cancelRepaint = useCallback(() => {
    if (timers.current.raf !== null) cancelAnimationFrame(timers.current.raf);
    if (timers.current.t) clearTimeout(timers.current.t);
    timers.current = { raf: null, t: null };
  }, []);
  useEffect(() => cancelRepaint, [cancelRepaint]);
  const onApplied = useCallback(() => {
    if (!annot.current.annotateActive) return;
    annot.current.onSelectionChange(null);
    const repaint = () => {
      const c = annot.current;
      if (c.annotateActive && c.reviewFile) c.bridgeRef.current?.refresh(c.reviewFile);
    };
    cancelRepaint();
    timers.current.raf = requestAnimationFrame(() => {
      timers.current.raf = null;
      repaint();
    });
    timers.current.t = setTimeout(() => {
      timers.current.t = null;
      repaint();
    }, REPAINT_SETTLE_MS);
  }, [cancelRepaint]);

  const base = useReadingMode({ kb, id, post, onApplied });
  const ref = useRef(base);
  ref.current = base;
  // The iframe loaded a document: its old ack is void, and the new runtime
  // starts from "off" - so restate the standing intent straight away rather
  // than waiting for its `kb:read-ready` (which is the backstop).
  const onFrameLoad = useCallback(() => {
    ref.current.onFrameLoad();
    ref.current.sync(true);
  }, []);
  const reading: ReadingMode = { ...base, onFrameLoad };

  // Standing intent: on mount and whenever the effective setting changes. A
  // frame that has not loaded yet drops the message; the caller's iframe
  // `onLoad` and the runtime's `kb:read-ready` cover that case.
  useEffect(() => {
    if (id) ref.current.sync();
  }, [id, base.effective]);

  // The SPA theme changed: an applied sheet re-reads the scheme. No attribute
  // observer - the theme-apply path announces itself.
  useEffect(() => {
    const onTheme = () => ref.current.syncScheme();
    window.addEventListener(THEME_APPLIED_EVENT, onTheme);
    return () => window.removeEventListener(THEME_APPLIED_EVENT, onTheme);
  }, []);

  useEffect(() => {
    function onMessage(e: MessageEvent) {
      if (!isOriginOfArtifact(e.origin, id, kb, hostSuffix)) return;
      const frame = iframeRef.current?.contentWindow;
      // Strict: a source-less event is NOT let through (acks drive state and
      // a toast; the legacy pumps tolerate synthetic events, this one cannot).
      if (!frame || e.source !== frame) return;
      const data = e.data as { kind?: unknown } | null;
      if (!data || typeof data !== "object") return;
      if (data.kind === "kb:read-ready") {
        ref.current.onReady();
        ref.current.sync(true);
        return;
      }
      const ack = parseReadAck(data);
      if (ack) ref.current.onAck(ack);
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [iframeRef, hostSuffix, id, kb]);

  return reading;
}

/// A modal dialog is open - the same markers `isOverlayOpen` (useRovingCursor)
/// and HotkeyRoot use, with one exception. On a phone the reader-tools sheet
/// (`#kb-reader-sheet`, PreviewInspector `asSheet`) is `role=dialog
/// aria-modal` in the DOM for as long as the reader is mounted, open or not,
/// so only an OPEN sheet (`.detail--inspector-open` on the route) counts.
export function modalOpen(doc: Document = document): boolean {
  const sheetOpen = !!doc.querySelector(".detail--inspector-open");
  for (const n of Array.from(
    doc.querySelectorAll('dialog[open], [role="dialog"][aria-modal="true"]'),
  )) {
    if (n.id === "kb-reader-sheet" && !sheetOpen) continue;
    return true;
  }
  return false;
}

/// Whether a keydown is a bare `r` meant for the reader: lowercase, no
/// modifier of any kind (shift included), not mid-IME, not typing in an
/// input/textarea/select/contenteditable (CM6 composers are contenteditable),
/// and no dialog/sheet holding the keyboard.
export function isReadingKey(e: KeyboardEvent): boolean {
  if (e.key !== "r") return false;
  if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || e.isComposing) return false;
  const t = e.target as HTMLElement | null;
  if (
    t &&
    (t.tagName === "INPUT" ||
      t.tagName === "TEXTAREA" ||
      t.tagName === "SELECT" ||
      t.isContentEditable)
  ) {
    return false;
  }
  return !modalOpen();
}

/// Bare `r` toggles reading mode on the FOCUSED pane: every pane mounts this,
/// but only the focused one listens, so two panes never both act on one key
/// (the same rule as the pane's `o`/`b` binds). The route mounts ArtifactPane
/// only on the reader, so "off the reader" needs no check of its own.
export function useReadingKey(focused: boolean, toggle: () => void): void {
  const t = useRef(toggle);
  t.current = toggle;
  useEffect(() => {
    if (!focused) return;
    function onKey(e: KeyboardEvent) {
      if (!isReadingKey(e)) return;
      e.preventDefault();
      t.current();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [focused]);
}
