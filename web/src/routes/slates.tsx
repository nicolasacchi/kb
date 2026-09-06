import { Link } from "react-router-dom";
import { useSlates } from "../hooks/useSlates";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { attentionCount, orderSlates } from "../lib/slateLanes";
import { glyphForKind } from "../lib/slateGlyphs";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { relativeAge } from "../lib/time";

// SL4 — the slate list (the `/inbox` route's shape: one query, one ordered
// list of cards, deep-links into the thing itself).
//
// A slate is per-project coordination, so this page ranks by ATTENTION —
// unacknowledged hands, open asks, contested takes, stale takes — and then
// by recency. That sum is `lib/slateLanes.attentionCount`, the SAME function
// the nav chip uses, computed client-side off the daemon's per-section
// counts. The daemon reports counts; it never reports a ranking, and this
// page must not start asking it to.

const SECTION_CHIPS = [
  { key: "hand_unack", kind: "hand", label: "unacked hands" },
  { key: "ask_open", kind: "ask", label: "open asks" },
  { key: "take_contested", kind: "take", label: "contested takes" },
  { key: "take_stale", kind: "take", label: "stale takes" },
  { key: "take_live", kind: "take", label: "live takes" },
  { key: "now", kind: "now", label: "now lines" },
  { key: "warn", kind: "warn", label: "warnings" },
  { key: "found", kind: "found", label: "found" },
  { key: "idea", kind: "idea", label: "ideas" },
  { key: "tried", kind: "tried", label: "tried" },
] as const;

export default function SlatesRoute() {
  useDocumentTitle("Slates");
  const { slates, loading, error } = useSlates();
  const rows = orderSlates(slates);

  return (
    <div className="slates-view">
      <header className="slates-view__head">
        <h1 className="slates-view__title">Slates</h1>
        <p className="slates-view__sub">
          One board per project — what is in flight, who holds what, what has
          already been tried. The same ledger every agent reads as a digest.
        </p>
      </header>

      {error && (
        <div className="slates-view__error" role="alert">
          Couldn’t load the slates: {error}
        </div>
      )}

      {!error && !loading && rows.length === 0 && (
        <EmptyState
          icon={<Icon.Tasks />}
          title="No slates yet"
          hint="A slate is created by the first post to it — from an agent, or from this page once one exists."
          cli="kb slate post found &quot;…&quot;"
        />
      )}

      {rows.length > 0 && (
        <ul className="slates-list">
          {rows.map((s) => {
            const attention = attentionCount(s.counts);
            // D27 — how many sessions have reported a cursor here. Hidden at
            // zero (the attention chip's own reflex) and absent entirely on a
            // pre-v0.42 daemon, which is why it is read defensively rather
            // than rendered as a confident 0. It is NOT a rank term:
            // `orderSlates` never sees it.
            const served = s.sessions_served ?? 0;
            return (
              <li key={s.slug} className="slates-row">
                <Link
                  className="slates-row__link"
                  to={`/slates/${encodeURIComponent(s.slug)}`}
                  data-testid={`slate-row-${s.slug}`}
                >
                  <span className="slates-row__slug">{s.slug}</span>
                  {attention > 0 && (
                    <span
                      className="slates-row__attention"
                      title="unacknowledged hands + open asks + contested takes + stale takes"
                    >
                      {attention}
                    </span>
                  )}
                  {s.closed && <span className="slates-row__closed">closed</span>}
                  {served > 0 && (
                    <span
                      className="slates-row__served"
                      title={`${served} session${served === 1 ? " has" : "s have"} been served this slate — a reported cursor (D27), never a read receipt`}
                    >
                      served {served}
                    </span>
                  )}
                  <span className="slates-row__seq">#{s.head_seq}</span>
                  <span className="slates-row__age">
                    {relativeAge(s.updated_unix)}
                  </span>
                </Link>
                <div className="slates-row__counts">
                  {SECTION_CHIPS.filter((c) => (s.counts?.[c.key] ?? 0) > 0).map(
                    (c) => {
                      const g = glyphForKind(c.kind);
                      return (
                        <span
                          key={c.key}
                          className="slates-row__chip"
                          title={c.label}
                          style={{ ["--slate-kind" as string]: `var(${g.token})` }}
                        >
                          <span aria-hidden="true">{g.glyph}</span>
                          <span className="slates-row__chip-n">
                            {s.counts[c.key]}
                          </span>
                          <span className="slates-row__chip-w">{c.label}</span>
                        </span>
                      );
                    },
                  )}
                </div>
                {s.topics.length > 0 && (
                  <div className="slates-row__topics">
                    {s.topics.map((t) => (
                      <Link
                        key={t}
                        className="slate-topic"
                        to={`/slates/${encodeURIComponent(s.slug)}?topic=${encodeURIComponent(t)}`}
                      >
                        {t}
                      </Link>
                    ))}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
