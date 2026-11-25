// FUWEBLAUNCH — "New terminal" on /sessions: start a kb-term session of a
// chosen harness in a chosen directory, then watch it inline.
//
// Shown only when the caller is admitted to the terminal tier (the parent
// gates on `GET /api/identity` `terminal_admitted`). Operator intent: nothing
// is sent until the Launch click. `cwd` is validated client-side exactly as
// the server does and suggestions come only from data the page already has
// (never a directory listing route).

import { useEffect, useId, useRef, useState } from "react";
import { launchTerm } from "../api/terms";
import { toast } from "../lib/toast";
import {
  loadLastTermHarness,
  loadRecentTermCwds,
  saveLastTermHarness,
  saveRecentTermCwd,
} from "../api/prefs";
import {
  LAUNCH_HARNESSES,
  cwdSuggestions,
  launchBody,
  pickLaunchHarness,
  validateCwd,
} from "../lib/termLaunch";
import { LaunchedTerminal } from "./LaunchedTerminal";

type Phase =
  | { kind: "closed" }
  | { kind: "form" }
  | { kind: "launching" }
  | { kind: "open"; termId: string; harness: string };

export function NewTerminalPanel({
  admitted,
  sessionCwds = [],
  pollMs,
  deadlineMs,
  onLaunched,
}: {
  /// `terminal_admitted` from `GET /api/identity`.
  admitted: boolean;
  /// cwds of sessions the page already lists (suggestions only).
  sessionCwds?: readonly (string | null | undefined)[];
  /// Binding-poll timings (tests shorten them; defaults in `termLaunch.ts`).
  pollMs?: number;
  deadlineMs?: number;
  /// FUTERMLIST: when set, a 2xx hands the new terminal to the page (which opens
  /// it at `/term/<id>`) instead of mounting one inline.
  onLaunched?: (termId: string, harness: string) => void;
}) {
  const [phase, setPhase] = useState<Phase>({ kind: "closed" });
  const [harness, setHarness] = useState(() => pickLaunchHarness(loadLastTermHarness()));
  const [cwd, setCwd] = useState("");
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const listId = useId();
  // The in-flight launch request. Cancel and unmount abort it; a terminal the
  // runner already started stays recoverable from the Terminals list.
  const inflight = useRef<AbortController | null>(null);
  useEffect(() => () => inflight.current?.abort(), []);
  if (!admitted) return null;

  const cwdTrim = cwd.trim();
  const invalid = validateCwd(cwdTrim);
  const suggestions = cwdSuggestions(loadRecentTermCwds(), sessionCwds);

  const launch = () => {
    setTouched(true);
    if (invalid !== null) return;
    setError(null);
    setPhase({ kind: "launching" });
    inflight.current?.abort();
    const ctl = new AbortController();
    inflight.current = ctl;
    launchTerm(launchBody(harness, cwdTrim), ctl.signal).then(
      (r) => {
        if (ctl.signal.aborted) return;
        saveLastTermHarness(harness);
        saveRecentTermCwd(cwdTrim);
        if (onLaunched) {
          setPhase({ kind: "closed" });
          setCwd("");
          onLaunched(r.id, harness);
          return;
        }
        setPhase({ kind: "open", termId: r.id, harness });
      },
      (e: unknown) => {
        if (ctl.signal.aborted) return;
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
        toast.err(`terminal launch failed: ${msg}`);
        setPhase({ kind: "form" });
      },
    );
  };

  return (
    <div className="kb-ses__newterm" data-testid="new-terminal">
      <button
        type="button"
        className="kb-ses__resume-copy kb-ses__newterm-open"
        data-testid="new-terminal-open"
        aria-expanded={phase.kind !== "closed"}
        disabled={phase.kind === "launching" || phase.kind === "open"}
        onClick={() => setPhase((p) => (p.kind === "closed" ? { kind: "form" } : { kind: "closed" }))}
        title="Start a kb-term session of a harness in a directory on the daemon's host"
      >
        New terminal
      </button>
      {(phase.kind === "form" || phase.kind === "launching") && (
        <form
          className="kb-ses__newterm-form"
          data-testid="new-terminal-form"
          onSubmit={(e) => {
            e.preventDefault();
            launch();
          }}
        >
          <label className="kb-ses__newterm-field">
            <span>harness</span>
            <select
              data-testid="new-terminal-harness"
              value={harness}
              onChange={(e) => setHarness(e.target.value)}
            >
              {LAUNCH_HARNESSES.map((h) => (
                <option key={h.id} value={h.id}>
                  {h.label}
                  {h.experimental ? " (experimental)" : ""}
                </option>
              ))}
            </select>
          </label>
          <label className="kb-ses__newterm-field">
            <span>directory</span>
            <input
              type="text"
              inputMode="url"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              data-testid="new-terminal-cwd"
              placeholder="/absolute/path"
              list={listId}
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              aria-invalid={touched && invalid !== null}
            />
            <datalist id={listId}>
              {suggestions.map((s) => (
                <option key={s} value={s} />
              ))}
            </datalist>
          </label>
          <div className="kb-ses__handoff-actions">
            <button
              type="submit"
              className="kb-ses__newterm-btn kb-ses__newterm-btn--primary"
              data-testid="new-terminal-launch"
              disabled={phase.kind === "launching"}
            >
              {phase.kind === "launching" ? "launching…" : "Launch"}
            </button>
            <button
              type="button"
              className="kb-ses__newterm-btn"
              data-testid="new-terminal-cancel"
              onClick={() => {
                inflight.current?.abort();
                inflight.current = null;
                setPhase({ kind: "closed" });
              }}
            >
              Cancel
            </button>
          </div>
          {touched && invalid !== null && (
            <p className="kb-ses__ins-errors" role="alert" data-testid="new-terminal-invalid">
              {invalid}
            </p>
          )}
          {error !== null && (
            <p className="kb-ses__ins-errors" role="alert" data-testid="new-terminal-error">
              terminal launch failed: {error}
            </p>
          )}
        </form>
      )}
      {phase.kind === "open" && (
        <LaunchedTerminal
          termId={phase.termId}
          harness={phase.harness}
          pollMs={pollMs}
          deadlineMs={deadlineMs}
          onClose={() => setPhase({ kind: "closed" })}
        />
      )}
    </div>
  );
}
