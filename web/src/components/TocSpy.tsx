import { useEffect, useMemo, useState } from "react";
import { useTocSpyState } from "../hooks/useTocSpyState";
import { artifactOrigin, isOriginOfArtifact } from "../lib/artifactHost";
import { artifactHref } from "../lib/artifactHref";
import { Icon } from "./icons";
import AddToListButton from "./lists/AddToListButton";
import type { ReadingSummary } from "../api/reading";

type TocItem = { id: string; text: string; level: number; top: number };

// v0.12 P3 — TOC mini-spy overlay.
//
// Subscribes to the artifact runtime's `kb:toc` + `kb:section`
// postMessages (P3-side counterpart to the runtime's scroll capture).
// Renders a compact right-edge strip listing each heading; the active
// section gets an accent border-left. Clicking a row posts
// `kb:scroll-to-id` back to the iframe.
//
// Reader control: a binary toggle (expanded ↔ shrunk) persisted
// globally via `useTocSpyState`. Header click in expanded; rail
// click in shrunk — and the `g t` chord from HotkeyRoot does the
// same. The empty-TOC short-circuit still runs first so artifacts
// without headings show no rail regardless of stored state.
export default function TocSpy({
  iframeRef,
  hostSuffix,
  summary,
  kb,
  relPath,
  artifactId,
}: {
  iframeRef: React.RefObject<HTMLIFrameElement | null>;
  hostSuffix: string;
  // RP-track — when present, each TOC row is shaded by its reading state
  // (read / skim / unseen) with intensity scaled by dwell, and the
  // stop-point gets a marker. Joined on the heading id.
  summary?: ReadingSummary | null;
  // RLs1 — the artifact's address, for the per-row copy-section-link
  // (`?sec=<heading-id>` permalink). The heading ids the runtime emits
  // are the same id space `?sec=` (and list Section anchors) target.
  kb: string;
  relPath: string;
  // RLs4 — for the per-row add-section-to-reading-list picker.
  artifactId: string;
}) {
  const [toc, setToc] = useState<TocItem[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const { state, advance } = useTocSpyState();

  useEffect(() => {
    function onMessage(e: MessageEvent) {
      // W3.P-a — EXACT origin, not the suffix-only trust boundary: with two
      // artifact panes on screen every artifact iframe passes the suffix
      // check, so a suffix-only guard would let the other pane's TOC /
      // active-section overwrite this spy's.
      if (!isOriginOfArtifact(e.origin, artifactId, kb, hostSuffix)) return;
      const data = e.data as { kind?: string; toc?: TocItem[]; id?: string } | null;
      if (!data || typeof data !== "object" || !data.kind) return;
      if (data.kind === "kb:toc" && Array.isArray(data.toc)) {
        setToc(data.toc as TocItem[]);
      } else if (data.kind === "kb:section" && typeof data.id === "string") {
        setActive(data.id);
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [hostSuffix, artifactId, kb]);

  // Clear when the iframe ref changes (e.g. user navigated to a new
  // artifact) so the spy doesn't briefly show the previous TOC.
  useEffect(() => {
    setToc([]);
    setActive(null);
  }, [iframeRef]);

  // Cap to ~20 items to keep the spy compact; deeply-nested artifacts
  // still get something rather than an unbounded rail.
  const items = useMemo(() => toc.slice(0, 20), [toc]);

  // RP-track — join the reading summary onto the TOC by heading id (the
  // runtime's live DOM id, shared on both sides). Unknown ids render unshaded
  // (degrade). `--kb-dwell` (0..1) scales the heat by dwell vs the most-dwelt
  // section. These hooks run before the empty-TOC early return so hook order
  // stays stable.
  const secById = useMemo(() => {
    const m = new Map<string, { state: string; dwell: number }>();
    for (const s of summary?.sections ?? []) {
      m.set(s.section_id, { state: s.state, dwell: s.dwell_ms });
    }
    return m;
  }, [summary]);
  const maxDwell = useMemo(() => {
    let mx = 1;
    for (const s of summary?.sections ?? []) mx = Math.max(mx, s.dwell_ms);
    return mx;
  }, [summary]);
  const stopId = summary?.stopped_at?.section_id ?? null;

  if (items.length === 0) return null;

  if (state === "shrunk") {
    return (
      <button
        type="button"
        className="kb-toc-spy kb-toc-spy--shrunk"
        onClick={advance}
        aria-label="expand topics"
        title="expand Topics (g t)"
      >
        <span className="kb-toc-spy__rune">Topics</span>
      </button>
    );
  }

  const jumpTo = (id: string) => {
    const frame = iframeRef.current?.contentWindow;
    if (!frame) return;
    try {
      // Target the artifact's own origin (not "*") — invariant #7.
      frame.postMessage(
        { kind: "kb:scroll-to-id", id },
        artifactOrigin(artifactId, kb, hostSuffix),
      );
    } catch (e) {
      console.warn("[toc] postMessage failed", e);
    }
  };

  // RLs1 — copy a `?sec=` section permalink. Same quiet-failure shape as
  // the ContextBar's copyLink (clipboard may be denied in non-https).
  const copySectionLink = async (id: string) => {
    try {
      await navigator.clipboard.writeText(
        window.location.origin + artifactHref(kb, relPath, { sec: id }),
      );
    } catch {
      // Fail quietly — the row click still scrolls.
    }
  };

  return (
    <div className="kb-toc-spy" aria-label="table of contents">
      <button
        type="button"
        className="kb-toc-spy__head"
        onClick={advance}
        title="shrink Topics (g t)"
        aria-label="shrink topics"
      >
        Topics
      </button>
      {items.map((h) => {
        const sec = secById.get(h.id);
        const heat = sec ? sec.dwell / maxDwell : 0;
        const cls = [
          "kb-toc-spy__item",
          `kb-toc-spy__item--l${h.level}`,
          h.id === active ? "is-on" : "",
          sec ? `kb-toc-spy__item--${sec.state}` : "",
          h.id === stopId ? "is-stop" : "",
        ]
          .filter(Boolean)
          .join(" ");
        return (
          // A row wraps the jump button + the hover-revealed copy button
          // (buttons can't nest, so the row is a div).
          <div key={h.id} className="kb-toc-spy__row">
            <button
              type="button"
              className={cls}
              style={
                sec ? ({ "--kb-dwell": heat } as React.CSSProperties) : undefined
              }
              onClick={() => jumpTo(h.id)}
              title={sec ? `${h.text} — ${sec.state}` : h.text}
            >
              {h.text}
            </button>
            <button
              type="button"
              className="kb-toc-spy__copy"
              onClick={() => void copySectionLink(h.id)}
              title="copy link to this section"
              aria-label={`copy link to section ${h.text}`}
            >
              <Icon.Copy />
            </button>
            <AddToListButton
              kb={kb}
              artifactId={artifactId}
              anchor={{ kind: "section", id: h.id, tag: null, snippet: null }}
              variant="icon"
            />
          </div>
        );
      })}
    </div>
  );
}
