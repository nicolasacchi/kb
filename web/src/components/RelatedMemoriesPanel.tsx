import { useState } from "react";
import { Link } from "react-router-dom";
import {
  addMemoryLink,
  removeMemoryLink,
  type RecallHit,
} from "../api/client";
import { artifactHref } from "../lib/artifactHref";
import { useRelatedMemories } from "../hooks/useRelatedMemories";
import { ScoreExplain, fmtScore } from "./ScoreExplain";

// W1.reader — Batch-1's recall decomposition (rank/rel/decay/age_days) is on
// the wire but not yet in the regenerated `RecallHit` ts-rs bindings in this
// tree (the server phase's `just types` hasn't landed here). Additive +
// intersected at the use site below; every read is guarded (an old cached
// response, staleTime: Infinity, may simply lack them) so the row falls back
// to the plain salience label rather than rendering a half-built popover.
type RecallHitScoreExt = {
  rank?: number | null;
  rel?: number | null;
  decay?: number | null;
  age_days?: number | null;
};

/// L10 / v0.22 — "Related memories" rail panel on the artifact detail view.
///
/// Backed by the shared `useRelatedMemories` hook (TanStack + SSE bridge), so
/// it fetches the SAME recall the rail's count badge reads. Each row shows
/// title + salience and a "Pin to this kb" / "Unpin" inline action. "Pin to
/// this kb" only appears when the memory is global (i.e. NOT explicitly linked
/// to the current kb) — adding an explicit edge scopes the memory in (and a
/// subsequent click would unscope it).
///
/// `query` re-anchors the recall to this artifact (D3 passes the doc title);
/// absent = the kb-wide recency list. Empty + loading + error states are all
/// rendered in-place; nothing disrupts the surrounding inspector layout.
export default function RelatedMemoriesPanel({
  kb,
  query,
}: {
  kb: string;
  query?: string;
}) {
  const { hits, loading, error, refresh } = useRelatedMemories(kb, query);

  if (loading && hits.length === 0) {
    return (
      <>
        <h4>Related memories</h4>
        <div className="kb-pinsp__hint">loading…</div>
      </>
    );
  }
  if (error) {
    return (
      <>
        <h4>Related memories</h4>
        <div className="kb-pinsp__hint">recall failed: {error}</div>
      </>
    );
  }
  if (hits.length === 0) {
    return (
      <>
        <h4>Related memories</h4>
        <div className="kb-pinsp__hint">
          No memories visible to this kb yet.{" "}
          <Link
            to={`/memory?kb=${encodeURIComponent(kb)}`}
            className="kb-pinsp__memlink"
          >
            open /memory →
          </Link>
        </div>
      </>
    );
  }

  return (
    <>
      <h4>
        Related memories{" "}
        <Link
          to={`/memory?kb=${encodeURIComponent(kb)}`}
          className="kb-pinsp__memlink"
          title={`all memories visible to ${kb}`}
        >
          ↗
        </Link>
      </h4>
      <div className="kb-pinsp__memlist">
        {hits.map((h) => (
          <RelatedMemoryRow key={`${h.kb}:${h.id}`} h={h} forKb={kb} onChanged={refresh} />
        ))}
      </div>
    </>
  );
}

function RelatedMemoryRow({
  h,
  forKb,
  onChanged,
}: {
  h: RecallHit;
  forKb: string;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const linked = (h.linked_kbs ?? []).includes(forKb);
  // The memory is "implicit" here when it's only visible because of
  // the `*` sentinel. Explicit link = the user already pinned it to
  // this kb; the action then flips to "unpin from this kb".
  const explicitToThisKb = linked && !!h.global ? true : linked;

  const toggle = async () => {
    setBusy(true);
    try {
      if (explicitToThisKb) {
        await removeMemoryLink(h.kb, h.id, forKb);
      } else {
        await addMemoryLink(h.kb, h.id, forKb);
      }
      onChanged();
    } catch (e) {
      console.warn("[related-memories] link toggle failed", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="kb-pinsp__memrow">
      <Link
        className="kb-pinsp__memtitle"
        to={artifactHref(h.kb, h.source_relative)}
        title={h.title}
      >
        {h.title}
      </Link>
      {h.summary && (
        <span className="kb-pinsp__memsummary" title={h.summary}>
          {h.summary}
        </span>
      )}
      <div className="kb-pinsp__memmeta">
        <span className="kb-pinsp__memkb" title={`home kb: ${h.kb}`}>
          {h.kb}
        </span>
        {h.global && (
          <span
            className="kb-mem__chip kb-mem__chip--global"
            title="global memory"
          >
            ★
          </span>
        )}
        <RecallScoreChip h={h} />
        <button
          type="button"
          className="kb-pinsp__mempin"
          onClick={toggle}
          disabled={busy}
          title={
            explicitToThisKb
              ? `unpin from ${forKb}`
              : `pin this memory to ${forKb}`
          }
        >
          {explicitToThisKb ? "unpin" : `pin to ${forKb}`}
        </button>
      </div>
    </div>
  );
}

/// W1.reader — the recall score decomposition (invariant #10:
/// rank-position × salience × decay, deterministic + LLM-free). Renders the
/// shared `ScoreExplain` chip when the wire carries the decomposed terms;
/// falls back to the old plain "sal N.NN" label otherwise (an in-flight
/// response cached before the server phase shipped rank/rel/decay/age_days,
/// or the corpus-wide recall path that doesn't compute them).
function RecallScoreChip({ h }: { h: RecallHit }) {
  const ext = h as RecallHit & RecallHitScoreExt;
  const hasDecomposition =
    typeof ext.rank === "number" &&
    typeof ext.rel === "number" &&
    typeof ext.decay === "number";
  if (!hasDecomposition) {
    return <span className="kb-pinsp__memsal">sal {h.salience.toFixed(2)}</span>;
  }
  const ageLabel = typeof ext.age_days === "number" ? `${ext.age_days}` : "?";
  return (
    <ScoreExplain
      label="recall score"
      total={h.score}
      terms={[
        { label: "relevance", value: ext.rel!, detail: `1/(60+${ext.rank})` },
        { label: "salience", value: h.salience },
        {
          label: "decay",
          value: ext.decay!,
          detail: `e^(-k·age), age ${ageLabel} d`,
        },
      ]}
      footnote="score = rel × salience × decay"
    >
      <span className="kb-pinsp__memsal">score {fmtScore(h.score)}</span>
    </ScoreExplain>
  );
}
