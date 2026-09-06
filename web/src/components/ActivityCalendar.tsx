import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { fetchCalendar } from "../api/client";
import {
  buildCalendarGrid,
  cellTooltip,
  dayBoundsUnix,
  unixToUtcDay,
  type CalendarCell,
} from "../lib/calendar";
import { galleryUrl } from "../lib/galleryUrl";
import { censusBump } from "../lib/census";
import { Icon } from "./icons";

// W2.10 — the corpus activity calendar: a GitHub-graph-shaped year density
// grid, mounted as a collapsible header of the history view. Calm-computing
// contract (the kb-resurface ruling, same posture as GalleryLobby/EchoStrip):
// density only — no streak counts, no "longest run", no totals framed as an
// achievement. The one line of prose here is a plain count.
//
// UTC note: the server aggregates `history` rows via sqlite's UTC-only
// `unixepoch` modifier (mirrors the `echoes` route's convention — no
// per-user timezone concept), so this grid's day boundaries are UTC, not
// the viewer's local calendar day. `lib/calendar.ts` is UTC-explicit
// end-to-end; the one place that could silently reintroduce a local-day
// assumption is this component's own date math, which is why it never
// calls `new Date(...)`/`Date.now()`-derived local getters directly — only
// `unixToUtcDay`/`dayBoundsUnix`. The note below says so out loud.

const WINDOW_DAYS = 365;
const CELL_PX = 11;
const COLLAPSE_KEY = "kb:activity-calendar:collapsed";

type Props = { kb: string };

export default function ActivityCalendar({ kb }: Props) {
  const [collapsed, setCollapsed] = useState(
    () => sessionStorage.getItem(COLLAPSE_KEY) === "1",
  );

  // The trailing-365-day window is pinned at mount, not recomputed every
  // render — a moving `to` boundary would otherwise change the ["calendar",
  // kb] queryFn's closure (and thus its result) on every render tick.
  const [fromUnix, toUnix] = useMemo(() => {
    const to = Math.floor(Date.now() / 1000);
    return [to - WINDOW_DAYS * 86400, to];
  }, []);

  const calQ = useQuery({
    queryKey: ["calendar", kb],
    queryFn: ({ signal }) => fetchCalendar(kb, { from: fromUnix, to: toUnix }, signal),
    staleTime: Infinity,
  });

  const grid = useMemo(
    () => buildCalendarGrid(unixToUtcDay(fromUnix), unixToUtcDay(toUnix), calQ.data?.days ?? []),
    [calQ.data, fromUnix, toUnix],
  );

  const toggle = () => {
    setCollapsed((c) => {
      const next = !c;
      try {
        sessionStorage.setItem(COLLAPSE_KEY, next ? "1" : "0");
      } catch {
        /* private mode — collapse state just won't persist */
      }
      return next;
    });
  };

  const colStyle = { gridTemplateColumns: `repeat(${grid.weeks}, ${CELL_PX}px)` };

  return (
    <section
      className={`activity-calendar${collapsed ? " activity-calendar--collapsed" : ""}`}
      aria-label="activity calendar"
    >
      <button
        type="button"
        className="activity-calendar__head"
        onClick={toggle}
        aria-expanded={!collapsed}
        title={collapsed ? "expand the activity calendar" : "collapse the activity calendar"}
      >
        <Icon.Chevron
          className={`activity-calendar__chev${collapsed ? "" : " activity-calendar__chev--open"}`}
        />
        <span className="activity-calendar__title">activity</span>
        <span className="activity-calendar__count mono">
          {grid.totalEvents.toLocaleString()} event{grid.totalEvents === 1 ? "" : "s"} this year
        </span>
      </button>

      {!collapsed && grid.cells.length > 0 && (
        <div className="activity-calendar__body">
          <div className="activity-calendar__months" style={colStyle} aria-hidden="true">
            {grid.monthLabels.map((m) => (
              <span
                key={`${m.week}-${m.label}`}
                className="activity-calendar__month"
                style={{ gridColumn: m.week + 1 }}
              >
                {m.label}
              </span>
            ))}
          </div>
          <div
            className="activity-calendar__cells"
            style={colStyle}
            role="group"
            aria-label={`${grid.totalEvents} events in the last year, by UTC day`}
          >
            {grid.cells.map((c) => (
              <DayCell key={c.day} cell={c} kb={kb} />
            ))}
          </div>
          <p className="activity-calendar__note">days are labeled in UTC, not your local timezone</p>
        </div>
      )}
    </section>
  );
}

function DayCell({ cell, kb }: { cell: CalendarCell; kb: string }) {
  const { from, to } = dayBoundsUnix(cell.day);
  const tip = cellTooltip(cell);
  return (
    <Link
      to={galleryUrl(kb, { from, to })}
      className={`activity-calendar__cell activity-calendar__cell--level-${cell.level}`}
      style={{ gridColumn: cell.week + 1, gridRow: cell.weekday + 1 }}
      title={tip}
      aria-label={tip}
      onClick={() => censusBump("calendar.day")}
    />
  );
}
