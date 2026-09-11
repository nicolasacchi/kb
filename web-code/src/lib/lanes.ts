// V76-R3a — `aug-lane/1` Facts surface (H4b). Pure derivation for the
// rail tab, the hover extra, and the TWO existing-gutter variants
// (diagnostics slot 3, blame slot 1). Nothing here mints a fifth
// `lineGutter` lane; git.behavior stays rail-only.
//
// Freshness is FOLDED INTO THE CLASS RENDERING for display only: a fact
// older than retention/2 is captioned "aging", older than retention
// "stale". The wire `class` is never rewritten.

import type {
  AbsentLane,
  FactsOut,
  LaneEntry,
  LaneFactOut,
  LaneTrustClass,
  LanesOut,
} from "../api/types";
import type { DiagnosticGutterMark, DiagnosticSeverityLabel } from "./diagnostics";

export const SECS_PER_DAY = 86_400;

export const GIT_BEHAVIOR = "git.behavior";
export const COVERAGE_SIMPLECOV = "coverage.simplecov";
export const RUBOCOP = "rubocop";
export const SARIF_PREFIX = "sarif.";

export type FreshnessCaption = "aging" | "stale" | null;

export type CoverageBand = "covered" | "uncovered" | "nodata";

export type LaneGroupStatus = "enabled" | "disabled" | "empty";

export interface DisplayFact {
  fact: LaneFactOut;
  /// Derived caption — never written back onto `fact.class`.
  freshness: FreshnessCaption;
  valueText: string;
  ageText: string;
}

export interface LaneGroup {
  id: string;
  title: string;
  status: LaneGroupStatus;
  statusReason: string;
  retentionDays: number;
  facts: DisplayFact[];
}

export type FactsViewKind = "loading" | "empty" | "error" | "partial" | "ready";

export interface FactsView {
  kind: FactsViewKind;
  groups: LaneGroup[];
  truncated: boolean;
  withheldDisabled: number;
  notes: string[];
  error?: string;
}

/// Display-only. The wire class is untouched; this only decides a caption.
export function freshnessCaption(ageSecs: number, retentionDays: number): FreshnessCaption {
  if (!Number.isFinite(ageSecs) || ageSecs < 0) return null;
  const days = Number.isFinite(retentionDays) && retentionDays > 0 ? retentionDays : 30;
  const retentionSecs = days * SECS_PER_DAY;
  if (ageSecs > retentionSecs) return "stale";
  if (ageSecs > retentionSecs / 2) return "aging";
  return null;
}

export function formatAgeSecs(ageSecs: number): string {
  if (!Number.isFinite(ageSecs) || ageSecs < 0) return "0s";
  if (ageSecs < 60) return `${Math.floor(ageSecs)}s`;
  if (ageSecs < 3600) return `${Math.floor(ageSecs / 60)}m`;
  if (ageSecs < SECS_PER_DAY) return `${Math.floor(ageSecs / 3600)}h`;
  return `${Math.floor(ageSecs / SECS_PER_DAY)}d`;
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v !== "" ? v : null;
}

/// Kind-aware one-line value: coverage hits, diagnostic message+cop,
/// churn, co-change partners, last-touch author kind. Unknown kinds
/// render a compact JSON so a newer daemon is still readable.
export function formatFactValue(fact: LaneFactOut): string {
  const v = fact.value ?? {};
  switch (fact.kind) {
    case "coverage": {
      const hits = num(v.hits);
      if (hits === null) return "coverage";
      return hits > 0 ? `${hits} hits` : "uncovered";
    }
    case "coverage_summary": {
      const covered = num(v.covered);
      const total = num(v.total);
      const pct = num(v.pct);
      const pctText = pct === null ? "—" : `${pct}%`;
      return `${covered ?? "—"}/${total ?? "—"} covered (${pctText})`;
    }
    case "diagnostic": {
      const message = str(v.message) ?? "";
      const cop = str(v.cop) ?? str(v.rule_id) ?? "";
      return [cop, message].filter(Boolean).join(" — ") || "diagnostic";
    }
    case "churn": {
      const commits = num(v.commits);
      const authors = num(v.authors);
      const window = num(v.window_days);
      const bits = [
        commits === null ? null : `${commits} commits`,
        authors === null ? null : `${authors} authors`,
        window === null ? null : `${window}d`,
      ].filter(Boolean);
      return bits.join(", ") || "churn";
    }
    case "co_change": {
      const total = num(v.total);
      const partners = Array.isArray(v.partners) ? v.partners : [];
      const names = partners
        .map((p) => (p && typeof p === "object" ? str((p as { path?: unknown }).path) : null))
        .filter((p): p is string => p !== null)
        .slice(0, 3);
      const extra = total !== null && total > names.length ? ` +${total - names.length}` : "";
      return names.length > 0 ? `co-change: ${names.join(", ")}${extra}` : "co-change: none";
    }
    case "last_touch": {
      const kind = str(v.author_kind) ?? "unknown";
      const author = str(v.author);
      return author ? `last touch: ${kind} (${author})` : `last touch: ${kind}`;
    }
    default: {
      try {
        return JSON.stringify(v);
      } catch {
        return fact.kind;
      }
    }
  }
}

export function isFamilyTemplate(lane: LaneEntry): boolean {
  return lane.family === true;
}

export function isDiagnosticLane(id: string): boolean {
  return id === RUBOCOP || id.startsWith(SARIF_PREFIX);
}

export function isCoverageLane(id: string): boolean {
  return id === COVERAGE_SIMPLECOV;
}

export function trustClassOf(fact: LaneFactOut): LaneTrustClass {
  const c = fact.class;
  if (c === "exact" || c === "likely" || c === "candidate" || c === "orphan") return c;
  return "orphan";
}

/// Trust in LINE STYLE. `orphan` is its own class (dotted + the word),
/// never collapsed into `candidate` and never hue-only.
export function trustClassName(cls: string): string {
  const t = trustClassOf({ class: cls } as LaneFactOut);
  return t === "orphan" ? "kbc-fact-trust kbc-fact-trust--orphan" : `kbc-fact-trust kbc-trust-${t}`;
}

function absentByLane(facts: FactsOut | undefined): Map<string, AbsentLane> {
  const map = new Map<string, AbsentLane>();
  for (const a of facts?.absent ?? []) map.set(a.lane, a);
  return map;
}

function factsByLane(facts: FactsOut | undefined): Map<string, LaneFactOut[]> {
  const map = new Map<string, LaneFactOut[]>();
  for (const f of facts?.facts ?? []) {
    const list = map.get(f.lane);
    if (list) list.push(f);
    else map.set(f.lane, [f]);
  }
  return map;
}

function displayFacts(rows: LaneFactOut[], retentionDays: number): DisplayFact[] {
  return rows.map((fact) => ({
    fact,
    freshness: freshnessCaption(fact.age_secs, retentionDays),
    valueText: formatFactValue(fact),
    ageText: formatAgeSecs(fact.age_secs),
  }));
}

/// Group the current file's facts by registry lane. Family templates are
/// skipped (they are not addressable). Disabled / empty lanes keep an
/// honest reason rather than disappearing.
export function groupLanes(registry: LanesOut | undefined, facts: FactsOut | undefined): LaneGroup[] {
  const lanes = registry?.lanes ?? [];
  const byLane = factsByLane(facts);
  const absent = absentByLane(facts);
  const groups: LaneGroup[] = [];
  for (const lane of lanes) {
    if (isFamilyTemplate(lane)) continue;
    const retentionDays = lane.retention_days > 0 ? lane.retention_days : 30;
    const rows = byLane.get(lane.id) ?? [];
    if (!lane.enabled) {
      groups.push({
        id: lane.id,
        title: lane.title,
        status: "disabled",
        statusReason: lane.note || "not in [lanes] enabled",
        retentionDays,
        facts: displayFacts(rows, retentionDays),
      });
      continue;
    }
    if (rows.length === 0) {
      const miss = absent.get(lane.id);
      groups.push({
        id: lane.id,
        title: lane.title,
        status: "empty",
        statusReason: miss?.reason || lane.note || "nothing to say about this path",
        retentionDays,
        facts: [],
      });
      continue;
    }
    groups.push({
      id: lane.id,
      title: lane.title,
      status: "enabled",
      statusReason: "",
      retentionDays,
      facts: displayFacts(rows, retentionDays),
    });
  }
  return groups;
}

export function buildFactsView(args: {
  registry: LanesOut | undefined;
  facts: FactsOut | undefined;
  registryLoading: boolean;
  factsLoading: boolean;
  error: string | null;
}): FactsView {
  const groups = groupLanes(args.registry, args.facts);
  const truncated = args.facts?.truncated === true;
  const withheldDisabled = args.facts?.withheld_disabled ?? 0;
  const notes = args.facts?.notes ?? [];
  if (args.error) {
    return { kind: "error", groups, truncated, withheldDisabled, notes, error: args.error };
  }
  if ((args.registryLoading && !args.registry) || (args.factsLoading && !args.facts)) {
    return { kind: "loading", groups, truncated, withheldDisabled, notes };
  }
  if (truncated) {
    return { kind: "partial", groups, truncated, withheldDisabled, notes };
  }
  const anyFacts = groups.some((g) => g.facts.length > 0);
  if (!anyFacts) {
    return { kind: "empty", groups, truncated, withheldDisabled, notes };
  }
  return { kind: "ready", groups, truncated, withheldDisabled, notes };
}

export function factsForLine(facts: readonly LaneFactOut[] | undefined, line: number): LaneFactOut[] {
  if (!facts || !Number.isFinite(line) || line < 1) return [];
  return facts.filter((f) => {
    const start = f.line ?? 0;
    if (start < 1) return false;
    const end = f.line_end && f.line_end >= start ? f.line_end : start;
    return line >= start && line <= end;
  });
}

const SEV_FROM_WORD: Record<string, DiagnosticSeverityLabel> = {
  error: "error",
  warning: "warning",
  info: "info",
  information: "info",
  hint: "hint",
  convention: "info",
  refactor: "hint",
  fatal: "error",
};

export function laneSeverityLabel(raw: string | null | undefined): DiagnosticSeverityLabel {
  if (!raw) return "unknown";
  return SEV_FROM_WORD[raw.toLowerCase()] ?? "unknown";
}

function sevRank(label: DiagnosticSeverityLabel): number {
  switch (label) {
    case "error":
      return 0;
    case "warning":
      return 1;
    case "info":
      return 2;
    case "hint":
      return 3;
    default:
      return 4;
  }
}

/// Diagnostic facts (rubocop / sarif.*) as gutter marks for slot 3.
/// `source` is `"lane"` so the inspector card can say which, vs LSP.
export function laneDiagnosticMarks(facts: readonly LaneFactOut[]): Map<number, DiagnosticGutterMark> {
  const byLine = new Map<number, LaneFactOut[]>();
  for (const f of facts) {
    if (!isDiagnosticLane(f.lane) || f.kind !== "diagnostic") continue;
    const start = f.line ?? 0;
    if (start < 1) continue;
    const end = f.line_end && f.line_end >= start ? f.line_end : start;
    for (let line = start; line <= end; line++) {
      const list = byLine.get(line);
      if (list) list.push(f);
      else byLine.set(line, [f]);
    }
  }
  const marks = new Map<number, DiagnosticGutterMark>();
  for (const [line, rows] of byLine) {
    let worst: DiagnosticSeverityLabel = "unknown";
    for (const r of rows) {
      const label = laneSeverityLabel(r.severity ?? str(r.value?.severity_raw));
      if (sevRank(label) < sevRank(worst)) worst = label;
    }
    const title =
      rows.length === 1
        ? `${rows[0].lane}: ${formatFactValue(rows[0])}`
        : `${rows.length} lane diagnostics`;
    marks.set(line, { severity: worst, title, count: rows.length, source: "lane" });
  }
  return marks;
}

/// Overlay lane marks onto LSP marks. Same slot; a line with both is
/// `source: "both"`. Worst severity wins.
export function mergeDiagnosticMarks(
  lsp: Map<number, DiagnosticGutterMark> | null | undefined,
  lane: Map<number, DiagnosticGutterMark> | null | undefined,
): Map<number, DiagnosticGutterMark> {
  const out = new Map<number, DiagnosticGutterMark>();
  if (lsp) {
    for (const [line, mark] of lsp) out.set(line, { ...mark, source: mark.source ?? "lsp" });
  }
  if (!lane) return out;
  for (const [line, mark] of lane) {
    const existing = out.get(line);
    if (!existing) {
      out.set(line, { ...mark, source: "lane" });
      continue;
    }
    const severity = sevRank(mark.severity) < sevRank(existing.severity) ? mark.severity : existing.severity;
    out.set(line, {
      severity,
      title: `${existing.title} · ${mark.title}`,
      count: existing.count + mark.count,
      source: "both",
    });
  }
  return out;
}

/// Coverage facts as a blame-gutter BAND variant. Off by default (caller
/// passes an empty map). `nodata` only when a file-level summary exists.
export function coverageBandMarks(
  facts: readonly LaneFactOut[] | undefined,
  lineCount: number,
): Map<number, CoverageBand> {
  const out = new Map<number, CoverageBand>();
  if (!facts || facts.length === 0 || lineCount < 1) return out;
  const cov = facts.filter((f) => isCoverageLane(f.lane));
  if (cov.length === 0) return out;
  const hasSummary = cov.some((f) => f.kind === "coverage_summary");
  if (hasSummary) {
    for (let line = 1; line <= lineCount; line++) out.set(line, "nodata");
  }
  for (const f of cov) {
    if (f.kind !== "coverage") continue;
    const start = f.line ?? 0;
    if (start < 1) continue;
    const end = f.line_end && f.line_end >= start ? f.line_end : start;
    const hits = num(f.value?.hits) ?? 0;
    const band: CoverageBand = hits > 0 ? "covered" : "uncovered";
    for (let line = start; line <= end && line <= lineCount; line++) out.set(line, band);
  }
  return out;
}

export function diagnosticFactsOf(facts: readonly LaneFactOut[] | undefined): LaneFactOut[] {
  return (facts ?? []).filter((f) => isDiagnosticLane(f.lane) && f.kind === "diagnostic");
}

export function gitBehaviorFactsOf(facts: readonly LaneFactOut[] | undefined): LaneFactOut[] {
  return (facts ?? []).filter((f) => f.lane === GIT_BEHAVIOR);
}

export function formatRunProvenance(fact: LaneFactOut): string {
  const tool = fact.run.tool;
  const ver = fact.run.tool_version;
  const when = fact.run.ingested_at;
  const whenText =
    typeof when === "number" && when > 0 ? new Date(when * 1000).toISOString().replace(/\.\d+Z$/, "Z") : "";
  return [tool, ver, whenText].filter(Boolean).join(" · ");
}

export function formatLastIngest(unix: number | null | undefined): string {
  if (typeof unix !== "number" || unix <= 0) return "never";
  return new Date(unix * 1000).toISOString().replace(/\.\d+Z$/, "Z");
}
