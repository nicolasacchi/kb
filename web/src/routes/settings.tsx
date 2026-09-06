import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import Admin from "../components/settings/Admin";
import DaemonConfig from "../components/settings/DaemonConfig";
import Errors from "../components/settings/Errors";
import Excluded from "../components/settings/Excluded";
import Live from "../components/settings/Live";
import Overview from "../components/settings/Overview";
import Pipeline from "../components/settings/Pipeline";
import Preferences from "../components/settings/Preferences";
import Quarantine from "../components/settings/Quarantine";
import Shares from "../components/settings/Shares";
import Slo from "../components/settings/Slo";
import Tabs, { type TabSpec } from "../components/settings/Tabs";
import Traffic from "../components/settings/Traffic";
import Users from "../components/settings/Users";
import VitalSigns from "../components/VitalSigns";
import { useConfirm } from "../components/ConfirmProvider";
import {
  fetchIdentity,
  fetchZeroHitQueries,
  fetchZeroHitQueriesAll,
  type Identity,
} from "../api/client";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useActiveKb } from "../hooks/useActiveKb";
import { relTime } from "../lib/commentFmt";
import { censusRead, censusReset, CENSUS_LABELS, type Census } from "../lib/census";

function fmtUptime(startedAt: string): string {
  const t0 = Date.parse(startedAt);
  if (!Number.isFinite(t0)) return "?";
  let s = Math.max(0, Math.floor((Date.now() - t0) / 1000));
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  s -= h * 3600;
  const m = Math.floor(s / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${s}s`;
}

function shortSha(sha: string | undefined): string | null {
  if (!sha || sha === "unknown") return null;
  return sha.replace(/-dirty$/, "").slice(0, 7) + (sha.endsWith("-dirty") ? "-dirty" : "");
}

const CENSUS_ZERO_HIT_LIMIT = 100;

type ZeroHitRow = { kb: string; query: string; count: number; lastSeen: string };

// W1.pulse — "Feature census — local only". Standing rule 3's evidence
// surface: local-only usage counters from `lib/census.ts` (never leave
// this machine — the panel says so) plus the zero-hit query list (per-kb
// + fleet-wide), each query a one-click link back into search. Lives as
// its own tab so `/settings#census` (VitalSigns' deep link) resolves via
// Tabs' existing hash-selects-tab-by-id contract.
function CensusSection() {
  const activeKb = useActiveKb();
  const confirm = useConfirm();
  const [counts, setCounts] = useState<Census>(() => censusRead());

  const zeroHitKb = useQuery({
    queryKey: ["zeroHitQueries", activeKb, CENSUS_ZERO_HIT_LIMIT] as const,
    enabled: !!activeKb,
    queryFn: ({ signal }) =>
      fetchZeroHitQueries(activeKb as string, CENSUS_ZERO_HIT_LIMIT, signal),
    staleTime: 0,
  });
  const zeroHitAll = useQuery({
    queryKey: ["zeroHitQueries", "all", CENSUS_ZERO_HIT_LIMIT] as const,
    queryFn: ({ signal }) =>
      fetchZeroHitQueriesAll(CENSUS_ZERO_HIT_LIMIT, 1, signal),
    staleTime: 0,
  });

  const rows = useMemo<ZeroHitRow[]>(() => {
    // Dedupe by (kb, query) — the per-kb call and the fleet-wide fan-out
    // both cover `activeKb`; the fleet-wide row wins (same ring, same
    // shape) since it arrives second.
    const byKey = new Map<string, ZeroHitRow>();
    for (const g of zeroHitKb.data ?? []) {
      if (!activeKb) break;
      byKey.set(`${activeKb}::${g.query}`, {
        kb: activeKb,
        query: g.query,
        count: g.count,
        lastSeen: g.last_seen,
      });
    }
    for (const entry of zeroHitAll.data ?? []) {
      for (const g of entry.groups) {
        byKey.set(`${entry.kb}::${g.query}`, {
          kb: entry.kb,
          query: g.query,
          count: g.count,
          lastSeen: g.last_seen,
        });
      }
    }
    return Array.from(byKey.values()).sort((a, b) =>
      a.lastSeen < b.lastSeen ? 1 : a.lastSeen > b.lastSeen ? -1 : 0,
    );
  }, [zeroHitKb.data, zeroHitAll.data, activeKb]);

  const entries = Object.entries(counts);

  return (
    <>
      <section className="settings__section" id="census" aria-label="feature census">
        <h2 className="settings__h2">Feature census — local only</h2>
        <p className="settings__hint">
          Counts of how often you reach for a handful of in-progress
          features (atlas opens, saved/recalled cameras, resurface review
          sessions, zero-hit retries, lobby explores). Roadmap calls — e.g.
          promoting a surface to a home tab — get made from this evidence
          instead of appetite.
        </p>
        <p className="settings__hint">
          Stored in this browser's localStorage. It never leaves this
          machine — nothing here is sent anywhere, and this is the only
          place it's shown.
        </p>
        {entries.length === 0 ? (
          <p className="kb-census__empty">No feature usage recorded yet.</p>
        ) : (
          <ul className="kb-census__list">
            {entries.map(([key, n]) => (
              <li key={key} className="kb-census__row">
                <span className="kb-census__label">
                  {CENSUS_LABELS[key] ?? key}
                </span>
                <span className="kb-census__val">{n}</span>
              </li>
            ))}
          </ul>
        )}
        <button
          type="button"
          className="settings__btn settings__btn--sm settings__btn--danger"
          disabled={entries.length === 0}
          onClick={async () => {
            const ok = await confirm({
              title: "Reset feature census?",
              body: "Clears every local usage counter in this browser. This can't be undone.",
              confirmLabel: "Reset",
            });
            if (!ok) return;
            censusReset();
            setCounts(censusRead());
          }}
        >
          reset counters
        </button>
      </section>

      <section className="settings__section" aria-label="zero-hit queries">
        <h2 className="settings__h2">Zero-hit queries</h2>
        <p className="settings__hint">
          Queries that returned nothing, since this daemon started — the
          in-memory ring resets on restart, so this is a live corpus-gap
          signal, not a durable log.
        </p>
        {rows.length === 0 ? (
          <p className="kb-census__empty">No zero-hit queries recorded yet.</p>
        ) : (
          <ul className="kb-census__list">
            {rows.map((r) => (
              <li key={`${r.kb}::${r.query}`} className="kb-census__row">
                <Link
                  to={`/search?kb=${encodeURIComponent(r.kb)}&q=${encodeURIComponent(r.query)}`}
                  className="kb-census__query"
                  title={`retry “${r.query}” in ${r.kb}`}
                >
                  {r.query}
                </Link>
                <span className="kb-census__kb">{r.kb}</span>
                <span className="kb-census__count">{r.count}×</span>
                <span className="kb-census__age">{relTime(r.lastSeen)}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}

// Operator dashboard. Header strip surfaces daemon identity (name +
// version + build + uptime); the tab strip below holds eight panels
// covering observability (Overview/Pipeline/Traffic/Errors/Live),
// daemon actions (Shares/Admin) and browser-local prefs (Preferences).
// Phases S2-S5 fill the staged panels — S1 ships the shell + Preferences.
export default function Settings() {
  useDocumentTitle("Settings");
  const activeKb = useActiveKb();
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [identityError, setIdentityError] = useState<string | null>(null);
  // Re-render once a minute so the uptime badge stays current without
  // hammering setInterval at sub-second granularity.
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 60_000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    fetchIdentity(ctrl.signal)
      .then((id) => {
        if (!cancelled) setIdentity(id);
      })
      .catch((e) => {
        if (!cancelled && e?.name !== "AbortError") {
          setIdentityError(String(e?.message ?? e));
        }
      });
    return () => {
      cancelled = true;
      ctrl.abort();
    };
  }, []);

  const tabs: TabSpec[] = useMemo(
    () => [
      {
        id: "overview",
        label: "Overview",
        body: () => <Overview />,
      },
      {
        id: "pipeline",
        label: "Pipeline",
        body: () => <Pipeline />,
      },
      {
        id: "traffic",
        label: "Traffic",
        body: () => <Traffic />,
      },
      {
        id: "errors",
        label: "Errors",
        body: () => <Errors />,
      },
      {
        id: "quarantine",
        label: "Quarantine",
        body: () => <Quarantine />,
      },
      {
        id: "excluded",
        label: "Excluded",
        body: () => <Excluded />,
      },
      {
        id: "shares",
        label: "Shares",
        body: () => <Shares />,
      },
      {
        id: "live",
        label: "Live",
        body: () => <Live />,
      },
      // CT-F5 — corpus-health SLOs. An observability panel, so it sits with
      // the other read-only ones (Overview…Live) and BEFORE the daemon-action
      // tabs; `/settings#slo` resolves via Tabs' hash-selects-tab-by-id.
      {
        id: "slo",
        label: "SLOs",
        body: () => <Slo />,
      },
      {
        id: "admin",
        label: "Admin",
        body: () => <Admin />,
      },
      {
        id: "config",
        label: "Config",
        body: () => <DaemonConfig />,
      },
      {
        id: "preferences",
        label: "Preferences",
        body: () => <Preferences />,
      },
      {
        id: "users",
        label: "Users",
        body: () => <Users />,
      },
      {
        id: "census",
        label: "Census",
        body: () => <CensusSection />,
      },
    ],
    [],
  );

  return (
    <div className="settings">
      <header className="settings__header" aria-label="daemon identity">
        <div className="settings__header-main">
          <h1 className="settings__title">{identity?.name ?? "daemon"}</h1>
          {identity?.host && identity.host !== identity.name && (
            <span className="settings__badge settings__badge--host" title="hostname">
              {identity.host}
            </span>
          )}
        </div>
        <div className="settings__badges">
          {identity?.version && (
            <span className="settings__badge" title="kb version">
              v{identity.version}
            </span>
          )}
          {shortSha(identity?.build_sha) && (
            <span className="settings__badge" title="build commit">
              {shortSha(identity?.build_sha)}
            </span>
          )}
          {identity?.started_at && (
            <span className="settings__badge" title={`started ${identity.started_at}`}>
              up {fmtUptime(identity.started_at)}
            </span>
          )}
          {identity?.kbs && identity.kbs.length > 0 && (
            <span className="settings__badge" title="kbs on this daemon">
              {identity.kbs.length} kb{identity.kbs.length === 1 ? "" : "s"}
            </span>
          )}
        </div>
        {identityError && (
          <div className="settings__error" role="alert">
            identity unreachable: {identityError}
          </div>
        )}
      </header>

      <VitalSigns kb={activeKb} />

      <Tabs tabs={tabs} label="settings sections" />
    </div>
  );
}
