// RAILCHANGES — pure model for the Changes tab: directory grouping of the
// session's files, the commit cap, the tier lookup against kb-code's code lane
// and the door builders. No React, no fetching. Surfaced, never scored: nothing
// here ranks by verdict or invents a tier (kb's file/commit rows carry none).

import type { CommitOut } from "../../../api/generated/CommitOut";
import type { SessionFileOut } from "../../../api/generated/SessionFileOut";
import { commitHref, type CommitConfidence, type CommitItem } from "../../../api/sessionCard";
import { codeSearchUrl } from "../../../lib/codeLensUrl";
import { relativeAge } from "../../../lib/time";

export const COMMIT_CAP = 20;
export const NOT_CAPTURED_REASON = "Updates after the next capture.";
export const LIVE_CAPTURE_HINT = "Live capture may be off: new data appears after the next capture.";

export interface FileRow {
  path: string;
  basename: string;
  action: string;
  edited: boolean;
  viaSubagent: boolean;
  inCorpus: boolean;
  kb?: string;
  sourceRelative?: string;
}

export interface DirGroup {
  dir: string;
  rows: FileRow[];
}

export function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  if (i < 0) return ".";
  if (i === 0) return "/";
  return path.slice(0, i);
}

function baseOf(f: SessionFileOut): string {
  if (f.basename) return f.basename;
  const i = f.path.lastIndexOf("/");
  return i < 0 ? f.path : f.path.slice(i + 1);
}

function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

export function isEdit(action: string): boolean {
  return action !== "read";
}

/** Files that changed (edit/write), de-duplicated per path+action+origin. */
export function toRows(files: readonly SessionFileOut[]): FileRow[] {
  const seen = new Set<string>();
  const out: FileRow[] = [];
  for (const f of files) {
    const key = `${f.path}\u0000${f.action}\u0000${f.via_subagent ? 1 : 0}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({
      path: f.path,
      basename: baseOf(f),
      action: f.action,
      edited: isEdit(f.action),
      viaSubagent: f.via_subagent,
      inCorpus: f.in_corpus,
      kb: f.kb,
      sourceRelative: f.source_relative,
    });
  }
  return out;
}

/**
 * Group by directory: directories sorted; inside a group edited/written rows
 * come before read ones, then by path. `includeReads` false drops read rows
 * (and any directory left empty).
 */
export function groupFiles(files: readonly SessionFileOut[], includeReads: boolean): DirGroup[] {
  const by = new Map<string, FileRow[]>();
  for (const r of toRows(files)) {
    if (!includeReads && !r.edited) continue;
    const d = dirOf(r.path);
    const list = by.get(d);
    if (list) list.push(r);
    else by.set(d, [r]);
  }
  return [...by.keys()]
    .sort(cmp)
    .map((dir) => ({
      dir,
      rows: (by.get(dir) as FileRow[]).sort(
        (a, b) => Number(b.edited) - Number(a.edited) || cmp(a.path, b.path) || cmp(a.action, b.action),
      ),
    }));
}

export function readCount(files: readonly SessionFileOut[]): number {
  return toRows(files).filter((r) => !r.edited).length;
}

export function capCommits<T>(items: readonly T[], showAll: boolean, cap: number = COMMIT_CAP): T[] {
  return showAll || items.length <= cap ? [...items] : items.slice(0, cap);
}

const MIN_PREFIX = 7;

/** The kb-code code-lane item for a commit sha (prefix match, either way), or null. */
export function matchCodeItem(sha: string | undefined, items: readonly CommitItem[] | undefined): CommitItem | null {
  if (!sha || sha.length < MIN_PREFIX || !items) return null;
  const s = sha.toLowerCase();
  for (const it of items) {
    const t = it.sha.toLowerCase();
    if (t.length >= MIN_PREFIX && (t.startsWith(s) || s.startsWith(t))) return it;
  }
  return null;
}

/** A tier is shown only when the matched code-lane item carries one. */
export function tierFor(item: CommitItem | null): CommitConfidence | null {
  return item ? item.confidence : null;
}

/** Commit door: only for a matched, visible code-lane item under a code_url. */
export function commitDoor(codeUrl: string | null, item: CommitItem | null): string | null {
  if (!codeUrl || !item || !item.visible) return null;
  return commitHref(codeUrl, item.repo, item.sha);
}

/** File door: the unscoped kb-code search for the path (no repo is guessed). */
export function fileDoor(codeUrl: string | null, path: string): string | null {
  return codeUrl ? codeSearchUrl(codeUrl, undefined, path) : null;
}

/** The session page path (new tab). No stable section ids are relied on. */
export function sessionPageHref(sid: string): string {
  return `/sessions/${encodeURIComponent(sid)}`;
}

export function shortSha(c: CommitOut): string | null {
  return c.sha ? c.sha.slice(0, 7) : null;
}

/** "As of last capture, 5m ago" from the session's last-event time (unix s). */
export function freshnessText(endedAt: number | null | undefined, nowMs: number): string {
  if (!endedAt || endedAt <= 0) return "Capture time unknown";
  const age = relativeAge(endedAt, nowMs);
  return `As of last capture, ${age === "now" ? "just now" : `${age} ago`}`;
}
