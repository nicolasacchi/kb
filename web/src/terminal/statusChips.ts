// BHEAD — the typed status the live-status row carries (model, context %, cost,
// permission mode, git branch) as header chips. Pure: the panel reads the row
// through the existing live-status query; nothing here scrapes terminal bytes.
// An absent field is an absent chip; the header is the single home of these
// chips (D7).

import type { LiveStatusInfo } from "../api/generated/LiveStatusInfo";

export interface StatusChip {
  /** Stable key + `data-chip` attribute. */
  id: "model" | "context" | "cost" | "mode" | "branch";
  /** Plain text, never HTML (rendered as a React text child). */
  label: string;
  /** What the chip is, where it came from, and how fresh it can be. */
  title: string;
}

const SOURCE =
  "reported by the harness hook, as of the last live-status poll (updates on session activity, polled every 10 s)";

const describe = (what: string): string => `${what}, ${SOURCE}`;

/** The chips for one live-status `status`, in display order. */
export function statusChips(info: LiveStatusInfo | undefined | null): StatusChip[] {
  if (!info) return [];
  const out: StatusChip[] = [];
  if (typeof info.model === "string" && info.model !== "") {
    out.push({ id: "model", label: info.model, title: describe("Model") });
  }
  if (typeof info.context_pct === "number" && Number.isFinite(info.context_pct)) {
    out.push({
      id: "context",
      label: `ctx ${Math.round(info.context_pct)}%`,
      title: describe("Context window used"),
    });
  }
  if (typeof info.cost_usd === "number" && Number.isFinite(info.cost_usd)) {
    out.push({ id: "cost", label: `$${info.cost_usd.toFixed(2)}`, title: describe("Session cost so far") });
  }
  const mode = info.permission_mode;
  if (mode === "plan" || mode === "accept" || mode === "bypass") {
    out.push({ id: "mode", label: mode, title: describe("Permission mode") });
  }
  if (typeof info.branch === "string" && info.branch !== "") {
    out.push({ id: "branch", label: info.branch, title: describe("Git branch") });
  }
  return out;
}
