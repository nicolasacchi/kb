// The tour composer (V74-L3b, D12) — where a recorded path becomes a tour.
//
// A recorded window of hops is raw material: it knows WHERE someone went and
// nothing about why. A tour is the why. So this dialog prefills the steps and
// asks for the two things only a human has — a title and the prose per step —
// and it is the ONLY writer of a tour from this SPA.
//
// **It checks itself before it sends.** `draftToDoc` returns the document AND
// every problem with it (`draftProblems`) AND any coordinate-shaped key
// (`coordinateKeysOutsidePins`, the daemon's own rule). A refusal here names
// the field; a 400 from the daemon reads as "the server is broken".
//
// **Every per-step note the recording could not establish is shown** — a hop
// that landed on a file with no line becomes a prose step and says so, rather
// than being given an invented range that would resolve `pinned` against
// bytes nobody looked at.

import { useMemo, useState } from "react";
import { ApiError } from "../../api/client";
import { Icon } from "../../components/icons";
import { useApplyTour } from "../../hooks/useTours";
import { draftToDoc, MAX_TOUR_STEPS, type TourDraft } from "../../lib/tourDoc";
import { toast } from "../../lib/toast";

function refusalMessage(err: unknown): string {
  if (err instanceof ApiError && (err.status === 403 || err.status === 404)) {
    return `${err.message} — applying a tour is LOOPBACK-ONLY; it only works when kb-code is reached at 127.0.0.1.`;
  }
  return err instanceof Error ? err.message : String(err);
}

/// A slug proposal from a title. Deterministic and total; the human may edit
/// it, and `draftProblems` is what actually decides whether it is legal.
export function slugFromTitle(title: string): string {
  return (
    title
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+/, "")
      .replace(/-+$/, "")
      .slice(0, 64) || ""
  );
}

export interface TourComposerProps {
  repo: string;
  draft: TourDraft;
  onClose(): void;
  onApplied(slug: string): void;
}

export default function TourComposer({ repo, draft: initial, onClose, onApplied }: TourComposerProps) {
  const [draft, setDraft] = useState<TourDraft>(initial);
  const apply = useApplyTour(repo);
  const composed = useMemo(() => draftToDoc(repo, draft), [repo, draft]);
  const blocked = composed.problems.length > 0 || composed.coordinateViolations.length > 0;

  function setStep(i: number, patch: Partial<TourDraft["steps"][number]>) {
    setDraft((d) => ({
      ...d,
      steps: d.steps.map((s, j) => (j === i ? { ...s, ...patch } : s)),
    }));
  }

  function removeStep(i: number) {
    setDraft((d) => ({ ...d, steps: d.steps.filter((_, j) => j !== i) }));
  }

  async function send() {
    if (blocked) return;
    try {
      const out = await apply.mutateAsync({ doc: composed.doc });
      toast.ok(
        `${out.created ? "created" : out.unchanged ? "unchanged" : "updated"} ${out.slug} — ${out.steps} step${out.steps === 1 ? "" : "s"}, ${out.status}`,
      );
      onApplied(out.slug);
    } catch (e) {
      toast.err(refusalMessage(e));
    }
  }

  return (
    <div className="kbc-tourcomposer" role="dialog" aria-modal="true" aria-label="compose a tour" data-kbc-tour-composer>
      <div className="kbc-tourcomposer__panel">
        <header className="kbc-tourcomposer__head">
          <h2>Record a tour</h2>
          <button type="button" onClick={onClose} aria-label="close" data-kbc-tour-composer-close>
            <Icon.X />
          </button>
        </header>

        <p className="kbc-tourcomposer__hint">
          {draft.steps.length} step{draft.steps.length === 1 ? "" : "s"} from where you have just
          been. A tour is prose about a path — the steps are already here, the why is not. Nothing
          is written until you apply, and applying is loopback-only.
        </p>
        {draft.notes.map((n, i) => (
          <p className="kbc-tourcomposer__note" key={i} data-kbc-tour-composer-note>
            {n}
          </p>
        ))}

        <label className="kbc-tourcomposer__field">
          <span>Title</span>
          <input
            value={draft.title}
            data-kbc-tour-composer-title
            onChange={(e) => {
              const title = e.target.value;
              setDraft((d) => ({
                ...d,
                title,
                // The slug follows the title until the human edits it — after
                // that it is theirs, because a slug is an address and an
                // address that moved under someone is a broken link.
                slug: d.slug === slugFromTitle(d.title) ? slugFromTitle(title) : d.slug,
              }));
            }}
          />
        </label>
        <label className="kbc-tourcomposer__field">
          <span>Slug</span>
          <input
            value={draft.slug}
            data-kbc-tour-composer-slug
            onChange={(e) => setDraft((d) => ({ ...d, slug: e.target.value }))}
          />
        </label>
        <label className="kbc-tourcomposer__field">
          <span>Description</span>
          <textarea
            rows={2}
            value={draft.description_md}
            data-kbc-tour-composer-desc
            onChange={(e) => setDraft((d) => ({ ...d, description_md: e.target.value }))}
          />
        </label>

        <ol className="kbc-tourcomposer__steps" data-kbc-tour-composer-steps>
          {draft.steps.map((s, i) => (
            <li key={s.id} data-kbc-tour-composer-step={s.id}>
              <div className="kbc-tourcomposer__step-head">
                <span className="kbc-tourcomposer__step-n">{i + 1}</span>
                <input
                  className="kbc-tourcomposer__step-title"
                  value={s.title}
                  aria-label={`step ${i + 1} title`}
                  onChange={(e) => setStep(i, { title: e.target.value })}
                />
                <button
                  type="button"
                  onClick={() => removeStep(i)}
                  aria-label={`drop step ${i + 1}`}
                  data-kbc-tour-composer-drop={s.id}
                >
                  <Icon.X />
                </button>
              </div>
              <code className="kbc-tourcomposer__step-ref" data-kbc-tour-composer-ref={s.id}>
                {s.ref || "(a prose step — no code reference)"}
              </code>
              <textarea
                rows={2}
                placeholder="why does this step matter?"
                value={s.body_md}
                aria-label={`step ${i + 1} prose`}
                onChange={(e) => setStep(i, { body_md: e.target.value })}
              />
              {s.notes.map((n, j) => (
                <p className="kbc-tourcomposer__step-note" key={j}>
                  {n}
                </p>
              ))}
            </li>
          ))}
        </ol>

        {composed.problems.length > 0 && (
          <ul className="kbc-tourcomposer__problems" data-kbc-tour-composer-problems>
            {composed.problems.map((p, i) => (
              <li key={i}>{p}</li>
            ))}
          </ul>
        )}
        {composed.coordinateViolations.length > 0 && (
          <ul className="kbc-tourcomposer__problems" data-kbc-tour-composer-coords>
            {composed.coordinateViolations.map((p, i) => (
              <li key={i}>
                refusing to send — {p} is geometry, and a tour is coordinate-free
              </li>
            ))}
          </ul>
        )}

        <footer className="kbc-tourcomposer__foot">
          <span className="kbc-tourcomposer__cap">
            cap {MAX_TOUR_STEPS} steps
          </span>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            disabled={blocked || apply.isPending}
            onClick={() => void send()}
            data-kbc-tour-composer-apply
          >
            <Icon.Check /> {apply.isPending ? "Applying…" : "Apply"}
          </button>
        </footer>
      </div>
    </div>
  );
}
