// RP-track — client for the per-artifact reading summary
// (GET /api/kb/{kb}/artifacts/{id}/reading). Best-effort: a 404 / bad JSON /
// daemon hiccup resolves to null so the Detail view degrades to no heatmap.
import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (kb-core/src/reading.rs
// is the source of truth; `just types` regenerates). `sections` is
// absent in the lite view and when nothing was captured — treat as [].
import type { SectionState } from "./generated/SectionState";
import type { ReadingSection } from "./generated/ReadingSection";
import type { ReadingStopPoint } from "./generated/ReadingStopPoint";
import type { ReadingSummary } from "./generated/ReadingSummary";

export type { SectionState, ReadingSection, ReadingStopPoint, ReadingSummary };

export async function fetchReading(
  kb: string,
  id: string,
  opts: { lite?: boolean; signal?: AbortSignal } = {},
): Promise<ReadingSummary | null> {
  try {
    const q = opts.lite ? "?lite=true" : "";
    const r = await fetch(
      `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(
        id,
      )}/reading${q}`,
      { headers: { Accept: "application/json" }, signal: opts.signal },
    );
    if (!r.ok) return null;
    return (await r.json()) as ReadingSummary;
  } catch {
    return null;
  }
}
