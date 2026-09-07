// `/r/{repo}/~tours/{slug}` — ONE tour, walked (V74-L3b, D12 + D10).
//
// **The walkthrough machinery is `~boards`', not a second one.** `?step=` is
// 1-based on the wire and 0-based internally with the conversion in exactly
// one module (`lib/toursUrl.ts`, mirroring `lib/boardsUrl.ts`); `n`/`k`/`p`
// are the SAME registry ids (`walkthrough.next`/`prev`/`play`), gated on the
// same `walkthrough` context key; Escape leaves through the SAME existing
// `dismiss.mode` rung. The only thing added is that a tour is ALWAYS in its
// walkthrough — a tour with no step selected is a tour nobody is walking — so
// `?step=` absent means step 1 rather than "not walking".
//
// **A step's card is `BoardCard`.** A tour step's resolved reference is a
// `BoardNode` because a tour IS a board whose nodes are its steps (server
// invariant 26(a)), so the card that paints it is the board's own — which is
// in turn the review document's `LiveRefCard` for a `code` node. Three
// surfaces, one component, no chance of disagreeing about what `carried`
// looks like.
//
// **The camera is a HINT that is rendered, never a geometry that is applied.**
// `fold` folds the step's own card and `context` turns the daemon's `?ctx=1`
// read on; there is no `x`/`y` anywhere, and a camera carrying one would have
// been refused at apply time by the boards coordinate lint.
//
// **Every count and every state comes from the wire** (`tourCensus`), and an
// orphan step is a card on the surface like any other.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import BoardCard from "../components/boards/BoardCard";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { useTour } from "../hooks/useTours";
import { useLoopback } from "../hooks/useLoopback";
import { appendTrail, codeUrl } from "../lib/codeUrl";
import { cameraCaption, tourCensus } from "../lib/tourDoc";
import {
  TOUR_CTX_PARAM,
  TOUR_STEP_PARAM,
  parseTourFlag,
  parseTourStep,
  toursHref,
} from "../lib/toursUrl";
import "../styles/tours.css";

/// `BoardDetail`'s own autoplay cadence, deliberately the same number: two
/// walkthroughs that felt different would be two features.
const AUTOPLAY_MS = 6000;

export default function TourDetail() {
  const { repo = "", slug = "" } = useParams<{ repo: string; slug: string }>();
  const [params, setParams] = useSearchParams();
  const ctx = parseTourFlag(params.get(TOUR_CTX_PARAM));
  const tour = useTour(repo, slug, { ctx });
  const loopback = useLoopback();
  const [playing, setPlaying] = useState(false);
  const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());

  const data = tour.data;
  const steps = useMemo(() => data?.steps ?? [], [data]);
  // A tour is always being WALKED — an absent `?step=` is step one, not "no
  // step". That is the one place this surface's walkthrough differs from a
  // board's, and it differs because a board is a map you may simply look at.
  const parsed = parseTourStep(params.get(TOUR_STEP_PARAM), steps.length);
  const stepIndex = parsed ?? (steps.length > 0 ? 0 : null);
  const step = stepIndex !== null ? steps[stepIndex] : null;

  const setParam = useCallback(
    (key: string, value: string | null) => {
      const next = new URLSearchParams(params);
      if (value === null) next.delete(key);
      else next.set(key, value);
      // A view knob is not a navigation step.
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const goToStep = useCallback(
    (index: number) => {
      if (steps.length === 0) return;
      const clamped = Math.min(Math.max(index, 0), steps.length - 1);
      setParam(TOUR_STEP_PARAM, String(clamped + 1));
    },
    [setParam, steps.length],
  );

  useEffect(() => {
    if (!playing || steps.length === 0) return;
    const t = window.setInterval(() => goToStep((stepIndex ?? 0) + 1), AUTOPLAY_MS);
    return () => window.clearInterval(t);
  }, [playing, steps.length, stepIndex, goToStep]);

  useEffect(() => {
    if (!playing) return;
    const stop = () => setPlaying(false);
    window.addEventListener("keydown", stop);
    return () => window.removeEventListener("keydown", stop);
  }, [playing]);

  // The camera's `fold` is applied to the step's OWN card when it arrives, and
  // never to any other. A camera is a hint about how to show this stop, so it
  // cannot fold a card the reader chose to open.
  useEffect(() => {
    if (!step) return;
    const id = step.node.id;
    setFolded((prev) => {
      const want = step.camera?.fold === true;
      if (want === prev.has(id)) return prev;
      const next = new Set(prev);
      if (want) next.add(id);
      else next.delete(id);
      return next;
    });
  }, [step]);

  const toggleFold = useCallback((id: string) => {
    setFolded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  // `mode.active` rides along for the SAME reason `BoardDetail` publishes it:
  // it is the context key the EXISTING `dismiss.mode` rung reads, and a
  // walkthrough is exactly that. No second Escape row.
  useCommandScope("board", {
    board: "tours",
    walkthrough: stepIndex !== null,
    "mode.active": stepIndex !== null,
  });
  useCommandHandlers({
    "walkthrough.next": () => goToStep((stepIndex ?? 0) + 1),
    "walkthrough.prev": () => goToStep((stepIndex ?? 0) - 1),
    "walkthrough.play": () => setPlaying((v) => !v),
    "dismiss.mode": () => {
      setPlaying(false);
      setParam(TOUR_STEP_PARAM, null);
    },
  });

  if (tour.isLoading) return <div className="kbc-reader__hint">Loading tour…</div>;
  if (tour.error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-tour-error>
        {(tour.error as Error).message}
      </div>
    );
  }
  if (!data) return null;

  // A step's own reader link carries the tour BACK — the linked-tab chip's
  // whole grammar (`lib/codeUrl.ts`'s `TrailLinkSource`), so a tab opened
  // from here knows which stop it came from and can re-focus it.
  const readerHref =
    step?.node.code?.path != null
      ? appendTrail(
          codeUrl({
            repo,
            path: step.node.code.path,
            line: step.node.code.range[0],
          }),
          { id: slug, step: step.ordinal, src: "tour" },
        )
      : null;

  return (
    <div className="kbc-tour" data-kbc-tour={data.slug}>
      <header className="kbc-tour__head">
        <div className="kbc-tour__title-row">
          <Link to={toursHref(repo)} className="kbc-tour__back">
            <Icon.ArrowLeft /> Tours
          </Link>
          <h1 className="kbc-tour__title">{data.title}</h1>
          <span
            className={`kbc-tours__status kbc-tours__status--${data.status}`}
            data-kbc-tour-status={data.status}
          >
            {data.status}
          </span>
          {!loopback && (
            <span className="kbc-tour__gate" data-kbc-tour-gate>
              editing is loopback-only
            </span>
          )}
        </div>
        {data.description_md ? (
          <p className="kbc-tour__desc" data-kbc-tour-desc>
            {data.description_md}
          </p>
        ) : null}
        <ul className="kbc-tour__census" data-kbc-tour-census>
          {tourCensus(data).map((line, i) => (
            <li key={i}>{line}</li>
          ))}
          {data.honesty.notes.map((n, i) => (
            <li key={`n${i}`} data-kbc-tour-honesty-note>
              {n}
            </li>
          ))}
        </ul>
        {data.authored_ref ? (
          <p className="kbc-tour__caption" data-kbc-tour-authored-ref>
            authored at <code>{data.authored_ref}</code> — advisory context only; every step above
            was re-resolved against the working tree just now
          </p>
        ) : null}
      </header>

      {steps.length === 0 ? (
        <EmptyState
          icon={<Icon.Spark />}
          title="This tour has no steps"
          hint="A tour with no steps is not a tour — the daemon refuses to store one, so this is a read of something that changed under us."
        />
      ) : (
        <>
          <div className="kbc-tour__bar" data-kbc-tour-bar>
            <span data-kbc-tour-step-counter>
              step {(stepIndex ?? 0) + 1} of {steps.length}
            </span>
            <button
              type="button"
              onClick={() => goToStep((stepIndex ?? 0) - 1)}
              disabled={(stepIndex ?? 0) === 0}
              data-kbc-tour-prev
            >
              Prev
            </button>
            <button
              type="button"
              onClick={() => goToStep((stepIndex ?? 0) + 1)}
              disabled={(stepIndex ?? 0) >= steps.length - 1}
              data-kbc-tour-next
            >
              Next
            </button>
            <button type="button" onClick={() => setPlaying((v) => !v)} data-kbc-tour-play>
              {playing ? "Pause" : "Play"}
            </button>
            <label className="kbc-tour__ctx">
              <input
                type="checkbox"
                checked={ctx}
                onChange={(e) => setParam(TOUR_CTX_PARAM, e.target.checked ? "1" : null)}
                data-kbc-tour-ctx
              />
              Fetch ± context
              <span className="kbc-tour__caption">
                — the daemon reads each step's context range; a camera asking for context needs it
                on
              </span>
            </label>
            {playing && (
              <span data-kbc-tour-playing>
                playing — one step every {AUTOPLAY_MS / 1000}s; any key stops
              </span>
            )}
          </div>

          {step && (
            <section className="kbc-tour__step" data-kbc-tour-step={step.ordinal}>
              {step.node.title ? (
                <h2 className="kbc-tour__step-title" data-kbc-tour-step-title>
                  {step.node.title}
                </h2>
              ) : null}
              {step.node.body_md ? (
                <p className="kbc-tour__step-body" data-kbc-tour-step-body>
                  {step.node.body_md}
                </p>
              ) : null}
              {cameraCaption(step.camera) ? (
                <p className="kbc-tour__caption" data-kbc-tour-camera>
                  camera: {cameraCaption(step.camera)}
                  {step.camera?.context != null && step.camera.context > 0 && !ctx ? (
                    <>
                      {" "}
                      — turn on <em>Fetch ± context</em> above to actually read it
                    </>
                  ) : null}
                </p>
              ) : null}
              <BoardCard
                node={step.node}
                repo={repo}
                folded={folded.has(step.node.id)}
                expanded={ctx}
                focused
                onToggleFold={toggleFold}
                onThread={() => undefined}
              />
              <div className="kbc-tour__step-foot">
                {step.ref ? (
                  <code className="kbc-tour__ref" data-kbc-tour-step-ref>
                    {step.ref}
                  </code>
                ) : null}
                {readerHref ? (
                  <Link to={readerHref} data-kbc-tour-open-reader>
                    Open in the reader
                  </Link>
                ) : null}
              </div>
            </section>
          )}

          <ol className="kbc-tour__strip" data-kbc-tour-strip>
            {steps.map((s) => (
              <li key={s.node.id}>
                <button
                  type="button"
                  className={
                    "kbc-tour__strip-item" + (s.ordinal === stepIndex ? " is-current" : "")
                  }
                  aria-current={s.ordinal === stepIndex ? "step" : undefined}
                  data-kbc-tour-strip-item={s.ordinal}
                  data-kbc-tour-strip-state={s.node.state}
                  onClick={() => goToStep(s.ordinal)}
                >
                  <span className="kbc-tour__strip-n">{s.ordinal + 1}</span>
                  <span className="kbc-tour__strip-label">
                    {s.node.title ?? s.node.address}
                  </span>
                  <span className={`kbc-tour__strip-state kbc-tour__strip-state--${s.node.state}`}>
                    {s.node.state}
                  </span>
                </button>
              </li>
            ))}
          </ol>
        </>
      )}
    </div>
  );
}
