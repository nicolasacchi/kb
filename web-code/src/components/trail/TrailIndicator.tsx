// The kbc-trail/1 opt-in INDICATOR (V74-L3b, design D17).
//
// D17 permits a server-side attention ledger only behind "an explicit opt-in
// that is off on first boot **with a visible indicator**". That sentence is
// this component's whole reason to exist, and three rules follow from it:
//
//   1. **It always renders when the daemon permits the feature at all.** Off
//      is a STATE, shown, not an absence. The only case it renders nothing is
//      `enabled: false` — the operator has not permitted the feature, so there
//      is no ledger to have a posture about, and a permanent "off" chip in the
//      chrome of every daemon that will never record anything is noise.
//   2. **It never claims a state it did not read.** The mode comes from
//      `GET /api/trails/state` and is normalised with the same fail-closed
//      rule the daemon applies (`normalizeTrailMode`): an unknown mode reads
//      as `off`, because an indicator saying "recording" for a mode this build
//      cannot interpret is the one lie this surface exists to prevent.
//   3. **The control is ABSENT, not disabled, for a caller who cannot use
//      it.** Changing the mode is loopback-only and the daemon says so per
//      caller (`mutable`), so a non-loopback browser gets the indicator and a
//      caption, never a button that 403s.

import { useCallback, useState } from "react";
import { Icon } from "../icons";
import { useCommandHandlers } from "../../commands/CommandRoot";
import { normalizeTrailMode, useSetTrailState, useTrailState } from "../../hooks/useTrails";
import { toast } from "../../lib/toast";

/// The three modes, as a human reads them. The wire vocabulary is snake-free
/// already; this is the display twin and the ONLY place the two are related.
export const TRAIL_MODE_LABEL: Readonly<Record<string, string>> = {
  off: "trail off",
  recording: "recording",
  paused: "trail paused",
};

export default function TrailIndicator() {
  const state = useTrailState();
  const setMode = useSetTrailState();
  const [open, setOpen] = useState(false);
  const data = state.data;
  const mode = normalizeTrailMode(data?.mode);

  const toggle = useCallback(() => {
    if (!data?.enabled) {
      toast.warn(
        "trails are disabled on this daemon — set [trails] enabled = true in kb-code.toml and restart. kbc-trail/1 is off on first boot by design.",
      );
      return;
    }
    if (!data.mutable) {
      toast.warn(
        "changing the trail mode is loopback-only — reach kb-code at 127.0.0.1 to turn recording on or off.",
      );
      return;
    }
    const next = mode === "recording" ? "paused" : "recording";
    setMode.mutate(next, {
      onSuccess: (out) =>
        toast.ok(
          out.mode === "recording"
            ? "recording your trail — one dwell number per step, never per line"
            : "trail paused — nothing is being recorded; what was recorded is still there",
        ),
      onError: (e) => toast.err(e instanceof Error ? e.message : String(e)),
    });
  }, [data, mode, setMode]);

  // `Space k p` — one key, and the indicator changes. D17's "pausable with a
  // visible indicator" is a pair, so the key and the chip are wired together
  // rather than the key toggling something invisible.
  useCommandHandlers({ "trail.pause": toggle });

  // Nothing to indicate on a daemon where the feature is not permitted — see
  // rule 1. A read that has not landed yet renders nothing either, rather than
  // flashing a wrong posture.
  if (!data || !data.enabled) return null;

  return (
    <div className="kbc-trailind" data-kbc-trail-indicator={mode}>
      <button
        type="button"
        className={`kbc-iconbtn kbc-trailind__btn kbc-trailind__btn--${mode}`}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-label={`trail: ${TRAIL_MODE_LABEL[mode]}`}
        title={`kbc-trail/1: ${TRAIL_MODE_LABEL[mode]} — click for the controls (Space k p toggles)`}
        data-kbc-trail-toggle
      >
        {mode === "recording" ? <Icon.Record /> : mode === "paused" ? <Icon.Pause /> : <Icon.Dot />}
        <span className="kbc-trailind__label">{TRAIL_MODE_LABEL[mode]}</span>
      </button>
      {open && (
        <div className="kbc-trailind__pop" role="dialog" aria-label="trail recording" data-kbc-trail-pop>
          <p className="kbc-trailind__mode">
            <strong>{TRAIL_MODE_LABEL[mode]}</strong>
          </p>
          <p className="kbc-trailind__meta">
            retention {data.retention_days === 0 ? "off (nothing expires)" : `${data.retention_days} days`} ·
            dwell floored to {data.step_granularity_secs}s
          </p>
          {data.mutable ? (
            <div className="kbc-trailind__actions">
              {data.modes_available.map((m) => (
                <button
                  key={m}
                  type="button"
                  disabled={m === mode || setMode.isPending}
                  onClick={() =>
                    setMode.mutate(m as "off" | "recording" | "paused", {
                      onError: (e) => toast.err(e instanceof Error ? e.message : String(e)),
                    })
                  }
                  data-kbc-trail-mode={m}
                >
                  {TRAIL_MODE_LABEL[m] ?? m}
                </button>
              ))}
            </div>
          ) : (
            <p className="kbc-trailind__gate" data-kbc-trail-gate>
              changing this is loopback-only — your own movement record never leaves your box
            </p>
          )}
          <ul className="kbc-trailind__notes">
            {data.notes.map((n, i) => (
              <li key={i}>{n}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
