import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueries } from "@tanstack/react-query";
import {
  fetchDocsPage,
  fetchEchoes,
  fetchResurface,
  type DocSummary,
  type EchoOut,
  type KbSummary,
} from "../api/client";
import { fetchInbox, type InboxResponse } from "../api/inbox";
import { useDesk } from "../hooks/useDesk";
import { useSlateBoard, useSlates } from "../hooks/useSlates";
import { artifactHref } from "../lib/artifactHref";
import { censusBump } from "../lib/census";
import { relativeAge } from "../lib/time";
import { composeDraftExclusion } from "../lib/draft";
import { mastheadVolume } from "../lib/broadsheet";
import { sumWords } from "../lib/lobby";
import {
  collapsedKey,
  composeFrontPage,
  countNewSince,
  echoAge,
  echoTarget,
  editionLine,
  fleetKbs,
  FLEET_KB_CAP,
  mastheadDate,
  parseVisit,
  pickIndexHero,
  rotateVisit,
  rowKey,
  visitKey,
  type FrontRow,
} from "../lib/frontpage";
import { ResurfaceReview } from "./ResurfaceReview";
import { Icon } from "@kb/ui/icons";

// OL L3 — the Front Page: ONE band at the top of the unfiltered Library that
// replaces the lobby, sessions, resurface, echo and pinned-index strips. A
// masthead (name, date, edition line), a lead with the reason it surfaces as
// its dateline, a few features, "on this day" briefs, and a quiet continuing-
// coverage column of what is open. Calm computing: every number is either
// server-authoritative or omitted (lib/frontpage.ts `editionLine`), nothing
// is scored in the UI beyond the deterministic ranking, and the band
// collapses to its masthead line (remembered per kb).
//
// Server state rides the existing TanStack keys, so the SSE bridge's prefix
// invalidations (["resurface"], ["inbox"], ["docs", kb], ["slates"], …) cover
// it with no bridge change (#23). No hook here fetches outside useQuery.

const FLEET_DOCS_LIMIT = 60;
const COMMENTS_SHOWN = 3;
const DESK_SHOWN = 3;
const ECHOES_SHOWN = 4;

type Props = {
  /// Explicit kb (`?kb=` / path); null = fleet-wide ("All corpora").
  kb: string | null;
  kbs: KbSummary[];
  /// The Library's loaded rows for `kb` (ignored fleet-wide).
  rows: DocSummary[];
  /// Server-authoritative artifact total for `kb` (envelope total).
  total: number;
  hasMore: boolean;
  loading: boolean;
  sessions?: { sessions: number; folders: number } | null;
};

function localIsoDate(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function readStore(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function writeStore(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable — the band just forgets */
  }
}

/// Once per mount: rotate the per-scope visit record (see `rotateVisit`).
function useSince(scope: string | null): number | null {
  const [since] = useState<number | null>(() => {
    const key = visitKey(scope);
    let seen = false;
    try {
      seen = sessionStorage.getItem(key) === "1";
      sessionStorage.setItem(key, "1");
    } catch {
      /* no sessionStorage: treat every mount as the first of its session */
    }
    const out = rotateVisit(
      parseVisit(readStore(key)),
      seen,
      Math.floor(Date.now() / 1000),
    );
    if (!seen) writeStore(key, JSON.stringify(out.state));
    return out.since;
  });
  return since;
}

export default function FrontPage({ kb, kbs, rows, total, hasMore, loading, sessions }: Props) {
  const scope = kb;
  const since = useSince(scope);
  const [nowMs] = useState(() => Date.now());
  const [collapsed, setCollapsed] = useState(() => readStore(collapsedKey(scope)) === "1");
  const [reviewOpen, setReviewOpen] = useState(false);

  useEffect(() => {
    censusBump("frontpage.view");
  }, []);

  const scopeKbs = useMemo(() => (kb ? [kb] : fleetKbs(kbs)), [kb, kbs]);
  const allShelfKbs = useMemo(() => kbs.filter((k) => !k.memory_scope).length, [kbs]);

  // Fleet: each kb's newest page, through the SAME ["docs", kb] prefix the
  // bridge invalidates. Explicit kb reuses the Library's own rows.
  const fleetDocs = useQueries({
    queries: (kb ? [] : scopeKbs).map((k) => ({
      queryKey: ["docs", k, "frontpage"] as const,
      queryFn: () =>
        fetchDocsPage(k, {
          offset: 0,
          limit: FLEET_DOCS_LIMIT,
          sort: "recent",
          q: composeDraftExclusion(""),
        }),
      staleTime: Infinity,
    })),
  });
  const resurfaceQs = useQueries({
    queries: scopeKbs.map((k) => ({
      queryKey: ["resurface", k] as const,
      queryFn: ({ signal }: { signal: AbortSignal }) => fetchResurface(k, signal),
      staleTime: Infinity,
    })),
  });
  const echoQs = useQueries({
    queries: scopeKbs.map((k) => ({
      queryKey: ["echoes", k] as const,
      queryFn: ({ signal }: { signal: AbortSignal }) => fetchEchoes(k, signal),
      staleTime: Infinity,
    })),
  });
  const inboxQ = useQuery({
    queryKey: kb ? (["inbox", kb] as const) : (["inbox"] as const),
    queryFn: ({ signal }) => fetchInbox(kb ? { kb } : {}, signal),
    staleTime: Infinity,
  });
  const desk = useDesk(kb ?? undefined);
  const { slates } = useSlates();

  // Cheap (<= 8 pages of <= 60 rows) and derived from query results whose
  // array lengths vary with the kb list, so these are plain per-render
  // derivations rather than memos with variable-length dependency lists.
  const groups = kb
    ? [{ kb, rows, hasMore }]
    : scopeKbs.flatMap((k, i) => {
        const pg = fleetDocs[i]?.data;
        return pg ? [{ kb: k, rows: pg.docs, hasMore: pg.has_more }] : [];
      });
  const frontRows: FrontRow[] = groups.flatMap((g) =>
    g.rows.map((doc) => ({ kb: g.kb, doc })),
  );
  const openMass = useMemo(() => {
    const m = new Map<string, number>();
    for (const it of inboxQ.data?.items ?? []) {
      const k = rowKey(it.kb, it.artifact_id);
      m.set(k, (m.get(k) ?? 0) + 1);
    }
    return m;
  }, [inboxQ.data]);
  const resurface = scopeKbs.flatMap((k, i) =>
    (resurfaceQs[i]?.data?.items ?? []).map((item) => ({ kb: k, item })),
  );
  const page = composeFrontPage({ rows: frontRows, openMass, resurface, since, nowMs });

  const echoes: { kb: string; item: EchoOut }[] = scopeKbs
    .flatMap((k, i) =>
      (echoQs[i]?.data?.items ?? []).slice(0, 2).map((item) => ({ kb: k, item })),
    )
    .slice(0, ECHOES_SHOWN);

  const newSince = countNewSince(groups, since);
  const edition = editionLine({
    total: kb
      ? total
      : kbs.filter((k) => scopeKbs.includes(k.name)).reduce((a, k) => a + k.doc_count, 0),
    corpora: kb ? undefined : scopeKbs.length,
    corporaCapped: !kb && allShelfKbs > FLEET_KB_CAP,
    words: kb && !hasMore ? sumWords(rows) : null,
    newSince,
  });

  const indexHero = kb ? pickIndexHero(rows) : null;
  const name = kb ?? "All corpora";
  const multi = !kb;
  const { lead, features } = page;
  const hasEchoes = echoes.length > 0;

  const toggle = () => {
    const next = !collapsed;
    setCollapsed(next);
    writeStore(collapsedKey(scope), next ? "1" : "0");
    if (next) censusBump("frontpage.collapse");
  };

  const singleResurface = kb ? resurfaceQs[0]?.data : undefined;

  return (
    <section
      className={`kb-fp${collapsed ? " kb-fp--collapsed" : ""}`}
      aria-label="Front page"
      data-testid="frontpage"
    >
      <header className="kb-fp__mast">
        <div className="kb-fp__nameplate">
          <h2 className="kb-fp__title">{name}</h2>
          <div className="kb-fp__dateline">
            <time className="kb-fp__date" dateTime={localIsoDate(nowMs)}>
              {mastheadDate(nowMs)}
            </time>
            <span className="kb-fp__vol mono">{mastheadVolume(nowMs)}</span>
            <span className="kb-fp__edition" data-testid="frontpage-edition">
              {loading && frontRows.length === 0 ? "gathering today's edition" : edition}
            </span>
          </div>
        </div>
        <div className="kb-fp__tools">
          {indexHero && (
            <Link
              to={artifactHref(kb as string, indexHero.source_relative)}
              className="kb-fp__tool"
              title={indexHero.title || "index"}
            >
              Index
            </Link>
          )}
          <button
            type="button"
            className="kb-fp__tool"
            onClick={toggle}
            aria-expanded={!collapsed}
            data-testid="frontpage-collapse"
            title={collapsed ? "Show today's edition" : "Collapse to the masthead"}
          >
            <Icon.Chevron
              className={`kb-fp__chev${collapsed ? "" : " kb-fp__chev--open"}`}
            />
            {collapsed ? "Expand" : "Collapse"}
          </button>
        </div>
      </header>

      {!collapsed && (
        <div className="kb-fp__page">
          <div className="kb-fp__main">
            {lead && (
              <article className="kb-fp__lead" data-testid="frontpage-lead">
                <Link
                  to={artifactHref(lead.kb, lead.source_relative)}
                  className="kb-fp__lead-link"
                  onClick={() => censusBump("frontpage.lead.open")}
                >
                  <div className="kb-fp__kicker">{lead.dateline}</div>
                  <h3 className="kb-fp__lead-title">{lead.title || "(untitled)"}</h3>
                  {lead.row?.summary && <p className="kb-fp__lead-dek">{lead.row.summary}</p>}
                </Link>
                <div className="kb-fp__meta mono">
                  {multi && <span>{lead.kb}</span>}
                  {lead.row?.mtime_unix != null && (
                    <span>{relativeAge(lead.row.mtime_unix, nowMs)}</span>
                  )}
                  {(lead.row?.word_count ?? 0) > 0 && (
                    <span>{(lead.row?.word_count ?? 0).toLocaleString()}w</span>
                  )}
                </div>
              </article>
            )}

            {features.length > 0 && (
              <section className="kb-fp__features" aria-label="features">
                {page.featuresKind === "new" && (
                  <h3 className="kb-fp__section-h">New since your last visit</h3>
                )}
                <div className="kb-fp__feature-grid">
                  {features.map(({ kb: fkb, doc }) => (
                    <Link
                      key={rowKey(fkb, doc.id)}
                      to={artifactHref(fkb, doc.source_relative)}
                      className="kb-fp__feature"
                    >
                      {(doc.folder || multi) && (
                        <div className="kb-fp__feature-kicker">
                          {[multi ? fkb : "", doc.folder].filter(Boolean).join(" · ")}
                        </div>
                      )}
                      <h4 className="kb-fp__feature-title">{doc.title || "(untitled)"}</h4>
                      {doc.summary && <p className="kb-fp__feature-dek">{doc.summary}</p>}
                      <div className="kb-fp__meta mono">
                        <span>{relativeAge(doc.mtime_unix, nowMs)}</span>
                      </div>
                    </Link>
                  ))}
                </div>
              </section>
            )}

            {hasEchoes && (
              <section className="kb-fp__briefs" aria-label="on this day">
                <h3 className="kb-fp__section-h">On this day</h3>
                <ul className="kb-fp__brief-list">
                  {echoes.map(({ kb: ekb, item }, i) => {
                    const to = echoTarget(ekb, item, artifactHref);
                    const label = item.title || item.detail;
                    return (
                      <li key={`${ekb}:${item.kind}:${item.artifact_id ?? item.session_id ?? i}`}>
                        <span className="kb-fp__brief-age mono">{echoAge(item.months_ago)}</span>
                        {to ? <Link to={to}>{label}</Link> : <span>{label}</span>}
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}
          </div>

          <CoverageRail
            kb={kb}
            comments={inboxQ.data}
            desk={desk.items}
            slates={slates}
            sessions={sessions ?? null}
            reviewCount={singleResurface?.items.length ?? 0}
            onReview={() => setReviewOpen(true)}
          />
        </div>
      )}

      {reviewOpen && kb && singleResurface && (
        <ResurfaceReview
          kb={kb}
          items={singleResurface.items}
          weights={singleResurface.weights}
          onClose={() => setReviewOpen(false)}
        />
      )}
    </section>
  );
}

type CoverageProps = {
  kb: string | null;
  comments: InboxResponse | undefined;
  desk: ReturnType<typeof useDesk>["items"];
  slates: ReturnType<typeof useSlates>["slates"];
  sessions: { sessions: number; folders: number } | null;
  reviewCount: number;
  onReview: () => void;
};

function CoverageRail({ kb, comments, desk, slates, sessions, reviewCount, onReview }: CoverageProps) {
  const open = comments?.items ?? [];
  const topComments = [...open]
    .sort((a, b) => b.updated_at - a.updated_at)
    .slice(0, COMMENTS_SHOWN);
  const shown = new Set(topComments.map((c) => rowKey(c.kb, c.artifact_id)));
  const deskRows = desk
    .filter((d) => !shown.has(rowKey(d.kb, d.id)))
    .slice(0, DESK_SHOWN);
  const slateRows = slates
    .filter((s) => !s.closed && s.counts.now > 0 && (kb == null || s.slug === kb))
    .slice(0, 2);
  const totalOpen = comments?.total_open ?? 0;
  const empty =
    totalOpen === 0 &&
    deskRows.length === 0 &&
    slateRows.length === 0 &&
    !sessions &&
    reviewCount === 0;
  if (empty) return null;

  return (
    <aside className="kb-fp__coverage" aria-label="continuing coverage">
      <h3 className="kb-fp__section-h">Continuing coverage</h3>

      {totalOpen > 0 && (
        <div className="kb-fp__cov-block">
          <div className="kb-fp__cov-h">
            {totalOpen.toLocaleString()} open comment{totalOpen === 1 ? "" : "s"}
          </div>
          <ul className="kb-fp__cov-list">
            {topComments.map((c) => (
              <li key={c.comment_id}>
                {c.source_relative ? (
                  <Link to={artifactHref(c.kb, c.source_relative, { panel: "comments" })}>
                    {c.title || "(untitled)"}
                  </Link>
                ) : (
                  <span>{c.title || "(untitled)"}</span>
                )}
                <span className="kb-fp__cov-meta">{c.excerpt}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {deskRows.length > 0 && (
        <div className="kb-fp__cov-block">
          <div className="kb-fp__cov-h">On your desk</div>
          <ul className="kb-fp__cov-list">
            {deskRows.map((d) => (
              <li key={rowKey(d.kb, d.id)}>
                <Link to={artifactHref(d.kb, d.source_relative)}>{d.title || "(untitled)"}</Link>
                <span className="kb-fp__cov-meta">
                  {d.changed_since_read ? "changed since you read it" : `updated ${relativeAge(d.updated_unix)} ago`}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {slateRows.map((s) => (
        <SlateNow key={s.slug} slug={s.slug} />
      ))}

      {(sessions || reviewCount > 0) && (
        <ul className="kb-fp__cov-list kb-fp__cov-list--links">
          {reviewCount > 0 && kb && (
            <li>
              <button type="button" className="kb-fp__link" onClick={onReview} data-kb-act="resurface-review">
                Review the reading queue ({reviewCount})
              </button>
            </li>
          )}
          {sessions && (
            <li>
              <Link to={`/sessions${kb ? `?kb=${encodeURIComponent(kb)}` : ""}`}>
                Session worklog · {sessions.sessions.toLocaleString()} session
                {sessions.sessions === 1 ? "" : "s"}
              </Link>
            </li>
          )}
        </ul>
      )}
    </aside>
  );
}

function SlateNow({ slug }: { slug: string }) {
  const { board } = useSlateBoard(slug);
  const lines = (board?.sections.now ?? []).slice(0, 2);
  if (lines.length === 0) return null;
  return (
    <div className="kb-fp__cov-block">
      <div className="kb-fp__cov-h">
        <Link to={`/slates/${encodeURIComponent(slug)}`}>Now on {slug}</Link>
      </div>
      <ul className="kb-fp__cov-list">
        {lines.map((c) => (
          <li key={c.id}>
            <span>{c.line}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
