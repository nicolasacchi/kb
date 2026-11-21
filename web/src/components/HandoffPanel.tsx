// Cross-harness session handoff (omp ⇄ Claude Code) — the web surface of
// `kb sessions handoff`. The operator asks explicitly: NOTHING is fetched
// until a target button is clicked (injection on request, never pushed).
//
// The brief is deterministic, LLM-free text a NEW session of the target
// harness receives as its first prompt. The `command` is server-computed
// and copy-only (the SPA never builds a shell command — same contract as
// `ResumeCommand`); it runs `kb sessions handoff` on the machine that has
// the session, because the daemon cannot launch a harness on the operator's
// terminal.
//
// "Open in terminal" (only when the card's terminal lane is admitted and the
// session cwd is known) launches the TRANSPLANT in a kb terminal: the pane
// runs `kb sessions handoff <sid> --to <target> --transplant --launch` on
// the daemon's host, which writes a native copy of this session for the
// target and resumes it there. The SPA sends only the closed harness name,
// the cwd and the source sid (`launchTerm`); the argv is built by the kb-term
// runner. Nothing is launched until the click.

import { Suspense, lazy, useEffect, useRef, useState } from "react";
import { StateCard } from "@kb/ui/StateCard";
import { fetchSessionHandoff } from "../api/sessions";
import { launchTerm } from "../api/terms";
import type { HandoffOut } from "../api/generated/HandoffOut";

// Same lazy seam as `TerminalSlot`: the panel chunk (xterm) loads only after
// a successful launch.
const TerminalPanel = lazy(() => import("../terminal/TerminalPanel.lazy"));

export type HandoffTarget = { to: "claude" | "omp"; label: string };

/// The handoff targets for a session of `harness`: the OTHER harness of the
/// pair. Every other harness (codex, gemini, …) has no handoff → `[]`.
export function handoffTargets(harness: string): HandoffTarget[] {
  if (harness === "claude") return [{ to: "omp", label: "Continue in omp" }];
  if (harness === "omp") return [{ to: "claude", label: "Continue in Claude Code" }];
  return [];
}

type State =
  | { kind: "idle" }
  | { kind: "loading"; to: "claude" | "omp" }
  | { kind: "error"; to: "claude" | "omp"; reason: string }
  | { kind: "ready"; to: "claude" | "omp"; out: HandoffOut };

const HELP =
  "Start a NEW session of the other harness from this one. The brief is " +
  "deterministic text (no LLM) that the new session receives as its first " +
  "prompt. The command runs `kb sessions handoff` on the machine that has " +
  "the session; add --transplant to resume a native copy instead.";

const TERM_HELP = (target: string) =>
  "Runs `kb sessions handoff --transplant --launch` in a kb terminal on the " +
  `daemon's host: it writes a native copy of this session for ${target} and ` +
  "resumes it there.";

type TermState =
  | { kind: "idle" }
  | { kind: "launching"; to: "claude" | "omp" }
  | { kind: "error"; to: "claude" | "omp"; reason: string }
  | { kind: "open"; to: "claude" | "omp"; termId: string };

function CopyButton({ text, label, testId }: { text: string; label: string; testId: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  };
  return (
    <button type="button" className="kb-ses__resume-copy" onClick={copy} data-testid={testId}>
      {copied ? "copied" : label}
    </button>
  );
}

export function HandoffPanel({
  sessionId,
  harness,
  terminalAdmitted,
  cwd,
}: {
  sessionId: string;
  harness: string;
  /// The card's terminal lane is admitted (and the terminal tier is on).
  terminalAdmitted?: boolean;
  /// The session's working directory; the transplant resumes there.
  cwd?: string | null;
}) {
  const targets = handoffTargets(harness);
  const [state, setState] = useState<State>({ kind: "idle" });
  const [term, setTerm] = useState<TermState>({ kind: "idle" });
  const ctl = useRef<AbortController | null>(null);
  const termCtl = useRef<AbortController | null>(null);
  useEffect(
    () => () => {
      ctl.current?.abort();
      termCtl.current?.abort();
    },
    [],
  );

  if (targets.length === 0) return null;
  const canLaunch = Boolean(terminalAdmitted && cwd);

  const openTerm = (to: "claude" | "omp") => {
    if (!cwd) return;
    termCtl.current?.abort();
    const c = new AbortController();
    termCtl.current = c;
    setTerm({ kind: "launching", to });
    launchTerm({ harness: to, cwd, handoff_from: sessionId }, c.signal).then(
      (r) => {
        if (!c.signal.aborted) setTerm({ kind: "open", to, termId: r.id });
      },
      (e: unknown) => {
        if (c.signal.aborted) return;
        setTerm({ kind: "error", to, reason: e instanceof Error ? e.message : String(e) });
      },
    );
  };

  const run = (to: "claude" | "omp") => {
    ctl.current?.abort();
    const c = new AbortController();
    ctl.current = c;
    setState({ kind: "loading", to });
    fetchSessionHandoff(sessionId, { to }, c.signal).then(
      (out) => {
        if (!c.signal.aborted) setState({ kind: "ready", to, out });
      },
      (e: unknown) => {
        if (c.signal.aborted) return;
        setState({ kind: "error", to, reason: e instanceof Error ? e.message : String(e) });
      },
    );
  };

  return (
    <div className="kb-ses__handoff" data-testid="session-handoff" title={HELP}>
      <div className="kb-ses__handoff-actions">
        {targets.map((t) => (
          <button
            key={t.to}
            type="button"
            className="kb-ses__resume-copy"
            onClick={() => run(t.to)}
            disabled={state.kind === "loading"}
            data-testid={`session-handoff-${t.to}`}
            title={HELP}
          >
            {t.label}
          </button>
        ))}
        {canLaunch &&
          targets.map((t) => (
            <button
              key={t.to}
              type="button"
              className="kb-ses__resume-copy"
              onClick={() => openTerm(t.to)}
              disabled={term.kind === "launching" || term.kind === "open"}
              data-testid={`session-handoff-term-${t.to}`}
              title={TERM_HELP(t.to)}
            >
              Open in terminal
            </button>
          ))}
        {state.kind === "loading" && (
          <span className="kb-ses__handoff-meta" data-testid="session-handoff-loading">
            building brief…
          </span>
        )}
        {term.kind === "launching" && (
          <span className="kb-ses__handoff-meta" data-testid="session-handoff-term-launching">
            launching terminal…
          </span>
        )}
      </div>
      {state.kind === "error" && (
        <p className="kb-ses__ins-errors" role="alert" data-testid="session-handoff-error">
          handoff failed: {state.reason}
        </p>
      )}
      {term.kind === "error" && (
        <p className="kb-ses__ins-errors" role="alert" data-testid="session-handoff-term-error">
          terminal launch failed: {term.reason}
        </p>
      )}
      {term.kind === "open" && (
        <Suspense
          fallback={<StateCard kind="loading" title="Loading terminal" reason={term.to} />}
        >
          <TerminalPanel
            termId={term.termId}
            sid={sessionId}
            harness={term.to}
            onClose={() => setTerm({ kind: "idle" })}
          />
        </Suspense>
      )}
      {state.kind === "ready" && <HandoffResult out={state.out} />}
    </div>
  );
}

function HandoffResult({ out }: { out: HandoffOut }) {
  return (
    <div className="kb-ses__handoff-result" data-testid="session-handoff-result">
      <div className="kb-ses__resume-cmd" title="run on the machine that has this session">
        <span className="kb-ses__resume-cmd-prompt">$</span>
        <code className="kb-ses__resume-cmd-text" data-testid="session-handoff-cmd">
          {out.command}
        </code>
        <CopyButton text={out.command} label="copy" testId="session-handoff-cmd-copy" />
      </div>
      <div className="kb-ses__handoff-actions">
        <CopyButton text={out.brief} label="copy brief" testId="session-handoff-brief-copy" />
        <span className="kb-ses__handoff-meta" data-testid="session-handoff-chars">
          {out.chars} chars
        </span>
        {out.scrubbed && (
          <span
            className="kb-ses__handoff-meta"
            data-testid="session-handoff-scrubbed"
            title="secrets were scrubbed from the brief before it left the daemon"
          >
            secrets scrubbed
          </span>
        )}
      </div>
      {out.truncated.length > 0 && (
        <p className="kb-ses__handoff-warn" data-testid="session-handoff-truncated">
          truncated to fit the budget: {out.truncated.join(", ")}
        </p>
      )}
      {out.notes.length > 0 && (
        <ul className="kb-ses__handoff-notes" data-testid="session-handoff-notes">
          {out.notes.map((n, i) => (
            <li key={`${i}:${n}`}>{n}</li>
          ))}
        </ul>
      )}
      <details className="kb-ses__handoff-preview">
        <summary>preview brief</summary>
        <pre className="kb-ses__resume" data-testid="session-handoff-brief">
          {out.brief}
        </pre>
      </details>
    </div>
  );
}
