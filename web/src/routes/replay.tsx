// W3.R-c — the session-replay reader: `/replay/:kb/:sid`.
//
// Transcript and artifact under ONE playhead. The left rail is the session's
// narrative (prompt-rooted segments, every beat in wire order); the right
// stage is whatever artifact was in play at that beat, section-highlighted
// through the iframe runtime's existing `kb:scroll-to-id`.
//
// Design rulings worth not re-litigating:
//
// * **A ROUTE, not a 7th inspector sub-tab.** Invariant #30 pins the reader's
//   merged inspector rail at 6 sub-tabs, and this surface needs the whole
//   viewport (two panes + a scrubber) anyway. Lazy-loaded from `app.tsx` like
//   every other route, so its chunk (and `styles/replay.css`, imported here
//   rather than from `main.tsx`) only ships to someone who opens a replay.
//
// * **The scrubber is INDEX-space, not time-space.** A real session is a few
//   dense minutes inside hours of idle; a time-linear scrubber would be
//   mostly dead air with every interesting beat crushed into a few pixels.
//   One stop per beat, with each stop's Δt printed so the clock is still
//   visible (`lib/replay.ts`'s golden-pinned `formatGap`).
//
// * **NO autoplay, no timers, no media.** Pull-only: the operator scrubs.
//   There is deliberately no play/pause control and nothing in this file
//   calls `setInterval`/`setTimeout` to advance anything.
//
// * **A replay scrub is NOT a read.** This route renders its OWN small
//   iframe instead of reusing `components/reader/ArtifactPane.tsx`, because
//   that pane unconditionally `recordOpen`s a visit on mount (and flushes
//   reading beacons on unmount). Scrubbing through 200 beats would INSERT
//   history rows for artifacts nobody opened — corrupting the append-only
//   history table and the derived reading progress (invariants #8 / #19).
//   The pane's other jobs (annotator, TOC spy, folio chrome, selection
//   actions) are all reader-mode concerns this surface doesn't have, so the
//   duplication is one `<iframe>` element and a sandbox constant.
//
// * **Highlight granularity is SECTION-level, and every derived signal is
//   labelled "detected".** Line ranges exist for only a minority of Read
//   calls (kb-core never infers one), and byte-level "watch the artifact come
//   into being" is not buildable — artifact snapshots only append on content
//   change, are capped, exclude memory-session artifacts, and no route serves
//   bytes at a version ref. So: the beat says which file, and when the
//   transcript happened to carry a line range we scroll to the heading that
//   range fell under. Nothing more is claimed.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useSessionReplay } from "../hooks/useSessionReplay";
import { useArtifactHostSuffix } from "../hooks/useArtifactHost";
import { artifactOrigin, isOriginOfArtifact } from "../lib/artifactHost";
import { isEditableTarget } from "../lib/keymap";
import {
  beatPathLabel,
  formatElapsed,
  formatGap,
  groupSegments,
  kindGlyph,
  kindLabel,
  lineRangeLabel,
  playheadTarget,
  segmentIndexOf,
  stepSegment,
  type PlayheadTarget,
} from "../lib/replay";
import type { ReplayBeatOut } from "../api/client";
import "../styles/replay.css";

// Mirrors `kb_core::iframe::SANDBOX_FLAGS` (and `ArtifactPane`'s copy): the
// artifact origin is a distinct `<id>.artifacts.<root>` origin, and
// allow-same-origin here means "same origin as ITSELF", not as the parent.
const SANDBOX =
  "allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox " +
  "allow-forms allow-modals allow-downloads";

/// Playhead position lives in the URL (`?b=`) so a replay stop is linkable
/// and survives a reload — written with `replace` so stepping a beat doesn't
/// bury the back button under a hundred history entries.
const PARAM = "b";

export default function ReplayRoute() {
  const { kb, sid } = useParams<{ kb: string; sid: string }>();
  const [params, setParams] = useSearchParams();
  const { data, loading, error } = useSessionReplay(sid ?? null);
  const hostSuffix = useArtifactHostSuffix();
  useDocumentTitle(sid ? `Replay · ${sid}` : "Replay");

  const beats: ReplayBeatOut[] = useMemo(() => data?.beats ?? [], [data]);
  const segments = useMemo(() => groupSegments(beats), [beats]);

  const raw = Number.parseInt(params.get(PARAM) ?? "0", 10);
  const index = beats.length === 0
    ? 0
    : Math.min(Math.max(Number.isFinite(raw) ? raw : 0, 0), beats.length - 1);

  const setIndex = useCallback(
    (next: number) => {
      const p = new URLSearchParams(params);
      p.set(PARAM, String(next));
      setParams(p, { replace: true });
    },
    [params, setParams],
  );

  // DEP-RR7 — `index` is derived from the URL (`?b=`), and under
  // `v7_startTransition` (main.tsx) the `setParams` call below now commits
  // through a LOW-priority React transition rather than synchronously. Two
  // ArrowRight/j presses fired back to back (as a real "hold the key" burst
  // does, and as Playwright's two `keyboard.press` calls do) can both reach
  // this handler before the first transition has committed and re-rendered
  // `index` — both would then compute `next` from the SAME stale `index`,
  // so the second press overwrote the first's target instead of advancing
  // past it. `pendingRef` mirrors the effective in-flight index: it's kept
  // in sync with the real (committed) `index` whenever they agree, but a
  // step ALSO advances it immediately, so the very next keydown in the same
  // burst steps from where the previous one left off rather than from
  // whatever has (or hasn't yet) landed in the URL.
  const pendingRef = useRef(index);
  useEffect(() => {
    pendingRef.current = index;
  }, [index]);

  // ── keyboard (scope "replay" in lib/keymap.ts) ──────────────────────────
  // One route-local window listener, the W2.6a two-layer rule: the registry
  // documents the grammar for the `?` sheet, this owns execution. Not
  // registered as `scope: "global"` because `useRovingCursor` runs its own
  // independent window listener and would double-handle j/k.
  const scrubRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    if (beats.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      const onScrubber = !!t && t === scrubRef.current;
      // Typing anywhere editable wins. The range input IS an INPUT, so it
      // trips `isEditableTarget` too — but its own native ←/→/Home/End are
      // exactly the semantics we want, so we let those through natively and
      // still serve the vim/segment keys while it holds focus.
      if (isEditableTarget(t) && !onScrubber) return;
      const last = beats.length - 1;
      const cur = pendingRef.current;
      let next: number | null = null;
      switch (e.key) {
        case "ArrowRight":
          if (onScrubber) return; // native range step
          next = Math.min(cur + 1, last);
          break;
        case "ArrowLeft":
          if (onScrubber) return; // native range step
          next = Math.max(cur - 1, 0);
          break;
        case "j":
          next = Math.min(cur + 1, last);
          break;
        case "k":
          next = Math.max(cur - 1, 0);
          break;
        case "]":
          next = stepSegment(segments, cur, 1);
          break;
        case "[":
          next = stepSegment(segments, cur, -1);
          break;
        case "Home":
          if (onScrubber) return;
          next = 0;
          break;
        case "End":
          if (onScrubber) return;
          next = last;
          break;
        default:
          return;
      }
      if (next === null) return;
      e.preventDefault();
      if (next !== cur) {
        pendingRef.current = next;
        setIndex(next);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // `index` deliberately NOT a dependency: the handler reads the playhead
    // via `pendingRef` (see above) precisely so it doesn't need to
    // re-subscribe — and wouldn't help if it did, since re-subscribing still
    // races the same deferred transition.
  }, [beats.length, segments, setIndex]);

  const target = useMemo(() => playheadTarget(beats, index), [beats, index]);
  const current = beats[index];
  const curSegment = segmentIndexOf(segments, index);
  const elapsed =
    current && data?.started_at != null ? current.beat.ts_unix - data.started_at : 0;

  // Keep the focused rail row in view as the playhead moves. Pure DOM, no
  // timer — `block: "nearest"` so a row already visible never jitters.
  const railRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = railRef.current?.querySelector<HTMLElement>(
      `[data-beat-index="${index}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [index]);

  if (!kb || !sid) return <div className="empty">missing replay address</div>;
  if (error) {
    return (
      <div className="kb-replay__error" role="alert">
        {error} — <Link to="/sessions">back to sessions</Link>
      </div>
    );
  }
  if (loading || !data) return <div className="empty">loading replay…</div>;

  const windowed = data.matched !== data.beats.length;

  return (
    <div className="kb-replay" data-testid="replay-view">
      <header className="kb-replay__head">
        <Link
          className="kb-replay__back"
          to={`/sessions?focus=${encodeURIComponent(sid)}`}
        >
          ← sessions
        </Link>
        <h1 className="kb-replay__title">Replay</h1>
        <code className="kb-replay__sid">{data.session_id}</code>
        <span className="kb-replay__kb">{data.kb}</span>
        <span className="kb-replay__counts">
          {data.beats.length} beat{data.beats.length === 1 ? "" : "s"}
          {" · "}
          {formatElapsed(data.duration_secs)} elapsed
          {data.collapsed > 0 && ` · ${data.collapsed} folded into runs`}
        </span>
      </header>

      {/* Honesty strip — every caveat the wire reported, never suppressed. */}
      <div className="kb-replay__caveats">
        {data.scrubbed && (
          <p className="kb-replay__caveat kb-replay__caveat--scrub" role="status">
            <strong>Redacted.</strong> This daemon applied {data.redactions}{" "}
            redaction{data.redactions === 1 ? "" : "s"} before building the
            timeline — prompt text, agent prose and edit snippets below are{" "}
            <em>not verbatim</em>.
          </p>
        )}
        {data.truncated && (
          <p className="kb-replay__caveat" role="status">
            Truncated at the timeline cap — {data.dropped} later beat
            {data.dropped === 1 ? "" : "s"} are not shown.
          </p>
        )}
        {windowed && (
          <p className="kb-replay__caveat" role="status">
            Showing {data.beats.length} of {data.matched} matching beats (
            {data.total_beats} in the session).
          </p>
        )}
        {data.metadata_skipped > 0 && (
          <p className="kb-replay__caveat kb-replay__caveat--quiet">
            {data.records} transcript records · {data.metadata_skipped} carried
            no timestamp (metadata, skipped)
            {data.out_of_order > 0 &&
              ` · ${data.out_of_order} arrived out of order (gaps clamped, instants kept)`}
          </p>
        )}
      </div>

      {beats.length === 0 ? (
        <div className="empty">
          no beats — this capture's transcript held nothing replayable
        </div>
      ) : (
        <>
          <div className="kb-replay__scrub">
            <input
              ref={scrubRef}
              className="kb-replay__range"
              data-testid="replay-scrubber"
              type="range"
              min={0}
              max={beats.length - 1}
              step={1}
              value={index}
              aria-label="Playhead — beat index"
              aria-valuetext={`beat ${index + 1} of ${beats.length}`}
              onChange={(e) => setIndex(Number(e.target.value))}
            />
            <span className="kb-replay__stop" data-testid="replay-stop">
              beat <strong>{index + 1}</strong>/{beats.length}
              {" · "}
              <span className="kb-replay__gap">
                {formatGap(current?.beat.delta_secs ?? 0)}
              </span>
              {" · "}
              {formatElapsed(elapsed)} in
              {" · "}
              segment {curSegment + 1}/{segments.length}
            </span>
          </div>

          <div className="kb-replay__body">
            <div className="kb-replay__rail" ref={railRef} data-testid="replay-rail">
              {segments.map((seg) => (
                <section className="kb-replay__seg" key={seg.startIndex}>
                  <h2 className="kb-replay__seg-head">
                    <span className="kb-replay__seg-n">
                      {seg.prompt ? `§${seg.index + 1}` : "§"}
                    </span>
                    <span className="kb-replay__seg-title">{seg.title}</span>
                  </h2>
                  <ol className="kb-replay__beats">
                    {seg.beats.map((b, i) => {
                      const flat = seg.startIndex + i;
                      const path = beatPathLabel(b);
                      const range = lineRangeLabel(b);
                      return (
                        <li key={b.beat.seq}>
                          <button
                            type="button"
                            className={`kb-replay__beat${
                              flat === index ? " kb-replay__beat--now" : ""
                            }${b.artifact_id ? " kb-replay__beat--resolved" : ""}`}
                            data-beat-index={flat}
                            data-beat-kind={b.beat.kind}
                            aria-current={flat === index ? "true" : undefined}
                            onClick={() => setIndex(flat)}
                          >
                            <span
                              className="kb-replay__glyph"
                              aria-hidden="true"
                            >
                              {kindGlyph(b.beat.kind)}
                            </span>
                            <span className="kb-replay__kind">
                              {kindLabel(b.beat.kind)}
                            </span>
                            <span className="kb-replay__detail">
                              {b.beat.detail}
                            </span>
                            {b.beat.count > 1 && (
                              <span
                                className="kb-replay__count"
                                title="consecutive identical beats folded into one"
                              >
                                ×{b.beat.count}
                              </span>
                            )}
                            <span className="kb-replay__dt">
                              {formatGap(b.beat.delta_secs)}
                            </span>
                            {path && (
                              <span
                                className={`kb-replay__path${
                                  b.artifact_id ? "" : " kb-replay__path--raw"
                                }`}
                                title={
                                  b.artifact_id
                                    ? `${b.kb} · ${path}`
                                    : `${path} — not in any corpus`
                                }
                              >
                                {path}
                                {range && (
                                  <span className="kb-replay__range-lbl">
                                    {" "}
                                    {range}
                                  </span>
                                )}
                              </span>
                            )}
                          </button>
                        </li>
                      );
                    })}
                  </ol>
                </section>
              ))}
            </div>

            <ReplayStage
              target={target}
              index={index}
              hostSuffix={hostSuffix}
              kbHint={kb}
            />
          </div>
        </>
      )}
    </div>
  );
}

// ── the artifact stage ─────────────────────────────────────────────────────

/// The right pane. Shows the artifact most recently in play at the playhead
/// (`playheadTarget` walks backward — see its doc), swapping the iframe `src`
/// only when the playhead crosses into a DIFFERENT artifact, and posting
/// `kb:scroll-to-id` when the beat carried a heading slug.
///
/// The iframe runtime already owns everything on the other side: it assigns
/// `kb-h-<slug>` ids to h1/h2/h3 and handles `kb:scroll-to-id` with
/// `flash: true` (scroll + a ~1.8 s amber wash). Zero new iframe code.
function ReplayStage({
  target,
  index,
  hostSuffix,
  kbHint,
}: {
  target: PlayheadTarget | null;
  index: number;
  hostSuffix: string;
  kbHint: string;
}) {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const artifactId = target?.kind === "artifact" ? target.artifactId : null;
  // ARTIFACT HOST GRAMMAR v2 — the QUALIFYING kb for this beat's artifact
  // origin. `target.kb` (not `kbHint`, the session's own kb) is correct: a
  // beat's artifact can live in a DIFFERENT kb than the session capture
  // itself (see the `target.kb !== kbHint` badge rendered below).
  const artifactKb = target?.kind === "artifact" ? target.kb : null;
  const slug = target?.kind === "artifact" ? target.slug : null;
  // Bumped when THIS artifact's iframe first talks to us, so the section post
  // below can't fire into a frame that hasn't booted its runtime yet.
  const [ready, setReady] = useState<string | null>(null);

  useEffect(() => setReady(null), [artifactId]);

  // Inbound: the exact per-artifact origin check (#7, W3.P-a). The
  // suffix-only `isArtifactOrigin` would accept a message from ANY artifact
  // iframe of this daemon — fine as a trust boundary, useless as an
  // attribution, and this pane's readiness is per-artifact.
  useEffect(() => {
    if (!artifactId || !artifactKb) return;
    const onMessage = (ev: MessageEvent) => {
      if (!isOriginOfArtifact(ev.origin, artifactId, artifactKb, hostSuffix)) return;
      setReady(artifactId);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [artifactId, artifactKb, hostSuffix]);

  useEffect(() => {
    if (!artifactId || !artifactKb || !slug) return;
    if (ready !== artifactId) return;
    const frame = frameRef.current?.contentWindow;
    if (!frame) return;
    try {
      // Target the artifact's OWN origin, never "*" (#7).
      frame.postMessage(
        { kind: "kb:scroll-to-id", id: slug, flash: true },
        artifactOrigin(artifactId, artifactKb, hostSuffix),
      );
    } catch {
      // Frame not ready / navigated away — the next beat re-posts.
    }
  }, [artifactId, artifactKb, slug, ready, hostSuffix, index]);

  if (!target) {
    return (
      <div className="kb-replay__stage" data-testid="replay-stage">
        <p className="kb-replay__stage-empty">
          Nothing in play yet — no file had been touched by this point in the
          session.
        </p>
      </div>
    );
  }

  if (target.kind === "path") {
    return (
      <div
        className="kb-replay__stage kb-replay__stage--path"
        data-testid="replay-stage"
        data-kb-path={target.path}
        data-beat-index={target.fromIndex}
      >
        <p className="kb-replay__stage-path">
          <code>{target.path}</code>
        </p>
        <p className="kb-replay__stage-note">
          Not in any corpus — a source file, a config, something outside the
          mounted kbs. Shown as the transcript recorded it.
        </p>
      </div>
    );
  }

  const src = `${artifactOrigin(target.artifactId, target.kb, hostSuffix)}/`;
  const stale = target.fromIndex !== index;
  return (
    <div
      className="kb-replay__stage"
      data-testid="replay-stage"
      data-kb-artifact={target.artifactId}
      data-kb-slug={target.slug ?? ""}
      data-beat-index={target.fromIndex}
    >
      <div className="kb-replay__stage-bar">
        <Link
          className="kb-replay__stage-link"
          to={`/a/${encodeURIComponent(target.kb)}/${(target.sourceRelative ?? "")
            .split("/")
            .map(encodeURIComponent)
            .join("/")}${target.slug ? `?sec=${encodeURIComponent(target.slug)}` : ""}`}
          title={`${target.kb} · open in the reader`}
        >
          {target.sourceRelative ?? target.artifactId} →
        </Link>
        {target.kb !== kbHint && (
          <span className="kb-replay__stage-kb">{target.kb}</span>
        )}
        {stale && (
          <span className="kb-replay__stage-stale">
            still showing beat {target.fromIndex + 1}
          </span>
        )}
        <span
          className="kb-replay__stage-grain"
          title="Section-level, detected from the transcript's line range — kb does not store per-beat bytes"
        >
          {target.slug ? "section highlight (detected)" : "no section signal"}
        </span>
      </div>
      <iframe
        key={src}
        ref={frameRef}
        className="kb-replay__frame"
        title={target.sourceRelative ?? target.artifactId}
        src={src}
        // Second readiness signal beside the runtime's own probe ping: `load`
        // can't tell us the runtime booted, but it CAN'T fire before the
        // document exists — and the probe may have landed before this pane's
        // message listener was attached. Whichever arrives first wins; the
        // post is idempotent (the runtime just scrolls to the id again).
        onLoad={() => setReady(target.artifactId)}
        sandbox={SANDBOX}
      />
    </div>
  );
}
