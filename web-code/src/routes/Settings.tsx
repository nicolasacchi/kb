// OL-C8 — kb-code's settings page: "how this instrument is set up", in one
// calm column. Top-level and repo-less (`/~settings`, a sibling of `/~inbox`).
// Read-only except what already writes today: the theme / contrast / density /
// keymap prefs are browser-local (`lib/prefs.ts`), and no new server route or
// mutation exists. Facts are label/value rows, not tiles.

import { useState, type ReactNode } from "react";
import { StateCard } from "@kb/ui/StateCard";
import type { IdentityOut, RepoListEntry } from "../api/types";
import { useCommands } from "../commands/CommandRoot";
import { KBC_PRESETS, type KbcPreset } from "../commands/registry.gen";
import { ReviewCredentialSection, ReviewStoreSection } from "../components/home/ReviewStoreSection";
import MetaLine from "../components/MetaLine";
import PageHeader from "../components/PageHeader";
import ThemePicker from "../components/ThemePicker";
import { useIdentity } from "../hooks/useIdentity";
import { useRepos } from "../hooks/useRepos";
import { effectiveIntelProviders } from "../lib/diagnostics";
import { loadKeyPreset, saveKeyPreset } from "../lib/prefs";
import "../styles/home.css";
import "../styles/settings.css";

function Row({ label, children, attr }: { label: string; children: ReactNode; attr?: string }) {
  return (
    <div className="kbc-settings__row" {...(attr ? { [attr]: true } : {})}>
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/// "3d 4h" / "12m" from an RFC 3339 start; null when absent or unparseable
/// (an older daemon, or a clock we cannot trust, prints no uptime at all).
export function uptimeLabel(startedAt: string | undefined, now: number = Date.now()): string | null {
  if (!startedAt) return null;
  const t = Date.parse(startedAt);
  if (Number.isNaN(t)) return null;
  const secs = Math.max(0, Math.floor((now - t) / 1000));
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function headLabel(head: RepoListEntry["head"]): string {
  if (!head || head.unborn) return head?.unborn ? "no commits yet" : "unknown";
  const sha = head.sha ? head.sha.slice(0, 8) : "";
  if (head.detached) return `detached @ ${sha}`;
  return `${head.branch ?? "(no branch)"}${sha ? ` @ ${sha}` : ""}`;
}

export function AppearanceSection() {
  return (
    <section className="kbc-settings__section" data-kbc-settings-section="appearance">
      <h2 className="kbc-settings__h2">Appearance</h2>
      <p className="kbc-settings__note">Saved in this browser only.</p>
      <ThemePicker inline />
    </section>
  );
}

export function KeyboardSection({
  onPreset,
  onShowHelp,
}: {
  onPreset?: (p: KbcPreset) => void;
  onShowHelp?: () => void;
}) {
  const [preset, setPreset] = useState<KbcPreset>(() => loadKeyPreset());
  return (
    <section className="kbc-settings__section" data-kbc-settings-section="keyboard">
      <h2 className="kbc-settings__h2">Keyboard</h2>
      <dl className="kbc-settings__rows">
        <Row label="Keymap profile">
          <span className="kbc-settings__seg" role="group" aria-label="keymap profile">
            {KBC_PRESETS.map((p) => (
              <button
                key={p.id}
                type="button"
                className="kbc-settings__segbtn"
                data-kbc-settings-preset={p.id}
                aria-pressed={preset === p.id}
                onClick={() => {
                  setPreset(p.id as KbcPreset);
                  saveKeyPreset(p.id as KbcPreset);
                  onPreset?.(p.id as KbcPreset);
                }}
              >
                {p.title}
              </button>
            ))}
          </span>
        </Row>
        <Row label="Reference">
          <button
            type="button"
            className="kbc-settings__link"
            data-kbc-settings-keys
            onClick={() => onShowHelp?.()}
          >
            Show all keys (<kbd>?</kbd>)
          </button>
        </Row>
      </dl>
    </section>
  );
}

export function RepositoriesSection({
  repos,
  loading,
  error,
}: {
  repos: RepoListEntry[];
  loading?: boolean;
  error?: Error | null;
}) {
  return (
    <section className="kbc-settings__section" data-kbc-settings-section="repositories">
      <h2 className="kbc-settings__h2">Repositories</h2>
      {loading ? (
        <StateCard size="inline" kind="loading" title="Loading repos…" />
      ) : error ? (
        <StateCard size="inline" kind="degraded" title="Couldn't load repos." reason={error.message} />
      ) : repos.length === 0 ? (
        <p className="kbc-settings__note">
          No repos configured. Add one under <code>[[repos]]</code> in <code>kb-code.toml</code>.
        </p>
      ) : (
        repos.map((r) => {
          const providers = effectiveIntelProviders(r);
          return (
            <div key={r.name} className="kbc-settings__repo" data-kbc-settings-repo={r.name}>
              <h3 className="kbc-settings__h3">{r.name}</h3>
              <dl className="kbc-settings__rows">
                <Row label="Path">
                  <code>{r.path}</code>
                </Row>
                <Row label="Indexed">
                  {r.file_count} files · {r.symbol_count} symbols
                </Row>
                <Row label="HEAD">{headLabel(r.head)}</Row>
                <Row label="Watcher">{r.watcher}</Row>
                <Row label="Intel providers">
                  {providers.length === 0
                    ? "none"
                    : providers
                        .map(
                          (p) =>
                            `${p.provider}${p.langs.length ? ` (${p.langs.join(", ")})` : ""} · ${
                              p.alive ? "running" : "not running"
                            }`,
                        )
                        .join("; ")}
                </Row>
              </dl>
              {r.catching_up && (
                <StateCard
                  size="inline"
                  kind="degraded"
                  title="Indexing the mirror…"
                  reason="Search results may be incomplete until it settles."
                />
              )}
            </div>
          );
        })
      )}
    </section>
  );
}

export function ReviewStoreBlock({
  repos,
  remote,
}: {
  repos: RepoListEntry[];
  remote: boolean | undefined;
}) {
  return (
    <section className="kbc-settings__section" data-kbc-settings-section="review-store">
      <h2 className="kbc-settings__h2">Review store</h2>
      {/* `remote_mutations` is optional on the wire: only rendered when the
          daemon actually reports it, never a fabricated "off". */}
      {remote !== undefined && (
        <p className="kbc-settings__line" data-kbc-home-remote-mutations={remote}>
          Remote review mutations: {remote ? "on" : "off"}
        </p>
      )}
      {repos.map((r) => (
        <div key={r.name} className="kbc-settings__repo" data-kbc-home-status-repo={r.name}>
          <h3 className="kbc-settings__h3">{r.name}</h3>
          <ReviewStoreSection repo={r.name} />
          <ReviewCredentialSection repo={r.name} />
        </div>
      ))}
    </section>
  );
}

export function SiblingSection({ identity }: { identity: IdentityOut | undefined }) {
  const enabled = identity?.kb_daemon_enabled;
  const url = identity?.kb_public_url;
  return (
    <section className="kbc-settings__section" data-kbc-settings-section="sibling">
      <h2 className="kbc-settings__h2">Sibling</h2>
      {!identity ? (
        <p className="kbc-settings__note">The daemon has not reported its identity yet.</p>
      ) : (
        <dl className="kbc-settings__rows">
          <Row label="kb daemon" attr="data-kbc-settings-kb-link">
            {enabled === undefined
              ? "not reported by this daemon"
              : enabled
                ? "linked"
                : "not configured"}
          </Row>
          {url !== undefined && (
            <Row label="kb public URL">{url === "" ? "none" : <code>{url}</code>}</Row>
          )}
        </dl>
      )}
    </section>
  );
}

export default function Settings() {
  const identity = useIdentity();
  const repos = useRepos();
  const bus = useCommands();
  const list = repos.data?.repos ?? [];
  const id = identity.data;
  return (
    <div className="kbc-settings" id="main" data-kbc-settings>
      <PageHeader title="Settings" lede="How this instrument is set up." />
      <MetaLine
        items={[
          id && <span data-kbc-settings-version>kb-code {id.version}</span>,
          id?.build_sha && <span>build {id.build_sha.slice(0, 8)}</span>,
          id?.sibling_protocol && (
            <span>
              {id.sibling_protocol}
              {id.sibling_major !== undefined ? ` v${id.sibling_major}` : ""}
            </span>
          ),
          uptimeLabel(id?.started_at) && <span>up {uptimeLabel(id?.started_at)}</span>,
        ]}
      />
      <AppearanceSection />
      <KeyboardSection
        onPreset={(p) => bus.setPreset(p)}
        onShowHelp={() => bus.run("help.keys")}
      />
      <RepositoriesSection
        repos={list}
        loading={repos.isLoading}
        error={(repos.error as Error | null) ?? null}
      />
      <ReviewStoreBlock repos={list} remote={id?.remote_mutations} />
      <SiblingSection identity={id} />
    </div>
  );
}
