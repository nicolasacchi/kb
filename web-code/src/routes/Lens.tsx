// DCB W2.B — the doc↔code lens page: one kb document's `codelens/1`
// resolution against ONE picked checkout. Renders entirely from TWO
// same-origin calls (`GET /api/doc-lens` + `GET /api/doc-lens/repos`) — no
// second, cross-origin call into kb itself (kb-code is kb's own doc-prose
// consumer here, not a relay). Route: `/r/:repo/~lens/:kb/:docId`
// (R2/D14 — the doc key is kb's ARTIFACT ID, no splat); the repo-less entry
// ramps (`LensEntry.tsx`/`LensEntryByPath.tsx`) land here once resolved.

import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router";
import CodeView, { type GotoSel } from "../components/CodeView";
import GroupRail from "../components/lens/GroupRail";
import type { GroupSelection } from "../components/lens/GroupRail";
import RefRow, { refTier } from "../components/lens/RefRow";
import Scorecard from "../components/lens/Scorecard";
import { useFile } from "../hooks/useFile";
import { useDocLens, useDocLensRepos, useSetDocLensPin } from "../hooks/useDocLens";
import { useLensKeys } from "../hooks/useLensKeys";
import { lensUrl, pinCorrection, refsForGroup, truncationCaption } from "../lib/docLensUrl";
import type { CodeLensRef } from "../api/types";
import "../styles/doclens.css";

function LensCodeView({ repo, path, gotoSel }: { repo: string; path: string; gotoSel: GotoSel | null }) {
  const file = useFile(repo, path, undefined);
  if (file.isLoading) return <div className="kbc-reader__hint">Loading…</div>;
  if (file.error) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</div>;
  }
  if (!file.data) return null;
  if (file.data.encoding !== "utf8") {
    return <div className="kbc-reader__hint">Binary file — preview not supported</div>;
  }
  // Read-only, deliberately thin: no `vim` callbacks (every buffer action
  // key is inert), no annotation/outline/history chrome — "the lens is an
  // index, not a reader" (the base plan's own words).
  return <CodeView content={file.data.content} spans={file.data.highlights} blobHash={file.data.blob_hash} gotoSel={gotoSel} />;
}

/// Why a selected ref isn't openable in `LensCodeView` — one line per tier,
/// mirrored against `RefRow.tsx`'s own `refTier`.
function lensEmptyReason(r: CodeLensRef): string {
  switch (refTier(r)) {
    case "issue":
      return "This reference cites an issue, not a file — use the outbound link.";
    case "ambiguous-inline":
    case "ambiguous-search":
      return "Ambiguous path — pick a candidate above, or open the search link.";
    case "symbol-ambiguous":
      return "Ambiguous symbol match — pick a hit above, or open the search link.";
    case "symbol-none":
      return "No matching symbol found in this checkout.";
    // DCB-W2.B.R fix 6 — its own honest reason, never "didn't resolve": an
    // external/vendor citation was never checked against this checkout at
    // all, by design.
    case "external":
      return "This reference is to an external (vendor/gem) path — not resolved against this checkout.";
    case "absent":
    default:
      return "This reference didn't resolve against the selected checkout.";
  }
}

export default function Lens() {
  const { repo = "", kb = "", docId = "" } = useParams<{ repo: string; kb: string; docId: string }>();
  const location = useLocation();
  const navigate = useNavigate();

  const scorecard = useDocLensRepos(kb, docId);
  const lens = useDocLens(kb, docId, repo);
  const setPin = useSetDocLensPin(kb, docId);

  // §4.0 — once mounted with a concrete `:repo`, correct to the doc's
  // pinned repo ONE TIME (Decision 1's "last pick per doc pre-selects the
  // switcher next time"), unless the operator has already picked a repo in
  // THIS browsing session. The correction DECISION is the pure
  // `pinCorrection` (W2.B.R fix 4, unit-pinned in `docLensUrl.test.ts`);
  // this effect only owns the seeded/not-yet-seeded bookkeeping and the nav.
  //
  // W2.B.R fix 7 — keyed on `docId`, not a bare boolean: the route only
  // swaps `:docId` between two lens docs (no unmount), so a plain
  // `useRef(false)` — set once and never cleared — would silently inherit
  // the PREVIOUS doc's "already corrected" state and skip the new doc's own
  // pin correction. Storing the docId itself re-arms the guard on change.
  const seededDocIdRef = useRef<string | null>(null);
  useEffect(() => {
    const seeded = seededDocIdRef.current === docId;
    const target = pinCorrection(scorecard.data?.pinned_repo, repo, seeded);
    if (target) {
      seededDocIdRef.current = docId;
      navigate(lensUrl(target, kb, docId) + location.search, { replace: true });
    } else if (scorecard.data) {
      seededDocIdRef.current = docId; // resolved (with or without a correction) for this doc
    }
  }, [scorecard.data, repo, kb, docId, navigate, location.search]);

  const [selectedGroup, setSelectedGroup] = useState<GroupSelection>(null);
  const [selectedRefOrdinal, setSelectedRefOrdinal] = useState<number | null>(null);
  // DCB-W2.B.R fix 8 — `activeRef.ordinal` is referentially STABLE across
  // re-clicks of the same row (the ordinal doesn't change just because you
  // clicked it again), so using it alone as `gotoSel`'s `nonce` meant
  // re-clicking an already-selected row never re-centered `LensCodeView`
  // (the buffer's own scroll/cursor may have drifted since). A monotonic
  // tick, bumped on EVERY selection change — including re-selecting the
  // current ordinal — mirrors `Tour.tsx`'s own "nonce must change on every
  // drive" nonce discipline.
  const [selectTick, setSelectTick] = useState(0);
  function selectRefOrdinal(o: number | null) {
    setSelectedRefOrdinal(o);
    setSelectTick((t) => t + 1);
  }

  function pickRepo(nextRepo: string) {
    setPin.mutate({ repo: nextRepo, docHash: lens.data?.doc_hash ?? scorecard.data?.doc_hash ?? null });
    navigate(lensUrl(nextRepo, kb, docId) + location.search);
  }

  const refs = lens.data?.refs ?? [];
  const activeRef = refs.find((r) => r.ordinal === selectedRefOrdinal) ?? null;

  // (R1) built from the RESOLUTION, never `line_hint`/`line_hint_end` (the
  // doc's own unverified hint). (deviation, recorded — mirrors kb's own
  // W1.D.R `RefRow` treatment: a `symbol_method`/`symbol_const` ref with a
  // UNIQUE (or container-disambiguated) symbol hit has no `resolved_path`
  // at all — `path_state` stays `null` for a pathless ref — but is exactly
  // as actionable as a resolved path, so it opens `LensCodeView` at its
  // one `symbol_hits[0]` target too, not just the `path_state === "present"`
  // case the base spec's own pseudocode literally checked.)
  const activeTarget = useMemo(() => {
    if (!activeRef) return null;
    if (activeRef.path_state === "present" && activeRef.resolved_path) {
      return { path: activeRef.resolved_path, line: activeRef.resolved_line };
    }
    if (
      (activeRef.symbol_state === "hit_unique" || activeRef.symbol_state === "hit_container_matched") &&
      activeRef.symbol_hits.length === 1
    ) {
      const hit = activeRef.symbol_hits[0];
      return { path: hit.path, line: hit.line_start as number | null };
    }
    return null;
  }, [activeRef]);

  const gotoSel = useMemo<GotoSel | null>(() => {
    if (!activeRef || !activeTarget || activeTarget.line == null) return null;
    return { start: activeTarget.line, end: activeTarget.line, nonce: selectTick };
  }, [activeRef, activeTarget, selectTick]);

  useLensKeys({
    groups: lens.data?.groups ?? [],
    ungroupedCount: lens.data?.ungrouped_count ?? 0,
    refs,
    selectedGroup,
    setSelectedGroup,
    selectedRefOrdinal,
    setSelectedRefOrdinal: selectRefOrdinal,
  });

  const rows = refsForGroup(refs, selectedGroup);
  // DCB-W2.B.R fix 3 — honesty caption, ported from kb's own
  // `CodeRefsSection.tsx` (W1.D.R #4): `truncated`/`partial`/`partial_reason`
  // were on the wire but never rendered anywhere in this SPA.
  const truncated = truncationCaption(lens.data);

  return (
    <div className="kbc-lens" data-kbc-lens>
      <Scorecard
        kb={kb}
        docId={docId}
        selectedRepo={repo}
        data={scorecard.data}
        loading={scorecard.isLoading}
        error={scorecard.error}
        onPick={pickRepo}
        docTitle={lens.data?.doc_title ?? undefined}
        docPublicHref={lens.data?.doc_href}
        resolvedUnix={lens.data?.resolved_unix}
        onRefresh={() => void lens.refetch()}
        hasLens={lens.data !== undefined}
      />

      {/* kb's THIRD state, checked BEFORE the body: a never-scanned doc
          arrives as a perfectly valid 200 with `refs: []`/`counts.total: 0`
          — without this arm the page would render an empty GroupRail over
          an empty refs column and silently read as "this doc cites no
          code", the opposite of the truth on any pre-DCB-backfill doc. */}
      {lens.data?.never_scanned && (
        <div className="kbc-reader__hint kbc-lens__never-scanned" data-kbc-lens-never-scanned>
          Not scanned yet — run <code>kb reindex --kb {kb}</code> on the kb
          daemon to populate code references for this document.
        </div>
      )}

      {lens.data && !lens.data.never_scanned && truncated && (
        <div className="kbc-lens__truncated" data-kbc-lens-truncated>
          {truncated}
        </div>
      )}

      {lens.data && !lens.data.never_scanned && (
        <div className="kbc-lens__body" data-kbc-lens-body>
          <div className="kbc-lens__hint-row">j/k rows · ( ) groups</div>
          <GroupRail
            groups={lens.data.groups}
            ungroupedCount={lens.data.ungrouped_count}
            selectedGroup={selectedGroup}
            onSelectGroup={setSelectedGroup}
          />
          <div className="kbc-lens__refs" data-kbc-lens-refs>
            {rows.map((r) => (
              <RefRow
                key={r.ordinal}
                r={r}
                repo={repo}
                selected={r.ordinal === selectedRefOrdinal}
                onSelect={() => selectRefOrdinal(r.ordinal)}
              />
            ))}
          </div>
          <div className="kbc-lens__reader" data-kbc-lens-reader>
            {activeTarget ? (
              <LensCodeView repo={repo} path={activeTarget.path} gotoSel={gotoSel} />
            ) : (
              <div className="kbc-reader__hint">
                {activeRef ? lensEmptyReason(activeRef) : "Pick a reference to open it here."}
              </div>
            )}
          </div>
        </div>
      )}

      {lens.isLoading && !lens.data && <div className="kbc-reader__hint">Loading…</div>}
      {lens.error && (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(lens.error as Error).message}</div>
      )}
    </div>
  );
}
