// `kbc-tour/1` — composing a tour document (V74-L3b, D12 + D10).
//
// Two jobs, and both are PURE: no fetch, no clock the caller cannot supply,
// no id the server would reject.
//
//   1. **Record a tour from navigation.** `hopsToDraft` turns a window of
//      `lib/trail.ts`'s browser-local hops into an editable draft — one step
//      per LANDING, in walk order. This is D12's "record-a-tour from
//      navigation" and it is deliberately a DRAFT rather than an apply: a
//      recorded path is raw material, and a tour is prose about it. The
//      composer is where a human writes that prose.
//   2. **`draftToDoc`** turns the edited draft into the `kbc-tour/1` JSON
//      `POST /api/tours/apply` takes.
//
// THREE RULES this module exists to hold.
//
// **No coordinates, ever.** `boards::lint`'s `coordinates` rule runs over a
// TOUR document unchanged (server invariant 26(a)), so this SPA is held to it
// and checks itself first — `lib/boardDoc.ts`'s `coordinateKeysOutsidePins`,
// reused rather than copied. A tour has no `pins` map at all, so ANY
// coordinate-shaped key anywhere in the payload is a violation here.
//
// **A step's reference is authored, never resolved.** A recorded hop knows a
// path and (sometimes) a line. It does NOT know the blob sha, and this module
// never invents one: the daemon captures the witness at apply time, under its
// own exact rule (the file on disk must be what the step claims), and a sha
// guessed here would manufacture a `pinned` that was never true. A hop with no
// line becomes a prose-only `note` step — with a NOTE saying so — rather than
// a `code` step with a fabricated range, because the daemon refuses a whole
// file citation for exactly this reason.
//
// **The step id is derived from the path, not the ordinal.** Nodes are keyed
// by the author's own id, which is what makes a step's thread survive a
// re-apply (server invariant 26(a)); an ordinal-keyed id would re-point every
// thread the moment a step was inserted.

import type { TourOut, TourStep } from "../api/types";
import type { TrailStep } from "./trail";
import { coordinateKeysOutsidePins, isValidBoardId, mintNodeId } from "./boardDoc";

export const TOUR_SCHEMA = "kbc-tour/1";

/// `tours::MAX_STEPS` — tighter than a board's node cap, because a tour is
/// walked one stop at a time by a human.
export const MAX_TOUR_STEPS = 60;

/// `tours::MAX_CAMERA_CONTEXT`.
export const MAX_CAMERA_CONTEXT = 40;

/// `tours::routes::RESERVED_SLUGS` — a slug shadowed by a literal route
/// segment under `/api/tours`.
export const RESERVED_TOUR_SLUGS = ["apply"] as const;

export interface TourCameraInput {
  fold?: boolean;
  context?: number;
}

/// One step of a DRAFT — what the composer edits. `ref` is the kbc-review/1
/// ref string the daemon's sugar accepts; a prose-only step leaves it unset.
export interface TourStepDraft {
  id: string;
  title: string;
  body_md: string;
  /// A `code:<path>:<line>[-<end>]` ref, or `""` for a prose-only step.
  ref: string;
  camera?: TourCameraInput;
  /// What the recording could NOT establish about this step. Shown in the
  /// composer BEFORE the apply, never discovered from a 400 afterwards.
  notes: string[];
}

export interface TourDraft {
  slug: string;
  title: string;
  description_md: string;
  steps: TourStepDraft[];
  /// Why the draft is shorter than the window asked for, when it is.
  notes: string[];
}

/// The wire document. `deny_unknown_fields` on the daemon side, so every key
/// here is one the server names.
export interface TourDocInput {
  schema: string;
  repo: string;
  slug: string;
  title: string;
  description_md: string;
  status?: string;
  ref?: string;
  steps: {
    id: string;
    title?: string;
    body_md?: string;
    ref?: string;
    camera?: TourCameraInput;
  }[];
}

/// `code:<path>:<line>[-<end>]` from a path and a line range. Returns `null`
/// when there is no line to cite — see the module doc's second rule.
///
/// No `@sha` is EVER emitted: the recording did not observe one, and the
/// daemon's own capture is the honest place for the witness.
export function codeRefFor(
  path: string | undefined,
  line: number | undefined,
  lineEnd?: number,
): string | null {
  if (!path) return null;
  if (!Number.isInteger(line) || (line as number) < 1) return null;
  const start = line as number;
  const end = Number.isInteger(lineEnd) && (lineEnd as number) > start ? (lineEnd as number) : null;
  return end ? `code:${path}:${start}-${end}` : `code:${path}:${start}`;
}

/// The path + line a recorded hop LANDED on, read off its encoded `to` URL.
///
/// Total: a URL this cannot parse yields `{}`, and the caller then makes a
/// prose-only step. It reads the `/r/{repo}/{path}` shape and the `?line=`
/// param — deliberately NOT a full `nav/location.ts` decode, because a decode
/// would pull the whole Location Contract into a pure composer and the two
/// facts needed here are the two a URL carries most plainly.
export function landingOf(url: string): { path?: string; line?: number } {
  if (!url) return {};
  const [pathPart, queryPart] = url.split("?", 2);
  const m = /^\/r\/[^/]+\/(.+)$/.exec(pathPart ?? "");
  const raw = m?.[1];
  if (!raw || raw.startsWith("~")) return {};
  let path: string;
  try {
    path = decodeURI(raw);
  } catch {
    path = raw;
  }
  // A sentinel segment (`~story`, `~diff`) trails the file path; the file is
  // everything before it.
  const cut = path.indexOf("/~");
  if (cut > 0) path = path.slice(0, cut);
  if (!path) return {};
  const params = new URLSearchParams(queryPart ?? "");
  const rawLine = params.get("line");
  const line = rawLine === null ? NaN : Number(rawLine);
  return Number.isInteger(line) && line >= 1 ? { path, line } : { path };
}

/// A short human label for a step, from its landing.
function stepTitleFor(path: string | undefined, line: number | undefined, via?: string): string {
  const where = path ? (line ? `${path}:${line}` : path) : "a step";
  return via ? `${where} (${via.replace(/_/g, " ")})` : where;
}

/// Turn a window of recorded hops into an editable draft — D12's
/// "record-a-tour from navigation".
///
/// `hops` is `Trail.steps.slice(from, to)`; one step per LANDING, in walk
/// order. Over `MAX_TOUR_STEPS` the window is TRUNCATED from the END and the
/// draft says so: a tour is a walk with a beginning, so keeping the first N
/// stops is the only truncation that preserves one.
export function hopsToDraft(
  hops: readonly TrailStep[],
  opts: { slug: string; title: string; description_md?: string } = {
    slug: "",
    title: "",
  },
): TourDraft {
  const notes: string[] = [];
  let window = hops;
  if (window.length > MAX_TOUR_STEPS) {
    notes.push(
      `${window.length} hops recorded; a tour caps at ${MAX_TOUR_STEPS} steps, so this draft keeps the FIRST ${MAX_TOUR_STEPS} — the walk's beginning, not a sample of it`,
    );
    window = window.slice(0, MAX_TOUR_STEPS);
  }
  const taken: string[] = [];
  const steps: TourStepDraft[] = window.map((hop) => {
    const { path, line } = landingOf(hop.to);
    const ref = codeRefFor(path, line);
    const id = mintNodeId(path ? `${path.split("/").pop() ?? path}` : "step", taken);
    taken.push(id);
    const stepNotes: string[] = [];
    if (!ref) {
      stepNotes.push(
        path
          ? "this hop landed on a file but no line — a tour step is a PLACE, so it is a prose step until you cite one"
          : "this hop's landing is not a file location — it is a prose step",
      );
    }
    return {
      id,
      title: stepTitleFor(path, line, hop.via),
      body_md: "",
      ref: ref ?? "",
      notes: stepNotes,
    };
  });
  if (steps.length === 0) {
    notes.push("no hops in this window — walk somewhere first, then record");
  }
  return {
    slug: opts.slug,
    title: opts.title,
    description_md: opts.description_md ?? "",
    steps,
    notes,
  };
}

/// Every reason a draft cannot be applied, in words — checked BEFORE the
/// request so a refusal names the field rather than arriving as a 400.
export function draftProblems(draft: TourDraft): string[] {
  const out: string[] = [];
  if (!isValidBoardId(draft.slug)) {
    out.push(
      `slug ${JSON.stringify(draft.slug)} — 1..=64 chars of [a-z0-9_-] starting with an alphanumeric (it rides a URL path segment)`,
    );
  }
  if ((RESERVED_TOUR_SLUGS as readonly string[]).includes(draft.slug)) {
    out.push(
      `slug ${JSON.stringify(draft.slug)} is reserved — it is a literal segment under /api/tours, so a tour named it would be unreachable`,
    );
  }
  if (draft.title.trim() === "") out.push("title must be non-empty");
  if (draft.steps.length === 0) out.push("a tour with no steps is not a tour");
  if (draft.steps.length > MAX_TOUR_STEPS) {
    out.push(`${draft.steps.length} steps; the cap is ${MAX_TOUR_STEPS}`);
  }
  const seen = new Set<string>();
  for (const s of draft.steps) {
    if (!isValidBoardId(s.id)) out.push(`step id ${JSON.stringify(s.id)} is not a valid id`);
    else if (seen.has(s.id)) out.push(`duplicate step id ${JSON.stringify(s.id)}`);
    seen.add(s.id);
    const ctx = s.camera?.context;
    if (ctx !== undefined && (!Number.isInteger(ctx) || ctx < 0 || ctx > MAX_CAMERA_CONTEXT)) {
      out.push(`step ${s.id}: camera context must be 0..=${MAX_CAMERA_CONTEXT}`);
    }
  }
  return out;
}

export interface ComposeTourResult {
  doc: TourDocInput;
  /// Non-empty ⇒ REFUSE to send. The SPA is held to the daemon's own
  /// coordinate rule and checks itself first.
  coordinateViolations: string[];
  problems: string[];
}

/// The draft as the document `apply` takes.
export function draftToDoc(repo: string, draft: TourDraft): ComposeTourResult {
  const doc: TourDocInput = {
    schema: TOUR_SCHEMA,
    repo,
    slug: draft.slug,
    title: draft.title.trim(),
    description_md: draft.description_md,
    steps: draft.steps.map((s) => ({
      id: s.id,
      ...(s.title.trim() ? { title: s.title.trim() } : {}),
      ...(s.body_md.trim() ? { body_md: s.body_md } : {}),
      ...(s.ref.trim() ? { ref: s.ref.trim() } : {}),
      ...(s.camera && (s.camera.fold !== undefined || s.camera.context !== undefined)
        ? { camera: s.camera }
        : {}),
    })),
  };
  return {
    doc,
    // A tour has no `pins` map, so this scan covers the WHOLE payload.
    coordinateViolations: coordinateKeysOutsidePins(doc),
    problems: draftProblems(draft),
  };
}

/// An existing tour, back as an editable draft — the composer's "edit" entry.
/// Deliberately lossy in ONE direction: only the AUTHORED half round-trips.
/// Re-sending a resolved field (`state`, `address`, the snippet) would be this
/// SPA claiming a fact it did not establish.
export function tourToDraft(tour: TourOut): TourDraft {
  return {
    slug: tour.slug,
    title: tour.title,
    description_md: tour.description_md,
    steps: tour.steps.map((s: TourStep) => ({
      id: s.node.id,
      title: s.node.title ?? "",
      body_md: s.node.body_md ?? "",
      ref: s.ref ?? "",
      ...(s.camera
        ? {
            camera: {
              ...(s.camera.fold != null ? { fold: s.camera.fold } : {}),
              ...(s.camera.context != null ? { context: s.camera.context } : {}),
            },
          }
        : {}),
      notes: [],
    })),
    notes: [],
  };
}

/// A tour's honesty strip, in words. Deliberately NOT `boards::boardCensus`:
/// that one reads "N nodes · M edges · K steps", and a tour has exactly one of
/// those three numbers (its nodes ARE its steps and its `then` chain is
/// generated). Printing three numbers that are one number restated would be
/// the kind of derived-count drift `boardCensus` itself avoids.
export function tourCensus(tour: TourOut): string[] {
  const h = tour.honesty;
  const out = [
    `${h.steps} step${h.steps === 1 ? "" : "s"}`,
    `${h.pinned} pinned · ${h.carried} carried · ${h.present} present · ${h.inert} inert · ${h.orphans} orphan${h.orphans === 1 ? "" : "s"}`,
  ];
  if (h.orphans > 0) {
    out.push("an orphan keeps its address and its last-known text — it is shown, never dropped");
  }
  if (h.carried > 0) {
    out.push(
      "a carried step's line numbers are where the code is NOW; the range it was authored at is kept beside them",
    );
  }
  if (h.truncated_snippets > 0) {
    out.push(
      `${h.truncated_snippets} snippet${h.truncated_snippets === 1 ? " was" : "s were"} cut at the ${h.budget.max_snippet_lines}-line cap`,
    );
  }
  return out;
}

/// A step's camera as a caption — what the reader is being asked to SHOW.
/// Returns `null` for a step with no camera, so nothing is rendered for one.
export function cameraCaption(camera: { fold?: boolean | null; context?: number | null } | null | undefined): string | null {
  if (!camera) return null;
  const bits: string[] = [];
  if (camera.fold) bits.push("folded to the step's own range");
  if (camera.context != null && camera.context > 0) bits.push(`±${camera.context} lines of context`);
  return bits.length > 0 ? bits.join(" · ") : null;
}
