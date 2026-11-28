// HUBSWITCH test kit: synthetic terminal rows joined and ordered the way the
// switcher does. Imported by tests only.
import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import { joinHub, orderHub, type HubRow } from "./termHub";

export function mkTerm(id: string, o: Partial<TermRow> = {}): TermRow {
  return {
    id,
    harness: "claude",
    cwd: `/work/${id}`,
    started_unix: 1000,
    resume_sid: null,
    pane_pid: 1,
    cols: 80,
    rows: 24,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: `s-${id}`,
    state: null,
    blocked: false,
    ...o,
  };
}

export function mkLive(sid: string, o: Partial<LiveStatusRow> = {}): LiveStatusRow {
  return {
    session_id: sid,
    harness: "claude",
    holder: "agent",
    state: "waiting",
    source: "hook",
    confidence: "exact",
    since_unix: 500,
    since_secs: 10,
    resume: "r",
    blocked: false,
    ...o,
  } as LiveStatusRow;
}

/** Joined + ordered rows, exactly the pipeline HubSwitcher runs. */
export function ordered(terms: TermRow[], live: LiveStatusRow[] = []): HubRow[] {
  return orderHub(joinHub(terms, live, { labels: {}, pins: new Set() })).rows;
}

/**
 * Fixture used across the switcher tests: p1 has A (blocked, since 100), B
 * (working, started 300); p2 has C (working, started 200); D is ended (p2);
 * E is blocked in p2 since 50 (the OLDEST block).
 */
export function fixtureTerms(): TermRow[] {
  return [
    mkTerm("a", { cwd: "/w/p1", blocked: true, started_unix: 100 }),
    mkTerm("b", { cwd: "/w/p1", state: "working", started_unix: 300 }),
    mkTerm("c", { cwd: "/w/p2", state: "working", started_unix: 200 }),
    mkTerm("d", { cwd: "/w/p2", pane_dead: true, started_unix: 400 }),
    mkTerm("e", { cwd: "/w/p2", blocked: true, started_unix: 50 }),
  ];
}
