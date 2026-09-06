import { Link } from "react-router-dom";
import { tagColor } from "../lib/derive";

// Small monospace pill for a single tag. When `accent` is true,
// the border + text shift to the tag's color (used for the
// `accentTag` — the primary / first tag of an artifact).
//
// v0.22 — when `to` is set the pill renders as a react-router <Link>
// (a clickable deep-link into a pre-filtered gallery); otherwise it stays
// a display-only <span> (back-compat for callers that don't navigate, and
// for contexts where the pill is already nested inside a parent <Link>).
export default function TagPill({
  tag,
  accent = false,
  to,
}: {
  tag: string;
  accent?: boolean;
  to?: string;
}) {
  const c = tagColor(tag);
  const style = {
    borderColor: accent ? c : "var(--border)",
    color: accent ? c : "var(--muted)",
  };
  if (to) {
    return (
      <Link
        className="tag-pill tag-pill--link"
        style={style}
        to={to}
        title={`filter the gallery by #${tag}`}
      >
        {tag}
      </Link>
    );
  }
  return (
    <span className="tag-pill" style={style}>
      {tag}
    </span>
  );
}
