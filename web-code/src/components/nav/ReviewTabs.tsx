import { Link, useLocation, useParams } from "react-router";
import { branchesUrl, compareUrl, prsUrl, reviewsUrl } from "../../lib/codeUrl";
import { navItemMatches } from "../../lib/navItems";
import "../../styles/reviewtabs.css";

// OL-C1 — the Review door's sub-nav. The old Branches / PRs / Reviews chips
// left the top bar with the Explore menu; this one row, mounted at the top of
// each review-family page (Reviews, PRs, Branches, Compare, Range-diff), keeps
// all of them one click away. The tab set is the single place that names the
// family — the door itself (`lib/navItems.ts`) only knows the family exists.

const TABS: { key: string; label: string; url: (repo: string) => string }[] = [
  { key: "reviews", label: "Reviews", url: reviewsUrl },
  { key: "prs", label: "PRs", url: prsUrl },
  { key: "branches", label: "Branches", url: branchesUrl },
  // `compareUrl` needs a range; blank inputs land on the page's own form.
  { key: "compare", label: "Compare", url: (repo) => compareUrl(repo, { from: "", to: "" }) },
];

export default function ReviewTabs() {
  const { repo = "" } = useParams<{ repo: string }>();
  const { pathname } = useLocation();
  if (!repo) return null;
  return (
    <nav className="kbc-reviewtabs" aria-label="Review" data-kbc-reviewtabs>
      {TABS.map((tab) => {
        const on = navItemMatches(pathname, tab.url(repo));
        return (
          <Link
            key={tab.key}
            to={tab.url(repo)}
            className={"kbc-reviewtabs__tab" + (on ? " is-active" : "")}
            aria-current={on ? "page" : undefined}
            data-kbc-reviewtab={tab.key}
          >
            {tab.label}
          </Link>
        );
      })}
    </nav>
  );
}
