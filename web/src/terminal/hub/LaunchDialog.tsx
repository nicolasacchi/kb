// HUBLAUNCH — the kb-term launcher: a modal on desktop, a Sheet on phone.
//
// Four parts: a cwd combobox fed only by data the SPA already holds, harness
// chips, a Resume tab over the existing kb sessions list, and a MANDATORY
// confirm view that shows the exact cwd + harness (+ the session being
// resumed) before the single `POST /api/terms`. Nothing is sent before the
// Launch click: not on open, not on a prefill load, not on a tab change, and
// Enter in the cwd field only moves to the confirm view.
//
// `prefill` is exactly `{ resumeSid }`. Harness and cwd are then derived from
// the FETCHED session row, never from a caller-supplied string. The confirm is
// a second view of this same dialog (no `window.confirm`, no second
// ConfirmModal), so the single confirm host rule (#32) stays intact. Server
// state is TanStack Query with finite staleTime and no bridge entry (#23).

import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Icon } from "@kb/ui/icons";
import { Sheet } from "@kb/ui/Sheet";
import { fetchSession, fetchSessionsPage } from "../../api/sessions";
import { launchTerm } from "../../api/terms";
import {
  loadLastTermHarness,
  loadRecentTermCwds,
  saveLastTermHarness,
  saveRecentTermCwd,
} from "../../api/prefs";
import { usePhonePanel } from "../../hooks/useIsMobile";
import { toast } from "../../lib/toast";
import {
  LAUNCH_HARNESSES,
  cwdSuggestions,
  launchBody,
  pickLaunchHarness,
  validateCwd,
} from "../../lib/termLaunch";
import { useTerms } from "../useTerms";
import {
  groupCwdsByProject,
  isPlainResumeToken,
  resumableRows,
  resumeRefusal,
} from "./launchResume";
import "./LaunchDialog.css";

export type LaunchDialogProps = {
  open: boolean;
  onClose(): void;
  onLaunched(id: string, harness: string): void;
  /// `terminal_admitted` from `GET /api/identity`; false renders nothing.
  admitted: boolean;
  /// The ONLY prefill: a session id. Harness and cwd come from its row.
  prefill?: { resumeSid: string };
};

type Target = { harness: string; cwd: string; sid?: string; name?: string };

const harnessLabel = (id: string) => LAUNCH_HARNESSES.find((h) => h.id === id)?.label ?? id;

function fmtDate(unix: number): string {
  if (!unix) return "";
  return new Date(unix * 1000).toISOString().slice(0, 10);
}

export function LaunchDialog(props: LaunchDialogProps) {
  if (!props.admitted || !props.open) return null;
  return <LaunchBody {...props} />;
}

function LaunchBody({ onClose, onLaunched, prefill }: LaunchDialogProps) {
  const phone = usePhonePanel();
  const [tab, setTab] = useState<"new" | "resume">("new");
  const [view, setView] = useState<"form" | "confirm">("form");
  const [harness, setHarness] = useState(() => pickLaunchHarness(loadLastTermHarness()));
  const [cwd, setCwd] = useState("");
  const [touched, setTouched] = useState(false);
  const [picked, setPicked] = useState<Target | null>(null);
  const [launching, setLaunching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [q, setQ] = useState("");
  const [detent, setDetent] = useState<"peek" | "full">("full");
  const listId = useId();
  const inflight = useRef<AbortController | null>(null);
  const busy = useRef(false);
  const dlg = useRef<HTMLDialogElement | null>(null);

  useEffect(() => () => inflight.current?.abort(), []);
  useEffect(() => {
    const t = setTimeout(() => setQ(search.trim()), 300);
    return () => clearTimeout(t);
  }, [search]);

  // Suggestions: only data the SPA already holds (sessions list, live terms,
  // the local recent list). There is no directory-listing route.
  const sessionsQ = useQuery({
    queryKey: ["term-launch-sessions"] as const,
    queryFn: ({ signal }) => fetchSessionsPage({ limit: 50 }, signal),
    staleTime: 60_000,
  });
  const { terms } = useTerms();
  const groups = useMemo(() => groupCwdsByProject(sessionsQ.data?.sessions ?? []), [sessionsQ.data]);
  const suggestions = useMemo(
    () =>
      cwdSuggestions(loadRecentTermCwds(), [...groups.map((g) => g.cwd), ...terms.map((t) => t.cwd)]),
    // loadRecentTermCwds is read once per mount of the body on purpose.
    [groups, terms],
  );
  const groupOf = useMemo(() => new Map(groups.map((g) => [g.cwd, g.group])), [groups]);

  const resumeQ = useQuery({
    queryKey: ["term-launch-resume", q] as const,
    queryFn: ({ signal }) => fetchSessionsPage({ q: q || undefined, limit: 20 }, signal),
    staleTime: 30_000,
    enabled: !prefill && tab === "resume",
  });
  const resumeRows = useMemo(() => resumableRows(resumeQ.data?.sessions ?? []), [resumeQ.data]);

  const prefillSid = prefill?.resumeSid;
  const prefillQ = useQuery({
    queryKey: ["term-launch-prefill", prefillSid] as const,
    queryFn: ({ signal }) => fetchSession(prefillSid as string, signal),
    staleTime: 30_000,
    enabled: prefillSid !== undefined,
    retry: false,
  });

  // The prefill outcome, derived from the fetched row only.
  let prefillTarget: Target | null = null;
  let prefillError: string | null = null;
  let prefillLoading = false;
  if (prefillSid !== undefined) {
    if (prefillQ.isPending) prefillLoading = true;
    else if (prefillQ.isError) prefillError = "Could not load that session.";
    else if (!prefillQ.data || prefillQ.data.session_id !== prefillSid) {
      prefillError = "Unknown session: nothing to resume.";
    } else {
      const refusal = resumeRefusal(prefillQ.data);
      if (refusal) prefillError = `Cannot resume: ${refusal}.`;
      else {
        prefillTarget = {
          harness: prefillQ.data.harness,
          cwd: prefillQ.data.cwd as string,
          sid: prefillQ.data.session_id,
          name: prefillQ.data.display_name,
        };
      }
    }
  }

  const cwdTrim = cwd.trim();
  const newInvalid = validateCwd(cwdTrim);
  let target: Target | null;
  if (prefillSid !== undefined) target = prefillTarget;
  else if (tab === "resume") target = picked;
  else target = newInvalid === null ? { harness, cwd: cwdTrim } : null;

  const abort = () => {
    inflight.current?.abort();
    inflight.current = null;
    busy.current = false;
    setLaunching(false);
  };
  const close = () => {
    abort();
    onClose();
  };

  // Desktop: native <dialog> (focus trap, ::backdrop, Esc), like ConfirmModal.
  useEffect(() => {
    if (phone) return;
    const opener = document.activeElement as HTMLElement | null;
    const el = dlg.current;
    if (el && !el.open) {
      if (typeof el.showModal === "function") el.showModal();
      else el.setAttribute("open", "");
    }
    const onCancel = (e: Event) => {
      e.preventDefault();
      close();
    };
    el?.addEventListener("cancel", onCancel);
    return () => {
      el?.removeEventListener("cancel", onCancel);
      opener?.focus?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phone]);

  const review = () => {
    setTouched(true);
    if (!target) return;
    setError(null);
    setView("confirm");
  };

  const back = () => {
    abort();
    setError(null);
    setView("form");
  };

  const launch = () => {
    if (busy.current) return;
    const t = target;
    if (!t || validateCwd(t.cwd) !== null || (t.sid !== undefined && !isPlainResumeToken(t.sid))) {
      setView("form");
      return;
    }
    busy.current = true;
    setError(null);
    setLaunching(true);
    inflight.current?.abort();
    const ctl = new AbortController();
    inflight.current = ctl;
    launchTerm(launchBody(t.harness, t.cwd, t.sid), ctl.signal).then(
      (r) => {
        if (ctl.signal.aborted) return;
        busy.current = false;
        saveLastTermHarness(t.harness);
        saveRecentTermCwd(t.cwd);
        onLaunched(r.id, t.harness);
        onClose();
      },
      (e: unknown) => {
        if (ctl.signal.aborted) return;
        busy.current = false;
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
        toast.err(`terminal launch failed: ${msg}`);
        setLaunching(false);
      },
    );
  };

  const switchTab = (next: "new" | "resume") => {
    setTab(next);
    setPicked(null);
    setError(null);
  };

  const pickRow = (r: (typeof resumeRows)[number]) => {
    setPicked({ harness: r.harness, cwd: r.cwd as string, sid: r.session_id, name: r.display_name });
    setError(null);
    setView("confirm");
  };

  const title = view === "confirm" ? "Confirm launch" : "Start a terminal";

  const confirmView = target && (
    <div className="kb-launch__confirm" data-testid="launch-confirm-view">
      <p className="kb-launch__lead">This starts a process on the daemon&apos;s host. Check it:</p>
      <dl className="kb-launch__facts">
        <div>
          <dt>Directory</dt>
          <dd data-testid="launch-confirm-cwd">
            <span className="kb-launch__sr">Directory: </span>
            <code>{target.cwd}</code>
          </dd>
        </div>
        <div>
          <dt>Harness</dt>
          <dd data-testid="launch-confirm-harness">
            <span className="kb-launch__sr">Harness: </span>
            <code>{harnessLabel(target.harness)}</code>
          </dd>
        </div>
        {target.sid !== undefined && (
          <div>
            <dt>Resuming session</dt>
            <dd data-testid="launch-confirm-session">
              <span className="kb-launch__sr">Resuming session: </span>
              <code>
                {target.name} ({target.sid})
              </code>
            </dd>
          </div>
        )}
      </dl>
      {error && (
        <p role="alert" className="kb-launch__err" data-testid="launch-error">
          {error}
        </p>
      )}
      <div className="kb-launch__actions">
        <button type="button" className="kb-launch__btn" data-testid="launch-back" onClick={back}>
          Back
        </button>
        <button
          type="button"
          className="kb-launch__btn kb-launch__btn--go"
          data-testid="launch-confirm"
          disabled={launching}
          onClick={launch}
        >
          {launching ? "Launching…" : "Launch"}
        </button>
      </div>
    </div>
  );

  const prefillView = (
    <div className="kb-launch__prefill" data-testid="launch-prefill">
      {prefillLoading && (
        <div role="status" className="kb-launch__skel" data-testid="launch-prefill-loading">
          Loading session…
        </div>
      )}
      {prefillError && (
        <p role="alert" className="kb-launch__err" data-testid="launch-prefill-error">
          {prefillError}
        </p>
      )}
      {prefillTarget && (
        <dl className="kb-launch__facts">
          <div>
            <dt>Resuming</dt>
            <dd>{prefillTarget.name}</dd>
          </div>
          <div>
            <dt>Harness</dt>
            <dd>{harnessLabel(prefillTarget.harness)}</dd>
          </div>
          <div>
            <dt>Directory</dt>
            <dd>
              <code>{prefillTarget.cwd}</code>
            </dd>
          </div>
        </dl>
      )}
      <div className="kb-launch__actions">
        <button type="button" className="kb-launch__btn" data-testid="launch-cancel" onClick={close}>
          Cancel
        </button>
        <button
          type="button"
          className="kb-launch__btn kb-launch__btn--go"
          data-testid="launch-review"
          disabled={!prefillTarget}
          onClick={review}
        >
          Review launch
        </button>
      </div>
    </div>
  );

  const newTab = (
    <form
      className="kb-launch__form"
      data-testid="launch-new-form"
      role="tabpanel"
      aria-label="New terminal"
      onSubmit={(e) => {
        e.preventDefault();
        review();
      }}
    >
      <label className="kb-launch__field">
        <span>Directory</span>
        <input
          type="text"
          inputMode="url"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          data-testid="launch-cwd"
          placeholder="/absolute/path"
          list={listId}
          value={cwd}
          onChange={(e) => setCwd(e.target.value)}
          aria-invalid={touched && newInvalid !== null}
        />
        <datalist id={listId}>
          {suggestions.map((c) => (
            <option key={c} value={c} label={groupOf.get(c)} />
          ))}
        </datalist>
      </label>
      {touched && newInvalid && (
        <p role="alert" className="kb-launch__err" data-testid="launch-cwd-error">
          {newInvalid}
        </p>
      )}
      <div className="kb-launch__field">
        <span id={`${listId}-h`}>Harness</span>
        <div role="radiogroup" aria-labelledby={`${listId}-h`} className="kb-launch__chips">
          {LAUNCH_HARNESSES.map((h) => (
            <button
              key={h.id}
              type="button"
              role="radio"
              aria-checked={harness === h.id}
              className="kb-launch__chip"
              data-testid={`launch-harness-${h.id}`}
              onClick={() => setHarness(h.id)}
            >
              {h.label}
              {h.experimental && <span className="kb-launch__exp"> (experimental)</span>}
            </button>
          ))}
        </div>
      </div>
      <div className="kb-launch__actions">
        <button type="button" className="kb-launch__btn" data-testid="launch-cancel" onClick={close}>
          Cancel
        </button>
        <button type="submit" className="kb-launch__btn kb-launch__btn--go" data-testid="launch-review">
          Review launch
        </button>
      </div>
    </form>
  );

  const resumeTab = (
    <div className="kb-launch__resume" role="tabpanel" aria-label="Resume a session" data-testid="launch-resume-panel">
      <input
        type="search"
        className="kb-launch__search"
        data-testid="launch-resume-search"
        aria-label="Search sessions"
        placeholder="Search past sessions"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />
      {resumeQ.isPending && <p role="status">Loading sessions…</p>}
      {resumeQ.isError && (
        <p role="alert" className="kb-launch__err">
          Could not search sessions.
        </p>
      )}
      {resumeQ.data && resumeRows.length === 0 && <p className="kb-launch__empty">No resumable sessions.</p>}
      <ul className="kb-launch__rows">
        {resumeRows.map((r) => (
          <li key={r.session_id}>
            <button
              type="button"
              className="kb-launch__row"
              data-testid="launch-resume-row"
              data-sid={r.session_id}
              onClick={() => pickRow(r)}
            >
              <span className="kb-launch__row-title">{r.display_name}</span>
              <span className="kb-launch__row-meta">
                <Icon.Terminal /> {harnessLabel(r.harness)} · {fmtDate(r.started_at)}
              </span>
              <code className="kb-launch__row-cwd">{r.cwd}</code>
            </button>
          </li>
        ))}
      </ul>
      <div className="kb-launch__actions">
        <button type="button" className="kb-launch__btn" data-testid="launch-cancel" onClick={close}>
          Cancel
        </button>
      </div>
    </div>
  );

  const body = (
    <div className="kb-launch" data-testid="launch-dialog">
      {view === "confirm" && confirmView ? (
        confirmView
      ) : prefillSid !== undefined ? (
        prefillView
      ) : (
        <>
          <div role="tablist" aria-label="Launch mode" className="kb-launch__tabs">
            {(["new", "resume"] as const).map((t) => (
              <button
                key={t}
                type="button"
                role="tab"
                aria-selected={tab === t}
                className="kb-launch__tab"
                data-testid={`launch-tab-${t}`}
                onClick={() => switchTab(t)}
              >
                {t === "new" ? "New" : "Resume"}
              </button>
            ))}
          </div>
          {tab === "new" ? newTab : resumeTab}
        </>
      )}
    </div>
  );

  if (phone) {
    return (
      <Sheet
        open
        detent={detent}
        onDetentChange={(d) => setDetent(d === "peek" ? "peek" : "full")}
        onClose={close}
        title={title}
        id="kb-launch-sheet"
      >
        {body}
      </Sheet>
    );
  }
  return (
    <dialog ref={dlg} className="kb-launch__modal" aria-labelledby={`${listId}-t`} data-testid="launch-modal">
      <div className="kb-launch__head">
        <h2 id={`${listId}-t`} className="kb-launch__title">
          {title}
        </h2>
        <button type="button" className="kb-launch__x" aria-label="Close" data-testid="launch-close" onClick={close}>
          <Icon.X />
        </button>
      </div>
      {body}
    </dialog>
  );
}
