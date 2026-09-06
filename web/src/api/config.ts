// CE — client for GET/PUT /api/config (the daemon's kb.toml editor).
//
// Types mirror `crates/kb-core/src/config.rs`: Rust `Option<T>` → `T | null`,
// `BTreeMap<KbName, KbSection>` → `Record<string, KbSection>`, `PathBuf` →
// `string`. The editor sends the WHOLE config back on PUT (the daemon
// rewrites the file), so callers must deep-merge edits onto the pristine
// GET payload to avoid dropping fields this type doesn't know about — see
// `mergeConfig`.

import { currentDaemonBase } from "./base";

export type RateLimit = {
  search?: number | null;
  atlas_recompute?: number | null;
  review_post?: number | null;
  history_post?: number | null;
};

export type ServerSection = {
  addr: string;
  mdns: boolean;
  artifact_host_suffix: string;
  parent_origin: string;
  trusted_proxies: string[];
  rate_limit?: RateLimit | null;
};

export type DaemonSection = { name?: string | null };

export type UiSection = {
  theme?: string | null;
  accent?: string | null;
  density?: string | null;
};

export type IndexerSection = {
  debounce_ms?: number | null;
  reconcile_secs?: number | null;
  indexer_nice?: number | null;
};

export type DefaultsSection = {
  embedding_model?: string | null;
  disable_embedder_fallback: boolean;
};

export type CloudflareShareConfig = {
  account_id: string;
  team_domain: string;
  google_idp?: string | null;
  github_idp?: string | null;
  api_token_env: string; // env-var NAME, never the secret
};

export type GithubShareConfig = {
  owner: string;
  token_env: string; // env-var NAME, never the secret
};

export type ShareSection = {
  cloudflare?: CloudflareShareConfig | null;
  github?: GithubShareConfig | null;
  live_origin?: string | null;
};

export type RegexRule = { pattern: string; replacement: string };
export type OutboundSection = { strip_kb_prompt: boolean; redactions: RegexRule[] };
export type AtlasSection = { k?: number | null; layout?: string | null };

// W2.9 — `[kb.<name>.resurface]`: per-kb override of the resurfacing
// queue's scoring weights (kb_core::resurface::ResurfaceWeights). All five
// fields independently optional; an absent field keeps the shipped
// default for just that knob (mirrors `AtlasSection`).
export type ResurfaceSection = {
  comment_weight?: number | null;
  read_weight?: number | null;
  comment_saturation?: number | null;
  read_halflife_days?: number | null;
  score_floor?: number | null;
};

export type KbSection = {
  path: string;
  skip_patterns: string[];
  ui: UiSection;
  embedding_model?: string | null;
  // Present on the Rust struct (config.rs) but were missing here, so a
  // hand-built new-kb entry omitted them; both are #[serde(default)] (null /
  // false) which is the correct default for a fresh corpus (A2).
  reranker_model?: string | null;
  chunked_embeddings?: boolean;
  outbound?: OutboundSection | null;
  atlas?: AtlasSection | null;
  templates: Record<string, string>;
  memory_scope?: string | null;
  decay_policy?: string | null;
  versions?: string | null;
  resurface?: ResurfaceSection | null;
};

export type DaemonConfig = {
  daemon: DaemonSection;
  server: ServerSection;
  ui: UiSection;
  indexer: IndexerSection;
  share: ShareSection;
  defaults: DefaultsSection;
  kb: Record<string, KbSection>;
};

export const ATLAS_LAYOUTS = ["umap", "pca"] as const;
export const DECAY_POLICIES = ["strict", "balanced", "loose"] as const;
export const MEMORY_SCOPES = ["global", "project"] as const;
// Track V — per-kb version-timeline source for the artifact Versions/Diff panel.
export const VERSIONS_MODES = ["auto", "git", "index", "both", "off"] as const;

export type ConfigGetResponse = {
  config: DaemonConfig;
  config_path: string;
  /// Presence (not value) of each configured share token env var.
  env_present: Record<string, boolean>;
  /// Embedding-model registry names for the per-kb model select.
  embedding_models: string[];
};

export type FieldError = { pointer: string; detail: string };

/// Thrown by `saveDaemonConfig` on a 400/422 with field-level issues, so
/// the editor can map each back to its input. `message` is the flat
/// summary; `fields` is the per-pointer detail.
export class ConfigValidationError extends Error {
  fields: FieldError[];
  constructor(message: string, fields: FieldError[]) {
    super(message);
    this.name = "ConfigValidationError";
    this.fields = fields;
  }
}

export type SaveConfigResponse = {
  restarting: boolean;
  config_path: string;
  warnings: FieldError[];
};

export async function fetchDaemonConfig(signal?: AbortSignal): Promise<ConfigGetResponse> {
  const r = await fetch(`${currentDaemonBase()}/api/config`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) {
    const ct = r.headers.get("content-type") ?? "";
    if (ct.includes("application/problem+json")) {
      const p = (await r.json()) as { title?: string; detail?: string };
      throw new Error(`${p.title ?? "Error"}: ${p.detail ?? ""}`);
    }
    throw new Error(`${r.status} ${r.statusText}`);
  }
  return (await r.json()) as ConfigGetResponse;
}

export async function saveDaemonConfig(config: DaemonConfig): Promise<SaveConfigResponse> {
  const r = await fetch(`${currentDaemonBase()}/api/config`, {
    method: "PUT",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(config),
  });
  if (!r.ok) {
    const ct = r.headers.get("content-type") ?? "";
    if (ct.includes("application/problem+json")) {
      const p = (await r.json()) as {
        title?: string;
        detail?: string;
        errors?: FieldError[];
      };
      const msg = `${p.title ?? "Error"}: ${p.detail ?? ""}`;
      throw new ConfigValidationError(msg, p.errors ?? []);
    }
    throw new Error(`${r.status} ${r.statusText}`);
  }
  return (await r.json()) as SaveConfigResponse;
}

/// Deep-merge `draft` onto `base`, authoritative for `draft`'s key set:
/// object values merge recursively, but a key present in `base` and ABSENT
/// from `draft` is DROPPED, so an editor deletion (e.g. removing a per-kb
/// `templates` entry — a `Record<string,string>`) actually persists. Arrays
/// + scalars (and explicit `null`s) from `draft` replace `base` wholesale.
///
/// This is safe for forward-compat: `draft` is a deep `structuredClone` of
/// the pristine server GET (DaemonConfig.tsx) and every sub-editor emits
/// `{ ...value, field }`, so the draft always carries unknown/future server
/// fields — the only keys ever missing are intentional deletions. The server
/// then drops any key absent from the PUT body (config.rs `merge_changes`),
/// completing the round-trip.
///
/// (The previous implementation spread `base` first and only overwrote
/// draft-present keys, so deletions of any `Record`-typed field were
/// silently re-merged back in and never persisted.)
///
/// Aliasing note: the result shares nested object references with `draft`
/// (draft-only subtrees and the scalar/`null`-base fallback return the live
/// ref, not a clone). That's safe because the sole caller serialises the
/// result immediately (`saveDaemonConfig` → `JSON.stringify`); a caller that
/// instead mutates the returned config in place would corrupt the draft
/// React state, so deep-clone first if you need an independent copy.
export function mergeConfig<T>(base: T, draft: T): T {
  if (
    base &&
    draft &&
    typeof base === "object" &&
    typeof draft === "object" &&
    !Array.isArray(base) &&
    !Array.isArray(draft)
  ) {
    const b = base as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const [k, dv] of Object.entries(draft as Record<string, unknown>)) {
      out[k] = k in b ? mergeConfig(b[k], dv) : dv;
    }
    return out as T;
  }
  return draft;
}
