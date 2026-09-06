import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import RangeDiffTable from "../components/reviews/RangeDiffTable";
import RefTypeahead from "../components/RefTypeahead";
import { useRangeDiff } from "../hooks/useRangeDiff";
import { useRefs } from "../hooks/useRefs";
import { rangeDiffUrl } from "../lib/codeUrl";
import "../styles/history.css";

/// `/r/:repo/~range-diff?old=&new=` (Phase C6's SPA half) — `git range-diff`
/// summary table: which commits in two overlapping ranges (typically the
/// SAME topic branch before/after a rebase or amend) are equal/modified/
/// added/removed relative to each other — `history::range_diff`'s own
/// module doc has the full disposition grammar this page renders. Linked
/// from the Compare page's header ("compare rebases →"), and directly
/// deep-linkable (`rangeDiffUrl`) like `~compare`/`~branches` before it.
export default function RangeDiff() {
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();

  const oldParam = searchParams.get("old") ?? "";
  const newParam = searchParams.get("new") ?? "";

  const [oldInput, setOldInput] = useState(oldParam);
  const [newInput, setNewInput] = useState(newParam);
  const { data: refsData } = useRefs(repo);
  const refItems = useMemo(() => refsData?.refs.map((r) => r.name) ?? [], [refsData]);

  // Same "the URL is the source of truth" reset as Compare.tsx's own
  // fromInput/toInput effect — a fresh navigation (the Compare page's
  // "compare rebases →" link, or a hard-navigated permalink) must never
  // leave the PREVIOUS range-diff's typed values showing.
  useEffect(() => {
    setOldInput(oldParam);
    setNewInput(newParam);
  }, [oldParam, newParam]);

  const rangeDiff = useRangeDiff(repo, oldParam || undefined, newParam || undefined);

  function go(next: { old?: string; new?: string }) {
    navigate(rangeDiffUrl(repo, { old: next.old ?? oldInput, new: next.new ?? newInput }));
  }

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    go({});
  }

  function onSwap() {
    setOldInput(newInput);
    setNewInput(oldInput);
    go({ old: newInput, new: oldInput });
  }

  const data = rangeDiff.data;

  return (
    <div className="kbc-rangediff">
      <form className="kbc-rangediff__head" onSubmit={onSubmit}>
        <RefTypeahead
          value={oldInput}
          onChange={setOldInput}
          items={refItems}
          placeholder="old range, e.g. main..topic-v1"
          aria-label="old range"
          inputClassName="kbc-rangediff__input"
          inputProps={{ "data-kbc-rangediff-old": "" }}
        />
        <button
          type="button"
          className="kbc-rangediff__swap"
          onClick={onSwap}
          title="Swap old/new"
          aria-label="Swap old/new"
          data-kbc-rangediff-swap
        >
          <Icon.Swap />
        </button>
        <RefTypeahead
          value={newInput}
          onChange={setNewInput}
          items={refItems}
          placeholder="new range, e.g. main..topic-v2"
          aria-label="new range"
          inputClassName="kbc-rangediff__input"
          inputProps={{ "data-kbc-rangediff-new": "" }}
        />
        <button type="submit" className="kbc-rangediff__go">
          Diff
        </button>
      </form>

      {!oldParam || !newParam ? (
        <div className="kbc-reader__hint" data-kbc-rangediff-empty>
          Enter both an old and a new range — the same topic branch's two range endpoints, before and after a
          rebase/amend (e.g. old <code>main..topic-v1</code>, new <code>main..topic-v2</code>).
        </div>
      ) : rangeDiff.isLoading ? (
        <div className="kbc-reader__hint">Loading range-diff…</div>
      ) : rangeDiff.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(rangeDiff.error as Error).message}</div>
      ) : data ? (
        data.pairs.length === 0 ? (
          <EmptyState
            icon={<Icon.Branch />}
            title="No pairs"
            hint="These two ranges have nothing to compare — double-check the range syntax on each side."
          />
        ) : (
          <RangeDiffTable repo={repo} pairs={data.pairs} truncated={data.truncated} />
        )
      ) : null}
    </div>
  );
}
