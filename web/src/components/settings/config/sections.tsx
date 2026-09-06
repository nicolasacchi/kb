// CE — per-section editors for the daemon config. Each is presentational:
// it takes its config slice, the full error map (JSON-pointer → message),
// optional extras (env presence, model registry), and an onChange that
// hands back the edited slice for the container to splice into the draft.

import {
  ATLAS_LAYOUTS,
  DECAY_POLICIES,
  MEMORY_SCOPES,
  VERSIONS_MODES,
  type CloudflareShareConfig,
  type DefaultsSection,
  type GithubShareConfig,
  type IndexerSection,
  type KbSection,
  type ServerSection,
  type ShareSection,
  type UiSection,
} from "../../../api/config";
import {
  NumberField,
  PairListField,
  pairsToRecord,
  recordToPairs,
  SelectField,
  StringListField,
  TextField,
  ToggleField,
} from "./fields";

export type Errors = Record<string, string>;

const errAt = (errors: Errors, pointer: string): string | undefined => errors[pointer];
// First error whose pointer starts with `prefix` (for list fields whose
// per-element pointers we surface at the field level).
const errUnder = (errors: Errors, prefix: string): string | undefined =>
  Object.entries(errors).find(([p]) => p.startsWith(prefix))?.[1];

function SectionShell({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="settings__section cfg__section" aria-label={title}>
      <h2 className="settings__h2">{title}</h2>
      {children}
    </section>
  );
}

function PresencePill({ name, present }: { name: string; present: boolean | undefined }) {
  if (present === undefined) {
    return <span className="cfg__present">env unknown — save to evaluate</span>;
  }
  return present ? (
    <span className="cfg__present cfg__present--set">{name} set ✓</span>
  ) : (
    <span className="cfg__present cfg__present--unset">{name} not set</span>
  );
}

export function ServerEditor({
  value,
  errors,
  onChange,
}: {
  value: ServerSection;
  errors: Errors;
  onChange: (v: ServerSection) => void;
}) {
  const rl = value.rate_limit ?? {};
  const setRl = (k: keyof NonNullable<ServerSection["rate_limit"]>, v: number | null) =>
    onChange({ ...value, rate_limit: { ...rl, [k]: v } });
  return (
    <SectionShell title="server">
      <TextField
        label="bind addr"
        value={value.addr}
        mono
        error={errAt(errors, "/server/addr")}
        hint="host:port — changing this restarts the daemon on a new socket"
        onChange={(addr) => onChange({ ...value, addr })}
      />
      <ToggleField
        label="mDNS"
        value={value.mdns}
        hint="advertise on the LAN (_kb._tcp)"
        onChange={(mdns) => onChange({ ...value, mdns })}
      />
      <TextField
        label="artifact suffix"
        value={value.artifact_host_suffix}
        mono
        onChange={(artifact_host_suffix) => onChange({ ...value, artifact_host_suffix })}
      />
      <TextField
        label="parent origin"
        value={value.parent_origin}
        mono
        onChange={(parent_origin) => onChange({ ...value, parent_origin })}
      />
      <StringListField
        label="trusted proxies"
        values={value.trusted_proxies}
        hint={errUnder(errors, "/server/trusted_proxies") ?? "reverse-proxy IPs that may set X-Forwarded-For"}
        onChange={(trusted_proxies) => onChange({ ...value, trusted_proxies })}
      />
      <h3 className="settings__h3 cfg__h3">rate limits (/min/token)</h3>
      <NumberField
        label="search"
        value={rl.search}
        min={1}
        error={errAt(errors, "/server/rate_limit/search")}
        onChange={(v) => setRl("search", v)}
      />
      <NumberField
        label="atlas recompute"
        value={rl.atlas_recompute}
        min={1}
        error={errAt(errors, "/server/rate_limit/atlas_recompute")}
        onChange={(v) => setRl("atlas_recompute", v)}
      />
      <NumberField
        label="review post"
        value={rl.review_post}
        min={1}
        error={errAt(errors, "/server/rate_limit/review_post")}
        onChange={(v) => setRl("review_post", v)}
      />
      <NumberField
        label="history post"
        value={rl.history_post}
        min={1}
        error={errAt(errors, "/server/rate_limit/history_post")}
        onChange={(v) => setRl("history_post", v)}
      />
    </SectionShell>
  );
}

export function IndexerEditor({
  value,
  onChange,
}: {
  value: IndexerSection;
  onChange: (v: IndexerSection) => void;
}) {
  return (
    <SectionShell title="indexer">
      <NumberField
        label="debounce ms"
        value={value.debounce_ms}
        hint="fs-event debounce window (default 400)"
        onChange={(debounce_ms) => onChange({ ...value, debounce_ms })}
      />
      <NumberField
        label="reconcile secs"
        value={value.reconcile_secs}
        hint="periodic walk interval; 0 disables (default 60)"
        onChange={(reconcile_secs) => onChange({ ...value, reconcile_secs })}
      />
      <NumberField
        label="indexer nice"
        value={value.indexer_nice}
        hint="embedder subprocess nice 0–19 (default 20→19)"
        onChange={(indexer_nice) => onChange({ ...value, indexer_nice })}
      />
    </SectionShell>
  );
}

export function DefaultsEditor({
  value,
  models,
  onChange,
}: {
  value: DefaultsSection;
  models: string[];
  onChange: (v: DefaultsSection) => void;
}) {
  return (
    <SectionShell title="defaults">
      <SelectField
        label="embedding model"
        value={value.embedding_model}
        options={models}
        emptyLabel="(registry default)"
        onChange={(embedding_model) => onChange({ ...value, embedding_model })}
      />
      <ToggleField
        label="disable embedder fallback"
        value={value.disable_embedder_fallback}
        hint="run lexical-only when no model is configured"
        onChange={(disable_embedder_fallback) => onChange({ ...value, disable_embedder_fallback })}
      />
    </SectionShell>
  );
}

export function UiEditor({
  value,
  onChange,
  title = "ui",
}: {
  value: UiSection;
  onChange: (v: UiSection) => void;
  title?: string;
}) {
  return (
    <SectionShell title={title}>
      <TextField
        label="theme"
        value={value.theme ?? ""}
        hint="paper | ink"
        onChange={(t) => onChange({ ...value, theme: t === "" ? null : t })}
      />
      <TextField
        label="accent"
        value={value.accent ?? ""}
        onChange={(a) => onChange({ ...value, accent: a === "" ? null : a })}
      />
      <TextField
        label="density"
        value={value.density ?? ""}
        hint="compact | normal | spacious"
        onChange={(d) => onChange({ ...value, density: d === "" ? null : d })}
      />
    </SectionShell>
  );
}

export function ShareEditor({
  value,
  envPresent,
  onChange,
}: {
  value: ShareSection;
  envPresent: Record<string, boolean>;
  onChange: (v: ShareSection) => void;
}) {
  const cf = value.cloudflare ?? null;
  const gh = value.github ?? null;
  const setCf = (next: CloudflareShareConfig | null) => onChange({ ...value, cloudflare: next });
  const setGh = (next: GithubShareConfig | null) => onChange({ ...value, github: next });
  return (
    <SectionShell title="share">
      <TextField
        label="live origin"
        value={value.live_origin ?? ""}
        mono
        hint="e.g. https://kb.example.com"
        onChange={(v) => onChange({ ...value, live_origin: v === "" ? null : v })}
      />

      <ToggleField
        label="cloudflare pages"
        value={cf !== null}
        onChange={(on) =>
          setCf(
            on
              ? {
                  account_id: "",
                  team_domain: "",
                  google_idp: null,
                  github_idp: null,
                  api_token_env: "KB_CF_API_TOKEN",
                }
              : null,
          )
        }
      />
      {cf && (
        <div className="cfg__subsection">
          <TextField label="account id" value={cf.account_id} mono onChange={(v) => setCf({ ...cf, account_id: v })} />
          <TextField label="team domain" value={cf.team_domain} mono onChange={(v) => setCf({ ...cf, team_domain: v })} />
          <TextField
            label="google idp"
            value={cf.google_idp ?? ""}
            mono
            onChange={(v) => setCf({ ...cf, google_idp: v === "" ? null : v })}
          />
          <TextField
            label="github idp"
            value={cf.github_idp ?? ""}
            mono
            onChange={(v) => setCf({ ...cf, github_idp: v === "" ? null : v })}
          />
          <TextField
            label="API token env"
            value={cf.api_token_env}
            mono
            hint="env var NAME the daemon reads the token from — never the value"
            onChange={(v) => setCf({ ...cf, api_token_env: v })}
          />
          <div className="settings__field cfg__field">
            <span className="settings__label">token status</span>
            <PresencePill name={cf.api_token_env} present={envPresent[cf.api_token_env]} />
          </div>
        </div>
      )}

      <ToggleField
        label="github pages"
        value={gh !== null}
        onChange={(on) => setGh(on ? { owner: "", token_env: "KB_GH_TOKEN" } : null)}
      />
      {gh && (
        <div className="cfg__subsection">
          <TextField label="owner" value={gh.owner} mono onChange={(v) => setGh({ ...gh, owner: v })} />
          <TextField
            label="token env"
            value={gh.token_env}
            mono
            hint="env var NAME — never the value"
            onChange={(v) => setGh({ ...gh, token_env: v })}
          />
          <div className="settings__field cfg__field">
            <span className="settings__label">token status</span>
            <PresencePill name={gh.token_env} present={envPresent[gh.token_env]} />
          </div>
        </div>
      )}
    </SectionShell>
  );
}

// --- per-kb ---

export function PerKbEditor({
  name,
  value,
  errors,
  models,
  onChange,
}: {
  name: string;
  value: KbSection;
  errors: Errors;
  models: string[];
  onChange: (v: KbSection) => void;
}) {
  const ptr = (suffix: string) => `/kb/${name}/${suffix}`;
  const atlas = value.atlas ?? null;
  const outbound = value.outbound ?? null;
  const resurface = value.resurface ?? null;
  return (
    <details className="cfg__kb-card">
      <summary className="cfg__kb-summary">
        <span className="cfg__kb-name">{name}</span>
        <span className="cfg__kb-path">{value.path}</span>
      </summary>
      <div className="cfg__kb-body">
        <TextField
          label="path"
          value={value.path}
          mono
          error={errAt(errors, ptr("path"))}
          onChange={(path) => onChange({ ...value, path })}
        />
        <SelectField
          label="embedding model"
          value={value.embedding_model}
          options={models}
          emptyLabel="(defaults / registry)"
          error={errAt(errors, ptr("embedding_model"))}
          onChange={(embedding_model) => onChange({ ...value, embedding_model })}
        />
        <SelectField
          label="memory scope"
          value={value.memory_scope}
          options={MEMORY_SCOPES}
          emptyLabel="(not a memory corpus)"
          onChange={(memory_scope) => onChange({ ...value, memory_scope })}
        />
        <SelectField
          label="decay policy"
          value={value.decay_policy}
          options={DECAY_POLICIES}
          emptyLabel="(daemon default)"
          onChange={(decay_policy) => onChange({ ...value, decay_policy })}
        />
        <SelectField
          label="versions"
          value={value.versions}
          options={VERSIONS_MODES}
          emptyLabel="(auto)"
          onChange={(versions) => onChange({ ...value, versions })}
        />
        <StringListField
          label="skip patterns"
          values={value.skip_patterns}
          placeholder="*.tmp"
          onChange={(skip_patterns) => onChange({ ...value, skip_patterns })}
        />

        <ToggleField
          label="atlas overrides"
          value={atlas !== null}
          onChange={(on) => onChange({ ...value, atlas: on ? { k: null, layout: null } : null })}
        />
        {atlas && (
          <div className="cfg__subsection">
            <NumberField
              label="clusters (k)"
              value={atlas.k}
              min={1}
              onChange={(k) => onChange({ ...value, atlas: { ...atlas, k } })}
            />
            <SelectField
              label="layout"
              value={atlas.layout}
              options={ATLAS_LAYOUTS}
              emptyLabel="(umap)"
              onChange={(layout) => onChange({ ...value, atlas: { ...atlas, layout } })}
            />
          </div>
        )}

        <ToggleField
          label="resurface weights"
          value={resurface !== null}
          hint="tuning knobs for the resurfacing queue's scoring — leave a field blank to keep its shipped default"
          onChange={(on) =>
            onChange({
              ...value,
              resurface: on
                ? {
                    comment_weight: null,
                    read_weight: null,
                    comment_saturation: null,
                    read_halflife_days: null,
                    score_floor: null,
                  }
                : null,
            })
          }
        />
        {resurface && (
          <div className="cfg__subsection">
            <NumberField
              label="comment weight"
              value={resurface.comment_weight}
              min={0}
              placeholder="0.6"
              error={errAt(errors, ptr("resurface/comment_weight"))}
              onChange={(comment_weight) =>
                onChange({ ...value, resurface: { ...resurface, comment_weight } })
              }
            />
            <NumberField
              label="read weight"
              value={resurface.read_weight}
              min={0}
              placeholder="0.4"
              error={errAt(errors, ptr("resurface/read_weight"))}
              onChange={(read_weight) =>
                onChange({ ...value, resurface: { ...resurface, read_weight } })
              }
            />
            <NumberField
              label="comment saturation"
              value={resurface.comment_saturation}
              min={1}
              placeholder="4"
              error={errAt(errors, ptr("resurface/comment_saturation"))}
              onChange={(comment_saturation) =>
                onChange({ ...value, resurface: { ...resurface, comment_saturation } })
              }
            />
            <NumberField
              label="read half-life (days)"
              value={resurface.read_halflife_days}
              min={0}
              placeholder="45"
              error={errAt(errors, ptr("resurface/read_halflife_days"))}
              onChange={(read_halflife_days) =>
                onChange({ ...value, resurface: { ...resurface, read_halflife_days } })
              }
            />
            <NumberField
              label="score floor"
              value={resurface.score_floor}
              min={0}
              placeholder="0.05"
              error={errAt(errors, ptr("resurface/score_floor"))}
              onChange={(score_floor) =>
                onChange({ ...value, resurface: { ...resurface, score_floor } })
              }
            />
          </div>
        )}

        <ToggleField
          label="outbound scrub"
          value={outbound !== null}
          onChange={(on) =>
            onChange({
              ...value,
              outbound: on ? { strip_kb_prompt: false, redactions: [] } : null,
            })
          }
        />
        {outbound && (
          <div className="cfg__subsection">
            <ToggleField
              label="strip kb-prompt"
              value={outbound.strip_kb_prompt}
              onChange={(strip_kb_prompt) =>
                onChange({ ...value, outbound: { ...outbound, strip_kb_prompt } })
              }
            />
            <PairListField
              label="redactions"
              pairs={outbound.redactions.map((r) => ({ a: r.pattern, b: r.replacement }))}
              aPlaceholder="regex pattern"
              bPlaceholder="replacement"
              hint={errUnder(errors, ptr("outbound/redactions")) ?? undefined}
              onChange={(pairs) =>
                onChange({
                  ...value,
                  outbound: {
                    ...outbound,
                    redactions: pairs.map((p) => ({ pattern: p.a, replacement: p.b })),
                  },
                })
              }
            />
          </div>
        )}

        <PairListField
          label="templates"
          pairs={recordToPairs(value.templates)}
          aPlaceholder="name"
          bPlaceholder="/path/to/template.html"
          onChange={(pairs) => onChange({ ...value, templates: pairsToRecord(pairs) })}
        />
      </div>
    </details>
  );
}
