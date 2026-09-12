import { Link } from "react-router";
import type { BranchOut } from "../../api/types";
import { readerUrl } from "../../lib/breadcrumbs";
import { codeBasePath, commitUrl, reviewsUrl } from "../../lib/codeUrl";
import { relativeTime, shortSha } from "../../lib/format";
import { Icon } from "../icons";

export interface BranchesHeroProps {
  repo: string;
  defaultBranch: string | null;
  defaultRow: BranchOut | undefined;
}

/// Zone (a) — default-branch card. Origin/HEAD vs HEAD-fallback is not
/// on the wire, so no source marker is invented.
export default function BranchesHero({ repo, defaultBranch, defaultRow }: BranchesHeroProps) {
  if (defaultBranch === null) {
    return (
      <section className="kbc-landing-hero kbc-landing-hero--detached" data-kbc-branches-hero>
        <p className="kbc-landing-hero__hint" data-kbc-branches-hero-detached>
          Detached HEAD — no default branch to pin this page to.
        </p>
      </section>
    );
  }

  const last = defaultRow?.last;
  const sha = defaultRow?.target_sha;

  return (
    <section className="kbc-landing-hero" data-kbc-branches-hero>
      <div className="kbc-landing-hero__top">
        <Icon.Branch />
        <Link
          to={readerUrl(repo, "", defaultBranch)}
          className="kbc-landing-hero__name"
          data-kbc-branches-hero-name
        >
          {defaultBranch}
        </Link>
        <span className="kbc-landing-hero__default-chip" data-kbc-branches-hero-default>
          default
        </span>
      </div>
      <p className="kbc-landing-hero__last">
        {sha ? (
          <Link to={commitUrl(repo, sha)} className="kbc-landing-hero__last-link">
            {last ? last.subject : shortSha(sha)}
          </Link>
        ) : (
          <span>—</span>
        )}
        {last && (
          <span className="kbc-landing-hero__when"> · {relativeTime(last.author_time)}</span>
        )}
      </p>
      <nav className="kbc-landing-hero__links" aria-label="default branch shortcuts">
        <Link to={readerUrl(repo, "", defaultBranch)} data-kbc-branches-hero-browse>
          Browse
        </Link>
        <Link
          to={`${codeBasePath(repo, "")}/~compare?from=${encodeURIComponent(defaultBranch)}`}
          data-kbc-branches-hero-compare
        >
          Compare
        </Link>
        <Link to={reviewsUrl(repo)} data-kbc-branches-hero-reviews>
          Reviews
        </Link>
      </nav>
    </section>
  );
}
