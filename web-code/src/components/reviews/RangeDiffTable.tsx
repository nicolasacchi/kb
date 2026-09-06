import { Link } from "react-router-dom";
import type { RangeDiffPair } from "../../api/types";
import { commitUrl } from "../../lib/codeUrl";
import { shortSha } from "../../lib/format";

export interface RangeDiffTableProps {
  repo: string;
  pairs: RangeDiffPair[];
  truncated?: boolean;
}

function glyphFor(disposition: string): string {
  switch (disposition) {
    case "equal":
      return "=";
    case "modified":
      return "!";
    case "added":
      return "+";
    case "removed":
      return "-";
    default:
      return "?";
  }
}

/// Presentational range-diff disposition table — extracted from
/// `routes/RangeDiff.tsx` so the review cockpit's interdiff mode can reuse
/// the exact same markup/styles rather than a second hand-rolled copy.
export default function RangeDiffTable({ repo, pairs, truncated }: RangeDiffTableProps) {
  return (
    <>
      {truncated && (
        <div className="kbc-rangediff__truncated" data-kbc-rangediff-truncated>
          Showing a bounded subset of pairs.
        </div>
      )}
      <table className="kbc-rangediff__table" data-kbc-rangediff-table>
        <thead>
          <tr>
            <th aria-label="disposition"></th>
            <th>old</th>
            <th>new</th>
            <th>subject</th>
          </tr>
        </thead>
        <tbody>
          {pairs.map((p, i) => (
            <tr key={i} data-kbc-rangediff-pair={p.disposition}>
              <td className={`kbc-rangediff__glyph kbc-rangediff__glyph--${p.disposition}`}>
                {glyphFor(p.disposition)}
              </td>
              <td>
                {p.old_sha ? (
                  <Link to={commitUrl(repo, p.old_sha)}>{shortSha(p.old_sha)}</Link>
                ) : (
                  <span className="kbc-rangediff__dash">—</span>
                )}
              </td>
              <td>
                {p.new_sha ? (
                  <Link to={commitUrl(repo, p.new_sha)}>{shortSha(p.new_sha)}</Link>
                ) : (
                  <span className="kbc-rangediff__dash">—</span>
                )}
              </td>
              <td className="kbc-rangediff__subject">{p.old_subject ?? p.new_subject ?? ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}
