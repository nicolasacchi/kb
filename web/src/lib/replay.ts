// W3.R-c — the pure half of the session-replay reader.
//
// Everything in this file is a total function of its arguments: no clock
// (never `Date.now()`), no DOM, no fetch, no React. That mirrors kb-core's
// `sessions::replay` (the extractor that produced the beats) and is what
// makes the grammar golden-pinnable in `replay.test.ts` — the route above it
// is then only wiring.
//
// THE HONESTY RULE (product, not style): every signal here is DERIVED from a
// transcript. Line ranges exist for a minority of Read calls, heading slugs
// only for beats that both resolved to a corpus artifact AND carried a line
// range, and a Bash/commit beat is a *detected* action, not a verified one.
// Nothing in this module invents a value it didn't get from the wire — an
// unresolved beat keeps its raw path and stays in the list (dropping it would
// silently rewrite the session's history).

import type { ReplayBeatOut } from "../api/generated/ReplayBeatOut";
import type { ReplayKind } from "../api/generated/ReplayKind";

// ── Δt formatting ──────────────────────────────────────────────────────────

/// Format a beat's `delta_secs` gap as a compact, deterministic label.
///
/// Grammar (golden-pinned in `replay.test.ts` — the scrubber's stop labels
/// and every rail row read it, so a change here is a visible grammar change):
///
///   < 1 min   `+0s` · `+27s`
///   < 1 hour  `+2m24s` · `+2m04s`   (seconds zero-padded to 2)
///   < 1 day   `+1h04m`              (minutes zero-padded to 2)
///   else      `+2d03h`              (hours zero-padded to 2)
///
/// Negative input clamps to `+0s`: kb-core already clamps `delta_secs` to 0
/// when a transcript's clock runs backwards (it keeps the raw `ts_unix` so
/// the inversion stays visible), and a "-3s" gap in a playhead label would
/// read as a bug rather than as the clock skew it is.
export function formatGap(deltaSecs: number): string {
  const s = Number.isFinite(deltaSecs) ? Math.max(0, Math.floor(deltaSecs)) : 0;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (s < 60) return `+${s}s`;
  if (s < 3600) return `+${Math.floor(s / 60)}m${pad(s % 60)}s`;
  if (s < 86400) return `+${Math.floor(s / 3600)}h${pad(Math.floor((s % 3600) / 60))}m`;
  return `+${Math.floor(s / 86400)}d${pad(Math.floor((s % 86400) / 3600))}h`;
}

/// Absolute elapsed time from the timeline's first beat, for the scrubber's
/// "where am I in the whole session" readout. Same grammar as `formatGap`
/// without the leading `+`.
export function formatElapsed(secs: number): string {
  return formatGap(secs).slice(1);
}

// ── segments ───────────────────────────────────────────────────────────────

/// One display segment: a human prompt and everything the agent did before
/// the next prompt. `prompt === null` only for the leading segment of a
/// transcript that starts mid-work (a resumed session, or a capture whose
/// first records are tool calls) — those beats are shown, never dropped.
export interface ReplaySegment {
  /// 0-based position of this segment in the segment list.
  index: number;
  /// The prompt beat that opened the segment, or `null` for the leading
  /// pre-prompt run.
  prompt: ReplayBeatOut | null;
  /// Every beat in the segment INCLUDING the opening prompt, in wire order.
  beats: ReplayBeatOut[];
  /// Index into the FLAT beat array of this segment's first beat — what the
  /// `[` / `]` segment jumps set the playhead to.
  startIndex: number;
  /// One-line label: the prompt's own detail, or a stand-in for the leading
  /// segment. Never fabricated from an assistant beat (that would attribute
  /// the agent's words to the human).
  title: string;
}

/// Group a flat beat window into prompt-rooted segments.
///
/// Pure and order-preserving: the input order IS the causal order (kb-core
/// never sorts, see its module docs), so this only ever cuts, never moves.
/// The flat array can always be recovered by concatenating `beats`.
export function groupSegments(beats: readonly ReplayBeatOut[]): ReplaySegment[] {
  const out: ReplaySegment[] = [];
  let cur: ReplaySegment | null = null;
  beats.forEach((b, i) => {
    const isPrompt = b.beat.kind === "prompt";
    if (isPrompt || cur === null) {
      cur = {
        index: out.length,
        prompt: isPrompt ? b : null,
        beats: [],
        startIndex: i,
        title: isPrompt ? b.beat.detail : "before the first prompt",
      };
      out.push(cur);
    }
    cur.beats.push(b);
  });
  return out;
}

/// Which segment holds the flat beat at `index`. Returns 0 for an
/// out-of-range index on a non-empty list (the playhead is always inside the
/// timeline), and -1 when there are no segments at all.
export function segmentIndexOf(
  segments: readonly ReplaySegment[],
  beatIndex: number,
): number {
  if (segments.length === 0) return -1;
  for (let i = segments.length - 1; i >= 0; i--) {
    if (beatIndex >= segments[i].startIndex) return i;
  }
  return 0;
}

/// `[` / `]` — the flat beat index to jump the playhead to when stepping
/// segments. `]` goes to the next segment's first beat; `[` goes to the
/// START of the current segment first (the vim `{`/`}` feel) and only to the
/// previous segment when the playhead is already parked there.
export function stepSegment(
  segments: readonly ReplaySegment[],
  beatIndex: number,
  dir: 1 | -1,
): number {
  if (segments.length === 0) return beatIndex;
  const cur = segmentIndexOf(segments, beatIndex);
  if (dir === 1) {
    const next = segments[cur + 1];
    return next ? next.startIndex : segments[segments.length - 1].startIndex;
  }
  if (beatIndex > segments[cur].startIndex) return segments[cur].startIndex;
  const prev = segments[cur - 1];
  return prev ? prev.startIndex : segments[0].startIndex;
}

// ── selectors ──────────────────────────────────────────────────────────────

/// Every beat that resolved to `artifactId`. A pure selector over an already
/// fetched window — the wire's own `?artifact=` filter is the server-side
/// twin (use that when you want the WHOLE session narrowed; use this when
/// you already hold the full timeline and want one artifact's slice of it).
export function beatsForArtifact(
  beats: readonly ReplayBeatOut[],
  artifactId: string | null | undefined,
): ReplayBeatOut[] {
  if (!artifactId) return [];
  return beats.filter((b) => b.artifact_id === artifactId);
}

/// What the right pane should show at playhead `index`.
///
/// `kind: "artifact"` — the beat resolved to an indexed artifact, so the pane
/// can iframe it and (when `slug` is set) post `kb:scroll-to-id`.
/// `kind: "path"` — the beat carried a raw path that is in no corpus (a
/// source file, `/etc/hosts`, a scratch file). Rendered as plain text.
/// `null` — nothing readable has happened yet.
///
/// **Walks BACKWARD** from `index` to the most recent beat that carries
/// something showable. That is the honest behaviour: the playhead shows the
/// artifact most recently *in play*, exactly as a reader's screen would still
/// be showing the last file they opened while they think. Blanking the pane
/// on every prompt/assistant beat would be both noisier and less true.
///
/// `fromIndex` reports WHICH beat the target came from, so the UI can say
/// "still showing the file from beat 12" rather than implying the current
/// beat touched it.
export type PlayheadTarget =
  | {
      kind: "artifact";
      kb: string;
      artifactId: string;
      sourceRelative: string | null;
      /// Heading id for `kb:scroll-to-id` — present only when the source beat
      /// carried a real line range AND the daemon found a preceding heading.
      slug: string | null;
      fromIndex: number;
    }
  | { kind: "path"; path: string; fromIndex: number };

export function playheadTarget(
  beats: readonly ReplayBeatOut[],
  index: number,
): PlayheadTarget | null {
  if (beats.length === 0) return null;
  const start = Math.min(Math.max(0, Math.floor(index)), beats.length - 1);
  for (let i = start; i >= 0; i--) {
    const b = beats[i];
    if (b.artifact_id && b.kb) {
      return {
        kind: "artifact",
        kb: b.kb,
        artifactId: b.artifact_id,
        sourceRelative: b.source_relative ?? null,
        slug: b.heading_slug ?? null,
        fromIndex: i,
      };
    }
    if (b.beat.path) {
      return { kind: "path", path: b.beat.path, fromIndex: i };
    }
  }
  return null;
}

// ── labels ─────────────────────────────────────────────────────────────────

/// Short, stable glyph per beat kind for the rail + scrubber ticks. ASCII/
/// simple symbols only (no emoji — the rest of the SPA's chrome has none).
const KIND_GLYPH: Record<ReplayKind, string> = {
  prompt: "»",
  assistant: "·",
  read: "◦",
  edit: "±",
  write: "+",
  bash: "$",
  commit: "⌥",
  decision: "?",
  search: "⌕",
  subagent: "@",
  outcome: "■",
  other: "•",
};

export function kindGlyph(kind: ReplayKind): string {
  return KIND_GLYPH[kind] ?? "•";
}

/// Human label for a beat kind — the rail's per-row tag and the legend.
const KIND_LABEL: Record<ReplayKind, string> = {
  prompt: "prompt",
  assistant: "agent",
  read: "read",
  edit: "edit",
  write: "write",
  bash: "bash",
  commit: "commit",
  decision: "decision",
  search: "search",
  subagent: "subagent",
  outcome: "outcome",
  other: "tool",
};

export function kindLabel(kind: ReplayKind): string {
  return KIND_LABEL[kind] ?? "tool";
}

/// The path a beat should DISPLAY: the resolved artifact's source-relative
/// path when it resolved, else the raw transcript path verbatim. `null` when
/// the beat touched no file at all (a prompt, a decision).
export function beatPathLabel(b: ReplayBeatOut): string | null {
  return b.source_relative ?? b.beat.path ?? null;
}

/// `1-based inclusive` line range as a label, or null. Deliberately NOT
/// synthesised for beats without one (invented ranges would be the exact
/// dishonesty this surface must avoid) — ~4 of 5 Read calls carry no range.
export function lineRangeLabel(b: ReplayBeatOut): string | null {
  const r = b.beat.line_range;
  if (!r) return null;
  return r[0] === r[1] ? `L${r[0]}` : `L${r[0]}–${r[1]}`;
}
