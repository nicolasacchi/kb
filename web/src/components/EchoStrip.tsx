import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { Icon } from "./icons";
import { fetchEchoes } from "../api/client";
import type { BeliefOut, EchoOut } from "../api/client";
import { artifactHref } from "../lib/artifactHref";

/// CT-E6 — the beliefs lane renders as a stacked list beneath the main
/// row, so it stays capped even on a day that lands on many memories'
/// anniversary at once (the SERVER lane itself is uncapped — "surfaced,
/// never scored" — this is purely a calm-UI limit).
const MAX_BELIEFS = 4;

// Pull-only "on this day" strip — a calm SECOND strip beside ResurfaceStrip
// (W2.2). Joins created/read/worked-on dates against 1-3yr + a 6mo
// half-anniversary, entirely server-computed and deterministic. Anti-feature
// contract, same as resurface: renders NOTHING when there's nothing to show
// (no celebration, no "you're on a streak"), carries no counts into
// persistent chrome, and the only dismiss is per-tab (sessionStorage) — the
// server never stores strip state. Own query key (["echoes", kb], see
// queryClient.ts) — zero coupling to the resurface wire.
const DISMISS_KEY = "kb:echoes:hidden";

/// Short distance chip: "1y"/"2y"/"3y" for whole years, "6mo" for the
/// half-anniversary.
function ageChip(monthsAgo: number): string {
  return monthsAgo % 12 === 0 ? `${monthsAgo / 12}y` : `${monthsAgo}mo`;
}

/// Artifact echoes (`created`/`read`) deep-link via the shared permalink
/// builder; session echoes (`worked`) deep-link into the sessions worklog,
/// focused on that capture (`?focus=` is already consumed there).
function echoHref(kb: string, item: EchoOut): string | null {
  if (item.kind === "worked") {
    return item.session_id
      ? `/sessions?kb=${encodeURIComponent(kb)}&focus=${encodeURIComponent(item.session_id)}`
      : null;
  }
  return item.source_relative ? artifactHref(kb, item.source_relative) : null;
}

/// Full sentence prefix — distinct from `ageChip`'s short badge, since the
/// beliefs lane reads as a sentence ("one year ago you learned…"), not an
/// inline chip row.
function agePhrase(monthsAgo: number): string {
  if (monthsAgo === 6) return "6 months ago";
  if (monthsAgo === 12) return "one year ago";
  if (monthsAgo % 12 === 0) return `${monthsAgo / 12} years ago`;
  return `${monthsAgo} months ago`;
}

function formatDate(unix: number | null | undefined): string {
  if (unix == null) return "?";
  return new Date(unix * 1000).toISOString().slice(0, 10);
}

/// "still active" / "superseded <date>" / "forgotten" — the same honesty
/// the server's `BeliefStatus` carries, never silently dropped the way a
/// low-salience/superseded hit is dropped from recall.
function beliefStatusPhrase(b: BeliefOut): string {
  switch (b.status) {
    case "active":
      return "still active";
    case "forgotten":
      return "forgotten";
    case "superseded":
      return b.superseded_at != null
        ? `superseded ${formatDate(b.superseded_at)}`
        : "superseded";
    default:
      return b.status;
  }
}

export function EchoStrip({ kb }: { kb: string | null }) {
  const [hidden, setHidden] = useState(
    () => sessionStorage.getItem(DISMISS_KEY) === "1",
  );
  const enabled = !hidden && !!kb;
  const q = useQuery({
    queryKey: ["echoes", kb],
    enabled,
    queryFn: ({ signal }) => fetchEchoes(kb!, signal),
    staleTime: Infinity,
  });
  const items = (q.data?.items ?? []).slice(0, 2);
  // CT-E6 — the beliefs lane. Additive: independent of `items`, so a kb
  // with no anniversary artifact/session hits today but a memory born on
  // this date still shows something.
  const beliefs = (q.data?.beliefs ?? []).slice(0, MAX_BELIEFS);
  const beliefsCaveat = q.data?.beliefs_tombstone_caveat;
  if (hidden || !kb || (items.length === 0 && beliefs.length === 0)) return null;
  return (
    <div className="gallery-echo-strip" role="region" aria-label="On this day">
      <span className="gallery-echo-strip__lead">◷ On this day</span>
      {items.map((it, i) => {
        const href = echoHref(kb, it);
        const key = `${it.kind}-${it.artifact_id ?? it.session_id ?? i}`;
        const body = (
          <>
            <span className="gallery-echo-strip__title">{it.title}</span>
            <span className="gallery-echo-strip__why">{it.detail}</span>
          </>
        );
        return (
          <div key={key} className="gallery-echo-strip__row">
            {href ? (
              <Link
                to={href}
                className="gallery-echo-strip__item"
                title={it.detail}
              >
                {body}
              </Link>
            ) : (
              <span className="gallery-echo-strip__item gallery-echo-strip__item--static">
                {body}
              </span>
            )}
            <span className="gallery-echo-strip__chip">
              {ageChip(it.months_ago)}
            </span>
          </div>
        );
      })}
      {beliefs.length > 0 && (
        <div
          className="gallery-echo-strip__beliefs"
          role="region"
          aria-label="On this day — memories"
        >
          {beliefs.map((b) => (
            <Link
              key={b.id}
              to={`/memory?kb=${encodeURIComponent(b.kb)}`}
              className="gallery-echo-strip__belief"
              title={`${agePhrase(b.months_ago)} you learned "${b.title}" — ${beliefStatusPhrase(b)}`}
            >
              {agePhrase(b.months_ago)} you learned{" "}
              <strong>{b.title}</strong> — {beliefStatusPhrase(b)}
            </Link>
          ))}
          {beliefsCaveat && (
            <p className="gallery-echo-strip__belief-caveat">{beliefsCaveat}</p>
          )}
        </div>
      )}
      <button
        type="button"
        className="gallery-echo-strip__hide"
        data-kb-act="echoes-hide"
        aria-label="Hide until next session"
        title="Hide until next session"
        onClick={() => {
          sessionStorage.setItem(DISMISS_KEY, "1");
          setHidden(true);
        }}
      >
        <Icon.X />
      </button>
    </div>
  );
}
