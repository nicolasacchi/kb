// V71-E2 — the Usages dock (D4): census → chips → grouped tree → preview.
//
// docs/research/kb-code-v7-evidence/research/usages-browsing.md §4.1: "Top: a
// one-line census strip. Below: a filter chip row. Below: the grouped tree.
// Right/below: a preview rendered by the same CM6 read-only reader … so the
// preview is not a degraded view."
//
// It lives in the BOTTOM DRAWER, which is where `desk/placement.ts` has said
// usages belong since V70-A4 (`{region: "drawer", set: "usages"}`) — this
// unit is what flips that row's `shipped` flag. No new region, so the desk
// landmark golden is untouched.
//
// Everything numeric comes from `lib/usages2.ts` (pure, unit-pinned). This
// component renders; it does not count. The census strip's `reasons` list is
// the load-bearing part: a chip or a server cap that hides rows ALWAYS
// produces a line here, which is how "a count never changes without an
// on-screen reason" is enforced rather than promised.

import { useMemo } from "react";
import type { UsageRow2, Usages2Out } from "../../api/types";
import EmptyState from "../EmptyState";
import TrustBadge from "../TrustBadge";
import CodeView from "../CodeView";
import { useFile } from "../../hooks/useFile";
import {
  applyChips,
  censusOf,
  EXCLUDE_BITS,
  GROUP_AXES,
  GROUP_AXIS_LABEL,
  groupRows,
  TRUST_ORDER,
  usagesTitle,
  walkOrder,
  type ExcludeRole,
  type GroupAxis,
  type UsageChips,
  type UsageTrust,
} from "../../lib/usages2";

/// How many kind chips the strip renders before collapsing the rest into a
/// counted "+N more" — the closed vocabulary has 34 names and a Rails file
/// can legitimately touch a dozen of them.
const KIND_CHIP_CAP = 8;

export interface UsagesDockProps {
  repo: string;
  out: Usages2Out;
  chips: UsageChips;
  onChips: (next: UsageChips) => void;
  axis: GroupAxis;
  onAxis: (next: GroupAxis) => void;
  /// The grep lane's count, when the mentions chip is on. `null` = the chip
  /// is off, so the lane was never asked — distinct from "asked, got zero".
  mentions: number | null;
  mentionsOn: boolean;
  onToggleMentions: () => void;
  /// Index into `walkOrder(groups)`; `-1` = nothing selected yet.
  cursor: number;
  onCursor: (next: number) => void;
  /// Open the row. `rung` mirrors `nav/ramp.ts`'s vocabulary so the dock's
  /// click/middle-click behave exactly like every other result surface —
  /// and so a new-tab open is TRAIL-LINKED to where the reader came from.
  onOpenRow: (row: UsageRow2, rung: "here" | "other" | "tab") => void;
  /// `?ref=` the dock's preview should read at, when the reader is pinned.
  refName?: string;
}

function Chip({
  on,
  label,
  count,
  title,
  onClick,
  handle,
}: {
  on: boolean;
  label: string;
  count?: number;
  title: string;
  onClick: () => void;
  handle: string;
}) {
  return (
    <button
      type="button"
      className={"kbc-usages__chip" + (on ? " is-on" : "")}
      aria-pressed={on}
      title={title}
      data-kbc-usages-chip={handle}
      onClick={onClick}
    >
      {label}
      {count !== undefined && <span className="kbc-usages__chip-count">{count}</span>}
    </button>
  );
}

export default function UsagesDock(props: UsagesDockProps) {
  const { repo, out, chips, onChips, axis, onAxis, cursor, onOpenRow } = props;

  const filtered = useMemo(() => applyChips([...out.exact, ...out.likely, ...out.candidate], chips), [out, chips]);
  const groups = useMemo(() => groupRows(filtered, axis), [filtered, axis]);
  const flat = useMemo(() => walkOrder(groups), [groups]);
  const census = useMemo(() => censusOf(out, chips, props.mentions), [out, chips, props.mentions]);
  const current = cursor >= 0 ? flat[cursor] : undefined;

  // The preview is the SAME read-only CM6 reader the main pane uses — the
  // research's own requirement. It reads the file the cursor row lives in;
  // no row selected ⇒ no fetch at all (`useFile` is disabled on undefined).
  const preview = useFile(current ? repo : undefined, current?.path, props.refName);

  function toggleKind(kind: string) {
    const on = chips.kinds.includes(kind);
    onChips({ ...chips, kinds: on ? chips.kinds.filter((k) => k !== kind) : [...chips.kinds, kind] });
  }
  function toggleTrust(t: UsageTrust) {
    const on = chips.trust.includes(t);
    onChips({ ...chips, trust: on ? chips.trust.filter((x) => x !== t) : [...chips.trust, t] });
  }
  function toggleExclude(e: ExcludeRole) {
    const on = chips.exclude.includes(e);
    onChips({ ...chips, exclude: on ? chips.exclude.filter((x) => x !== e) : [...chips.exclude, e] });
  }

  const kindChips = census.byKind.slice(0, KIND_CHIP_CAP);
  const hiddenKinds = census.byKind.length - kindChips.length;

  return (
    <div className="kbc-usages" data-kbc-usages-dock>
      {/* --- census: the answer BEFORE the list ------------------------ */}
      <header className="kbc-usages__census" data-kbc-usages-census>
        <span className="kbc-usages__title">{usagesTitle(out)}</span>
        <strong className="kbc-usages__total" data-kbc-usages-total>
          {census.total} usages
        </strong>
        {census.byTrust.map((b) => (
          <span key={b.trust} className="kbc-usages__census-trust">
            <TrustBadge cls={b.trust} />
            {b.total}
          </span>
        ))}
        <span className="kbc-usages__census-shown" data-kbc-usages-shown>
          showing {census.shown} of {census.returned} fetched
        </span>
      </header>
      {census.reasons.length > 0 && (
        <ul className="kbc-usages__reasons" data-kbc-usages-reasons>
          {census.reasons.map((r) => (
            <li
              key={r.id}
              className={"kbc-usages__reason" + (r.hiding ? " is-hiding" : "")}
              data-kbc-usages-reason={r.id}
            >
              {r.text}
            </li>
          ))}
        </ul>
      )}

      {/* --- chips ----------------------------------------------------- */}
      <div className="kbc-usages__chips" role="group" aria-label="usage filters">
        {TRUST_ORDER.filter((t) => (out.totals[t] ?? 0) > 0).map((t) => (
          <Chip
            key={t}
            handle={`trust:${t}`}
            on={chips.trust.includes(t)}
            label={t}
            count={out.totals[t]}
            title={`keep only ${t} rows`}
            onClick={() => toggleTrust(t)}
          />
        ))}
        <span className="kbc-usages__chip-sep" />
        {kindChips.map((k) => (
          <Chip
            key={k.kind}
            handle={`kind:${k.kind}`}
            on={chips.kinds.includes(k.kind)}
            label={k.kind}
            count={k.total}
            title={`keep only ${k.kind} rows`}
            onClick={() => toggleKind(k.kind)}
          />
        ))}
        {hiddenKinds > 0 && (
          <span className="kbc-usages__chip-more" title="kinds past the chip cap — still counted in the census">
            +{hiddenKinds} more kinds
          </span>
        )}
        <span className="kbc-usages__chip-sep" />
        {(Object.keys(EXCLUDE_BITS) as ExcludeRole[]).map((e) => (
          <Chip
            key={e}
            handle={`exclude:${e}`}
            on={chips.exclude.includes(e)}
            label={`no ${e}`}
            title={`drop rows whose PATH matches the ${e} scope — a location heuristic, never a claim about content`}
            onClick={() => toggleExclude(e)}
          />
        ))}
        <span className="kbc-usages__chip-sep" />
        {/* The grep lane, kept as an EXPLICIT chip: a different question,
            so it never silently changes a classified count (recon §6.3). */}
        <Chip
          handle="mentions"
          on={props.mentionsOn}
          label="mentions"
          count={props.mentions ?? undefined}
          title="text mentions from /api/xrefs — a word-boundary grep over the working tree, not a classified usage"
          onClick={props.onToggleMentions}
        />
        <label className="kbc-usages__scope">
          <span className="kbc-usages__scope-label">scope</span>
          <input
            type="text"
            className="kbc-usages__scope-input"
            placeholder="app/"
            value={chips.scope}
            data-kbc-usages-scope
            onChange={(e) => onChips({ ...chips, scope: e.target.value })}
          />
        </label>
        <label className="kbc-usages__axis">
          <span className="kbc-usages__scope-label">group by</span>
          <select
            className="kbc-usages__axis-select"
            value={axis}
            data-kbc-usages-axis
            onChange={(e) => onAxis(e.target.value as GroupAxis)}
          >
            {GROUP_AXES.map((a) => (
              <option key={a} value={a}>
                {GROUP_AXIS_LABEL[a]}
              </option>
            ))}
          </select>
        </label>
      </div>

      {/* --- grouped tree + preview ------------------------------------- */}
      <div className="kbc-usages__body">
        <div className="kbc-usages__tree" role="listbox" aria-label="usages">
          {flat.length === 0 ? (
            <EmptyState
              variant="inline"
              title="No rows match"
              hint="Every chip above narrows the fetched page; the census strip says how many are hidden."
            />
          ) : (
            groups.map((g) => (
              <section key={g.key} className="kbc-usages__group" data-kbc-usages-group={g.key}>
                <h3 className="kbc-usages__group-head">
                  <span className="kbc-usages__group-label">{g.label}</span>
                  <span className="kbc-usages__group-count">{g.rows.length}</span>
                </h3>
                {g.rows.map((row) => {
                  const idx = flat.indexOf(row);
                  return (
                    <div
                      key={`${row.path}:${row.line}:${row.col}:${idx}`}
                      className={"kbc-usages__row" + (idx === cursor ? " is-cursor" : "")}
                      role="option"
                      aria-selected={idx === cursor}
                      data-kbc-usages-row={idx}
                      onMouseDown={(e) => {
                        if (e.button === 1) {
                          e.preventDefault();
                          props.onCursor(idx);
                          onOpenRow(row, "tab");
                        }
                      }}
                      onClick={(e) => {
                        props.onCursor(idx);
                        if (e.ctrlKey || e.metaKey) onOpenRow(row, "tab");
                        else if (e.shiftKey) onOpenRow(row, "other");
                        else onOpenRow(row, "here");
                      }}
                    >
                      <TrustBadge cls={row.trust} />
                      <span className="kbc-usages__row-kind" title={`precision: ${row.precision}`}>
                        {row.kind}
                      </span>
                      <span className="kbc-usages__row-loc">
                        {row.path}:{row.line}
                      </span>
                      {row.enclosing && (
                        <span className="kbc-usages__row-encl" title="the innermost symbol containing this line">
                          in {row.enclosing.container ? `${row.enclosing.container}#` : ""}
                          {row.enclosing.name}
                        </span>
                      )}
                      <code className="kbc-usages__row-text">{row.context}</code>
                    </div>
                  );
                })}
              </section>
            ))
          )}
        </div>
        <div className="kbc-usages__preview" data-kbc-usages-preview>
          {!current ? (
            <p className="kbc-usages__preview-hint">
              Select a row (or press <kbd>]u</kbd>) to preview it here.
            </p>
          ) : preview.isLoading ? (
            <p className="kbc-usages__preview-hint">loading {current.path}…</p>
          ) : preview.data ? (
            <CodeView
              content={preview.data.content}
              spans={preview.data.highlights}
              blobHash={preview.data.blob_hash}
              gotoSel={{ start: current.line, end: current.line, nonce: cursor }}
            />
          ) : (
            <p className="kbc-usages__preview-hint">
              {current.path} could not be read — the row still names where it is.
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
