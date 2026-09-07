import type { CSSProperties } from "react";
import { Link } from "react-router-dom";
import type { BranchFactRow as Row } from "../../api/types";
import { compareUrl, reviewUrl } from "../../lib/codeUrl";
import { shortSha } from "../../lib/format";
import { provHueStyle } from "../../lib/provHue";
import { BASE_CLASS_LABELS } from "../../lib/branchViews";

// V75-M3 (D15) — ONE row of `branch-facts/1`.
//
// The rule this file exists to keep: **it derives nothing.** Every badge,
// chip, count and sentence is read off the wire. `reasons[]` arrives as
// `{code, text}` pairs in a fixed server-side order and is rendered
// verbatim — the CLI prints the same strings, so the two surfaces cannot
// drift into describing the same row differently. The one thing computed
// here is a HUE INDEX from the session id, which carries no meaning beyond
// identity (see `lib/provHue.ts`).

export interface BranchFactRowProps {
  repo: string;
  row: Row;
  focused: boolean;
  compact: boolean;
  /// `null` when this caller cannot reach the loopback-only review route —
  /// the CTA is then ABSENT, not disabled (a disabled row is a map of the
  /// mutation surface, `kbc-actions/1`'s own rule).
  onCompareWithBase: ((row: Row) => void) | null;
  onToggleFavourite: (row: Row) => void;
  onFocus: () => void;
}

/// The witness sentence a `merged` badge shows on hover. Built from the
/// witness the server sent; never a guess about how it was proved.
function mergedTitle(row: Row): string {
  const m = row.merged;
  if (!m) return "";
  if (m.kind === "ancestry") {
    return `merged into ${m.into} by ancestry${m.into_sha ? ` @${shortSha(m.into_sha)}` : ""} — 0 commits ahead of the base IS the proof`;
  }
  return `merged into ${m.into} by patch-id — ${m.equivalent ?? 0} commit(s) had an equivalent patch upstream (what catches a squash, which ancestry cannot see)`;
}

export default function BranchFactRow({
  repo,
  row,
  focused,
  compact,
  onCompareWithBase,
  onToggleFavourite,
  onFocus,
}: BranchFactRowProps) {
  const agent = row.agent.class !== "none";
  // The provenance rail keys on the SESSION when the trailer named one, and
  // otherwise on the branch's own ref — so two branches from one agent run
  // share a hue and an email-only `likely` row still gets a stable one.
  const railStyle = agent
    ? (provHueStyle(row.agent.session_id ?? row.full_ref) as CSSProperties)
    : undefined;

  return (
    <li
      className={[
        "kbc-bfact",
        focused ? "kbc-bfact--focused" : "",
        compact ? "kbc-bfact--compact" : "",
        agent ? "kbc-bfact--agent" : "",
        row.stack ? "kbc-bfact--stacked" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={railStyle}
      data-kbc-bfact={row.name}
      data-kbc-bfact-agent={row.agent.class}
      data-kbc-bfact-base={row.base.class}
      // A stack layer is INDENTED by its depth — the structure D15 says a
      // flat list of N rows is lying about.
      data-kbc-bfact-depth={row.stack?.depth ?? 0}
      onMouseEnter={onFocus}
    >
      <div className="kbc-bfact__head">
        <button
          type="button"
          className="kbc-bfact__star"
          aria-pressed={row.favourite}
          aria-label={row.favourite ? `Unstar ${row.name}` : `Star ${row.name}`}
          data-kbc-bfact-star={row.name}
          onClick={() => onToggleFavourite(row)}
        >
          {row.favourite ? "★" : "☆"}
        </button>
        <Link
          className="kbc-bfact__name"
          to={compareUrl(repo, { from: row.base.ref ?? "", to: row.name, threeDot: true })}
          data-kbc-bfact-name={row.name}
        >
          {row.name}
        </Link>
        {row.remote && <span className="kbc-bfact__remote">{row.remote}</span>}

        <span className="kbc-bfact__base" data-kbc-bfact-baseref>
          {row.base.ref ? (
            <>
              base {row.base.ref}{" "}
              <span className="kbc-bfact__baseclass" title={`base ladder rung: ${row.base.class}`}>
                ({BASE_CLASS_LABELS[row.base.class] ?? row.base.class})
              </span>
            </>
          ) : (
            // `unknown` is a REAL answer, and the row says so rather than
            // showing a blank where a base would be.
            <span className="kbc-bfact__baseclass" title="no upstream, no fork point, no merge base">
              base unknown
            </span>
          )}
        </span>

        <span className="kbc-bfact__ab">
          <span data-kbc-bfact-ahead>{row.ahead ?? "–"}</span>
          <span aria-hidden="true"> / </span>
          <span data-kbc-bfact-behind>{row.behind ?? "–"}</span>
        </span>
      </div>

      <div className="kbc-bfact__badges">
        {row.is_head && <span className="kbc-chip kbc-chip--head" data-kbc-bfact-head>HEAD</span>}
        {row.worktree && (
          <span className="kbc-chip" data-kbc-bfact-worktree={row.worktree.id} title={row.worktree.path}>
            worktree {row.worktree.id}
          </span>
        )}
        {agent && (
          <span
            className="kbc-chip kbc-chip--agent"
            data-kbc-bfact-agentchip={row.agent.class}
            title={`agent provenance: ${row.agent.class} (${row.agent.via})`}
          >
            agent {row.agent.class}
          </span>
        )}
        {row.mine && <span className="kbc-chip" data-kbc-bfact-mine>mine</span>}
        {row.merged && (
          <span className="kbc-chip kbc-chip--merged" data-kbc-bfact-merged={row.merged.kind} title={mergedTitle(row)}>
            merged · {row.merged.kind}
          </span>
        )}
        {row.stale && (
          <span className="kbc-chip kbc-chip--stale" data-kbc-bfact-stale>
            stale
          </span>
        )}
        {row.upstream?.gone && (
          <span className="kbc-chip kbc-chip--warn" data-kbc-bfact-gone>
            upstream gone
          </span>
        )}
        {row.reviews.map((r) => (
          <Link key={r.id} className="kbc-chip kbc-chip--review" to={reviewUrl(repo, r.id)} data-kbc-bfact-review={r.id}>
            review #{r.id}
          </Link>
        ))}
        {row.pr && (
          <span className="kbc-chip kbc-chip--pr" data-kbc-bfact-pr={row.pr.number} title={row.pr.title}>
            PR #{row.pr.number}
            {row.pr.draft ? " (draft)" : ""}
          </span>
        )}
        {row.ci && (
          <span className={`kbc-chip kbc-chip--ci-${row.ci.status}`} data-kbc-bfact-ci={row.ci.status}>
            ci {row.ci.status} ({row.ci.checks})
          </span>
        )}
        {row.stack && (
          <span className="kbc-chip" data-kbc-bfact-stack={row.stack.base} title="a review from here compares against the stack parent, not the default branch">
            stack on {row.stack.base}
            {row.stack.stale ? " (base moved)" : ""}
          </span>
        )}
      </div>

      {!compact && (
        <ul className="kbc-bfact__reasons" data-kbc-bfact-reasons>
          {row.reasons.map((r) => (
            // Rendered VERBATIM — the server's sentence, not a lookup keyed
            // on `code` (that is how two surfaces start disagreeing).
            <li key={`${r.code}:${r.text}`} className="kbc-bfact__reason" data-kbc-bfact-reason={r.code}>
              {r.text}
            </li>
          ))}
        </ul>
      )}

      {onCompareWithBase && (
        <button
          type="button"
          className="kbc-bfact__cta"
          data-kbc-bfact-compare={row.name}
          onClick={() => onCompareWithBase(row)}
        >
          Compare with common base
        </button>
      )}
    </li>
  );
}
