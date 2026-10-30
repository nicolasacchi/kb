// OL IDEA3 — the session evidence card, SPA side. The server derives the
// verdict and its one sentence (`kb_core::sessions::checks`); everything here
// is layout and the copy that sits AROUND those strings. Display-only: nothing
// here is persisted, sorted on, filtered on or fed back into triage
// (invariant #11 — surfaced, never scored).
import type { EvidenceVerdict, SessionCheckOut, SessionEvidenceOut } from "../api/sessions";

/** The verdicts a worklog row spells out. `no_edits` and `not_extracted` say
 *  nothing on the row: an unchecked claim is only worth a line when there is
 *  something that could have been checked. */
const ROW_VERDICTS: ReadonlySet<EvidenceVerdict> = new Set<EvidenceVerdict>([
  "verified",
  "failing",
  "unconfirmed",
  "stale",
  "unchecked",
]);

export type RowEvidence = {
  verdict: EvidenceVerdict;
  /** The server's sentence, verbatim. */
  phrase: string;
  /** The deciding command, `$ `-prefixed (the row tooltip); undefined when
   *  the verdict rests on no single check (`unchecked`). */
  title: string | undefined;
};

/** The words a worklog row carries after its facts, or null when it carries
 *  none (no evidence, `no_edits`, `not_extracted`). Words only — no glyph, no
 *  colour: the row stays quiet. */
export function rowEvidence(ev: SessionEvidenceOut | null | undefined): RowEvidence | null {
  if (!ev || !ROW_VERDICTS.has(ev.verdict)) return null;
  const phrase = ev.phrase?.trim();
  if (!phrase) return null;
  return { verdict: ev.verdict, phrase, title: ev.deciding ? `$ ${ev.deciding.cmd}` : undefined };
}

/** The muted caption every Checks section carries. */
export const CHECKS_CAPTION =
  "From this session's main transcript: edits are Edit/Write tool calls (edits made through Bash or by subagents are not seen); checks are a fixed list of test, lint and build commands.";

/** ` · N edits to agent or scratch files not counted`, or "" when none. */
export function ignoredEditsNote(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "";
  return ` · ${n} ${n === 1 ? "edit" : "edits"} to agent or scratch files not counted`;
}

/** The caption on a segmented session's section; null for an ordinary one. */
export function segmentCaption(
  idx: number | undefined,
  count: number | undefined,
): string | null {
  if (!count || count <= 1) return null;
  return `part ${idx ?? 1} of ${count}: this part's checks and edits only`;
}

export const NOT_EXTRACTED_LINE =
  "Not extracted for this capture: it predates the evidence card. Reindexing the sessions corpus fills it.";

export const NO_EDITS_LINE = "No edits seen in this session's main transcript, so nothing to check.";

export const UNCOMMITTED_LINE = "No commit detected in this session.";

/** `Not named in the closing text: a.rs, b.ts (+3 more)`, or null when none. */
export function notNamedLine(
  names: readonly string[] | undefined,
  total: number | undefined,
): string | null {
  if (!names || names.length === 0) return null;
  const more = Math.max(0, (total ?? names.length) - names.length);
  return `Not named in the closing text: ${names.join(", ")}${more > 0 ? ` (+${more} more)` : ""}`;
}

/** Where a check ran relative to the last edit. */
export function checkWhen(c: Pick<SessionCheckOut, "after_last_edit">): string {
  if (c.after_last_edit === true) return "after the last edit";
  if (c.after_last_edit === false) return "before the last edit";
  return "";
}

/** The two resume-block lines the CLI prints too: the checks phrase with its
 *  deciding command, and a missing commit. Empty when there is nothing to
 *  say (no evidence, `no_edits`). */
export function resumeEvidenceLines(ev: SessionEvidenceOut | null | undefined): string[] {
  if (!ev) return [];
  const out: string[] = [];
  if (ev.verdict !== "no_edits") {
    if (ev.phrase) {
      out.push(`Checks: ${ev.phrase}`);
      if (ev.deciding) out.push(`  $ ${ev.deciding.cmd}`);
    } else if (ev.verdict === "not_extracted") {
      out.push("Checks: not extracted for this capture");
    }
  }
  if (ev.uncommitted) out.push("Not committed: no commit detected in this session");
  return out;
}
