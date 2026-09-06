import type { UseQueryResult } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { Icon } from "./icons";
import { artifactHref } from "../lib/artifactHref";
import { codeReaderUrl, codeSearchUrl } from "../lib/codeLensUrl";
import { relativeAge } from "../lib/time";
import type {
  DocLensOut,
  DocLensScorecardOut,
  ResolvedCodeRef,
} from "../api/doclens";

// DCB W1.D — the Code section inside PreviewInspector's Links tab. Own
// `<h4>Code · N</h4>` heading (NOT folded into `itabBadge("links")` —
// 13-w1d-kb-spa.md §3.1: a rail-icon badge is one number per icon, and
// "8 links + 4 code refs" collapsed into "12" is uninterpretable at a
// glance; the in-body `<h4>` count is the established precedent for a
// third, semantically distinct count — Outlinks/Backlinks/Sessions/Commits
// all do this already).
//
// Purely presentational — the fetch plumbing (useCodeRefs/
// useDocLensScorecard/useDocLens/useSetDocLensPin) lives in
// PreviewInspector.tsx (called unconditionally, Rules of Hooks); this
// component receives the resolved query objects + a `pickRepo` callback.

const AMBIGUITY_INLINE_MAX = 3;

export const COMPOUND_UNAVAILABLE =
  "Code bridge unavailable from this origin — kb-code isn't reachable (its CORS allowlist may not include this page, or the daemon may be down).";

/// R12 — the discriminator between degrade state 2 (fetch throws; a browser
/// cannot tell "not CORS-allowlisted" from "kb-code down" from "network
/// unreachable" apart, so those collapse into ONE honest compound message)
/// and state 3 (fetch SUCCEEDED with a non-2xx `{error, reason}` body — a
/// server-authored refusal, rendered verbatim) is the PRESENCE of `.reason`
/// on the thrown error, not any attempt to distinguish the underlying cause.
/// Exported (W1.D.R #3) so the reason-present/reason-absent branches get
/// direct unit coverage instead of only exercising through a full render.
export function degradeMessage(error: unknown): string {
  const reason = (error as (Error & { reason?: string }) | undefined)?.reason;
  if (reason) {
    return error instanceof Error ? error.message : String(error);
  }
  return COMPOUND_UNAVAILABLE;
}

/// CT-E3 — the honest-staleness summary line under the Code heading: the
/// ONE home (#30) for "how fresh are this doc's code citations", shared by
/// the About stack and the Links tab (the same section renders in both).
/// Tier (a) is always available (the coderef/1 header the sub-tab already
/// fetches: ref_count/extracted_at/never_scanned); tier (b) upgrades it to
/// verified drift counts when the doclens lane against the PINNED checkout
/// has data. The rung is explicit on the returned `kind` (and rendered as a
/// `data-kb-coderef-fresh` attribute) so an unreachable kb-code is
/// STRUCTURALLY incapable of rendering as verified — it degrades to the
/// tier-(a) wording, never a fake "fresh" (exact CT-B6 ladder precedent:
/// counts · no-pin · server-refusal · unreachable are four distinct
/// states).
export type CodeFreshness = {
  kind:
    | "never-scanned" // no code_refs_docs row at all — "not scanned" ≠ "0 refs"
    | "verified" // tier (b): lens resolved against the pinned checkout
    | "no-pin" // kb-code reachable, but no checkout pinned — drift unchecked
    | "refusal" // kb-code reached, server-authored refusal (verbatim)
    | "degraded" // kb-code unreachable — tier (a) wording, never fake-fresh
    | "hints-only"; // no code_url configured (or lens still in flight)
  text: string;
  /// `verified` only — drives the `.is-drifted` emphasis + the rail-icon
  /// drift dot; null on every other rung (an unverified doc has no drift
  /// NUMBER, honest or otherwise).
  drifted: number | null;
  /// Hover detail (the compound unreachable message on `degraded`, the
  /// no-pin rationale on `no-pin`); null when the text stands alone.
  title: string | null;
};

/// Pure state ladder (exported for direct unit coverage of every rung).
/// Returns null for a scanned zero-ref doc: a doc that cites nothing makes
/// no claim that can go stale, so there is no freshness line to render
/// (and the section itself doesn't show for that state — `showCodeSection`
/// in PreviewInspector is unchanged).
export function freshnessState(args: {
  neverScanned: boolean;
  /// The coderef/1 SERVER total (`CodeRefsResponse.ref_count`), same field
  /// the heading count reads.
  refCount: number;
  /// `CodeRefsResponse.extracted_at` (unix secs); null iff never_scanned.
  extractedAt: number | null;
  hasCodeUrl: boolean;
  pinned: boolean;
  scorecardOk: boolean;
  lens: DocLensOut | undefined;
  /// The one failed cross-daemon query's error (scorecard first, else the
  /// lens once a repo is pinned) — null when neither failed.
  crossErr: unknown;
  nowMs?: number;
}): CodeFreshness | null {
  const {
    neverScanned,
    refCount,
    extractedAt,
    hasCodeUrl,
    pinned,
    scorecardOk,
    lens,
    crossErr,
    nowMs,
  } = args;
  if (neverScanned) {
    return {
      kind: "never-scanned",
      text: "code refs never scanned",
      drifted: null,
      title: null,
    };
  }
  if (refCount === 0) return null;
  const extracted =
    extractedAt != null
      ? ` · extracted ${relativeAge(extractedAt, nowMs)}`
      : "";
  const tierA = `cites ${refCount} code ref${refCount === 1 ? "" : "s"}${extracted}`;
  if (!hasCodeUrl) {
    return { kind: "hints-only", text: tierA, drifted: null, title: null };
  }
  if (crossErr != null) {
    const reason = (crossErr as Error & { reason?: string }).reason;
    if (reason) {
      // Server-authored refusal (kb-code was REACHED) — its message
      // verbatim, exactly like the body hint below it (R12 discriminator).
      const msg = crossErr instanceof Error ? crossErr.message : String(crossErr);
      return {
        kind: "refusal",
        text: `${tierA} · kb-code: ${msg}`,
        drifted: null,
        title: msg,
      };
    }
    // Unreachable/CORS/down are indistinguishable in a browser — degrade to
    // the tier-(a) wording (the extraction facts kb itself is authoritative
    // for), NEVER a drift verdict.
    return {
      kind: "degraded",
      text: tierA,
      drifted: null,
      title: COMPOUND_UNAVAILABLE,
    };
  }
  if (lens) {
    const drifted = lens.counts.drifted;
    return {
      kind: "verified",
      text: `cites ${refCount} code location${refCount === 1 ? "" : "s"} · ${drifted} drifted · checked ${relativeAge(lens.resolved_unix, nowMs)}`,
      drifted,
      title: null,
    };
  }
  if (scorecardOk && !pinned) {
    return {
      kind: "no-pin",
      text: `${tierA} · freshness unchecked — no checkout pinned`,
      drifted: null,
      title:
        "drift can only be measured against a chosen checkout; pick one below (a checkout is never auto-selected)",
    };
  }
  // Scorecard/lens still in flight — the tier-(a) facts are already honest
  // on their own; the line upgrades in place when the lens lands.
  return { kind: "hints-only", text: tierA, drifted: null, title: null };
}

/// W1.D.R #4 — one caption line surfacing truncation/partial-resolution
/// honestly, gated on `lens.truncated || lens.partial || codeRefsTruncated`
/// (three independent surfaces: kb-core's own coderef/1 extraction cap,
/// kb-code's lens-level row cap, and the resolution loop's deadline).
/// `lens` is the current `DocLensOut` (undefined before a repo is picked or
/// while it's loading — the coderef/1-only case still renders a caption
/// off `codeRefsTruncated` alone). Exported for direct unit coverage.
export function truncationCaption(
  codeRefsTruncated: boolean,
  lens: DocLensOut | undefined,
): string | null {
  const parts: string[] = [];
  if (lens?.truncated) {
    parts.push(`showing ${lens.refs.length} of ${lens.counts.total}`);
  } else if (codeRefsTruncated) {
    // No lens fetched yet (or the lens itself wasn't truncated) — the
    // coderef/1 extraction cap doesn't carry a true total, so this is
    // honest about what it can say rather than fabricating an "of M".
    parts.push("showing a capped subset of references");
  }
  if (lens?.partial) {
    parts.push(`resolution incomplete (${lens.partial_reason ?? "budget"})`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

export default function CodeRefsSection({
  kb,
  docId,
  docPath,
  refCount,
  extractedAt,
  codeRefsTruncated,
  neverScanned,
  codeUrl,
  repoParam,
  onPickRepo,
  scorecardQuery,
  docLensQuery,
}: {
  kb: string;
  docId: string;
  docPath: string;
  /// W1.D.R #4 — the coderef/1 SERVER total (`CodeRefsResponse.ref_count`),
  /// never `codeRefs.length` client-side — the two happen to agree today
  /// (the single-doc route never truncates its own `refs` array beyond what
  /// `ref_count` already reflects), but the field name says what it means:
  /// the count this kb daemon is authoritative for.
  refCount: number;
  /// CT-E3 — `CodeRefsResponse.extracted_at` (unix secs, null iff
  /// never_scanned): the tier-(a) freshness fact the summary line renders.
  extractedAt: number | null;
  /// W1.D.R #4 — `CodeRefsResponse.truncated`: kb-core's own extraction hit
  /// `MAX_REFS_PER_DOC`. One of three independent truncation/partial-ness
  /// signals feeding `truncationCaption` (the other two live on the lens).
  codeRefsTruncated: boolean;
  /// State 5 (13-w1d-kb-spa.md §6) — kb has no `code_refs_docs` row at all
  /// for this doc, distinct from "scanned, genuinely zero refs". Takes
  /// precedence over states 1–3 (observable with zero network calls, so
  /// checked first) — a doc that hasn't been scanned gets "not scanned
  /// yet", never "not linked to a code repo" even when `codeUrl` is also
  /// absent.
  neverScanned: boolean;
  codeUrl: string | null;
  repoParam: string | null;
  onPickRepo: (repo: string) => void;
  scorecardQuery: UseQueryResult<DocLensScorecardOut>;
  docLensQuery: UseQueryResult<DocLensOut>;
}) {
  const trimmedCodeUrl = codeUrl ? codeUrl.replace(/\/+$/, "") : null;
  const caption = truncationCaption(codeRefsTruncated, docLensQuery.data);
  // W1.D.R #10 — a fully unreachable kb-code fails BOTH the scorecard and
  // (once `?repo=` is set) the lens query with the IDENTICAL compound
  // message; render it once rather than twice. A `.reason`-bearing failure
  // (server-authored refusal — repo_indexing/repo_unavailable/…) is never
  // deduped: scorecard and lens can legitimately fail for DIFFERENT
  // server-authored reasons, and each is worth showing.
  const scorecardErrMsg = scorecardQuery.isError
    ? degradeMessage(scorecardQuery.error)
    : null;
  const lensErrMsgRaw =
    repoParam && docLensQuery.isError ? degradeMessage(docLensQuery.error) : null;
  const lensErrMsg =
    lensErrMsgRaw === COMPOUND_UNAVAILABLE && scorecardErrMsg === COMPOUND_UNAVAILABLE
      ? null
      : lensErrMsgRaw;
  // CT-E3 — the honest-staleness summary line (see `freshnessState`).
  // `crossErr` mirrors CT-B6's precedence: a scorecard failure first, else
  // the lens failure once a repo is pinned (an unpinned lens query never ran).
  const crossErr = scorecardQuery.isError
    ? scorecardQuery.error
    : repoParam && docLensQuery.isError
      ? docLensQuery.error
      : null;
  const fresh = freshnessState({
    neverScanned,
    refCount,
    extractedAt,
    hasCodeUrl: !!trimmedCodeUrl,
    pinned: !!repoParam,
    scorecardOk: scorecardQuery.data !== undefined,
    lens: docLensQuery.data,
    crossErr,
  });
  return (
    <>
      <h4 className="kb-pinsp__coderef-head">
        <span>Code{neverScanned ? "" : ` · ${refCount}`}</span>
        {trimmedCodeUrl && (
          <span className="kb-pinsp__coderef-head-acts">
            <button
              type="button"
              className="kb-pinsp__coderef-refresh"
              onClick={() => void scorecardQuery.refetch()}
              title="re-scan checkouts"
              aria-label="re-scan checkouts"
            >
              ↻
            </button>
            {/* R22 — the one deep link INTO kb-code's own Lens page.
                Repo-less + id-addressed on purpose: kb-code's LensEntry
                resolves the checkout from its own pinned_repo, so the
                reader's ?repo= pick here is never smuggled across the
                bridge as a second, competing source of truth. Rendered
                even in degrade states 2/3/5 — a kb-code unreachable from
                THIS origin may still be reachable in a new tab. */}
            <a
              className="kb-pinsp__coderef-lens-link"
              href={`${trimmedCodeUrl}/~lens/${encodeURIComponent(kb)}/${encodeURIComponent(docId)}`}
              target="_blank"
              rel="noopener noreferrer"
              title="open this doc's full lens in kb-code"
            >
              Open as lens <Icon.External aria-hidden />
            </a>
          </span>
        )}
      </h4>
      {fresh && (
        <div
          className={`kb-pinsp__coderef-fresh${
            fresh.drifted != null && fresh.drifted > 0 ? " is-drifted" : ""
          }`}
          data-kb-coderef-fresh={fresh.kind}
          title={fresh.title ?? undefined}
        >
          {fresh.text}
        </div>
      )}
      {!neverScanned && caption && (
        <div className="kb-pinsp__coderef-truncated">{caption}</div>
      )}

      {neverScanned ? (
        // CT-E3 — the status FACT lives on the freshness line above ("code
        // refs never scanned"); this hint is the remedy only, so the two
        // never say the same thing twice.
        <div className="kb-pinsp__hint">
          Run <code>kb reindex --kb {kb}</code> to populate code references.
        </div>
      ) : !codeUrl ? (
        <div className="kb-pinsp__hint">
          Not linked to a code repo — set <code>code_url</code> on this kb to
          enable jump links.
        </div>
      ) : (
        <>
          {scorecardErrMsg ? (
            <div className="kb-pinsp__hint">{scorecardErrMsg}</div>
          ) : scorecardQuery.data ? (
            <ScorecardPicker
              data={scorecardQuery.data}
              repoParam={repoParam}
              onPickRepo={onPickRepo}
            />
          ) : null}

          {repoParam &&
            (lensErrMsg ? (
              <div className="kb-pinsp__hint">{lensErrMsg}</div>
            ) : docLensQuery.data ? (
              <TieredRows
                kb={kb}
                docPath={docPath}
                codeUrl={codeUrl}
                lens={docLensQuery.data}
                onRefresh={() => void docLensQuery.refetch()}
              />
            ) : null)}
        </>
      )}
    </>
  );
}

function ScorecardPicker({
  data,
  repoParam,
  onPickRepo,
}: {
  data: DocLensScorecardOut;
  repoParam: string | null;
  onPickRepo: (repo: string) => void;
}) {
  // W1.D.R #7 — an empty `repos: []` (no checkouts configured on this kb-
  // code daemon) previously rendered a blank `role="tablist"` — indistinguish-
  // able from "still loading" or a rendering bug. Say so instead.
  if (data.repos.length === 0) {
    return (
      <div className="kb-pinsp__hint">kb-code has no checkouts configured</div>
    );
  }
  return (
    <div
      className="kb-pinsp__coderef-scorecard"
      role="tablist"
      aria-label="choose a checkout"
    >
      {data.repos.map((r) => {
        const total =
          (r.present ?? 0) + (r.ambiguous ?? 0) + (r.absent ?? 0) + (r.external ?? 0);
        const notReady = r.state !== "ready";
        const reasonLabel = r.reason ?? r.state;
        return (
          <button
            key={r.name}
            type="button"
            role="tab"
            aria-selected={repoParam === r.name}
            disabled={r.state === "indexing"}
            aria-label={notReady ? `${r.name}: ${reasonLabel}` : undefined}
            className={`kb-pinsp__coderef-repo${repoParam === r.name ? " is-on" : ""}${r.dirty ? " is-dirty" : ""}`}
            onClick={() => onPickRepo(r.name)}
            title={
              notReady
                ? reasonLabel
                : `${r.present} present · ${r.ambiguous} ambiguous · ${r.absent} absent`
            }
          >
            <span className="kb-pinsp__coderef-repo-name">{r.name}</span>
            {r.dirty && (
              <span
                className="kb-pinsp__coderef-dirty-dot"
                aria-hidden
                title="uncommitted changes"
              />
            )}
            {notReady ? (
              <span className="kb-pinsp__coderef-repo-n">
                {r.state === "indexing" ? "indexing…" : "error"}
              </span>
            ) : (
              <span className="kb-pinsp__coderef-repo-n">
                {r.present}/{total}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

type RefGroupBucket = {
  key: string | null;
  label: string;
  anchor: string | null;
  refs: ResolvedCodeRef[];
};

/// Buckets `lens.refs` by `groups[].key` (row order preserved, groups
/// ordered by `ordinal`), appending an "Ungrouped" trailer LAST. The
/// trailer's visible COUNT is `lens.ungrouped_count` (the envelope field,
/// R9) — never a client-side count of the bucket, which can under-report
/// when the response is `truncated`; the bucket itself is still populated
/// by a plain `ref.group === null` filter so the actual rows render.
///
/// W1.D.R #6 — the trailer gate is `lens.ungrouped_count > 0 ||
/// ungrouped.length > 0`, not `ungrouped_count` alone: a dangling-FK ref
/// (`ref.group` names a group key the response's `groups[]` doesn't carry —
/// possible when a group's own row was pruned/renamed between extraction and
/// resolution) lands in `ungrouped` via the `byKey.get` miss above even
/// though the server's own `ungrouped_count` didn't count it as such: with
/// only the `ungrouped_count` check, those rows were silently dropped from
/// render entirely. Exported (W1.D.R #3) for direct unit coverage of the
/// grouped/ungrouped/dangling-FK cases.
export function groupRefs(lens: DocLensOut): RefGroupBucket[] {
  const sorted = [...lens.groups].sort((a, b) => a.ordinal - b.ordinal);
  const buckets: RefGroupBucket[] = sorted.map((g) => ({
    key: g.key,
    label: g.label,
    anchor: g.anchor || null,
    refs: [],
  }));
  const byKey = new Map(buckets.map((b) => [b.key, b]));
  const ungrouped: ResolvedCodeRef[] = [];
  for (const r of lens.refs) {
    const bucket = r.group !== null ? byKey.get(r.group) : undefined;
    if (bucket) bucket.refs.push(r);
    else ungrouped.push(r);
  }
  const out = buckets.filter((b) => b.refs.length > 0);
  if (lens.ungrouped_count > 0 || ungrouped.length > 0) {
    out.push({ key: null, label: "Ungrouped", anchor: null, refs: ungrouped });
  }
  return out;
}

function TieredRows({
  kb,
  docPath,
  codeUrl,
  lens,
  onRefresh,
}: {
  kb: string;
  docPath: string;
  codeUrl: string;
  lens: DocLensOut;
  onRefresh: () => void;
}) {
  const shortSha = lens.repo.head_sha ? lens.repo.head_sha.slice(0, 7) : "unknown";
  const groups = groupRefs(lens);
  return (
    <>
      {lens.repo.dirty === true && (
        <div className="kb-pinsp__coderef-dirty-banner">
          resolved against uncommitted working tree @ {shortSha}
        </div>
      )}
      <div className="kb-pinsp__coderef-resolved">
        resolved {relativeAge(lens.resolved_unix)} @ {shortSha}
        <button
          type="button"
          className="kb-pinsp__coderef-refresh"
          onClick={onRefresh}
          title="refresh this checkout's lens"
          aria-label="refresh this checkout's lens"
        >
          ↻
        </button>
      </div>
      {groups.map((g) => (
        <div key={g.key ?? "__ungrouped__"}>
          {g.key === null ? (
            <h5 className="kb-pinsp__coderef-group">
              {/* W1.D.R #6 — `lens.ungrouped_count` under-counts a
                  dangling-FK bucket (rows whose `group` names a key
                  `groups[]` doesn't carry); the rendered length is the
                  honest floor. */}
              {g.label} · {Math.max(lens.ungrouped_count, g.refs.length)}
            </h5>
          ) : g.anchor ? (
            <h5 className="kb-pinsp__coderef-group">
              <Link
                to={artifactHref(kb, docPath, { sec: g.anchor })}
                title={`jump to §${g.label} in this doc`}
              >
                {g.label}
              </Link>
            </h5>
          ) : (
            <h5 className="kb-pinsp__coderef-group">{g.label}</h5>
          )}
          {g.refs.map((r) => (
            <RefRow key={r.ordinal} codeUrl={codeUrl} repo={lens.repo.name} r={r} />
          ))}
        </div>
      ))}
    </>
  );
}

/// `kind ∈ {path_line, path_range, path_list}` carry a line hint and get
/// the visually-primary line-state badge (13-w1d-kb-spa.md §7.4); every
/// other kind (`path`, `symbol_*`, `issue`, `external`) has nothing to
/// confirm. Exported (W1.D.R #3) for direct unit coverage of all four
/// branches, including R18's "moved +N · git-verified" on a `rev_remap`
/// confirmation with a nonzero delta.
export function LineBadge({ r }: { r: ResolvedCodeRef }) {
  const hasLineKind =
    r.kind === "path_line" || r.kind === "path_range" || r.kind === "path_list";
  if (!hasLineKind) return null;
  switch (r.line_state) {
    case "confirmed": {
      // R18 — a git-verified rev-remap that ALSO moved the line is still
      // the strongest evidence class, rendered honestly rather than as a
      // bare "confirmed" that silently masks the move.
      const moved =
        r.line_evidence === "rev_remap" &&
        r.line_hint_delta !== null &&
        r.line_hint_delta !== 0;
      if (moved) {
        const n = r.line_hint_delta as number;
        return (
          <span className="kb-pinsp__coderef-line kb-pinsp__coderef-line--confirmed">
            ✓ moved {n > 0 ? `+${n}` : n} · git-verified
          </span>
        );
      }
      return (
        <span className="kb-pinsp__coderef-line kb-pinsp__coderef-line--confirmed">
          ✓ confirmed
        </span>
      );
    }
    case "drifted":
      return (
        <span className="kb-pinsp__coderef-line kb-pinsp__coderef-line--drifted">
          ≈ now :{r.resolved_line ?? "?"}
        </span>
      );
    case "unverifiable":
      return (
        <span className="kb-pinsp__coderef-line kb-pinsp__coderef-line--unverifiable">
          ? unverifiable
        </span>
      );
    case "absent":
    default:
      return null;
  }
}

function RefRow({
  codeUrl,
  repo,
  r,
}: {
  codeUrl: string;
  repo: string;
  r: ResolvedCodeRef;
}) {
  // issue — a plain outbound GitHub link, built server-side; no
  // path/line badges (R11 — path_state/line_state are null/absent here).
  if (r.kind === "issue") {
    if (!r.issue) {
      return (
        <div className="kb-pinsp__coderef-row is-absent">
          <span>{r.raw}</span>
          {r.note && <span className="kb-pinsp__coderef-note">{r.note}</span>}
        </div>
      );
    }
    return (
      <div className="kb-pinsp__coderef-row">
        <a
          href={r.issue.href}
          target="_blank"
          rel="noopener noreferrer"
          title="open in new tab"
        >
          {r.raw} <Icon.External aria-hidden />
        </a>
      </div>
    );
  }

  // external (gem/vendor paths) — fully inert, never resolved against the
  // app repo.
  if (r.path_state === "external") {
    return (
      <div className="kb-pinsp__coderef-row is-external">
        <span>{r.raw}</span>
        <span className="kb-pinsp__coderef-external-tag">external</span>
      </div>
    );
  }

  // unique — the whole row is an outbound link, built from `reader`
  // (never re-derived from path_hint/line_hint — R1).
  if (r.path_state === "present" && r.reader) {
    return (
      <div className="kb-pinsp__coderef-row is-present">
        <LineBadge r={r} />
        <a
          href={codeReaderUrl(codeUrl, r.reader.repo, r.reader.path, r.reader.line)}
          target="_blank"
          rel="noopener noreferrer"
          title="opens in kb-code"
        >
          {r.raw} <Icon.External aria-hidden />
        </a>
      </div>
    );
  }

  // ambiguous — tier on candidate_count, NEVER candidates.length (a >3
  // ambiguous ref legitimately has candidates: [], which is NOT "zero
  // candidates").
  if (r.path_state === "ambiguous") {
    if (r.candidate_count <= AMBIGUITY_INLINE_MAX && r.candidates.length > 0) {
      return (
        <div className="kb-pinsp__coderef-row is-ambiguous">
          <span>{r.raw}</span>
          <ul className="kb-pinsp__coderef-candidates">
            {r.candidates.map((c) => (
              <li key={c}>
                <a
                  href={codeReaderUrl(codeUrl, repo, c)}
                  target="_blank"
                  rel="noopener noreferrer"
                  title="opens in kb-code"
                >
                  {c} <Icon.External aria-hidden />
                </a>
              </li>
            ))}
          </ul>
        </div>
      );
    }
    return (
      <div className="kb-pinsp__coderef-row is-ambiguous">
        <span>{r.raw}</span>
        {r.search && (
          <a
            className="kb-pinsp__coderef-search"
            href={codeSearchUrl(codeUrl, r.search.repo, r.search.q)}
            target="_blank"
            rel="noopener noreferrer"
            title="opens in kb-code"
          >
            {r.candidate_count} candidates <Icon.External aria-hidden />
          </a>
        )}
      </div>
    );
  }

  // miss (path_state === "absent") — inert row + search deep link + the
  // server-authored `note` verbatim (Decision 3), when present.
  if (r.path_state === "absent") {
    return (
      <div className="kb-pinsp__coderef-row is-absent">
        <span>{r.raw}</span>
        {r.search && (
          <a
            className="kb-pinsp__coderef-search"
            href={codeSearchUrl(codeUrl, r.search.repo, r.search.q)}
            target="_blank"
            rel="noopener noreferrer"
            title="opens in kb-code"
          >
            search <Icon.External aria-hidden />
          </a>
        )}
        {r.note && <span className="kb-pinsp__coderef-note">{r.note}</span>}
      </div>
    );
  }

  // (deviation, recorded — 13-w1d-kb-spa.md §7.2 tiers only on
  // path_state, silently assuming every non-issue/non-external ref carries
  // a path_hint. A `symbol_method`/`symbol_const` ref with NO path_hint at
  // all (D5 — `path_state: null`) is a real, common case in the motivating
  // corpus (e.g. "Algolia::SearchService#listable_results"): doc-lens still
  // resolves it against the repo's symbol table via `symbol_state`/
  // `symbol_hits`, just never against a path. A repo-unique (or
  // container-disambiguated) symbol hit is exactly as actionable as a
  // resolved path, so it gets the SAME "whole row is a link" treatment;
  // an ambiguous hit lists its (bounded, <=MAX_SYMBOL_HITS=5) candidates;
  // no hit is an inert row. `ref.search` is null for a pathless ref (no
  // basename to search on), so there is no search-fallback link here —
  // smallest choice consistent with Decision 3's "never fabricate
  // resolution" ethos.)
  if (
    (r.symbol_state === "hit_unique" || r.symbol_state === "hit_container_matched") &&
    r.symbol_hits.length === 1
  ) {
    const hit = r.symbol_hits[0];
    return (
      <div className="kb-pinsp__coderef-row is-present">
        <a
          href={codeReaderUrl(codeUrl, repo, hit.path, hit.line_start)}
          target="_blank"
          rel="noopener noreferrer"
          title="opens in kb-code"
        >
          {r.raw} <Icon.External aria-hidden />
        </a>
      </div>
    );
  }
  if (r.symbol_state === "hit_ambiguous" && r.symbol_hits.length > 0) {
    // W1.D.R #5 — the same 5-cap trap `candidate_count`/`candidates.length`
    // already guards against: `symbol_hits` is bounded at `MAX_SYMBOL_HITS`
    // (5), so more hits can exist than are rendered here. No search
    // fallback exists for a pathless symbol ref (`r.search` is null — no
    // basename to search on), so this is a plain count, not a link.
    return (
      <div className="kb-pinsp__coderef-row is-ambiguous">
        <span>{r.raw}</span>
        <ul className="kb-pinsp__coderef-candidates">
          {r.symbol_hits.map((hit, i) => (
            <li key={`${hit.path}:${hit.line_start}:${i}`}>
              <a
                href={codeReaderUrl(codeUrl, repo, hit.path, hit.line_start)}
                target="_blank"
                rel="noopener noreferrer"
                title="opens in kb-code"
              >
                {hit.path}:{hit.line_start} <Icon.External aria-hidden />
              </a>
            </li>
          ))}
        </ul>
        {r.symbol_hit_count > r.symbol_hits.length && (
          <span className="kb-pinsp__coderef-note">
            {r.symbol_hit_count} matches
          </span>
        )}
      </div>
    );
  }

  // Genuine miss: no path_hint, no symbol match (or `kind === "path"` with
  // no hint at all — D5's fifth case).
  return (
    <div className="kb-pinsp__coderef-row is-absent">
      <span>{r.raw}</span>
      {r.note && <span className="kb-pinsp__coderef-note">{r.note}</span>}
    </div>
  );
}
