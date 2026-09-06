// kb desk — client for GET /api/desk (fleet-wide, or ?kb= for one corpus).
// Wire types are hand-written until the sibling server job lands generated
// ts-rs bindings; keep this shape lock-step with that endpoint.

import { currentDaemonBase } from "./base";

export type DeskReadState = "never-opened" | "unread" | "in_progress" | "read";

export type DeskItem = {
  kb: string;
  id: string;
  source_relative: string;
  title: string;
  updated_unix: number;
  expires_at?: number;
  session_id?: string;
  comments_open: number;
  comments_total: number;
  read_state: DeskReadState;
  last_opened_unix?: number;
  changed_since_read: boolean;
};

export type DeskResponse = {
  items: DeskItem[];
  attention: number;
};

export async function fetchDesk(
  kb?: string,
  signal?: AbortSignal,
): Promise<DeskResponse> {
  const params = new URLSearchParams();
  if (kb) params.set("kb", kb);
  const qs = params.toString();
  const r = await fetch(`${currentDaemonBase()}/api/desk${qs ? `?${qs}` : ""}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) {
    throw new Error(`/api/desk: ${r.status} ${r.statusText}`);
  }
  return (await r.json()) as DeskResponse;
}
