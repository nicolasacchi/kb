// OL-SIBL2 - the kb Code chip: a pure model for the Spine `crossing` slot.
//
// kb extracts HINTS and never classifies them (invariant #2). The chip paints
// from the same-origin `coderef/1` hints at once and may only read as
// `verified`/`drifted` once kb-code's doclens lane has answered with a
// payload; every other input (no answer yet, a thrown error, a CORS-shaped
// failure) lands on a hint-worded kind. The ladder is `freshnessState`, the
// one the Links tab renders, so the two can never disagree.
//
// The door is labelled with its DESTINATION (`-> coderefs.rs:120`), never a
// bare count: a research doc here cites 30-60 refs.
//
// Every URL comes from the `codeLensUrl.ts` builders (#35).

import type {
  DocLensOut,
  DocLensScorecardOut,
  ResolvedCodeRef,
} from "../api/doclens";
import type { CodeRefGroupOut } from "../api/generated/CodeRefGroupOut";
import type { CodeRefOut } from "../api/generated/CodeRefOut";
import { freshnessState } from "./codeFreshness";
import { codeReaderUrl, codeSearchUrl } from "./codeLensUrl";

export type SiblingChipKind =
  | "none"
  | "hints"
  | "verified"
  | "drifted"
  | "refusal"
  | "degraded";

export type SiblingTarget =
  | { kind: "link"; href: string }
  | { kind: "inspect" };

export type SiblingChipState = {
  kind: SiblingChipKind;
  label: string;
  tone: "ok" | "warn";
  title: string;
  primary: SiblingTarget | null;
};

export type SiblingChipInput = {
  /// The kb's operator-entered `[kb.*] code_url` (unvalidated).
  codeUrl: string | null | undefined;
  neverScanned: boolean;
  /// coderef/1 SERVER total.
  refCount: number;
  extractedAt: number | null;
  truncated: boolean;
  refs: readonly CodeRefOut[];
  groups: readonly CodeRefGroupOut[];
  /// The pane's live `kb:section` heading id, else the URL's `?sec=`.
  sec: string | null;
  /// Heading ids in document order (the runtime's `kb:toc`); lets a section
  /// without refs of its own resolve to the nearest ref-bearing group above.
  tocIds?: readonly string[];
  /// `?repo=` - the same source the Links tab reads.
  repoParam: string | null;
  scorecard: DocLensScorecardOut | undefined;
  /// Only a SUCCESSFUL payload may be passed; undefined while loading/failed.
  lens: DocLensOut | undefined;
  /// The one failed cross-daemon query's error (scorecard first, else lens).
  crossErr: unknown;
  nowMs?: number;
};

/// http(s) only: `new URL("javascript:...")` parses, so `validCodeUrl` alone
/// is not enough for a value that becomes an `href`.
export function httpCodeUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  try {
    const u = new URL(raw);
    return u.protocol === "http:" || u.protocol === "https:" ? raw : null;
  } catch {
    return null;
  }
}

function baseName(path: string): string {
  const parts = path.split("/").filter((s) => s !== "");
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

function dest(path: string, line: number | null | undefined): string {
  const name = baseName(path);
  return line !== null && line !== undefined && Number.isFinite(line) && line > 0
    ? `${name}:${line}`
    : name;
}

/// Refs in the order they should be tried as the primary target: the group
/// nearest the live section first (the group whose heading IS `sec`, else the
/// last ref-bearing heading at or before it in document order), then every
/// other ref by ordinal. Only refs with a `path_hint` qualify; issues carry
/// `owner/repo` there and `external` refs name no checkout path.
export function orderCandidates(
  refs: readonly CodeRefOut[],
  groups: readonly CodeRefGroupOut[],
  sec: string | null,
  tocIds: readonly string[] = [],
): CodeRefOut[] {
  const usable = refs
    .filter((r) => r.path_hint && r.kind !== "issue" && r.kind !== "external")
    .slice()
    .sort((a, b) => a.ordinal - b.ordinal);
  if (!sec) return usable;
  let groupKey: string | null = null;
  const exact = groups.find((g) => g.anchor === sec || g.key === sec);
  if (exact) {
    groupKey = exact.key;
  } else {
    const at = tocIds.indexOf(sec);
    if (at >= 0) {
      let bestIdx = -1;
      for (const g of groups) {
        const gi = tocIds.indexOf(g.anchor);
        if (gi >= 0 && gi <= at && gi > bestIdx) {
          bestIdx = gi;
          groupKey = g.key;
        }
      }
    }
  }
  if (groupKey === null) return usable;
  const near = usable.filter((r) => r.group === groupKey);
  return [...near, ...usable.filter((r) => r.group !== groupKey)];
}

type Pick = {
  target: SiblingTarget;
  dest: string | null;
  note: string | null;
};

/// The ONE primary-target choice. See the module header; the branches:
///  - lens answered, ref `present` with a `reader` -> that resolved line;
///  - lens answered, `ambiguous` -> kb-code search; `absent`/`external` are
///    skipped as primary;
///  - no lens, a repo chosen (`?repo=`, else the doc's pin) -> hint line;
///  - no repo chosen: exactly one checkout measured present > 0 with every
///    other 0 -> that checkout's reader (a navigation to a file kb-code
///    measured, NOT a pin and NOT a verdict); otherwise kb-code search.
export function pickCrossingRef(input: PickInput): Pick | null {
  const { codeUrl, refs, groups, sec, tocIds, repoParam, scorecard, lens } = input;
  const ordered = orderCandidates(refs, groups, sec, tocIds);
  if (ordered.length === 0) return null;
  const lensByOrdinal = new Map<number, ResolvedCodeRef>();
  for (const r of lens?.refs ?? []) lensByOrdinal.set(r.ordinal, r);

  if (lens) {
    for (const ref of ordered) {
      const l = lensByOrdinal.get(ref.ordinal);
      if (!l) continue;
      if (l.path_state === "present" && l.reader) {
        return {
          target: {
            kind: "link",
            href: codeReaderUrl(codeUrl, l.reader.repo, l.reader.path, l.reader.line),
          },
          dest: dest(l.reader.path, l.reader.line),
          note: null,
        };
      }
      if (l.path_state === "ambiguous") {
        const q = l.search?.q ?? ref.path_hint!;
        const repo = l.search?.repo || undefined;
        return {
          target: { kind: "link", href: codeSearchUrl(codeUrl, repo, q) },
          dest: `search ${baseName(ref.path_hint!)}`,
          note: "ambiguous in this checkout; opens kb-code search",
        };
      }
      // absent / external / no answer for this ordinal: try the next ref.
    }
    return null;
  }

  const first = ordered[0];
  const hint = first.path_hint!;
  const repo = repoParam || scorecard?.pinned_repo || null;
  if (repo) {
    return {
      target: {
        kind: "link",
        href: codeReaderUrl(codeUrl, repo, hint, first.line_start),
      },
      dest: dest(hint, first.line_start),
      note: null,
    };
  }
  const all = scorecard?.repos ?? [];
  const withHits = all.filter((r) => (r.present ?? 0) > 0);
  if (
    scorecard &&
    withHits.length === 1 &&
    all.every((r) => r === withHits[0] || r.present === 0)
  ) {
    return {
      target: {
        kind: "link",
        href: codeReaderUrl(codeUrl, withHits[0].name, hint, first.line_start),
      },
      dest: dest(hint, first.line_start),
      note: `present in 1 of ${all.length} checkouts (not pinned)`,
    };
  }
  return {
    target: { kind: "link", href: codeSearchUrl(codeUrl, undefined, hint) },
    dest: `search ${baseName(hint)}`,
    note: "no repo chosen; opens kb-code search",
  };
}

export type PickInput = {
  codeUrl: string;
  refs: readonly CodeRefOut[];
  groups: readonly CodeRefGroupOut[];
  sec: string | null;
  tocIds?: readonly string[];
  repoParam: string | null;
  scorecard: DocLensScorecardOut | undefined;
  lens: DocLensOut | undefined;
};

export const HINTS_TITLE =
  "kb reads these citations from the document's text; it cannot check them against the code. Open the Links tab for kb-code's check.";

export function siblingChipState(input: SiblingChipInput): SiblingChipState {
  const none: SiblingChipState = {
    kind: "none",
    label: "",
    tone: "ok",
    title: "",
    primary: null,
  };
  const codeUrl = httpCodeUrl(input.codeUrl);
  if (!codeUrl || input.neverScanned || input.refCount <= 0) return none;

  const fresh = freshnessState({
    neverScanned: input.neverScanned,
    refCount: input.refCount,
    extractedAt: input.extractedAt,
    hasCodeUrl: true,
    pinned: !!(input.repoParam || input.scorecard?.pinned_repo),
    scorecardOk: input.scorecard !== undefined,
    lens: input.lens,
    crossErr: input.crossErr,
    nowMs: input.nowMs,
  });
  if (!fresh) return none;

  // A lens payload only counts when the ladder itself said `verified`.
  const lens = fresh.kind === "verified" ? input.lens : undefined;
  const kind: SiblingChipKind =
    fresh.kind === "verified"
      ? (fresh.drifted ?? 0) > 0
        ? "drifted"
        : "verified"
      : fresh.kind === "refusal"
        ? "refusal"
        : fresh.kind === "degraded"
          ? "degraded"
          : fresh.kind === "never-scanned"
            ? "none"
            : "hints";
  if (kind === "none") return none;

  const picked = pickCrossingRef({
    codeUrl,
    refs: input.refs,
    groups: input.groups,
    sec: input.sec,
    tocIds: input.tocIds,
    repoParam: input.repoParam,
    scorecard: input.scorecard,
    lens,
  });
  const n = `${input.refCount}${input.truncated ? "+" : ""}`;
  const plural = input.refCount === 1 ? "" : "s";

  let label: string;
  let primary: SiblingTarget;
  let note: string | null = null;
  if (picked && picked.dest) {
    label = `→ ${picked.dest}`;
    primary = picked.target;
    note = picked.note;
  } else {
    label = `${n} code ref${plural}`;
    primary = { kind: "inspect" };
  }
  if (kind === "drifted") label += ` · ${fresh.drifted} moved`;

  const lead =
    kind === "verified" || kind === "drifted"
      ? `kb-code checked these citations: ${fresh.text}.`
      : kind === "refusal"
        ? `${fresh.text}.`
        : kind === "degraded"
          ? `${HINTS_TITLE} (${fresh.title}).`
          : `${HINTS_TITLE}`;
  const title = [
    primary.kind === "inspect"
      ? `${lead} Opens the Links tab.`
      : `${lead} Opens kb-code at the nearest citation.`,
    note,
  ]
    .filter(Boolean)
    .join(" ");

  return {
    kind,
    label,
    tone: kind === "hints" || kind === "verified" ? "ok" : "warn",
    title,
    primary,
  };
}
