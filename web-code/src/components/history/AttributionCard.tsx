import type { LadderAttribution } from "../../api/types";
import { sessionUrl } from "../../lib/searchLanes";
import { Icon } from "../icons";

export interface AttributionCardProps {
  attribution: LadderAttribution;
  /// Compact renders just the confidence badge + display name/session link
  /// inline (Branches table cell); the default renders a fuller card
  /// (Commit page), mirroring `WhyPanel`'s own confidence-badge styling
  /// (`.kbc-why__confidence`) under a new `kbc-attrib` prefix.
  compact?: boolean;
}

/// A reusable join-ladder attribution display (Wave C's commit page +
/// branches table both need "who/what session produced this commit," the
/// SAME confidence/via vocabulary `WhyPanel` already renders for a blame
/// region's line-grade attribution) — kept as its own small component
/// rather than duplicated inline in both `routes/Commit.tsx` and
/// `routes/Branches.tsx`.
export default function AttributionCard({ attribution, compact }: AttributionCardProps) {
  const who = attribution.display_name ?? attribution.session_id;
  const honestNone = attribution.confidence === "none";

  const badge = (
    <span
      className={`kbc-attrib__confidence kbc-attrib__confidence--${attribution.confidence}`}
      data-kbc-attrib-confidence={attribution.confidence}
    >
      {attribution.confidence}
    </span>
  );

  const sessionLink = attribution.session_id ? (
    <a
      className="kbc-attrib__who"
      href={sessionUrl(attribution.session_id)}
      target="_blank"
      rel="noreferrer"
      data-kbc-attrib-session-link
    >
      {who}
    </a>
  ) : who ? (
    <span className="kbc-attrib__who">{who}</span>
  ) : null;

  if (compact) {
    return (
      <span className="kbc-attrib kbc-attrib--compact" data-kbc-attrib>
        {badge}
        {sessionLink ?? (
          <span className="kbc-attrib__none" data-kbc-attrib-none>
            —
          </span>
        )}
      </span>
    );
  }

  return (
    <div className="kbc-attrib kbc-attrib-card" data-kbc-attrib>
      <div className="kbc-attrib-card__head">
        {badge}
        {who && <span className="kbc-attrib-card__who">{who}</span>}
      </div>
      {honestNone && (
        <p className="kbc-attrib-card__none" data-kbc-attrib-none>
          no recorded session
        </p>
      )}
      {attribution.session_id && (
        <a
          className="kbc-attrib-card__open-session"
          href={sessionUrl(attribution.session_id)}
          target="_blank"
          rel="noreferrer"
        >
          Open session in kb <Icon.External width={12} height={12} aria-hidden />
        </a>
      )}
    </div>
  );
}
