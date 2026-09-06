import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import type { DocSummary } from "../api/client";
import { useVersions } from "../hooks/useVersions";
import { useArtifactSessions } from "../hooks/useSessions";

// W2.7 — folio chrome: the optional "published reading" density
// (`?view=folio`). Two quiet strips detail.tsx mounts AROUND the reading
// canvas (iframe or native note) in both render paths — the artifact's own
// bytes are never touched (invariant #5); this is SPA chrome only. Calm
// computing: no streaks/badges, just time-as-place framing (created ·
// edition · origin session) so a longread feels published rather than
// merely displayed.

/// The running header: "kb · folder · current section". Sticky + quiet
/// (mono, --ink-dim — see app.css); disappears in immersive (mobile.css
/// guards against `body.kb-immersive`, defense-in-depth since `?view=` only
/// carries one mode at a time). `section` is the current heading TITLE
/// (resolved from the runtime's `kb:toc`/`kb:section` postMessages by
/// detail.tsx, mirroring TocSpy's own join) — omitted on the native-note
/// path, which has no iframe runtime to emit it.
export function FolioHeader({
  kb,
  folder,
  section,
}: {
  kb: string;
  folder?: string | null;
  section?: string | null;
}) {
  const crumbs = [kb, folder || null, section || null].filter(
    (p): p is string => !!p,
  );
  return <div className="kb-folio-header">{crumbs.join(" · ")}</div>;
}

// Long-form "Month dd, yyyy" — a colophon reads as a publication stamp, not
// a relative "3d ago" tape (that's the gallery's `relativeAge`, a different
// register).
function colophonDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

/// The colophon: created/modified date · edition (version short-id) ·
/// origin-session link. Every value comes from hooks the always-docked
/// PreviewInspector rail already calls for the open artifact
/// (`useVersions`/`useArtifactSessions`, keyed `["versions", kb, id]` /
/// `["artifact-sessions", kb, id]`) — react-query dedupes by key, so
/// mounting this alongside the rail never adds a network round-trip of its
/// own (recon: no `content_hash` on the wire; `Version.short` is the closest
/// available "edition" marker).
export function FolioColophon({ kb, doc }: { kb: string; doc: DocSummary }) {
  const { versions } = useVersions(kb, doc.id, true);
  const latest = versions[0];
  const { sessions } = useArtifactSessions(kb, doc.id);
  const origin = sessions.find((s) => s.authored);

  const created = doc.created_unix ?? null;
  // Honest label: fall back to mtime (nullable on btime-less filesystems /
  // pre-v0.15 rows), but call it "modified" — it isn't the true birth time.
  const stamp = created ?? doc.mtime_unix ?? null;

  const items: { key: string; node: ReactNode }[] = [];
  if (stamp !== null) {
    items.push({
      key: "stamp",
      node: `${created !== null ? "created" : "modified"} ${colophonDate(stamp)}`,
    });
  }
  if (latest) {
    items.push({
      key: "edition",
      node: (
        <>
          edition{" "}
          <span className="kb-folio-colophon__short">{latest.short}</span>
        </>
      ),
    });
  }
  if (origin) {
    items.push({
      key: "session",
      node: (
        <Link
          to={`/sessions?kb=${encodeURIComponent(kb)}&focus=${encodeURIComponent(origin.session_id)}`}
          className="kb-folio-colophon__session"
          title="open the origin session in the worklog"
        >
          from session →
        </Link>
      ),
    });
  }
  if (items.length === 0) return null;

  return (
    <div className="kb-folio-colophon">
      {items.map((item, i) => (
        <span className="kb-folio-colophon__item" key={item.key}>
          {i > 0 && (
            <span className="kb-folio-colophon__sep" aria-hidden="true">
              {" "}
              ·{" "}
            </span>
          )}
          {item.node}
        </span>
      ))}
    </div>
  );
}
