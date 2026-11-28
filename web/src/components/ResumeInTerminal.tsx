// HUBHOST — the session page's Resume section door into the Terminals hub's
// launcher. Launching is the hub's job now (confirm step included): this only
// links to `/terminals?launch=1&resume=<sid>` and never POSTs.

import { ContinueInTerminal, useContinueEligible } from "./ContinueInTerminal";

export function ResumeInTerminal({
  sessionId,
  harness,
  cwd,
  hasLiveTerminal,
}: {
  sessionId: string;
  harness: string;
  cwd?: string | null;
  /// `card.terminal.available`: a live bound terminal already exists.
  hasLiveTerminal: boolean;
}) {
  const ok = useContinueEligible({ session_id: sessionId, harness, cwd: cwd ?? undefined }, hasLiveTerminal);
  if (!ok) return null;
  return (
    <div className="kb-ses__handoff" data-testid="session-resume-term">
      <div className="kb-ses__handoff-actions">
        <ContinueInTerminal
          sessionId={sessionId}
          harness={harness}
          cwd={cwd}
          hasLiveTerminal={hasLiveTerminal}
          className="kb-ses__resume-copy"
        />
      </div>
    </div>
  );
}
