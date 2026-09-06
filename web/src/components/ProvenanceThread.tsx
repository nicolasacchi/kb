// MI-W4.6 → CT-B6 — the /memory row's provenance surface, upgraded from a
// thin session→commits→files thread into the "why believed" dossier: one
// modal answering origin (how this memory came to exist), currency (is it
// still true — lineage, open flags, code-citation freshness) and attention
// (agent recalls vs. human reading), with the original why-chain kept
// intact below. Follows invariant #30's one-home-per-action rule: the SAME
// modal-overlay pattern `LineageViewer` already uses (a `<dialog>`
// triggered from the row's action strip), not a second dock.
//
// Session identity is rendered from the SAME pure chip helpers
// `SessionContextCard` uses (`harnessGlyph`/`outcomeLine`/`commitBadge`,
// `sessionDisplayName`) rather than the full card component itself: that
// card's own action row (`onJumpToOutcome`, follow-mode, presence) is
// meaningful only inside a transcript READER — it scrolls/tails the actual
// transcript view — which this standalone panel has no analogue for.
// Reusing its pure building blocks is the honest fit; forcing the whole
// interactive card in would leave those props with nothing sensible to do.
//
// CT-B6 data posture: every dossier section reuses an EXISTING hook/fetch
// lane — `useMemoryLineage` (same endpoint LineageViewer queries),
// `useCodeRefs` (same-origin coderef/1 hints), `useDocLensScorecard`/
// `useDocLens` (the sanctioned SPA→kb-code CORS pattern, invariant #23's
// no-SSE-tie exception: finite staleTime + a manual ↻), `ProvenanceChip`
// (the U3 origin-artifact resolve). "kb-code unreachable" is rendered as
// its own honest state, never conflated with "drifted".

import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useCommitFiles, useSessionDetail } from "../hooks/useSessions";
import {
  useMemoryCommittedIn,
  useMemoryLineage,
  useMemoryRecalledBy,
} from "../hooks/useMemories";
import { useCodeRefs } from "../hooks/useCodeRefs";
import { useDocLens, useDocLensScorecard } from "../hooks/useDocLens";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";
import { commitBadge, harnessGlyph, outcomeLine } from "../lib/sessionChips";
import { sessionDisplayName } from "../lib/sessionDisplayName";
import { replayUrl, sessionsUrl, sessionsWorklogUrl } from "../lib/sessionsUrl";
import { fileStalenessLabel } from "../lib/provenanceStaleness";
import { isAgentHotHumanCold, tensionLabel } from "../lib/attention";
import { relativeAge } from "../lib/time";
import type { CommitOut } from "../api/sessions";
import type {
  MemoryCommittedInRow,
  MemoryRecalledByRow,
  RecallHit,
} from "../api/client";
import ProvenanceChip from "./ProvenanceChip";
import { COMPOUND_UNAVAILABLE } from "./CodeRefsSection";
import { Icon } from "./icons";

/// CT-B2 — one row of the "Recall history" section below. Links go to the
/// recalling session at the SESSION level only (`sessionsUrl({ focus })`,
/// the SAME builder `memory.tsx`'s inline `from session:` chip uses for
/// the reverse direction) — a per-turn deep link would need the session's
/// OWN artifact identity (kb + source_relative), which this row doesn't
/// carry, so a `turn_id` renders as an inert marker rather than inventing
/// a new anchor grammar (the existing one, `artifactHref`'s `turn` opt +
/// `ArtifactPane`'s `t-<uuid12>` passthrough, is keyed off the SESSION's
/// artifact, not its conversational session_id).
function RecalledByRow({ row }: { row: MemoryRecalledByRow }) {
  const shortSid = row.session_id.slice(0, 8);
  const when = row.recalled_at
    ? new Date(row.recalled_at * 1000).toISOString().slice(0, 16).replace("T", " ")
    : null;
  return (
    <li className="kb-prov__recall" data-testid="provenance-recall-row">
      <Link className="kb-prov__link" to={sessionsUrl({ focus: row.session_id })}>
        {row.session_title ?? shortSid}
      </Link>{" "}
      <code className="kb-prov__recall-sid">{shortSid}</code>
      {when && <span className="kb-prov__recall-when">{when}</span>}
      {row.turn_id && (
        <span
          className="kb-prov__recall-turn"
          title="turn id recovered from the capture, session-level link only"
        >
          turn {row.turn_id}
        </span>
      )}
      {row.used && (
        <span
          className="kb-prov__recall-used"
          title="the session went on to explicitly name this memory (id or title) in a later turn — a lower bound: acting on a fact without naming it reads as unreferenced"
        >
          referenced
        </span>
      )}
    </li>
  );
}

function CommitFiles({ sessionId, sha, nowUnix }: { sessionId: string; sha: string; nowUnix: number }) {
  const { data, loading, error } = useCommitFiles(sessionId, sha, true);

  if (loading) {
    return <p className="kb-prov__files-status">checking…</p>;
  }
  if (error) {
    return (
      <p className="kb-prov__files-status" role="alert">
        lookup failed: {error}
      </p>
    );
  }
  if (!data || !data.available) {
    return <p className="kb-prov__files-status">unknown — this commit's repo isn't resolvable</p>;
  }
  if (data.files.length === 0) {
    return <p className="kb-prov__files-status">no files recorded for this commit</p>;
  }
  return (
    <ul className="kb-prov__files" data-testid="provenance-commit-files">
      {data.files.map((f) => (
        <li key={f.path} className="kb-prov__file">
          <code className="kb-prov__file-path">{f.path}</code>
          <span
            className={`kb-prov__file-staleness${f.changed_since ? " is-changed" : ""}`}
          >
            {fileStalenessLabel(f, nowUnix)}
          </span>
        </li>
      ))}
      {data.truncated && (
        <li className="kb-prov__files-truncated">…more files touched, not all shown</li>
      )}
    </ul>
  );
}

function CommitRow({
  sessionId,
  commit,
  nowUnix,
}: {
  sessionId: string;
  commit: CommitOut;
  nowUnix: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const sha = commit.sha_full ?? commit.sha ?? null;
  const canExpand = commit.resolved && !!sha;
  return (
    <li className="kb-prov__commit" data-testid="provenance-commit">
      <button
        type="button"
        className="kb-prov__commit-toggle"
        onClick={() => setExpanded((v) => !v)}
        disabled={!canExpand}
        aria-expanded={expanded}
        title={canExpand ? "check whether the touched files have changed since" : "this commit's git resolution never ran"}
      >
        {canExpand ? (expanded ? "▾" : "▸") : "·"}{" "}
        <code className="kb-prov__commit-sha">{(sha ?? commit.sha ?? "?").slice(0, 8)}</code>{" "}
        <span className="kb-prov__commit-subject">{commit.subject ?? "(no subject)"}</span>
      </button>
      {expanded && sha && <CommitFiles sessionId={sessionId} sha={sha} nowUnix={nowUnix} />}
    </li>
  );
}

/// CT-B6 — the dossier's origin taxonomy. Highlight-born wins over
/// session-born when both provenance sets are present (a highlight taken
/// DURING a session still originates in the highlighted artifact — the
/// session is how, the artifact is where).
export type OriginClass = "highlight-born" | "session-born" | "hand-written";

export function originClassOf(hit: RecallHit): OriginClass {
  if (hit.source_kb && hit.source_artifact) return "highlight-born";
  if (hit.session_id) return "session-born";
  return "hand-written";
}

const ORIGIN_GLOSS: Record<OriginClass, string> = {
  "highlight-born": "lifted from a passage highlighted in an artifact",
  "session-born": "distilled from a captured session",
  "hand-written": "no recorded origin session or source artifact",
};

/// HEADER — the origin-class line: how this memory came to exist, who put
/// it there, and roughly how old it is. The highlight-born class reuses
/// `ProvenanceChip` (CT-A1's U3 origin resolve — `fetchDoc` under the
/// SSE-invalidated `["doc", kb]` prefix) for the origin-artifact deep
/// link; the session-born class's link is the why-chain below. `age_days`
/// is the decay factor's own age input — the recall wire carries no
/// created timestamp, so the honest rendering is an approximate age, not
/// a fabricated creation date.
function OriginSection({ hit }: { hit: RecallHit }) {
  const cls = originClassOf(hit);
  return (
    <section className="kb-prov__hop" data-testid="provenance-origin">
      <h3>Origin</h3>
      <p className="kb-prov__origin-line">
        <span className={`kb-prov__origin-class kb-prov__origin-class--${cls}`}>
          {cls}
        </span>
        <span className="kb-prov__origin-gloss">{ORIGIN_GLOSS[cls]}</span>
      </p>
      {(hit.author || (hit.age_days != null && hit.age_days > 0)) && (
        <p className="kb-prov__origin-meta">
          {hit.author && <span title="recorded author role">by {hit.author}</span>}
          {hit.age_days != null && hit.age_days > 0 && (
            <span title="age the decay factor was computed against — the recall wire carries no exact created timestamp">
              ~{Math.round(hit.age_days)}d old
            </span>
          )}
        </p>
      )}
      {cls === "highlight-born" && <ProvenanceChip h={hit} />}
    </section>
  );
}

/// One label for a failed cross-daemon kb-code query. A `.reason`-bearing
/// failure is a SERVER-AUTHORED refusal (kb-code was reached; e.g. a repo
/// mid-index) and renders its own message; everything else — CORS
/// not-allowlisted, daemon down, network — is structurally
/// indistinguishable in a browser (see api/doclens.ts) and collapses into
/// the one honest "kb-code unreachable" state, DISTINCT from "drifted".
function codeErrLabel(error: unknown): { text: string; title: string } {
  const reason = (error as (Error & { reason?: string }) | undefined)?.reason;
  if (reason) {
    const msg = error instanceof Error ? error.message : String(error);
    return { text: `kb-code: ${msg}`, title: msg };
  }
  return { text: "kb-code unreachable", title: COMPOUND_UNAVAILABLE };
}

/// CURRENCY — is this memory still believable? Three independent signals:
/// the supersede lineage (a superseded memory is stale by definition), an
/// open `[kb-flag]` comment (CT-C1's wire `flagged`), and code-citation
/// freshness via the DCB doclens lane: coderef/1 hints (same-origin) →
/// kb-code's scorecard → the full lens against the doc's PINNED checkout
/// (Decision 1: a checkout is never auto-selected; the pin is the one
/// sanctioned pre-selection, the same seed PreviewInspector uses). With no
/// pin, drift is honestly "unchecked", never guessed.
function CurrencySection({ kb, id, hit }: { kb: string; id: string; hit: RecallHit }) {
  const lineage = useMemoryLineage(kb, id);
  const codeUrl = useCodeUrlForKb(kb);
  const codeRefsQuery = useCodeRefs(kb, id, true);
  const refCount = codeRefsQuery.data?.ref_count ?? 0;
  const hasHints =
    codeRefsQuery.data !== undefined &&
    !codeRefsQuery.data.never_scanned &&
    refCount > 0;
  const scorecardQuery = useDocLensScorecard(codeUrl, kb, id, hasHints);
  const pinnedRepo = scorecardQuery.data?.pinned_repo ?? null;
  const lensQuery = useDocLens(codeUrl, kb, id, hasHints ? pinnedRepo : null);

  const newer = lineage.data?.superseded_by_chain ?? [];
  const older = lineage.data?.supersedes_chain ?? [];

  const codeErr = scorecardQuery.isError
    ? scorecardQuery.error
    : pinnedRepo && lensQuery.isError
      ? lensQuery.error
      : null;
  const drifted = pinnedRepo ? (lensQuery.data?.counts.drifted ?? null) : null;

  // #23's manual-refresh affordance for the no-SSE-tie cross-daemon lane.
  const refreshCode = () => {
    void scorecardQuery.refetch();
    if (pinnedRepo) void lensQuery.refetch();
  };

  return (
    <section className="kb-prov__hop" data-testid="provenance-currency">
      <h3>Currency</h3>
      {hit.flagged && (
        <p className="kb-prov__flagged" data-testid="provenance-flagged">
          ⚑ flagged as wrong — an open [kb-flag] comment is on this memory
        </p>
      )}
      {lineage.isError && (
        <p className="kb-prov__hint">supersede lineage unavailable</p>
      )}
      {lineage.data &&
        (newer.length > 0 ? (
          <p
            className="kb-prov__currency-line is-superseded"
            data-testid="provenance-superseded"
          >
            superseded by <b>{newer[0].title}</b>
            {newer.length > 1 && <> (+{newer.length - 1} newer)</>}
          </p>
        ) : (
          <p className="kb-prov__currency-line">
            current
            {older.length > 0
              ? ` — supersedes ${older.length} older`
              : " — no supersede lineage"}
          </p>
        ))}
      {hasHints && (
        <p className="kb-prov__code-line" data-testid="provenance-code">
          <span>
            cites {refCount} code {refCount === 1 ? "location" : "locations"}
          </span>
          {codeUrl != null &&
            (codeErr != null ? (
              <span
                className="kb-prov__code-unreachable"
                data-testid="provenance-code-unreachable"
                title={codeErrLabel(codeErr).title}
              >
                {" · "}
                {codeErrLabel(codeErr).text}
              </span>
            ) : drifted != null && lensQuery.data ? (
              <>
                <span
                  className={`kb-prov__code-drift${drifted > 0 ? " is-drifted" : ""}`}
                  data-testid="provenance-code-drift"
                >
                  {" · "}
                  {drifted} drifted
                </span>
                <span className="kb-prov__code-resolved">
                  {" · "}checked {relativeAge(lensQuery.data.resolved_unix)}
                </span>
              </>
            ) : scorecardQuery.data && !pinnedRepo ? (
              <span
                className="kb-prov__code-unpinned"
                data-testid="provenance-code-unpinned"
                title="drift can only be measured against a chosen checkout; pin one from the artifact's Code section (a checkout is never auto-selected)"
              >
                {" · "}freshness unchecked — no checkout pinned in kb-code
              </span>
            ) : null)}
          {codeUrl != null && (
            <button
              type="button"
              className="kb-prov__code-refresh"
              onClick={refreshCode}
              title="re-check code citations against kb-code"
              aria-label="re-check code citations against kb-code"
            >
              ↻
            </button>
          )}
        </p>
      )}
    </section>
  );
}

/// CT-F1 — one exact-id citation row: a commit whose OWN message named
/// this memory's id (`Kb-Memory:` trailer). Rendered as inert text, not a
/// link: kb has no repo to open a sha in (that is kb-code's job, and the
/// cross-daemon lane is the Currency section's, keyed on a PINNED
/// checkout) — so a sha here is evidence to copy, never a promise that
/// something will resolve it.
function CommittedInRow({ row }: { row: MemoryCommittedInRow }) {
  return (
    <li className="kb-prov__commit" data-testid="provenance-exact-commit">
      <code className="kb-prov__commit-sha">{(row.sha_full ?? row.sha ?? "?").slice(0, 8)}</code>{" "}
      <span className="kb-prov__commit-subject">{row.subject ?? "(no subject)"}</span>
      {row.repo_root && (
        <span
          className="kb-prov__recall-when"
          title="which repo this sha lives in — a bare sha is meaningless across repos"
        >
          {row.repo_root}
        </span>
      )}
    </li>
  );
}

/// CT-F1 — the exact-id citation section, sitting between CURRENCY and the
/// CHAIN: these commits NAMED this memory, as opposed to the chain below,
/// which only says "the session that produced this memory also produced
/// these commits".
///
/// Renders NOTHING when there are no rows (an error still renders). The
/// `Kb-Memory:` trailer is opt-in per repo and off by default, so an empty
/// list overwhelmingly means "that repo never opted in" — a permanently
/// empty "no commits cited this memory" section would state a
/// configuration fact as a finding. Same call `kb why-memory`'s renderer
/// makes.
function CommittedInSection({ kb, id }: { kb: string; id: string }) {
  const { rows, loading, error } = useMemoryCommittedIn(kb, id);
  if (loading) return null;
  if (error) {
    return (
      <section className="kb-prov__hop" data-testid="provenance-committed-in">
        <h3>Cited in commits</h3>
        <p className="kb-prov__files-status" role="alert">
          lookup failed: {error}
        </p>
      </section>
    );
  }
  if (rows.length === 0) return null;
  return (
    <section className="kb-prov__hop" data-testid="provenance-committed-in">
      <h3>Cited in commits</h3>
      <p className="kb-prov__hint">
        exact-id: each commit's own message named this memory (opt-in per repo, so this list
        is never a complete record of what used it).
      </p>
      <ul className="kb-prov__commit-list">
        {rows.map((r) => (
          <CommittedInRow key={`${r.session_kb}:${r.sha_full}`} row={r} />
        ))}
      </ul>
    </section>
  );
}

export default function ProvenanceThread({
  sessionId,
  memoryKb,
  memoryId,
  hit,
  onClose,
}: {
  /// CT-E4 (owed CT-B6 follow-up) — now nullable: the dossier opens for
  /// ANY memory row. A hand-written memory has no origin session; the
  /// Origin/Currency/Attention sections render from `hit` alone and the
  /// why-chain below renders its honest absence instead of fetching.
  sessionId: string | null;
  /// CT-B2 — the memory's OWN identity (distinct from `sessionId`, its
  /// origin session), needed to fetch "Recall history" below. Optional so
  /// this component's existing session-thread callers/tests keep working
  /// unchanged when a caller has no memory row in hand.
  memoryKb?: string | null;
  memoryId?: string | null;
  /// CT-B6 — the memory's full recall row. Present ⇒ dossier mode: the
  /// Origin / Currency / Attention sections render above the why-chain.
  /// Absent ⇒ the pre-B6 thread (chain + recall history) renders alone.
  hit?: RecallHit | null;
  onClose: () => void;
}) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  const { detail, commits, loading } = useSessionDetail(sessionId);
  const dossierKb = memoryKb ?? hit?.kb ?? null;
  const dossierId = memoryId ?? hit?.id ?? null;
  const {
    rows: recalledBy,
    loading: recalledByLoading,
    error: recalledByError,
  } = useMemoryRecalledBy(dossierKb, dossierId);
  const nowUnix = Math.floor(Date.now() / 1000);

  // CT-B6 → CT-E4 — the agent-hot-human-cold tension: agents keep getting
  // this memory injected while the human has never opened it. Read through
  // the ONE shared condition (`lib/attention.ts`) the /memory row badge
  // and the census's ?sort=unverified also use; `read_pct` is the wire's
  // integer percent — 0/absent both mean "never meaningfully opened".
  const readPct = hit?.read_pct ?? 0;
  const showTension = !!hit && isAgentHotHumanCold(hit);

  return (
    <dialog
      ref={dlgRef}
      className="kb-prov"
      data-testid="provenance-thread"
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <header className="kb-prov__head">
        <h2>Provenance</h2>
        <button type="button" className="kb-prov__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      <div className="kb-prov__body">
        {hit && <OriginSection hit={hit} />}

        {hit && dossierKb && dossierId && (
          <CurrencySection kb={dossierKb} id={dossierId} hit={hit} />
        )}

        {/* CT-F1 — the exact-id citations, between CURRENCY and the CHAIN.
            Gated on the memory identity ALONE (not on `hit`): a citation
            is a fact about the memory, so it stays readable for a row the
            caller opened without a full recall hit in hand. */}
        {dossierKb && dossierId && <CommittedInSection kb={dossierKb} id={dossierId} />}

        {/* ATTENTION — recall receipts + the human read state on one
            surface, with CT-B2's recall-history rows (independent of the
            session-thread state below: a memory's recall history stays
            readable even when its origin session is no longer indexed,
            gated only on the memory identity being known). */}
        {(hit || (dossierKb && dossierId)) && (
          <section className="kb-prov__hop" data-testid="provenance-attention">
            <h3>Attention</h3>
            {hit && (
              <p className="kb-prov__receipts" data-testid="provenance-receipts">
                {hit.recall_count} {hit.recall_count === 1 ? "recall" : "recalls"} ·{" "}
                {hit.recall_used_count} referenced
              </p>
            )}
            {hit &&
              (showTension ? (
                <p className="kb-prov__tension" data-testid="provenance-tension">
                  {tensionLabel(hit.recall_count)}
                </p>
              ) : (
                <p className="kb-prov__readstate" data-testid="provenance-readstate">
                  {readPct > 0
                    ? `read ${readPct}% by you${
                        hit.last_read_at
                          ? ` · last opened ${new Date(hit.last_read_at * 1000)
                              .toISOString()
                              .slice(0, 10)}`
                          : ""
                      }`
                    : "never opened by you"}
                </p>
              ))}
            {dossierKb && dossierId && (
              <section className="kb-prov__recall-hist" data-testid="provenance-recalled-by">
                <h4 className="kb-prov__subhead">Recall history</h4>
                <p className="kb-prov__hint">
                  recalls the capture pipeline saw — a best-effort census, not a complete log.
                </p>
                {recalledByLoading && <p className="kb-prov__loading">loading…</p>}
                {recalledByError && (
                  <p className="kb-prov__files-status" role="alert">
                    lookup failed: {recalledByError}
                  </p>
                )}
                {!recalledByLoading && !recalledByError && recalledBy.length === 0 && (
                  <p className="kb-prov__none">no recalls the capture pipeline saw yet.</p>
                )}
                {recalledBy.length > 0 && (
                  <ul className="kb-prov__recall-list">
                    {recalledBy.map((r, i) => (
                      <RecalledByRow key={`${r.session_kb}:${r.session_id}:${i}`} row={r} />
                    ))}
                  </ul>
                )}
              </section>
            )}
          </section>
        )}

        {/* CHAIN — B1's why-chain, kept as-is below the dossier sections:
            origin session → its commits → whether the touched files have
            changed again since. CT-E4 — a null sessionId (the widened
            dossier gate: hand-written memories) renders its own honest
            absence, distinct from "was captured, no longer indexed". */}
        {loading && <p className="kb-prov__loading">loading…</p>}
        {!loading && !detail && !sessionId && (
          <p className="kb-prov__none" data-testid="provenance-no-origin-session">
            no origin session recorded for this memory.
          </p>
        )}
        {!loading && !detail && sessionId && (
          <p className="kb-prov__none" data-testid="provenance-no-session">
            this session's capture is no longer indexed.
          </p>
        )}
        {!loading && detail && sessionId && (
          <>
            <section className="kb-prov__hop" data-testid="provenance-session">
              <h3>1. session</h3>
              <div className="kb-prov__session">
                <span className="kb-prov__session-harness" title={detail.harness} aria-hidden>
                  {harnessGlyph(detail.harness)}
                </span>
                <span className="kb-prov__session-name">{sessionDisplayName(detail)}</span>
                {commitBadge(detail.commit_count) && (
                  <span className="kb-prov__session-chip">{commitBadge(detail.commit_count)}</span>
                )}
              </div>
              {(() => {
                const preview = outcomeLine(detail.outcome, detail.first_user_prompt);
                return preview ? <p className="kb-prov__session-outcome">{preview.text}</p> : null;
              })()}
              <div className="kb-prov__session-links">
                <Link className="kb-prov__link" to={replayUrl(detail.kb, detail.session_id)}>
                  replay →
                </Link>
                <Link
                  className="kb-prov__link"
                  to={sessionsWorklogUrl(detail.session_id, detail.project_key)}
                >
                  worklog →
                </Link>
              </div>
            </section>

            <section className="kb-prov__hop" data-testid="provenance-commits">
              <h3>2. commits</h3>
              {commits.length === 0 ? (
                <p className="kb-prov__none">this session produced no recorded commits.</p>
              ) : (
                <ul className="kb-prov__commit-list">
                  {commits
                    .filter((c) => c.kind === "commit")
                    .map((c, i) => (
                      <CommitRow
                        key={`${c.sha ?? c.sha_full ?? i}`}
                        sessionId={sessionId}
                        commit={c}
                        nowUnix={nowUnix}
                      />
                    ))}
                </ul>
              )}
            </section>

            <section className="kb-prov__hop" data-testid="provenance-hop3-hint">
              <h3>3. touched files</h3>
              <p className="kb-prov__hint">
                expand a commit above to check whether its files have changed again since.
              </p>
            </section>
          </>
        )}
      </div>
    </dialog>
  );
}
