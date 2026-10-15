// OL-C7 — the diagnostics Home used to carry inline (per-repo review-store
// registration + fetch credential, the remote-mutations flag), tucked into a
// collapsed section. Nothing removed from the product; until a kb-code
// settings page exists this is where they live. The body mounts only once
// opened, so a closed section costs no review-store requests.

import { useState } from "react";
import type { RepoListEntry } from "../../api/types";
import { useIdentity } from "../../hooks/useIdentity";
import { ReviewCredentialSection, ReviewStoreSection } from "./ReviewStoreSection";

function DaemonStatusBody({ repos }: { repos: RepoListEntry[] }) {
  const identity = useIdentity();
  const remote = identity.data?.remote_mutations;
  return (
    <div className="kbc-home__status-body">
      {/* `remote_mutations` is optional on the wire: only rendered when the
          daemon actually reports it, never a fabricated "off". */}
      {remote !== undefined && (
        <p className="kbc-home__status-line" data-kbc-home-remote-mutations={remote}>
          Remote review mutations: {remote ? "on" : "off"}
        </p>
      )}
      {repos.map((r) => (
        <div key={r.name} className="kbc-home__status-repo" data-kbc-home-status-repo={r.name}>
          <h3 className="kbc-home__status-repo-name">{r.name}</h3>
          <ReviewStoreSection repo={r.name} />
          <ReviewCredentialSection repo={r.name} />
        </div>
      ))}
    </div>
  );
}

export default function DaemonStatus({ repos }: { repos: RepoListEntry[] }) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="kbc-home__status"
      data-kbc-home-status
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
    >
      <summary className="kbc-home__status-summary">Daemon status</summary>
      {open && <DaemonStatusBody repos={repos} />}
    </details>
  );
}
