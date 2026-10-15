// OL-C7 — Home's lead section: where you were, with real code under it.
// The most recent location is a hero card (repo · path · line + the first
// lines of the file, read through the reader's own `useFile` so the query
// is a cache hit when you click through); up to four more are a compact
// list. Source: `lib/navHistory.ts`'s browser-local location ring — no new
// endpoint, no server state.

import { useMemo } from "react";
import { Link } from "react-router";
import { useFile } from "../../hooks/useFile";
import { readerUrl } from "../../lib/breadcrumbs";
import { continueLocations, type ContinueLocation } from "../../lib/continueLocations";
import { heroSnippet } from "../../lib/homeSnippet";
import { getRecentLocations } from "../../lib/navHistory";
import HighlightedSnippet from "../HighlightedSnippet";

function ContinueHero({ loc }: { loc: ContinueLocation }) {
  const file = useFile(loc.repo, loc.path, undefined);
  const snippet = file.data ? heroSnippet(file.data) : null;
  return (
    <div className="kbc-home__hero" data-kbc-home-continue-hero>
      <Link
        to={readerUrl(loc.repo, loc.path, undefined, loc.line)}
        className="kbc-home__hero-head"
        data-kbc-home-continue-row={`${loc.repo}:${loc.path}`}
      >
        <span className="kbc-home__hero-repo">{loc.repo}</span>
        <span className="kbc-home__hero-loc">
          {loc.path}:{loc.line}
        </span>
      </Link>
      {snippet !== null && (
        <HighlightedSnippet
          text={snippet}
          lang={file.data?.lang}
          path={loc.path}
          className="kbc-home__hero-code"
        />
      )}
    </div>
  );
}

export default function HomeContinue() {
  // Read once on mount: Home is a landing page, not a live view.
  const recent = useMemo(() => continueLocations(getRecentLocations(), 5), []);
  if (recent.length === 0) return null;
  const [hero, ...rest] = recent;
  return (
    <section
      className="kbc-home__section"
      aria-label="Continue where you left off"
      data-kbc-home-continue-global
    >
      <h2 className="kbc-home__label">Continue</h2>
      <ContinueHero loc={hero} />
      {rest.length > 0 && (
        <ul className="kbc-home__continue-list" data-kbc-home-continue-list>
          {rest.map((f) => (
            <li key={`${f.repo}\0${f.path}`}>
              <Link
                to={readerUrl(f.repo, f.path, undefined, f.line)}
                className="kbc-home__continue-row"
                data-kbc-home-continue-row={`${f.repo}:${f.path}`}
              >
                <span className="kbc-home__continue-repo">{f.repo}</span>
                <span className="kbc-home__continue-loc">
                  {f.path}:{f.line}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
