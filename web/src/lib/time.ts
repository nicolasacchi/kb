// Shared time formatter for history events. M-spa: pre-fix
// FloatingPill's smart formatter (`HH:MM` / `yest.` / `MM-DD`) and
// HistoryTimeline's `hhmm` (always `HH:MM`) diverged for older
// events. Both views render the same data; same event should
// produce the same label.
//
// Default: smart (today → `HH:MM`, yesterday → `yest.`, older →
// `MM-DD`). HistoryTimeline groups by day before rendering, so it
// passes `mode: "hhmm-only"` to keep the per-row label compact —
// the day stripe header handles the date.

export type HistoryTimeMode = "smart" | "hhmm-only";

export function formatHistoryTime(
  unixSeconds: number,
  mode: HistoryTimeMode = "smart",
): string {
  const d = new Date(unixSeconds * 1000);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  if (mode === "hhmm-only") {
    return `${hh}:${mm}`;
  }
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) {
    return `${hh}:${mm}`;
  }
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (
    d.getFullYear() === y.getFullYear() &&
    d.getMonth() === y.getMonth() &&
    d.getDate() === y.getDate()
  ) {
    return "yest.";
  }
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

// v0.14 T3 — day-bucket helpers, lifted out of HistoryTimeline so the
// /sessions view (and any future timeline-shaped view) can share the
// exact same "Today / Yesterday / Month dd, yyyy" grammar without
// re-implementing the date math.

/// YYYY-MM-DD in the user's local timezone, suitable as a group key
/// for unix-seconded events. Day boundaries follow tz so the
/// `dayHeading` "Today" / "Yesterday" labels feel right.
export function dayBucket(unix: number): string {
  const d = new Date(unix * 1000);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${dd}`;
}

/// Human-readable label for a `dayBucket` value: "Today" / "Yesterday"
/// when the bucket matches the current local date, else a long-form
/// "Month dd, yyyy" via Intl.DateTimeFormat (locale-aware).
export function dayHeading(bucket: string): string {
  const today = dayBucket(Math.floor(Date.now() / 1000));
  const yesterday = dayBucket(Math.floor(Date.now() / 1000) - 86400);
  if (bucket === today) return "Today";
  if (bucket === yesterday) return "Yesterday";
  const d = new Date(bucket + "T00:00:00");
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

/// Compact wall-clock duration from a millisecond span — shared by gallery
/// session cards and the `/sessions` list (`45s` / `5m` / `1h 3m` / `2h`).
/// Sub-second spans and zero collapse to `""` so callers can omit the chip.
export function humanizeDuration(ms: number): string {
  if (!ms || ms < 1000) return "";
  const totalSecs = Math.round(ms / 1000);
  if (totalSecs < 60) return `${totalSecs}s`;
  const mins = Math.floor(totalSecs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  const remMins = mins % 60;
  return remMins ? `${hours}h ${remMins}m` : `${hours}h`;
}

/// Compact "how long ago" for a unix-seconds timestamp. Single home for
/// gallery cards, session chips, inbox, biography, anchors — thresholds
/// match the session-card surface (most-seen): sub-minute → `now`, then
/// `Nm` / `Nh` / `Nd` (to 14d) / `Nw` (to 60d) / `Nmo` / `Ny`.
/// `nowMs` is injectable for deterministic tests (milliseconds).
export function relativeAge(
  unixSeconds: number | null | undefined,
  nowMs: number = Date.now(),
): string {
  if (unixSeconds == null) return "";
  const diff = Math.max(0, Math.floor(nowMs / 1000) - unixSeconds);
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 86400 * 14) return `${Math.floor(diff / 86400)}d`;
  if (diff < 86400 * 60) return `${Math.floor(diff / (86400 * 7))}w`;
  if (diff < 86400 * 365) return `${Math.floor(diff / (86400 * 30))}mo`;
  return `${Math.floor(diff / (86400 * 365))}y`;
}

// Short absolute stamp for dense lists (folder sidebar rows): same calendar
// year → "Jul 30"; older years → "2025-07-30". English month abbrev is
// intentional (matches the task's compact form; not locale-switched).
const SHORT_MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
] as const;

export function compactDate(
  unixSeconds: number,
  nowMs: number = Date.now(),
): string {
  const d = new Date(unixSeconds * 1000);
  const now = new Date(nowMs);
  if (d.getFullYear() === now.getFullYear()) {
    return `${SHORT_MONTHS[d.getMonth()]} ${d.getDate()}`;
  }
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${dd}`;
}
