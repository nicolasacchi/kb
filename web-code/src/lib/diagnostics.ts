// PRR-U9 — diagnostics through lip (design-addendum-2.md §D's UI unit).
// Pure derivation for the reader gutter + inspector Diagnostics card + the
// review diff's per-file chip/line marks. Kept free of React/CodeMirror/DOM
// concerns — `components/provenance/DiagnosticsCard.tsx`,
// `components/CodeView.tsx`'s `diagMarkersFrom`, and
// `components/diff/{UnifiedHunks,SplitHunks}.tsx` are the thin renderers
// that map whatever this module computes onto JSX, same split
// `lib/blameGutter.ts`/`lib/frameworkEdges.ts` already establish for their
// own cards/gutters.

import type { DiagnosticRow, DiagnosticsOut, RepoIntelStatus } from "../api/types";

// --- severity ---------------------------------------------------------

/// `DiagnosticRow.severity` is the RAW LSP `DiagnosticSeverity` int (1=
/// Error, 2=Warning, 3=Information, 4=Hint) — `api/types.ts`'s own doc.
/// This is the ONE place that maps it to a display label; every other
/// module in this unit reads the label, never the raw int.
export type DiagnosticSeverityLabel = "error" | "warning" | "info" | "hint" | "unknown";

const SEVERITY_LABELS: Record<number, DiagnosticSeverityLabel> = {
  1: "error",
  2: "warning",
  3: "info",
  4: "hint",
};

/// Display order (most to least severe) for chip text + card grouping.
/// `"unknown"` (a `null` severity, or a value outside 1-4 — forward
/// compat for a newer daemon) deliberately excluded: it ranks worst (see
/// `severityRank`) but is folded into the plain row list rather than its
/// own labeled bucket in the chip text.
export const SEVERITY_DISPLAY_ORDER: readonly DiagnosticSeverityLabel[] = [
  "error",
  "warning",
  "info",
  "hint",
];

const SEVERITY_RANK: Record<DiagnosticSeverityLabel, number> = {
  error: 0,
  warning: 1,
  info: 2,
  hint: 3,
  unknown: 4,
};

const SEVERITY_PLURAL: Record<DiagnosticSeverityLabel, [string, string]> = {
  error: ["error", "errors"],
  warning: ["warning", "warnings"],
  info: ["info", "info"],
  hint: ["hint", "hints"],
  unknown: ["diagnostic", "diagnostics"],
};

export function severityLabel(sev: number | null | undefined): DiagnosticSeverityLabel {
  if (sev != null && sev in SEVERITY_LABELS) return SEVERITY_LABELS[sev];
  return "unknown";
}

/// Lower rank = more severe. `null`/unrecognized sorts last — same
/// "unrecognized value sorts/degrades last" posture `lib/diffFindings.ts`'s
/// `severityRank` uses for findings.
export function severityRank(sev: number | null | undefined): number {
  return SEVERITY_RANK[severityLabel(sev)];
}

function pluralize(label: DiagnosticSeverityLabel, n: number): string {
  const [one, many] = SEVERITY_PLURAL[label];
  return `${n} ${n === 1 ? one : many}`;
}

// --- counts -------------------------------------------------------------

export interface DiagnosticSeverityCounts {
  error: number;
  warning: number;
  info: number;
  hint: number;
  unknown: number;
  total: number;
}

export function countBySeverity(rows: readonly DiagnosticRow[]): DiagnosticSeverityCounts {
  const counts: DiagnosticSeverityCounts = { error: 0, warning: 0, info: 0, hint: 0, unknown: 0, total: 0 };
  for (const row of rows) {
    counts[severityLabel(row.severity)]++;
    counts.total++;
  }
  return counts;
}

/// The most severe non-zero bucket, or `null` for an empty count (mirrors
/// `lib/diffFindings.ts`'s `worstSeverity` shape, scaled to 4+1 buckets).
/// `"unknown"` only wins when it's the ONLY non-zero bucket — every named
/// severity outranks it (`SEVERITY_DISPLAY_ORDER` is checked first).
export function worstSeverityLabel(counts: DiagnosticSeverityCounts): DiagnosticSeverityLabel | null {
  for (const label of SEVERITY_DISPLAY_ORDER) {
    if (counts[label] > 0) return label;
  }
  if (counts.unknown > 0) return "unknown";
  return null;
}

/// "2 errors · 5 warnings" — the file-header chip text (design-addendum-2
/// §D's own example). Skips zero buckets; `"unknown"` folds in under a
/// generic "diagnostic(s)" label only when it's the only non-empty bucket
/// content wasn't already covered by a named severity above it. Empty
/// input (`counts.total === 0`) returns `""` — callers decide the
/// zero-diagnostics copy (a chip renders nothing; the card says "clean").
export function formatSeveritySummary(counts: DiagnosticSeverityCounts): string {
  const parts: string[] = [];
  for (const label of SEVERITY_DISPLAY_ORDER) {
    if (counts[label] > 0) parts.push(pluralize(label, counts[label]));
  }
  if (parts.length === 0 && counts.unknown > 0) parts.push(pluralize("unknown", counts.unknown));
  return parts.join(" · ");
}

// --- row ordering ---------------------------------------------------------

/// Stable sort: worst severity first, then line, then column — the
/// inspector card's row order ("rows jump to line", design-addendum-2 §D).
export function sortDiagnosticsForCard(rows: readonly DiagnosticRow[]): DiagnosticRow[] {
  return [...rows].sort((a, b) => {
    const bySeverity = severityRank(a.severity) - severityRank(b.severity);
    if (bySeverity !== 0) return bySeverity;
    if (a.line !== b.line) return a.line - b.line;
    return a.col - b.col;
  });
}

// --- language gating ("enabled only when the repo's intel provider covers
// the file's lang") -----------------------------------------------------

/// Extension→lang-id table, mirroring `crates/kb-code-server/src/lang.rs`'s
/// `detect` switch (the extension arm only — that fn's shebang-sniff
/// fallback for extensionless scripts needs file bytes this gate doesn't
/// have cheaply on hand, so an extensionless bash script never gates a
/// diagnostics fetch open client-side; a deliberate, documented deviation,
/// not a bug — the server-side gate is authoritative either way, this is
/// only the client's "don't bother fetching" heuristic).
const EXTENSION_LANG: Record<string, string> = {
  rs: "rust",
  py: "python",
  rb: "ruby",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  sh: "bash",
  bash: "bash",
  yml: "yaml",
  yaml: "yaml",
  go: "go",
  toml: "toml",
  json: "json",
  erb: "erb",
};

/// `null` when the path has no extension, or one this table doesn't know —
/// matches `lang::detect`'s own `None` degrade (no grammar for this file).
export function detectLangId(path: string): string | null {
  const dot = path.lastIndexOf(".");
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (dot <= slash || dot === path.length - 1) return null;
  const ext = path.slice(dot + 1).toLowerCase();
  return EXTENSION_LANG[ext] ?? null;
}

/// The `useDiagnostics` enable-gate: `true` only when `intel` names a
/// provider AND that provider's `langs` list covers `path`'s detected
/// language. Does NOT consult `intel.alive` — a stale-but-not-yet-refreshed
/// "not alive" snapshot must not silently suppress the fetch; the server
/// route reports `provider_unavailable` honestly if the provider really is
/// down, which the card/chip surface as a named reason (`buildDiagnosticsView`).
export function langCoveredByIntel(
  path: string | null | undefined,
  intel: RepoIntelStatus | null | undefined,
): boolean {
  if (!path || !intel) return false;
  const lang = detectLangId(path);
  return lang !== null && intel.langs.includes(lang);
}

// ── S2-D (B4) — multi-provider intel status display (design-s2.md §S2-D).
// `GET /api/repos`'s `intel_providers` is ADDITIVE (every matching
// `[[intel.providers]]` entry, config order); this is the ONE place that
// picks it over the legacy single-valued `intel` field, so every render
// surface (`RepoCard.tsx` today, any future one) shares the same fallback.

/// `intel_providers` when non-empty, else `intel` wrapped in a one-element
/// list (or `[]` when both are absent/null) — "prefer the vec when
/// non-empty, fall back to existing single intel" (design-s2.md §S2-D).
export function effectiveIntelProviders(repo: {
  intel: RepoIntelStatus | null;
  intel_providers?: RepoIntelStatus[];
}): RepoIntelStatus[] {
  if (repo.intel_providers && repo.intel_providers.length > 0) return repo.intel_providers;
  return repo.intel ? [repo.intel] : [];
}

// --- gutter marks (reader CM6 gutter + review-diff new-side gutter) ------

export interface DiagnosticGutterMark {
  severity: DiagnosticSeverityLabel;
  /// Hover/title text — the single message when only one diagnostic
  /// touches the line, else a short count summary.
  title: string;
  count: number;
}

function markTitle(rows: readonly DiagnosticRow[]): string {
  if (rows.length === 1) return rows[0].message;
  const counts = countBySeverity(rows);
  return formatSeveritySummary(counts) || `${rows.length} diagnostics`;
}

/// Fold `rows` into a per-line mark map: every line in `[line, end_line]`
/// (or just `line` when `end_line` is `0`/less than `line` — an
/// unreported range end, `api/types.ts`'s own doc) gets the WORST severity
/// among the diagnostics touching it, mirroring `lib/blameGutter.ts`'s
/// `buildLineDots` region-spanning convention.
export function diagnosticGutterMarks(rows: readonly DiagnosticRow[]): Map<number, DiagnosticGutterMark> {
  const byLine = new Map<number, DiagnosticRow[]>();
  for (const row of rows) {
    const end = row.end_line >= row.line ? row.end_line : row.line;
    for (let line = row.line; line <= end; line++) {
      const existing = byLine.get(line);
      if (existing) existing.push(row);
      else byLine.set(line, [row]);
    }
  }
  const marks = new Map<number, DiagnosticGutterMark>();
  for (const [line, lineRows] of byLine) {
    const sorted = sortDiagnosticsForCard(lineRows);
    marks.set(line, {
      severity: severityLabel(sorted[0].severity),
      title: markTitle(sorted),
      count: lineRows.length,
    });
  }
  return marks;
}

// --- view model (the state matrix: absent / loading / reason / clean / rows) ---

export type DiagnosticsViewKind = "absent" | "loading" | "reason" | "clean" | "rows";

export interface DiagnosticsView {
  kind: DiagnosticsViewKind;
  /// Human text for `"reason"` — see `unavailableReasonLabel`.
  reason?: string;
  rows?: DiagnosticRow[];
  counts?: DiagnosticSeverityCounts;
  worst?: DiagnosticSeverityLabel | null;
}

const REASON_LABELS: Record<string, string> = {
  unknown_language: "file type not recognized",
  no_provider_configured: "no diagnostics provider configured for this file",
  file_unreadable: "file could not be read",
  provider_unavailable: "diagnostics provider unavailable",
  blob_stale: "file changed while fetching diagnostics — try again",
};

/// An unrecognized reason (a newer daemon) degrades to its own verbatim
/// text — same forward-compat posture `lib/diffFindings.ts`'s
/// `severityLabel`/`dispositionLabel` use for their own closed vocabularies.
export function unavailableReasonLabel(reason: string | null | undefined): string {
  if (!reason) return "diagnostics unavailable";
  return REASON_LABELS[reason] ?? reason;
}

/// The ONE state-matrix function both `DiagnosticsCard` (reader inspector)
/// and the review-diff file-header chip build their view off of —
/// `covered` is `useDiagnostics`'s own gate (`langCoveredByIntel`), `data`/
/// `isLoading` come straight off the TanStack Query result.
///
/// Named states (design-addendum-2 §D): no provider → `"absent"` (card
/// renders nothing at all); refused/unavailable → `"reason"` (one caption
/// line); `[]` → `"clean"` ("provider reports clean"); non-empty → `"rows"`.
export function buildDiagnosticsView(
  covered: boolean,
  data: DiagnosticsOut | null | undefined,
  isLoading: boolean,
): DiagnosticsView {
  if (!covered) return { kind: "absent" };
  if (isLoading || !data) return { kind: "loading" };
  if (data.diagnostics === null) {
    return { kind: "reason", reason: unavailableReasonLabel(data.unavailable_reason) };
  }
  if (data.diagnostics.length === 0) {
    return { kind: "clean" };
  }
  const counts = countBySeverity(data.diagnostics);
  return {
    kind: "rows",
    rows: sortDiagnosticsForCard(data.diagnostics),
    counts,
    worst: worstSeverityLabel(counts),
  };
}

/// The diff file-header chip text — non-`null` ONLY for `"rows"` (design-
/// addendum-2 §D's own example is a counts chip; absence/loading/clean/
/// reason stay quiet in the chip the same way `DiffFileHeader`'s existing
/// finding badge renders nothing for a zero count — the fuller state
/// breakdown is the inspector card's job, not the diff header's).
export function diagnosticsChipText(view: DiagnosticsView): string | null {
  if (view.kind !== "rows" || !view.counts) return null;
  const text = formatSeveritySummary(view.counts);
  return text || null;
}
