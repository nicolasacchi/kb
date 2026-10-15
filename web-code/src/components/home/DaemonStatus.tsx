// OL-C8 — the diagnostics Home used to carry (OL-C7 parked them in a
// collapsed section) moved to the Settings page's "Review store" section.
// Home keeps one line that points there.

import { Link } from "react-router";
import { settingsUrl } from "../../lib/codeUrl";

export default function DaemonStatus() {
  return (
    <p className="kbc-home__status" data-kbc-home-status>
      <Link to={settingsUrl()} className="kbc-home__status-link" data-kbc-home-status-link>
        Daemon status {"\u2192"} Settings
      </Link>
    </p>
  );
}
