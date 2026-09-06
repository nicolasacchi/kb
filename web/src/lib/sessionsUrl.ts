// W3.B — the single source of truth for every `/sessions` and session-artifact
// deep-link the SPA builds. Mirrors `galleryUrl.ts`'s #35 discipline (ONE
// builder, no ad-hoc query strings) applied to the sessions surface (R12:
// "ALL new SPA hrefs emit `?project=` from day one via `sessionsUrl.ts`").
//
// Grammar (designs/projects.md P6, synthesis-memo R12):
//
//   /sessions?view=projects|list|threads
//            &project=<registry-id|project_key>   (folder= accepted INBOUND
//                                                    as legacy — C-17 — but
//                                                    never EMITTED by this
//                                                    builder)
//            &q=&harness=&errors=&committed=&model=&from=&to=
//            &focus=<sid>
//            &substance=<csv>                      (S1 husk triage)
//
// and, on the artifact route, `?turn=<N|end>` (S5 — see `artifactTurnHref`).
//
// Empty / null / undefined axes are omitted, exactly like `galleryUrl`, so a
// bare `sessionsUrl()` is `/sessions` and the landing URL only ever carries
// the axes the caller actually set.

export type SessionsView = "projects" | "list" | "threads";

export type SessionsFilters = {
  view?: SessionsView;
  /// The P1-derived-key axis: a registered `[projects.*]` id, or a raw
  /// `project_key`. This builder ONLY ever emits `project=` — `folder=` is
  /// legacy-inbound-only (read by the route, never written here).
  project?: string | null;
  q?: string | null;
  /// W3.A/S1 — csv over `trivial|routine|substantive`. Order-preserved.
  substance?: string[];
  focus?: string | null;
};

/// The `/sessions` URL for a given filter set. A bare `sessionsUrl()` is the
/// unfiltered default landing (`/sessions`, D3: list view, no project pin).
export function sessionsUrl(f: SessionsFilters = {}): string {
  const p = new URLSearchParams();
  if (f.view && f.view !== "list") p.set("view", f.view);
  if (f.project) p.set("project", f.project);
  if (f.q) p.set("q", f.q);
  if (f.substance && f.substance.length > 0) {
    p.set("substance", f.substance.join(","));
  }
  if (f.focus) p.set("focus", f.focus);
  const qs = p.toString();
  return qs ? `/sessions?${qs}` : "/sessions";
}

/// `/sessions?project=<id>` — the projects-home card → project-scoped list
/// jump (P6). Kept as a named helper since it's the single most common call
/// site (every project card, every "view worklog" affordance).
export function sessionsProjectUrl(project: string, extra: SessionsFilters = {}): string {
  return sessionsUrl({ ...extra, project });
}

/// `/sessions?view=projects` — the "back to projects home" pill.
export function sessionsProjectsHomeUrl(): string {
  return sessionsUrl({ view: "projects" });
}

/// The reader → worklog deep-link (S3's SessionContextCard "worklog →"
/// action): jump straight to a session's row, focused, in its project scope
/// when known.
export function sessionsWorklogUrl(sessionId: string, project?: string | null): string {
  return sessionsUrl({ focus: sessionId, project: project ?? undefined });
}

/// `/replay/:kb/:sid` — the session-replay reader. Not part of the `/sessions`
/// query grammar (a distinct route), but kept here so every "replay →"
/// affordance (list row, context card, thread) builds it the same way.
export function replayUrl(kb: string, sessionId: string): string {
  return `/replay/${encodeURIComponent(kb)}/${encodeURIComponent(sessionId)}`;
}

/// S5 — `?turn=<N|end>` on the SPA artifact route
/// (`/a/:kb/*?turn=N` / `?turn=end`). Composes with `artifactHref`'s other
/// options via the `turn` field on `ArtifactHrefOpts` (see `artifactHref.ts`)
/// — this helper exists for call sites that only need the turn value itself
/// (e.g. building a raw query fragment for a non-artifactHref consumer).
export type SessionTurn = number | "end";

export function formatSessionTurn(turn: SessionTurn): string {
  return turn === "end" ? "end" : String(turn);
}
