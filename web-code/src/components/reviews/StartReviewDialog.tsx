import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ApiError } from "../../api/client";
import { useCreateReview } from "../../hooks/useReviews";
import { useRefs } from "../../hooks/useRefs";
import { toast } from "../../lib/toast";
import RefTypeahead from "../RefTypeahead";

export interface StartReviewDialogProps {
  repo: string;
  onClose: () => void;
  /// Called with the new review id on success (caller navigates).
  onCreated: (id: number) => void;
  /// Optional first-open seeds (V4.L2 ranked-list CTA). `initialHead`
  /// empty/omitted keeps the first-other-branch default; `initialBase`
  /// empty/omitted leaves the base field BLANK (RS-U11 — see the base
  /// field's own seeding-effect comment below).
  initialHead?: string;
  initialBase?: string;
}

/// "Start review" dialog — head_ref picker fed by the same `GET /api/refs`
/// data the RefPicker uses; title optional. RS-U11 (README D14): base_ref
/// is NEVER defaulted to the local HEAD branch here — it starts blank
/// unless a caller passes `initialBase` (the ranked-list CTA's own
/// explicit default-branch seed) or the operator types one, and a blank
/// field sends no `base_ref` at all, so the daemon's own resolution chain
/// picks it. Mutations are LOOPBACK-ONLY: a bare 404 is rendered as the
/// same class of explanation Prs/Checkout surfaces, not a generic error
/// toast.
export default function StartReviewDialog({
  repo,
  onClose,
  onCreated,
  initialHead,
  initialBase,
}: StartReviewDialogProps) {
  const { data: refsData } = useRefs(repo);
  const create = useCreateReview(repo);
  const dlgRef = useRef<HTMLDialogElement | null>(null);

  const branches = useMemo(
    () => refsData?.refs.filter((r) => r.kind === "branch") ?? [],
    [refsData],
  );
  const branchNames = useMemo(() => branches.map((b) => b.name), [branches]);
  const otherBranches = branches.filter((b) => !b.is_head);

  const [headRef, setHeadRef] = useState(initialHead ?? "");
  const [baseRef, setBaseRef] = useState(initialBase ?? "");
  const [title, setTitle] = useState("");
  const [loopbackMsg, setLoopbackMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  // Seed the head default once refs load (and whenever the head branch
  // identity changes, e.g. after a checkout). RS-U11 (README D14): the
  // base field is deliberately left UNSEEDED here — this dialog used to
  // default it to the local HEAD branch (usually "main"), which is
  // exactly the "stale local main" README §1 names as the root cause of
  // review 65's wrong diff. Leaving it blank sends no `base_ref` at all
  // (`submit`, below), so the daemon runs the real resolution chain
  // (forge API target, then caller, then the default branch) instead of
  // this dialog's own guess. `initialBase` (the V4.L2 ranked-list CTA's
  // OWN explicit seed — a caller-computed default branch, not a blind
  // local-HEAD guess) is unaffected: it already seeded `baseRef` at
  // mount, via `useState`'s initializer above.
  useEffect(() => {
    if (!headRef && otherBranches[0]) setHeadRef(otherBranches[0].name);
  }, [otherBranches, headRef]);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const head = headRef.trim();
    if (!head) return;
    setLoopbackMsg(null);
    setErrorMsg(null);
    try {
      const out = await create.mutateAsync({
        repo,
        head_ref: head,
        base_ref: baseRef.trim() || undefined,
        title: title.trim() || undefined,
      });
      onCreated(out.id);
    } catch (err) {
      // Same loopback-only signal as `postPrFetch` (bare 404 / "Not Found").
      if (err instanceof ApiError && err.status === 404) {
        setLoopbackMsg(
          "Creating a review is loopback-only — open kb-code from the machine running kb-code-server to start one.",
        );
        return;
      }
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(msg);
      toast.err(`couldn't start review: ${msg}`);
    }
  }

  const busy = create.isPending;
  const blocked = !!loopbackMsg;

  return (
    <dialog
      ref={dlgRef}
      className="confirm"
      aria-labelledby="kbc-start-review-title"
      data-kbc-start-review
    >
      <h2 id="kbc-start-review-title" className="confirm__title">
        Start review — {repo}
      </h2>
      <form className="confirm__body kbc-start-review" onSubmit={(e) => void submit(e)}>
        {loopbackMsg ? (
          <p className="kbc-checkout__error" data-kbc-start-review-loopback>
            {loopbackMsg}
          </p>
        ) : (
          <>
            <label className="kbc-start-review__field">
              <span>Head ref</span>
              <select
                value={headRef}
                onChange={(e) => setHeadRef(e.target.value)}
                required
                aria-label="head ref"
                data-kbc-start-review-head
              >
                <option value="">Pick a branch…</option>
                {branches.map((b) => (
                  <option key={b.full_name} value={b.name}>
                    {b.name}
                    {b.is_head ? " (HEAD)" : ""}
                  </option>
                ))}
              </select>
            </label>
            <label className="kbc-start-review__field">
              <span>Base ref</span>
              <RefTypeahead
                value={baseRef}
                onChange={setBaseRef}
                items={branchNames}
                placeholder="auto"
                aria-label="base ref"
                inputProps={{ "data-kbc-start-review-base": "" }}
              />
              <span className="kbc-start-review__hint">
                Left blank, kb-code resolves it itself — the PR's own target branch, then the repo default.
              </span>
            </label>
            <label className="kbc-start-review__field">
              <span>Title (optional)</span>
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder={headRef || "review title"}
                aria-label="review title"
                data-kbc-start-review-title
              />
            </label>
            {errorMsg && (
              <p className="kbc-checkout__error" data-kbc-start-review-error>
                {errorMsg}
              </p>
            )}
          </>
        )}
        <div className="confirm__actions">
          <button type="button" className="confirm__cancel" onClick={onClose} disabled={busy}>
            {blocked ? "Close" : "Cancel"}
          </button>
          {!blocked && (
            <button
              type="submit"
              className="confirm__go"
              disabled={busy || !headRef.trim()}
              data-kbc-start-review-submit
            >
              {busy ? "Starting…" : "Start review"}
            </button>
          )}
        </div>
      </form>
    </dialog>
  );
}
