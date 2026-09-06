import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { artifactHref } from "../lib/artifactHref";
import { useMemories } from "../hooks/useMemories";
import { useMemoryPolicy } from "../hooks/useMemoryPolicy";
import { useMemoryTriage } from "../hooks/useMemoryTriage";
import MemoryActions from "../components/MemoryActions";
import MemoryLinkChips from "../components/MemoryLinkChips";
import MemoryPromoteModal from "../components/MemoryPromoteModal";
import DecaySparkline from "../components/DecaySparkline";
import InjectionHistogram from "../components/InjectionHistogram";
import LineageViewer from "../components/LineageViewer";
import ProvenanceThread from "../components/ProvenanceThread";
import ProvenanceChip from "../components/ProvenanceChip";
import AgentEyeSimulator from "../components/AgentEyeSimulator";
import HygieneQueue from "../components/HygieneQueue";
import RecallQuadrantScatter from "../components/RecallQuadrantScatter";
import ScopeOverlapUpset from "../components/ScopeOverlapUpset";
import {
  patchMemorySalience,
  pinMemory,
  unpinMemory,
  type DecayPolicy,
  type MemoryScope,
  type RecallHit,
} from "../api/client";
import { decayHalfLifeDays, decayHalfLifeLabel, floorState, floorStateLabel } from "../lib/decayProjection";
import { isAgentHotHumanCold, orderUnverifiedFirst, tensionLabel } from "../lib/attention";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useReportQueryStats } from "../components/chrome/queryStats";
import { Icon } from "../components/icons";
import EmptyState from "../components/EmptyState";
import CitationText from "../components/CitationText";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";

const SCOPES: MemoryScope[] = ["all", "global", "project"];
const POLICIES: DecayPolicy[] = ["strict", "balanced", "loose"];
/// MI-W4.1(revision) — how many rows the DecayRail's "fastest-decaying
/// scores" rollup shows.
const ROLLUP_SIZE = 5;

// v0.10 M3 — refined-cartographic /memory view.
//
// Three-column body: sidebar (inherited from chrome shell) · main
// table with 10-bin salience histogram + ranked rows · right rail
// with corpus counts + salience distribution + decay-policy control.
//
// Rows show pin/forget actions (promote is v0.11 — needs a confirm
// modal + dest kb picker). Pin toggle is optimistic; an SSE refetch
// reconciles.
export default function Memory() {
  const [scope, setScope] = useState<MemoryScope>("all");
  const [q, setQ] = useState("");
  // Debounce the filter input so each keystroke doesn't fire a recall
  // fetch (and an embed) against the daemon — 250ms after typing settles.
  const [debouncedQ, setDebouncedQ] = useState("");
  useEffect(() => {
    const t = setTimeout(() => setDebouncedQ(q), 250);
    return () => clearTimeout(t);
  }, [q]);
  // v0.14 S6 — session lens. `?session=<sid>` switches the data
  // source to /api/sessions/{sid}/memories so the view shows only
  // memories produced during that Claude Code conversation. Clearing
  // the chip drops the query param + falls back to default recall.
  const [params, setParams] = useSearchParams();
  const sessionId = params.get("session");
  // L9 — per-kb lens via `?kb=<name>` URL param. Filters recall to
  // memories visible to that kb (global or explicitly linked). The
  // session lens takes precedence: if both are set the session view
  // wins (its data source is a different endpoint that doesn't honor
  // for_kb).
  const forKb = params.get("kb");
  const clearSession = () => {
    const next = new URLSearchParams(params);
    next.delete("session");
    setParams(next, { replace: true });
  };
  const clearKbLens = () => {
    const next = new URLSearchParams(params);
    next.delete("kb");
    setParams(next, { replace: true });
  };
  // MI-W4.7 — the scope-overlap UpSet's row click reuses this SAME `?kb=`
  // lens (never a new filter axis).
  const pivotToKb = (kbName: string) => {
    const next = new URLSearchParams(params);
    next.set("kb", kbName);
    setParams(next, { replace: true });
  };
  // CT-E4 — `?sort=unverified`: agent-hot-human-cold rows first. Carried in
  // the URL like the session/kb lenses above; the ordering is applied
  // CLIENT-side over the recall rows this view already holds (they carry
  // both signals — recall_count + read_pct — and the recall endpoint's
  // score order stays the tie-break within/after the bucket, matching the
  // census route's own default-order tie-break). The server-side twin is
  // GET /api/memory/census?sort=unverified, the API/CLI surface for the
  // same drift meter.
  const unverifiedSort = params.get("sort") === "unverified";
  const toggleUnverifiedSort = () => {
    const next = new URLSearchParams(params);
    if (unverifiedSort) next.delete("sort");
    else next.set("sort", "unverified");
    setParams(next, { replace: true });
  };
  const { hits, loading, error, refresh } = useMemories(
    scope,
    debouncedQ,
    sessionId,
    forKb,
  );
  // CT-E4 — the ROW LIST honours ?sort=unverified; the aggregations
  // (histogram, corpus stats, quadrant scatter, scope overlap) stay on
  // `hits` — they're order-insensitive, and reordering them would only
  // churn their memos.
  const orderedHits = useMemo(
    () => (unverifiedSort ? orderUnverifiedFirst(hits) : hits),
    [hits, unverifiedSort],
  );
  useDocumentTitle("Memory");
  // Memoise the stats object so QueryRibbon's reporter doesn't see a new
  // reference (and re-run) on every unrelated re-render.
  const queryStats = useMemo(
    () =>
      !loading && !error
        ? { total: hits.length, ms: 0, warnings: [] as string[] }
        : undefined,
    [loading, error, hits.length],
  );
  useReportQueryStats(queryStats);

  // Local pin overlay so the toggle reflects immediately while the
  // SSE/refetch reconciles.
  const [pinOverlay, setPinOverlay] = useState<Record<string, boolean>>({});
  // MI-W3.2b — same optimistic-overlay shape as pin, for the editable
  // salience control below.
  const [salienceOverlay, setSalienceOverlay] = useState<Record<string, number>>(
    {},
  );
  const [salienceError, setSalienceError] = useState<string | null>(null);
  // v0.13 D7 — promote modal target. null = closed.
  const [promoting, setPromoting] = useState<RecallHit | null>(null);
  // MI-W4.3 — lineage viewer target. null = closed.
  const [lineageTarget, setLineageTarget] = useState<{ kb: string; id: string } | null>(
    null,
  );
  // MI-W4.6 → CT-B6 → CT-E4 — provenance-dossier target: the WHOLE recall
  // row (the dossier's Origin/Currency/Attention sections read origin
  // metas, recall receipts, read state and the flagged bit straight off it
  // — no second fetch). CT-E4 widened the old session-only gate: the
  // dossier opens for ANY row — a session-less memory renders the
  // hand-written origin class + currency + attention, and the why-chain
  // shows its honest absence. null = closed. One dialog at a time, same
  // shape as `lineageTarget` above (invariant #30: one home per action).
  const [provenanceTarget, setProvenanceTarget] = useState<RecallHit | null>(null);
  // MI-W4.2b — the quadrant scatter's clock, memoised so it doesn't drift
  // (and re-trigger the scatter's own memo) on every unrelated re-render.
  const nowUnix = useMemo(() => Math.floor(Date.now() / 1000), []);
  // FIX2 — the quadrant's classification thresholds are wire-supplied
  // (kb_core::triage::HIGH_SALIENCE_THRESHOLD/DORMANT_DAYS via
  // GET /api/memory/triage), never a hand-duplicated TS constant. This is
  // the SAME query `HygieneQueue` below already fires unconditionally on
  // this route, so sharing the hook costs no extra round-trip.
  const triage = useMemoryTriage();

  const togglePin = async (h: RecallHit) => {
    const key = `${h.kb}:${h.id}`;
    const currentlyPinned = pinOverlay[key] ?? !!h.pinned;
    setPinOverlay((p) => ({ ...p, [key]: !currentlyPinned }));
    try {
      if (currentlyPinned) await unpinMemory(h.kb, h.id);
      else await pinMemory(h.kb, h.id);
    } catch (e) {
      // Roll back the overlay on failure.
      setPinOverlay((p) => ({ ...p, [key]: currentlyPinned }));
      console.warn("[memory] pin toggle failed", e);
    }
  };

  const updateSalience = async (h: RecallHit, next: number) => {
    const key = `${h.kb}:${h.id}`;
    const prev = salienceOverlay[key] ?? h.salience;
    setSalienceError(null);
    setSalienceOverlay((s) => ({ ...s, [key]: next }));
    try {
      await patchMemorySalience(h.kb, h.id, next);
    } catch (e) {
      setSalienceOverlay((s) => ({ ...s, [key]: prev }));
      setSalienceError(String(e));
    }
  };

  // Histogram bins (10 buckets over 0..1).
  const bins = useMemo(() => {
    const out = Array.from({ length: 10 }, () => 0);
    for (const h of hits) {
      const i = Math.min(9, Math.max(0, Math.floor(h.salience * 10)));
      out[i] += 1;
    }
    return out;
  }, [hits]);
  const maxBin = Math.max(1, ...bins);

  const corpusStats = useMemo(() => {
    const total = hits.length;
    const pinned = hits.filter(
      (h) => pinOverlay[`${h.kb}:${h.id}`] ?? !!h.pinned,
    ).length;
    return { total, pinned };
  }, [hits, pinOverlay]);

  return (
    <div className="kb-mem" data-testid="memory-view">
      <main className="kb-mem__main">
        <div className="kb-mem__head">
          <h1>memory</h1>
          <div className="kb-mem__head-meta">
            <span>
              {hits.length} {hits.length === 1 ? "memory" : "memories"}
              {q && ` matching ${JSON.stringify(q)}`}
              {sessionId && ` from session`}
            </span>
          </div>
        </div>

        {sessionId && (
          <div
            className="kb-mem__lens"
            data-testid="memory-session-lens"
            role="status"
          >
            <span className="kb-mem__lens-label">session:</span>
            <code className="kb-mem__lens-id">{sessionId}</code>
            <button
              type="button"
              className="kb-mem__lens-clear"
              onClick={clearSession}
              aria-label="clear session filter"
              title="clear session filter"
            >
              <Icon.X />
            </button>
          </div>
        )}

        {forKb && (
          <div
            className="kb-mem__lens"
            data-testid="memory-kb-lens"
            role="status"
          >
            <span className="kb-mem__lens-label">visible to kb:</span>
            <code className="kb-mem__lens-id">{forKb}</code>
            <button
              type="button"
              className="kb-mem__lens-clear"
              onClick={clearKbLens}
              aria-label="clear kb filter"
              title="show all memories"
            >
              <Icon.X />
            </button>
          </div>
        )}

        <div className="kb-mem__scope" role="tablist" aria-label="memory scope">
          {SCOPES.map((s) => (
            <button
              key={s}
              type="button"
              role="tab"
              aria-selected={scope === s}
              className={scope === s ? "is-on" : ""}
              data-testid={`memory-scope-${s}`}
              onClick={() => setScope(s)}
            >
              {s}
            </button>
          ))}
          <input
            className="kb-mem__search"
            placeholder="filter memories…"
            value={q}
            aria-label="filter memories"
            onChange={(e) => setQ(e.target.value)}
          />
          <button
            type="button"
            className={`kb-mem__sortbtn ${unverifiedSort ? "is-on" : ""}`}
            data-testid="memory-sort-unverified"
            aria-pressed={unverifiedSort}
            onClick={toggleUnverifiedSort}
            title="agent-hot-human-cold first: recalled by agents, never opened by you (?sort=unverified)"
          >
            unverified first
          </button>
        </div>

        {error && <div className="kb-mem__empty">recall failed: {error}</div>}
        {!error && loading && hits.length === 0 && (
          <div className="kb-mem__empty">loading…</div>
        )}
        {!error && !loading && hits.length === 0 && (
          // D6 — the salience histogram + table header used to render
          // unconditionally here, so a zero-result scope/filter combo showed
          // a flatlined chart and an empty table header above the "No
          // memories yet" line. Show ONLY the empty state instead.
          <div data-testid="memory-empty">
            <EmptyState
              icon={<Icon.Brain />}
              title="no memories yet"
              hint={
                q || sessionId || forKb
                  ? "Nothing matches the current filter — clear the query or lens above."
                  : "Memories are written by kb-memory's capture hooks and /kb-remember, not authored here."
              }
            />
          </div>
        )}

        {hits.length > 0 && (
          <>
            <div className="kb-mem__histo" title="salience distribution">
              {bins.map((c, i) => (
                <span
                  key={i}
                  className={`kb-mem__bin ${c > 0 ? "on" : ""}`}
                  style={{ height: `${4 + (c / maxBin) * 28}px` }}
                  title={`${(i / 10).toFixed(1)}–${((i + 1) / 10).toFixed(1)} · ${c} memories`}
                />
              ))}
              <span className="kb-mem__axis">
                salience ↑ · 0.00 ─── 1.00
              </span>
            </div>

            <div className="kb-mem__thead">
              <span>type · scope</span>
              <span>memory</span>
              <span>salience</span>
              <span>health</span>
              <span>links</span>
              <span className="num">score</span>
              <span />
            </div>

            {salienceError && (
              <div className="kb-mem__salience-err" role="alert">
                salience update failed: {salienceError}
              </div>
            )}
            {orderedHits.map((h) => (
              <MemoryRow
                key={`${h.kb}:${h.id}`}
                h={h}
                pinned={pinOverlay[`${h.kb}:${h.id}`] ?? !!h.pinned}
                onTogglePin={() => togglePin(h)}
                salience={salienceOverlay[`${h.kb}:${h.id}`] ?? h.salience}
                onSalienceChange={(next) => updateSalience(h, next)}
                onForgotten={refresh}
                onPromote={() => setPromoting(h)}
                onLinksChanged={refresh}
                onOpenLineage={() => setLineageTarget({ kb: h.kb, id: h.id })}
                onOpenProvenance={() => setProvenanceTarget(h)}
              />
            ))}
          </>
        )}

        {/* MI-W4.2b — the salience × recall quadrant scatter reflects
            whatever's currently in scope/filtered (the SAME `hits` the
            table above renders), so a filtered view's scatter matches the
            filtered table. Renders its own empty state when `hits` is
            empty — no extra conditional needed here.
            FIX2 — gated on `triage.data` because its classification
            thresholds are wire-supplied (no hardcoded TS fallback to fall
            back to); this is the SAME fetch `HygieneQueue` fires below, so
            in practice this resolves on the very first render pass. */}
        {triage.data && (
          <RecallQuadrantScatter
            rows={hits.map((h) => ({
              kb: h.kb,
              id: h.id,
              title: h.title,
              sourceRelative: h.source_relative,
              salience: h.salience,
              recallCount: h.recall_count,
              lastRecalledAt: h.last_recalled_at ?? null,
            }))}
            nowUnix={nowUnix}
            highSalienceThreshold={triage.data.high_salience_threshold}
            dormantDays={triage.data.dormant_days}
          />
        )}

        {/* MI-W4.4 — the bounded hygiene queue is corpus-wide (its own
            `/api/memory/triage` fetch), independent of the current
            scope/filter above. */}
        <HygieneQueue />

        {/* MI-W4.2d — the agent's-eye simulator, independent of the
            filter above (it drives its OWN recall call from the draft
            prompt). */}
        <AgentEyeSimulator />

        {/* MI-W4.7 — cross-kb scope overlap, aggregated client-side over
            the SAME `hits` the table above renders (no new route) — a
            filtered view's overlay matches the filtered table, same
            posture as the quadrant scatter above. */}
        <ScopeOverlapUpset hits={hits} onPivotKb={pivotToKb} />
      </main>

      {promoting && (
        <MemoryPromoteModal
          srcKb={promoting.kb}
          artifactId={promoting.id}
          title={promoting.title}
          onClose={() => setPromoting(null)}
          onPromoted={() => {
            // Refresh so the source memory's view re-renders (no state
            // changes on its row — the daemon left it in place — but
            // the user expects feedback). The new artifact in the dest
            // kb will surface via the daemon's watcher → reindex →
            // artifact.indexed SSE that the gallery already subscribes to.
            refresh();
          }}
        />
      )}

      {lineageTarget && (
        <LineageViewer
          kb={lineageTarget.kb}
          id={lineageTarget.id}
          onClose={() => setLineageTarget(null)}
        />
      )}

      {provenanceTarget && (
        <ProvenanceThread
          sessionId={provenanceTarget.session_id ?? null}
          memoryKb={provenanceTarget.kb}
          memoryId={provenanceTarget.id}
          hit={provenanceTarget}
          onClose={() => setProvenanceTarget(null)}
        />
      )}

      <DecayRail
        corpusTotal={corpusStats.total}
        pinned={corpusStats.pinned}
        bins={bins}
        maxBin={maxBin}
        hits={hits}
      />
    </div>
  );
}

function MemoryRow({
  h,
  pinned,
  onTogglePin,
  salience,
  onSalienceChange,
  onForgotten,
  onPromote,
  onLinksChanged,
  onOpenLineage,
  onOpenProvenance,
}: {
  h: RecallHit;
  pinned: boolean;
  onTogglePin: () => void;
  salience: number;
  onSalienceChange: (next: number) => void;
  onForgotten: () => void;
  onPromote: () => void;
  onLinksChanged: () => void;
  onOpenLineage: () => void;
  // CT-E4 — always present: the dossier opens for ANY hit now (MI-W4.6's
  // session-only disable is gone; a session-less memory's dossier renders
  // origin/currency/attention and an honest chain absence).
  onOpenProvenance: () => void;
}) {
  const { dropThreshold } = useMemoryPolicy();
  // CT-B5 — a memory's summary is free-text prose that may cite a commit
  // sha or a `path/to/file.rs:401`; linkify it into kb-code when this
  // memory's OWN kb has a code_url configured (a cross-kb `scope=all`
  // recall can mix kbs, so this must be per-row, not a page-level value).
  const codeUrl = useCodeUrlForKb(h.kb);
  return (
    <div
      className={`kb-mem__row ${pinned ? "pinned" : ""}`}
      data-testid="memory-item"
      data-id={h.id}
    >
      <span className="kb-mem__type">
        <span className="kb-mem__pill">memory</span>
        <span className="kb-mem__scope-tag">· {h.kb}</span>
      </span>
      <span className="kb-mem__titlecell">
        <Link
          className="kb-mem__title"
          to={artifactHref(h.kb, h.source_relative)}
        >
          {h.title}
        </Link>
        {h.summary && (
          <span className="kb-mem__summary">
            <CitationText text={h.summary} codeUrl={codeUrl} />
          </span>
        )}
        {isAgentHotHumanCold(h) && (
          <span
            className="kb-mem__tension"
            data-testid="memory-tension"
            title="agents keep getting this memory injected while you have never opened it — open it once to verify (the same condition the provenance dossier's Attention section shows)"
          >
            {tensionLabel(h.recall_count)}
          </span>
        )}
        {h.session_id && (
          <Link
            className="kb-mem__from-session"
            to={`/sessions?focus=${encodeURIComponent(h.session_id)}`}
            title="origin session"
          >
            from session: <code>{h.session_id.slice(0, 12)}</code>
          </Link>
        )}
        <ProvenanceChip h={h} />
      </span>
      <span className="kb-mem__salcell">
        <span className="kb-mem__salbar">
          <i style={{ width: `${(salience * 100).toFixed(0)}%` }} />
          <span className="kb-mem__ticks" aria-hidden>
            <span style={{ left: "25%" }} />
            <span style={{ left: "50%" }} />
            <span style={{ left: "75%" }} />
          </span>
        </span>
        <SalienceEdit value={salience} onCommit={onSalienceChange} />
      </span>
      <span className="kb-mem__healthcell">
        <DecaySparkline
          salience={salience}
          ageDays={h.age_days}
          decayK={h.decay_k}
          stability={h.stability}
          floor={dropThreshold}
          pinned={pinned}
        />
        <InjectionHistogram weekly={h.recall_weekly} usedCount={h.recall_used_count} />
      </span>
      <span className="kb-mem__linkscell">
        <MemoryLinkChips hit={h} onChanged={onLinksChanged} />
      </span>
      <span className="kb-mem__score">{h.score.toFixed(4)}</span>
      <span className="kb-mem__actions">
        <button
          type="button"
          className={`kb-mem__act ${pinned ? "is-on" : ""}`}
          onClick={onTogglePin}
          title={pinned ? "unpin" : "pin"}
          aria-pressed={pinned}
        >
          <Icon.Bookmark />
        </button>
        <button
          type="button"
          className="kb-mem__act"
          onClick={onOpenLineage}
          title="view supersede lineage"
        >
          lineage
        </button>
        <button
          type="button"
          className="kb-mem__act"
          onClick={onOpenProvenance}
          title="open this memory's provenance dossier — origin, currency, attention, and the session chain when one exists"
        >
          provenance
        </button>
        <button
          type="button"
          className="kb-mem__act"
          onClick={onPromote}
          title="promote to a regular kb"
        >
          ↗ promote
        </button>
        <MemoryActions kb={h.kb} id={h.id} onForgotten={onForgotten} />
      </span>
    </div>
  );
}

// MI-W3.2b — a minimal editable salience control: renders the number as a
// plain button; a click swaps it for a bounded `<input type=number>` that
// commits on Enter/blur and cancels on Escape. Local-only edit buffer (the
// parent owns the optimistic overlay + the actual PATCH call), so a failed
// write's rollback is visible immediately.
function SalienceEdit({
  value,
  onCommit,
}: {
  value: number;
  onCommit: (next: number) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(() => value.toFixed(2));

  if (!editing) {
    return (
      <button
        type="button"
        className="kb-mem__salnum kb-mem__salnum--edit"
        data-testid="memory-salience-value"
        title="click to edit salience"
        onClick={() => {
          setDraft(value.toFixed(2));
          setEditing(true);
        }}
      >
        {value.toFixed(2)}
      </button>
    );
  }

  const commit = () => {
    const parsed = Number(draft);
    setEditing(false);
    if (Number.isFinite(parsed)) {
      onCommit(Math.min(1, Math.max(0, parsed)));
    }
  };

  return (
    <input
      type="number"
      className="kb-mem__salinput"
      data-testid="memory-salience-input"
      min={0}
      max={1}
      step={0.05}
      value={draft}
      autoFocus
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.currentTarget.blur();
        } else if (e.key === "Escape") {
          setEditing(false);
        }
      }}
    />
  );
}

function DecayRail({
  corpusTotal,
  pinned,
  bins,
  maxBin,
  hits,
}: {
  corpusTotal: number;
  pinned: number;
  bins: number[];
  maxBin: number;
  hits: RecallHit[];
}) {
  // MI-W4.1 — hoisted from a component-local fetch to the shared
  // `useMemoryPolicy` hook so every `DecaySparkline`'s floor line reads
  // the SAME cached value instead of each re-fetching (invariant #23).
  const { policy, dropThreshold, error: policyError, flip } = useMemoryPolicy();
  const [flipError, setFlipError] = useState<string | null>(null);

  const flipPolicy = async (next: DecayPolicy) => {
    setFlipError(null);
    try {
      await flip(next);
    } catch (e) {
      setFlipError(String(e));
    }
  };

  // MI-W4.1(revision) — the "fastest-decaying scores" rollup: every
  // currently-listed hit, ranked by its TRUE score half-life ascending
  // (fastest-losing-rank first) — a real ranking-signal fact, unlike the
  // deleted "soonest to drop" rollup, which projected a floor-crossing date
  // that the actual filter (raw salience vs. floor) can never produce for
  // an above-floor memory. A hit that's ALSO below the floor right now
  // (only possible here for a pinned hit — `recall` already excludes
  // unpinned below-floor memories) shows that fact instead, since it's
  // more urgent/actionable than the half-life stat.
  const rollup = useMemo(() => {
    return hits
      .map((h) => ({
        h,
        halfLife: decayHalfLifeDays(h.decay_k ?? 0.01),
        state: floorState(h.salience, dropThreshold, h.pinned),
      }))
      .sort((a, b) => a.halfLife - b.halfLife)
      .slice(0, ROLLUP_SIZE);
  }, [hits, dropThreshold]);

  return (
    <aside className="kb-mem__rail" aria-label="memory stats">
      <section className="kb-mem__panel">
        <h4>Corpus</h4>
        <KV label="memories">{corpusTotal}</KV>
        <KV label="pinned">{pinned}</KV>
      </section>

      <section className="kb-mem__panel">
        <h4>Salience distribution</h4>
        <div className="kb-mem__distro">
          {bins.map((c, i) => (
            <span
              key={i}
              className={`b ${i >= 7 ? "high" : ""}`}
              style={{ height: `${6 + (c / maxBin) * 42}px` }}
            />
          ))}
        </div>
        <div className="kb-mem__distroaxis">
          <span>0.0</span>
          <span>0.5</span>
          <span>1.0</span>
        </div>
      </section>

      <section className="kb-mem__panel" data-testid="decay-rollup">
        <h4>Fastest-decaying scores</h4>
        {rollup.length === 0 ? (
          <p className="kb-mem__rollup-empty">no memories in view.</p>
        ) : (
          <ul className="kb-mem__rollup">
            {rollup.map(({ h, state }) => (
              <li key={`${h.kb}:${h.id}`}>
                <Link to={artifactHref(h.kb, h.source_relative)}>{h.title}</Link>
                <span className="kb-mem__rollup-label">
                  {state.kind === "below"
                    ? floorStateLabel(state)
                    : decayHalfLifeLabel(h.decay_k ?? 0.01)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="kb-mem__decay">
        <h5>Decay policy</h5>
        <p>
          Memories below the policy threshold drop from recall on the
          next pass. Pinned memories never decay regardless of policy.
        </p>
        <div className="kb-mem__decay-ctl">
          {POLICIES.map((p) => (
            <button
              key={p}
              type="button"
              className={policy === p ? "on" : ""}
              onClick={() => flipPolicy(p)}
              aria-pressed={policy === p}
            >
              {p}
            </button>
          ))}
        </div>
        {(policyError || flipError) && (
          <div className="kb-mem__decay-err" role="alert">
            {flipError ?? policyError}
          </div>
        )}
      </section>
    </aside>
  );
}

function KV({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="kb-mem__kv">
      <span>{label}</span>
      <b>{children}</b>
    </div>
  );
}
