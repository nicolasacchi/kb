// PRR-U2 §3 (findings vs. comments — the visual language) + §10 —
// `FindingCard` (the Report-tab, full card) and `FindingRow` (the side
// panel's compact row). One thread system per the design thesis: a finding
// IS an annotation with severity/category/slug — this card is a projection
// over `ReviewFinding`, not a second comment component.
import type { FindingLocation, FindingSeverity, ReviewFinding } from "../../api/types";
import { Icon } from "../icons";
import RefCard from "./RefCard";
import { actChipClass, findingCites, findingTombstoneText } from "../../lib/reviewDoc";
import { relativeTime } from "../../lib/format";
import { reviewDiffHref } from "./ReviewHeader";
import DispositionMenu from "./DispositionMenu";
// ── V76-R2a — severity/act/category chips with icons (mappings live in
// `lib/reviewRoom.ts`, never in this component). ──
import { ActChip, CategoryChip, SeverityChip } from "./RoomChips";
// ── PRR-U56 (design-ui.md §2 S5 — publish preview) — the ephemeral
// "mark for publish" toggle. Lives in the module store, not props, so it
// slots into both `FindingCard` (Report tab) and `FindingRow` (side panel)
// without threading a new prop through `ReportPanel`/`ReviewThreadsCard`.
import { eligibleForPublishMark, toggleMark, useIsMarked } from "../../lib/publishMarks";
import RecurrenceChip from "./RecurrenceChip";
import ProseBlock from "../prose/ProseBlock";
import HighlightedSnippet from "../HighlightedSnippet";

/// Section 02's severity ordering (blockers first, then concerns, then
/// verified-ok) — the SAME 3-value vocab `store::is_valid_severity`
/// enforces server-side, never the mock's 5-value draft.
export function severityRank(severity: FindingSeverity): number {
  if (severity === "blocker") return 0;
  if (severity === "concern") return 1;
  return 2; // "ok"
}

/**
 * findings v2 — a finding's SPEECH ACT. `"issue"` when the daemon did not
 * send one, which is exactly what every pre-V0034 row meant (and what an
 * older daemon that omits the field is describing).
 */
export function findingAct(finding: Pick<ReviewFinding, "act">): string {
  return finding.act ?? "issue";
}

/** The act chip's class — one home for the label's styling. */
export function findingActClass(finding: Pick<ReviewFinding, "act">): string {
  return actChipClass(finding.act);
}

/** A finding's secondary refs. Absent ≠ empty — see the wire type's own doc. */
export function findingCitesOf(finding: Pick<ReviewFinding, "cites">): string[] {
  return findingCites(finding);
}

/** `superseded_by` as a tombstone line, or `null` when the row is live. */
export function findingTombstone(
  finding: Pick<ReviewFinding, "superseded" | "superseded_by">,
): string | null {
  return findingTombstoneText(finding);
}

/**
 * Why a cited ref renders as an address rather than as a live card HERE.
 * `GET /api/reviews/{id}/findings` carries the ref STRINGS; only
 * `GET …/doc?resolve=true` mints cards, and this card is reachable without
 * a document at all. Saying so is the point — the Document tab's own
 * finding list shows the same refs against real cards.
 */
export const CITE_UNRESOLVED_REASON =
  "cited ref — cards are minted by the document read (`kb-code review doc --resolve`), not by the findings wire";

export function severityStripeClass(severity: FindingSeverity): string {
  return `kbc-finding--${severity}`;
}

/// `origin` (NOT `author`) decides the author-mark branch: an `"import"`
/// finding always renders the ✳ agent mark (even if a future import batch
/// authors as someone other than `"claude"`); a `"manual"` finding always
/// renders a plain "you"-style chip — `store::FINDING_ORIGIN_*`'s own
/// doc: import defaults to `"claude"`, manual defaults to `"you"`, but
/// EITHER can carry a different explicit `author` string, so branching on
/// `origin` (not sniffing the author string) is the honest signal.
export function findingAuthorDisplay(finding: ReviewFinding): { isAgent: boolean; label: string } {
  return { isAgent: finding.origin === "import", label: finding.author };
}

/// `location` → a single display chip, honoring the 4-kind ladder
/// (`design-server.md` §1.4): `single` → `path:N`; `range` → `path:A-B`;
/// `multi` → `path:A,B,C`; `whole_file` → bare `path`. A `removed` location
/// (the cited line/file was deleted by this diff) appends `(removed)`.
export function findingLocationLabel(location: FindingLocation): string {
  const lines = location.lines ?? [];
  let suffix = "";
  if (location.kind === "single" && lines.length === 1) {
    suffix = `:${lines[0]}`;
  } else if (location.kind === "range" && lines.length === 2) {
    suffix = `:${lines[0]}-${lines[1]}`;
  } else if (location.kind === "multi" && lines.length > 0) {
    suffix = `:${lines.join(",")}`;
  }
  const removedSuffix = location.removed ? " (removed)" : "";
  return `${location.path}${suffix}${removedSuffix}`;
}

/// Deep-link into the diff route for one finding's location — via the
/// EXISTING `reviewDiffHref(repo, id, file?)` builder signature (this unit
/// does not extend it). Appends `?line=` when the finding's per-request
/// `resolution` resolved a concrete line (mirrors `ReviewThreadsCard`'s
/// `threadHref` idiom exactly). The `finding=` deep-link param is a
/// SIBLING unit's (U3) upgrade to the diff overlay — this link works today
/// without it (lands on the file, not yet scrolled+flashed to the exact
/// finding); U3 adds `?finding=` on top, additively.
export function findingDiffHref(
  repo: string,
  reviewId: number,
  finding: ReviewFinding,
  ps?: string,
): string {
  const base = reviewDiffHref(repo, reviewId, finding.location.path);
  const params = new URLSearchParams();
  if (ps && ps !== "latest") params.set("ps", ps);
  if (!finding.resolution.orphaned && finding.resolution.line != null) {
    params.set("line", String(finding.resolution.line));
  }
  const qs = params.toString();
  return qs ? `${base}?${qs}` : base;
}

function copyText(text: string) {
  void navigator.clipboard?.writeText(text).catch(() => {
    // Clipboard permission denied / unavailable — silently a no-op, same
    // graceful-degrade posture the rest of this crate's copy affordances use
    // (no toast for a browser-permission edge case that isn't actionable).
  });
}

function SeverityMeta({ finding, showAgentMark = true }: { finding: ReviewFinding; showAgentMark?: boolean }) {
  const author = findingAuthorDisplay(finding);
  return (
    <div className="kbc-finding__meta">
      {/* V76-R2a — severity, act and category are coloured CHIPS WITH ICONS
          now (`RoomChips.tsx`, mappings in `lib/reviewRoom.ts`). The act is
          still the SPEECH-ACT axis BESIDE severity, not a second severity
          (findings v2, V73-K2b): an issue and a question about the same
          line at the same severity are different things to a reader. The
          `data-kbc-finding-act`/`data-kbc-finding-severity` hooks ride the
          chips so existing selectors keep working. */}
      <span data-kbc-finding-severity={finding.severity} className="kbc-finding__chipslot kbc-finding__sev">
        <SeverityChip severity={finding.severity} />
      </span>
      <span data-kbc-finding-act={findingAct(finding)} className={`kbc-finding__chipslot ${findingActClass(finding)}`}>
        <ActChip act={finding.act} />
      </span>
      <CategoryChip category={finding.category} />
      {/* `blocking` is the reviewer's OWN call and deliberately not derived
          from `severity` ("a blocker that is not blocking this PR" is a real
          thing to say). It reads as WEIGHT — never as a score. */}
      {finding.blocking && (
        <span
          className="kbc-finding__blocking"
          data-kbc-finding-blocking
          title="the reviewer's own call — not derived from severity"
        >
          blocking
        </span>
      )}
      <span className="kbc-finding__loc" data-kbc-finding-loc title={findingLocationLabel(finding.location)}>
        {findingLocationLabel(finding.location)}
      </span>
      {findingTombstone(finding) && (
        <span
          className="kbc-finding__tomb"
          data-kbc-finding-tombstone={finding.superseded_by ?? "superseded"}
          title="still here, still readable, naming its successor — a re-compose never destroys what a human formed a disposition against"
        >
          {findingTombstone(finding)}
        </span>
      )}
      {finding.resolution.orphaned && (
        <span className="kbc-finding__orphan" data-kbc-finding-orphaned title="anchor no longer resolves">
          <Icon.Unlink /> orphaned
        </span>
      )}
      {showAgentMark && (
        <span className="kbc-finding__agent" data-kbc-finding-author={author.label}>
          {author.isAgent ? (
            <>
              <Icon.Spark /> {author.label}
            </>
          ) : (
            author.label
          )}
        </span>
      )}
    </div>
  );
}

export interface FindingCardProps {
  repo: string;
  reviewId: number;
  finding: ReviewFinding;
  ps?: string;
}

/// Full Report-tab card (§3's table): severity stripe, sev/category/
/// location/slug meta row, title + rationale + optional evidence block +
/// recommendation, disposition chip-row. Replies live on the diff overlay /
/// side panel (this card links out via `view in diff`, rather than
/// re-embedding a second reply composer — `thread_count`/`unresolved_count`
/// are surfaced as a compact affordance instead).
export default function FindingCard({ repo, reviewId, finding, ps }: FindingCardProps) {
  const href = findingDiffHref(repo, reviewId, finding, ps);
  return (
    <div
      className={`kbc-finding ${severityStripeClass(finding.severity)}`}
      data-kbc-finding={finding.slug}
    >
      {/* V76-R2a — visibly separated zones: header (chips + slug) · body
          (title + rationale + evidence) · suggestion callout · actions.
          The left severity stripe (`.kbc-finding--*` `::before`) is
          unchanged. */}
      <header className="kbc-finding__head">
        <SeverityMeta finding={finding} />
        <div className="kbc-finding__slugrow">
          <button
            type="button"
            className="kbc-finding__slug"
            title="copy permalink"
            onClick={() => copyText(new URL(href, window.location.origin).toString())}
            data-kbc-finding-slug={finding.slug}
          >
            <Icon.Copy /> {finding.slug}
          </button>
        </div>
      </header>
      <div className="kbc-finding__body">
        <h3 className="kbc-finding__ti" data-kbc-finding-title>
          <ProseBlock text={finding.title} refs={finding.title_refs} repo={repo} reviewId={reviewId} inline />
        </h3>
        <div className="kbc-finding__ra" data-kbc-finding-rationale>
          <ProseBlock
            text={finding.rationale}
            refs={finding.rationale_refs}
            repo={repo}
            reviewId={reviewId}
            fallbackLang={finding.evidence?.lang}
          />
        </div>
        {finding.evidence && (finding.evidence.source || finding.evidence.lang) && (
          <div className="kbc-codewrap" data-kbc-finding-evidence>
            <div className="head">
              <span>{finding.location.path}</span>
              <span>{finding.evidence.lang ?? ""}</span>
            </div>
            <HighlightedSnippet
              text={finding.evidence.source ?? ""}
              lang={finding.evidence.lang}
              path={finding.location.path}
            />
          </div>
        )}
      </div>
      {finding.recommendation && (
        <div className="kbc-finding__rc" data-kbc-finding-recommendation>
          <ProseBlock
            text={finding.recommendation}
            refs={finding.recommendation_refs}
            repo={repo}
            reviewId={reviewId}
          />
        </div>
      )}
      {/* SECONDARY refs (findings v2). They never compete with the PRIMARY
          location above — that one carries the annotation anchor, and
          therefore the carry-forward ladder, the thread and the GitHub
          export. These are folded by default: a citation is context, and an
          expanded stack of them would bury the finding itself. */}
      {findingCitesOf(finding).length > 0 && (
        <div className="kbc-finding__cites" data-kbc-finding-cites={findingCitesOf(finding).length}>
          {findingCitesOf(finding).map((c) => (
            <RefCard
              key={c}
              span={{ kind: "unresolved", body: c, reason: CITE_UNRESOLVED_REASON }}
              repo={repo}
              reviewId={reviewId}
              folded
            />
          ))}
        </div>
      )}
      <div className="kbc-finding__foot kbc-finding__actions">
        <DispositionMenu repo={repo} reviewId={reviewId} finding={finding} />
        {eligibleForPublishMark(finding) && <PublishMarkToggle reviewId={reviewId} slug={finding.slug} />}
        <RecurrenceChip repo={repo} reviewId={reviewId} slug={finding.slug} />
        <span className="grow" />
        {finding.thread_count > 0 && (
          <span className="kbc-finding__thread-count" data-kbc-finding-thread-count>
            <Icon.Comment /> {finding.thread_count}
            {finding.unresolved_count > 0 ? ` · ${finding.unresolved_count} open` : ""}
          </span>
        )}
        <a className="kbc-btn kbc-btn--ghost" href={href} data-kbc-finding-view-in-diff={finding.slug}>
          view in diff →
        </a>
      </div>
    </div>
  );
}

export interface FindingRowProps {
  repo: string;
  reviewId: number;
  finding: ReviewFinding;
  ps?: string;
}

/// Compact side-panel row (§2 S2's mock `.kbc-frow`): severity/state bar,
/// slug, location, disposition state — no body text, click-through only.
/// PRR-U56: wrapped in a `.kbc-frow-row` sibling div so the publish-mark
/// `<button>` can sit beside the `<a>` (nesting a button inside an anchor is
/// invalid HTML — see `PublishMarkToggle`'s own doc); the anchor itself is
/// otherwise byte-identical to before this unit.
export function FindingRow({ repo, reviewId, finding, ps }: FindingRowProps) {
  const href = findingDiffHref(repo, reviewId, finding, ps);
  const done = finding.severity === "ok" || finding.disposition?.state === "agree";
  const barClass =
    finding.severity === "blocker"
      ? "bar--blocker"
      : finding.severity === "concern"
        ? "bar--concern"
        : "bar--ok";
  const loc = findingLocationLabel(finding.location);
  return (
    <div className="kbc-frow-row" data-kbc-finding-row-wrap={finding.slug}>
      <a
        href={href}
        className={"kbc-frow" + (done ? " kbc-frow--done" : "")}
        data-kbc-finding-row={finding.slug}
      >
        <span className={`bar ${barClass}`} aria-hidden="true" />
        {/* V76-R2a — the two-line row: slug line over `path:line` line,
            both middle-truncatable with the full value on hover, and the
            disposition chip pinned right, never wrapping mid-token. */}
        <div className="kbc-frow__body">
          <div className="slug" title={finding.slug}>
            {finding.slug}
          </div>
          <div className="loc" title={loc}>
            {loc}
          </div>
        </div>
        <span
          className={`state state--${finding.disposition?.state ?? "open"}`}
          data-kbc-finding-row-state={finding.disposition?.state ?? "open"}
        >
          {finding.disposition ? finding.disposition.state : finding.severity === "ok" ? "✓" : "open"}
        </span>
      </a>
      {eligibleForPublishMark(finding) && (
        <PublishMarkToggle reviewId={reviewId} slug={finding.slug} />
      )}
      <RecurrenceChip repo={repo} reviewId={reviewId} slug={finding.slug} />
    </div>
  );
}

// ── PRR-U56 — the publish-mark toggle itself. A `<button>`, never nested
// inside `FindingRow`'s `<a>` (interactive-in-interactive is invalid HTML;
// see `FindingRow`'s own restructuring below) — used as a sibling instead.
function PublishMarkToggle({ reviewId, slug }: { reviewId: number; slug: string }) {
  const marked = useIsMarked(reviewId, slug);
  return (
    <button
      type="button"
      className={"kbc-publish-mark" + (marked ? " kbc-publish-mark--on" : "")}
      aria-pressed={marked}
      title={marked ? "unmark for publish" : "mark for publish"}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        toggleMark(reviewId, slug);
      }}
      data-kbc-publish-mark={slug}
    >
      <Icon.Check /> {marked ? "marked" : "mark"}
    </button>
  );
}

/// Small helper the side panel + Timeline-adjacent surfaces can use for a
/// "N ago" caption without re-importing `relativeTime` everywhere — kept
/// here since it's finding-specific formatting (content_updated_at vs
/// created_at precedence: a carried-forward re-import updates content, so
/// the MOST RECENT of the two is the honest "last touched" timestamp).
export function findingLastTouched(finding: ReviewFinding, now?: number): string {
  const ts = Math.max(finding.content_updated_at ?? 0, finding.updated_at, finding.created_at);
  return relativeTime(ts, now);
}
