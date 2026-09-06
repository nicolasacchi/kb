import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import type { Anchor, ReviewFile } from "../api/client";
import { artifactOrigin } from "../lib/artifactHost";

// AnnotatorBridge — parent-side of the iframe annotator's postMessage
// protocol. Listens for `cm:compose` / `cm:focus` / `cm:probe` from the
// artifact subdomain (origin-checked), forwards a compose anchor to the
// panel, and exposes an imperative API for the CommentsPanel to push
// `cm:flash` / `cm:refresh` / `cm:emphasize` / `cm:mode` back into the iframe.
//
// The selection surface has two lanes, and both live here:
//   PUSH — `cm:selection` / `cm:selection-clear`, annotate.ts's mode-
//     independent capture, driven by the iframe's own selection events.
//   PULL — `querySelection()` → `cm:selection-query` → `cm:selection-pull`,
//     driven by a tap on chrome the SPA owns. The push lane cannot be made
//     reliable on every engine (Gecko collapses a selection BEFORE the click
//     that caused it, so the parent's mobile bar can unmount under the
//     user's finger); the pull lane sidesteps the whole class of problem by
//     asking at a moment WE choose. See annotate.ts's `cm:selection-query`
//     handler for the live→cache→null answer ladder.
//
// W2.16 — also relays `cm:selection` / `cm:selection-clear` (annotate.ts's
// mode-independent selection capture) to the SelectionActions chooser. The
// annotator's `rect` is a plain `Range.getBoundingClientRect()` — the
// IFRAME's own viewport coordinates, no scroll offset added. This bridge is
// what holds the iframe element itself, so it's the one place that can
// translate that rect into the PARENT page's viewport coordinates (iframe
// rect + child rect, both viewport-relative — no scroll math needed).
//
// Origin validation: every inbound message MUST come from the expected
// artifact origin (`<protocol>//<id>.artifacts.<root>:<port>`). The
// helper builds that origin from window.location's host + port; mismatches
// are dropped silently (no logs that could leak port info).

export type BridgeApi = {
  setMode: (on: boolean) => void;
  flash: (commentId: string) => void;
  /// Glow a comment's in-page highlight without scrolling (hover feedback).
  emphasize: (commentId: string, on: boolean) => void;
  refresh: (file: ReviewFile) => void;
  /// PULL the current selection out of the iframe (`cm:selection-query`).
  /// Fire-and-forget: the answer comes back asynchronously as exactly one
  /// `cm:selection-pull`, routed to `onSelectionPull`. This exists because
  /// the PUSH relay (`cm:selection`) is at the mercy of each engine's
  /// selection-event timing — on Gecko a tap collapses the selection before
  /// `click` fires, so the parent's bar can unmount under the user's own
  /// finger. A pull is driven by a tap on chrome the SPA owns, so nothing
  /// about the iframe's event ordering can defeat it.
  querySelection: () => void;
};

/// W2.16 — the narrowed Anchor shape a selection capture always carries
/// (never file/chapter/section). Shared by detail.tsx + SelectionActions so
/// neither has to re-narrow the union.
export type SelectionAnchor = Extract<Anchor, { kind: "selection" }>;
export type SelectionRect = { top: number; left: number; bottom: number };

export type AnnotatorBridgeProps = {
  iframeRef: React.RefObject<HTMLIFrameElement | null>;
  artifactId: string;
  /// The kb this artifact lives in — ARTIFACT HOST GRAMMAR v2 qualifies the
  /// iframe origin with it (`<kb_enc>--<artifactId>.artifacts.<root>`), so
  /// it must match what the detail view set on the iframe `src`, exactly
  /// like `artifactId`/`hostSuffix` below.
  kb: string;
  /// Daemon's authoritative artifact-subdomain suffix (from /api/identity).
  /// Must match what the detail view sets on the iframe `src`, or every
  /// postMessage gets dropped on the origin check.
  hostSuffix: string;
  /// Latest annotate-mode value. The bridge re-sends `cm:mode` to the
  /// annotator on every cm:probe (the iframe's hello on script load),
  /// using this ref so the iframe always learns the current mode after
  /// any navigation triggered by ?cm=on flipping.
  annotateMode: boolean;
  /// Called when the user clicks inside the artifact to start a comment
  /// (`cm:compose`). The detail view opens the panel composer (the shared
  /// tabbed editor) with this anchor pre-filled; body composition + save
  /// happen there, not in the iframe.
  onComposeAnchor: (anchor: Anchor) => void;
  /// Called when the user clicks an in-page marker (`cm:focus`). The
  /// detail view opens the panel + scrolls/flashes that comment's row.
  onFocusComment: (commentId: string) => void;
  /// Called when the user presses Esc inside the artifact iframe while
  /// annotate mode is on (`cm:exit-annotate`). Parent decides what to
  /// drop (in-progress draft vs. annotate mode itself).
  onExitAnnotate: () => void;
  /// W2.16 — a non-collapsed text selection was made inside the artifact
  /// (`cm:selection`, mode-independent — fires in view mode too). `rect`
  /// arrives already translated into the PARENT page's viewport
  /// coordinates (see the header comment); `scrollY` is the IFRAME's own
  /// scroll offset at capture time, passed through untranslated — it is the
  /// offset that rect is only valid at, and the caller compares it against
  /// the debounced `kb:scroll` beacon (see ArtifactPane's `kb:scroll`
  /// branch).
  onSelection: (
    anchor: SelectionAnchor,
    rect: SelectionRect,
    scrollY: number,
  ) => void;
  /// The selection collapsed (`cm:selection-clear`) — dismiss the chooser.
  onSelectionClear: () => void;
  /// Answer to a `querySelection()` pull (`cm:selection-pull`). `rect` is
  /// translated into parent-viewport coordinates exactly like `onSelection`
  /// above; both are null when the iframe had nothing to report (no live
  /// selection and no cached one within its freshness window), which the
  /// caller should surface rather than swallow (invariant #32).
  onSelectionPull?: (
    anchor: SelectionAnchor | null,
    rect: SelectionRect | null,
  ) => void;
};

const AnnotatorBridge = forwardRef<BridgeApi, AnnotatorBridgeProps>(
  function AnnotatorBridge(
    {
      iframeRef,
      artifactId,
      kb,
      hostSuffix,
      annotateMode,
      onComposeAnchor,
      onFocusComment,
      onExitAnnotate,
      onSelection,
      onSelectionClear,
      onSelectionPull,
    },
    ref,
  ) {
    // Hold latest props in refs so the message handler stays stable.
    const annotateModeRef = useRef(annotateMode);
    annotateModeRef.current = annotateMode;
    const onComposeRef = useRef(onComposeAnchor);
    onComposeRef.current = onComposeAnchor;
    const onFocusRef = useRef(onFocusComment);
    onFocusRef.current = onFocusComment;
    const onExitRef = useRef(onExitAnnotate);
    onExitRef.current = onExitAnnotate;
    const onSelectionRef = useRef(onSelection);
    onSelectionRef.current = onSelection;
    const onSelectionClearRef = useRef(onSelectionClear);
    onSelectionClearRef.current = onSelectionClear;
    const onSelectionPullRef = useRef(onSelectionPull);
    onSelectionPullRef.current = onSelectionPull;
    const annotatorReady = useRef(false);

    const expectedOrigin = expectedIframeOrigin(artifactId, kb, hostSuffix);

    useEffect(() => {
      function onMessage(ev: MessageEvent) {
        if (ev.origin !== expectedOrigin) return;
        const data = ev.data as Record<string, unknown> | null;
        if (!data || typeof data !== "object" || !("type" in data)) return;
        const type = data.type as string;
        if (type === "cm:probe") {
          annotatorReady.current = true;
          // Re-sync annotate mode after every probe so the iframe
          // learns the current value even after a navigation (e.g.
          // ?cm=on flipping rebuilds the iframe).
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:mode",
            on: annotateModeRef.current,
          });
          return;
        }
        if (type === "cm:focus") {
          const id = data.commentId as string | undefined;
          if (id) onFocusRef.current(id);
          return;
        }
        if (type === "cm:compose") {
          const anchor = data.anchor as Anchor | undefined;
          if (anchor) onComposeRef.current(anchor);
          return;
        }
        if (type === "cm:exit-annotate") {
          onExitRef.current();
          return;
        }
        if (type === "cm:selection") {
          const anchor = data.anchor as SelectionAnchor | undefined;
          const rect = data.rect as SelectionRect | undefined;
          const frame = iframeRef.current;
          if (anchor && rect && frame) {
            const f = frame.getBoundingClientRect();
            onSelectionRef.current(
              anchor,
              {
                top: f.top + rect.top,
                left: f.left + rect.left,
                bottom: f.top + rect.bottom,
              },
              typeof data.scroll_y === "number" ? data.scroll_y : 0,
            );
          }
          return;
        }
        if (type === "cm:selection-clear") {
          onSelectionClearRef.current();
          return;
        }
        if (type === "cm:selection-pull") {
          // The pull's answer. Same rect translation as `cm:selection` above
          // (iframe-viewport → parent-viewport) — deliberately the same
          // arithmetic rather than a shared helper, since the null branch
          // makes the shapes diverge and this is three lines.
          const anchor = (data.anchor as SelectionAnchor | null) ?? null;
          const rect = (data.rect as SelectionRect | undefined) ?? null;
          const frame = iframeRef.current;
          if (anchor && rect && frame) {
            const f = frame.getBoundingClientRect();
            onSelectionPullRef.current?.(anchor, {
              top: f.top + rect.top,
              left: f.left + rect.left,
              bottom: f.top + rect.bottom,
            });
          } else {
            // "Nothing selected" is a real answer, not a dropped message —
            // pass it through so the caller can tell the user (#32).
            onSelectionPullRef.current?.(null, null);
          }
          return;
        }
      }
      window.addEventListener("message", onMessage);
      return () => window.removeEventListener("message", onMessage);
    }, [iframeRef, expectedOrigin]);

    useImperativeHandle(
      ref,
      () => ({
        setMode: (on: boolean) =>
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:mode",
            on,
          }),
        flash: (commentId: string) =>
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:flash",
            commentId,
          }),
        emphasize: (commentId: string, on: boolean) =>
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:emphasize",
            commentId,
            on,
          }),
        refresh: (next: ReviewFile) =>
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:refresh",
            file: next,
          }),
        querySelection: () =>
          postToIframe(iframeRef.current, expectedOrigin, {
            type: "cm:selection-query",
          }),
      }),
      [iframeRef, expectedOrigin],
    );

    return null;
  },
);

export default AnnotatorBridge;

/// `<protocol>//<kb_enc>--<artifactId>.artifacts.<root>[:port]` — must
/// match the URL the detail view sets on the iframe `src`. `suffix` is the
/// daemon's authoritative `artifact_host_suffix` from /api/identity.
export function expectedIframeOrigin(
  artifactId: string,
  kb: string,
  suffix: string,
): string {
  return artifactOrigin(artifactId, kb, suffix);
}

function postToIframe(
  iframe: HTMLIFrameElement | null,
  targetOrigin: string,
  payload: unknown,
) {
  if (!iframe || !iframe.contentWindow) return;
  try {
    iframe.contentWindow.postMessage(payload, targetOrigin);
  } catch {
    // `postMessage` throws synchronously on a malformed targetOrigin
    // (e.g. the host suffix hasn't resolved yet, or the SPA is on a
    // bare IP). Drop the message rather than letting the throw escape
    // the effect that called us and tear down the React tree.
  }
}
