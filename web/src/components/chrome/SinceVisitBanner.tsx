// OL IDEA2 — "Changed since your last visit" strip above the artifact
// (parent chrome, never inside the iframe). Replaces the handoff-only
// DeskChangedBanner for every artifact. The data is the daemon's
// `GET …/since-visit` answer (web visits only; an agent `kb cat` is not a
// visit — #19): the SPA derives nothing, it renders the state it is given.
//
// Dismiss is per-artifact per-tab via sessionStorage; a newer
// `working_ts_unix` is a different key and shows the banner again.

import { useCallback, useState } from "react";
import { Icon } from "@kb/ui/icons";
import { useSinceVisit } from "../../hooks/useSinceVisit";
import type { ChangedSection, SinceVisitResponse } from "../../api/versions";
import { compactDate, formatHistoryTime } from "../../lib/time";

const MAX_CHIPS = 3;

export function sinceVisitDismissKey(
  kb: string,
  id: string,
  workingTsUnix: number,
): string {
  return `kb:since-visit-dismissed:${kb}:${id}:${workingTsUnix}`;
}

export function sinceVisitVisible(
  data: SinceVisitResponse | null | undefined,
  dismissed: boolean,
  asOf: boolean,
): boolean {
  return (
    !!data &&
    (data.state === "changed" || data.state === "changed_no_delta") &&
    !dismissed &&
    !asOf
  );
}

function readDismissed(key: string): boolean {
  try {
    return sessionStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeDismissed(key: string): void {
  try {
    sessionStorage.setItem(key, "1");
  } catch {
    // Private mode / quota — treat as dismissed for this render only.
  }
}

function fullLocal(unix: number): string {
  return new Date(unix * 1000).toLocaleString();
}

export type SinceVisitBannerViewProps = {
  kb: string;
  id: string;
  data: SinceVisitResponse | null | undefined;
  /// True when the reader is showing a historical `?at=` render.
  asOf?: boolean;
  /// Scroll the artifact to a section id. Absent (native-note path) ⇒ chips
  /// render as plain text.
  onJumpSection?: (id: string) => void;
  /// Open the versions dock with `from` preselected (`null` ⇒ its default).
  onViewChanges?: (ref: string | null) => void;
};

function SectionChip({
  s,
  onJumpSection,
}: {
  s: ChangedSection;
  onJumpSection?: (id: string) => void;
}) {
  const label = s.title ?? "Opening";
  if (s.change === "removed") {
    return (
      <s className="kb-since-visit__removed" title="section removed">
        {label}
      </s>
    );
  }
  if (!s.id || !onJumpSection) {
    return <span className="kb-since-visit__plain">{label}</span>;
  }
  const sid = s.id;
  return (
    <button
      type="button"
      className="kb-since-visit__chip"
      title={`${s.change}: +${s.added} −${s.removed} lines`}
      onClick={() => onJumpSection(sid)}
    >
      {label}
    </button>
  );
}

export function SinceVisitBannerView({
  kb,
  id,
  data,
  asOf = false,
  onJumpSection,
  onViewChanges,
}: SinceVisitBannerViewProps) {
  const key = data?.working_ts_unix
    ? sinceVisitDismissKey(kb, id, data.working_ts_unix)
    : "";
  // Local latch so a click re-renders without waiting for a storage event;
  // compared against `key` so a newer working_ts_unix re-shows.
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  const dismissed = key !== "" && (dismissedKey === key || readDismissed(key));
  const onDismiss = useCallback(() => {
    if (!key) return;
    writeDismissed(key);
    setDismissedKey(key);
  }, [key]);

  if (!sinceVisitVisible(data, dismissed, asOf) || !data) return null;

  const at = data.last_visit_unix;
  const headline =
    at === undefined
      ? "Changed since your last visit"
      : data.baseline === "current_visit"
        ? `Changed since you opened it at ${formatHistoryTime(at, "hhmm-only")}`
        : `Changed since your last visit, ${compactDate(at)}`;

  const sections = data.sections ?? [];
  const shown = sections.slice(0, MAX_CHIPS);
  const more = sections.length - shown.length;
  const ref = data.memento?.version?.ref ?? null;

  return (
    <div
      className="kb-since-visit"
      role="status"
      data-testid="since-visit-banner"
    >
      <span
        className="kb-since-visit__msg"
        title={at === undefined ? undefined : fullLocal(at)}
      >
        {headline}
      </span>
      {data.state === "changed" && (
        <>
          <span className="kb-since-visit__count">
            · {sections.length} section{sections.length === 1 ? "" : "s"}:
          </span>
          {shown.map((s, i) => (
            <SectionChip
              key={`${s.id ?? "opening"}-${s.change}-${i}`}
              s={s}
              onJumpSection={onJumpSection}
            />
          ))}
          {more > 0 && <span className="kb-since-visit__more">+{more} more</span>}
          {onViewChanges && ref && (
            <button
              type="button"
              className="kb-since-visit__act"
              onClick={() => onViewChanges(ref)}
            >
              · View changes
            </button>
          )}
        </>
      )}
      {data.state === "changed_no_delta" && data.reason === "no_version" && (
        <>
          <span className="kb-since-visit__count">
            · the version you saw is no longer kept
            {data.memento?.oldest_ts_unix !== undefined &&
              ` (oldest kept ${compactDate(data.memento.oldest_ts_unix)})`}
          </span>
          {onViewChanges && (
            <button
              type="button"
              className="kb-since-visit__act"
              onClick={() => onViewChanges(null)}
            >
              History
            </button>
          )}
        </>
      )}
      {data.state === "changed_no_delta" && data.reason === "mode_off" && (
        <span className="kb-since-visit__count">
          · this kb keeps no version history
        </span>
      )}
      <button
        type="button"
        className="kb-since-visit__x"
        aria-label="dismiss"
        title="dismiss"
        onClick={onDismiss}
      >
        <Icon.X aria-hidden />
      </button>
    </div>
  );
}

/// Connected banner: one since-visit fetch per mount (`useSinceVisit`).
export default function SinceVisitBanner(
  props: Omit<SinceVisitBannerViewProps, "data">,
) {
  const data = useSinceVisit(props.kb, props.id, !props.asOf);
  return <SinceVisitBannerView {...props} data={data} />;
}
