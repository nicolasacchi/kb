// OL PRES3 - the one-line presence pulse ("1 session working · 4 files in
// 10 min") shown on Home's RepoCard and as the Spine's right-slot chip. Both
// read the same `["presence", repo]` query through `usePresenceSummary`, so
// neither adds a poll. Renders nothing when there is nothing to say (no kb,
// pref off, no recent touches).

import { Link } from "react-router";
import { usePresenceSummary } from "../hooks/usePresence";
import { readerUrl } from "../lib/breadcrumbs";
import { Icon } from "./iconSet";

export function PresenceLine({ repo }: { repo: string }) {
  const s = usePresenceSummary(repo);
  if (!s) return null;
  return (
    <div className="kbc-home-card__presence" data-kbc-home-presence role="status">
      <Icon.Spark aria-hidden />
      <span>{s.text}</span>
    </div>
  );
}

export function PresenceChip({ repo }: { repo: string | undefined }) {
  const s = usePresenceSummary(repo);
  if (!repo || !s) return null;
  return (
    <Link
      to={readerUrl(repo, "")}
      className="kbc-topbar__presence"
      data-kbc-topbar-presence
      title={s.text}
      aria-label={`Agent activity: ${s.text}`}
    >
      <Icon.Spark aria-hidden />
      <span>{s.working > 0 ? `${s.working} working` : `${s.files} changed`}</span>
    </Link>
  );
}
