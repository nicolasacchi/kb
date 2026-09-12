import { useMemo, useRef, useState } from "react";
import { useParams, useSearchParams } from "react-router";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import RailsOrphans from "../components/rails/RailsOrphans";
import RailsSection from "../components/rails/RailsSection";
import { useRailsHome, useRailsOrphans } from "../hooks/useRails";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import { honestyLine, orphanIndex, passportFacts, sectionHeads } from "../lib/railsCards";

/// V72-I2 — `/r/:repo/~rails`: the `rails/1` dashboard.
///
/// MOUNTED THE WAY EVERY OTHER REPO-SCOPED SENTINEL IS (§D1: "any new
/// surface lands in an existing REGION"): a lazy route in `app.tsx` beside
/// `~todos`/`~workspaces`/`~hotspots`, NOT a second app shell and not a new
/// Desk center mode. `desk/centerModes.ts`'s `SHIPPED_CENTER_MODES` names
/// the modes that MOUNT the Desk (`reader`, and `dossier` since V72-G1.2),
/// and the landmark golden loops it asserting the five regions for each —
/// `~rails` mounts no Desk, so claiming `dashboard` there would make that
/// golden green over nothing.
///
/// EVERY NUMBER ON THIS PAGE IS THE DAEMON'S. The passport's per-noun counts
/// are `RailsHomeOut.counts` (TRUE totals over the whole index); each
/// section's page caption is built from that section's own response. Nothing
/// here counts rows.
export default function Rails() {
  // Root CLAUDE.md #31, ported: this page scrolls the WINDOW, so one offset
  // keyed on the full URL is the whole story.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [params] = useSearchParams();

  const home = useRailsHome(repo);
  const orphans = useRailsOrphans(repo, home.data?.detected !== false);
  const orphanLanes = useMemo(() => orphanIndex(orphans.data), [orphans.data]);
  const heads = useMemo(() => sectionHeads(home.data), [home.data]);

  // Which sections are expanded. ONE by default — the `?noun=` section, else
  // the first the passport counted — and each closed section still shows its
  // TRUE total, because the passport already carries every count.
  //
  // That default is a COST decision, not a display preference: `rails/1` is
  // computed per request and each noun list rebuilds the WHOLE join
  // (`rails::build_index` — there is no `rails_entity` table, deliberately).
  // Opening all eight on load would be ten full index builds per page view
  // on a repo whose measured cost has its own budget test. A section fetches
  // when it is opened and not before (`RailsSection`'s `enabled`).
  const focusNoun = params.get("noun");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  function isOpen(noun: string): boolean {
    const explicit = collapsed[noun];
    if (explicit !== undefined) return !explicit;
    return noun === (focusNoun ?? heads[0]?.noun);
  }

  // Section jumping (`rails.section-next`/`prev`). The refs are DOM nodes,
  // the cursor is an index into `heads` — no second copy of the section list.
  const sectionEls = useRef<Record<string, HTMLElement | null>>({});
  const cursor = useRef(0);
  function jumpSection(delta: number) {
    if (heads.length === 0) return;
    const next = (cursor.current + delta + heads.length) % heads.length;
    cursor.current = next;
    const head = heads[next];
    setCollapsed((c) => ({ ...c, [head.noun]: false }));
    const el = sectionEls.current[head.noun];
    el?.scrollIntoView({ block: "start" });
    el?.querySelector<HTMLElement>("[data-kbc-rails-section-toggle]")?.focus();
  }

  useCommandScope("board", { board: "rails" });
  useCommandHandlers({
    "rails.section-next": () => jumpSection(1),
    "rails.section-prev": () => jumpSection(-1),
  });

  const honesty = honestyLine(home.data?.honesty);

  // The page CHROME is the same in all three states — an error and a
  // not-a-Rails-repo answer are things this page says, not reasons for it to
  // become a different page (the landmark golden's own rule: a surface may
  // collapse a region, never move or rename one).
  const head = (
    <header className="kbc-rails__head">
      <h1 className="kbc-rails__title">Rails — {repo}</h1>
      <p className="kbc-rails__hint">
        rails/1 — a per-request join of the entity index, the rails-lens convention edges and the
        mirror index. Nothing here is stored, and no row is ever exact.
      </p>
    </header>
  );

  if (home.error) {
    return (
      <div className="kbc-rails" id="main" data-kbc-rails>
        {head}
        <p className="kbc-rails__error" data-kbc-rails-error>
          {(home.error as Error).message}
        </p>
      </div>
    );
  }

  if (home.data && !home.data.detected) {
    return (
      <div className="kbc-rails" id="main" data-kbc-rails data-kbc-rails-detected="false">
        {head}
        <EmptyState
          icon={<Icon.Layers />}
          title="Not a Rails application"
          hint={home.data.honesty.reason ?? "rails/1 found no Rails app in this repo."}
        />
      </div>
    );
  }

  return (
    <div className="kbc-rails" id="main" data-kbc-rails data-kbc-rails-detected={String(home.data?.detected ?? "")}>
      {head}

      {home.isLoading && <p className="kbc-rails__muted">Loading…</p>}

      {home.data && (
        <section className="kbc-rails__passport" data-kbc-rails-passport>
          <dl className="kbc-rails__passport-facts">
            {passportFacts(home.data).map((f) => (
              <div key={f.label} className="kbc-rails__passport-fact" data-kbc-rails-passport-fact={f.label}>
                <dt>{f.label}</dt>
                <dd title={f.note}>{f.value}</dd>
              </div>
            ))}
          </dl>
          <div className="kbc-rails__counts">
            {heads.map((h) => (
              <span className="kbc-rails__count" key={h.noun} data-kbc-rails-count={h.noun}>
                {h.title}
                <span className="kbc-rails__count-n">{h.total}</span>
              </span>
            ))}
          </div>
          {honesty && (
            <p className={`kbc-rails__honesty is-${honesty.state}`} role="status" data-kbc-rails-honesty={honesty.state}>
              {honesty.text}
            </p>
          )}
          {home.data.notes.map((note) => (
            <p className="kbc-rails__note" key={note} data-kbc-rails-note>
              {note}
            </p>
          ))}
        </section>
      )}

      {heads.map((h) => (
        <RailsSection
          key={h.noun}
          repo={repo}
          noun={h.noun}
          total={h.total}
          open={isOpen(h.noun)}
          onToggle={() => setCollapsed((c) => ({ ...c, [h.noun]: isOpen(h.noun) }))}
          orphanIndex={orphanLanes}
          sectionRef={(el) => {
            sectionEls.current[h.noun] = el;
          }}
        />
      ))}

      <RailsOrphans
        repo={repo}
        report={orphans.data}
        error={(orphans.error as Error) ?? null}
        loading={orphans.isLoading}
      />
    </div>
  );
}
