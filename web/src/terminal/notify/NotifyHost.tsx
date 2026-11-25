// TERMNOTIFY - mounted once at the app root beside the toast surface. Joins the
// existing terminals + live-status queries (no new fetch, stream or query key),
// derives attention events with the pure core and raises toasts (and, when the
// operator opted in and the tab is hidden, a browser Notification).

import { useEffect, useRef } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useIdentity } from "../../hooks/useArtifactHost";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { useTerms } from "../useTerms";
import { toast } from "../../lib/toast";
import { joinHub, parseHubUrl } from "../hub/termHub";
import { deriveAttention, transitions, type AttnEvent, type AttnRow } from "./derive";
import { setMarks, __resetMarks, type TermMarks } from "./attention";
import { eventLabel, eventTag, eventText, eventUrl } from "./notifyText";
import { getNotifyPref } from "./notifyPref";

const NO_PREFS = { labels: {}, pins: new Set<string>() };

function hidden(): boolean {
  return typeof document !== "undefined" && document.hidden === true;
}

export default function NotifyHost(): null {
  const admitted = useIdentity()?.terminal_admitted === true;
  const { terms, isLoading, isError } = useTerms();
  const { rows: live, loading: liveLoading } = useLiveSessions();
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const prevRef = useRef<AttnRow[] | null>(null);
  const finishedRef = useRef<Set<string>>(new Set());
  const navRef = useRef(navigate);
  navRef.current = navigate;

  const ready = admitted && !isLoading && !isError && !liveLoading;
  const viewed = parseHubUrl(pathname, "").id ?? legacyId(pathname);

  useEffect(() => {
    if (!ready) {
      prevRef.current = null;
      finishedRef.current = new Set();
      __resetMarks();
      return;
    }
    const rows: AttnRow[] = joinHub(terms, live, NO_PREFS).map((h) => ({
      termId: h.term.id,
      sid: h.term.session_id ?? null,
      state: h.live?.state ?? h.term.state ?? null,
      blocked: h.term.blocked === true || h.live?.blocked === true,
      ended: h.term.pane_dead,
      title: h.title,
      project: h.project,
    }));
    const isHidden = hidden();
    const { events } = deriveAttention(prevRef.current, rows, isHidden ? null : viewed);
    // Marks: finished-unseen set by finished events (also those behind a summary),
    // cleared on viewing while visible or when the terminal ends/disappears.
    const fin = finishedRef.current;
    const ids = new Map(rows.map((r) => [r.termId, r]));
    for (const e of transitions(prevRef.current, rows, isHidden ? null : viewed)) {
      if (e.kind === "finished") fin.add(e.termId);
    }
    for (const id of [...fin]) {
      const r = ids.get(id);
      if (r === undefined || r.ended || (!isHidden && id === viewed)) fin.delete(id);
    }
    const next: Record<string, { asks: boolean; finishedUnseen: boolean }> = {};
    for (const r of rows) {
      const asks = r.blocked && r.sid !== null && !r.ended;
      const fu = fin.has(r.termId);
      if (asks || fu) next[r.termId] = { asks, finishedUnseen: fu };
    }
    setMarks(next as TermMarks);
    prevRef.current = rows;

    const wantOs = isHidden && getNotifyPref() && osGranted();
    for (const e of events) {
      const url = eventUrl(e);
      const text = eventText(e);
      toast.info(text, { label: eventLabel(e), onClick: () => navRef.current(url) });
      if (wantOs) osNotify(e, text, () => navRef.current(url));
    }
  }, [ready, terms, live, viewed]);

  return null;
}

function legacyId(pathname: string): string | null {
  const m = /^\/term\/([a-z0-9][a-z0-9-]{0,31})\/?$/.exec(pathname);
  return m ? m[1] : null;
}

function osGranted(): boolean {
  return typeof Notification !== "undefined" && Notification.permission === "granted";
}

function osNotify(e: AttnEvent, body: string, go: () => void): void {
  try {
    const n = new Notification("kb terminal", { body, tag: eventTag(e) });
    n.onclick = () => {
      try {
        window.focus();
      } catch {
        // ignore
      }
      go();
      n.close?.();
    };
  } catch {
    // a notification failure never breaks the toast path
  }
}
