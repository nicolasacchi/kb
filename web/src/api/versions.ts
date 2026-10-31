// Track V — client for the artifact version timeline + diff
// (GET /api/kb/{kb}/artifacts/{id}/{versions,diff}). Mirrors the
// sessions client's raw-fetch + currentDaemonBase shape, but throws on
// error (parsing the problem+json `detail`) so the panel can surface
// "git not available" / "snapshot not found" instead of silently empty.

import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (routes/versions.rs +
// kb-core vcs.rs/versions.rs are the source of truth; `just types`
// regenerates — including the literal unions for `source` and `mode`).
import type { Version } from "./generated/Version";
import type { VersionsResponse } from "./generated/VersionsResponse";
import type { DiffTag } from "./generated/DiffTag";
import type { DiffLine } from "./generated/DiffLine";
import type { DiffHunk } from "./generated/DiffHunk";
import type { DiffResponse } from "./generated/DiffResponse";
import type { SinceVisitResponse } from "./generated/SinceVisitResponse";
import type { ChangedSection } from "./generated/ChangedSection";
import type { MementoOut } from "./generated/MementoOut";

export type {
  Version,
  VersionsResponse,
  DiffTag,
  DiffLine,
  DiffHunk,
  DiffResponse,
  SinceVisitResponse,
  ChangedSection,
  MementoOut,
};
export type VersionSource = Version["source"];

async function jsonOrThrow<T>(r: Response): Promise<T> {
  if (!r.ok) {
    let detail = `${r.status} ${r.statusText}`;
    try {
      const body = (await r.json()) as { detail?: unknown };
      if (typeof body.detail === "string") detail = body.detail;
    } catch {
      /* non-JSON error body — keep the status line */
    }
    throw new Error(detail);
  }
  return (await r.json()) as T;
}

export async function fetchVersions(
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<VersionsResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}/versions`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrThrow<VersionsResponse>(r);
}

export async function fetchDiff(
  kb: string,
  id: string,
  from: string,
  to: string,
  mode: "text" | "raw",
  signal?: AbortSignal,
): Promise<DiffResponse> {
  const params = new URLSearchParams({ from, to, mode });
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}/diff?${params.toString()}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrThrow<DiffResponse>(r);
}

/// OL IDEA2 — what changed in this artifact since the caller's last WEB
/// visit (`GET …/since-visit`). Visit, never "read": agent CLI opens do not
/// count (#19). Always 200 once the artifact exists; failures inside the
/// route come back as `state: "unavailable"`.
export async function fetchSinceVisit(
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<SinceVisitResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}/since-visit`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrThrow<SinceVisitResponse>(r);
}
