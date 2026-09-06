import { useEffect, useState } from "react";
import {
  fetchFolders,
  isAbortError,
  type FolderNode,
  type SearchMode,
  type SearchScope,
} from "../../api/client";
import FolderTree from "../FolderTree";
import { useTags } from "../../hooks/useTags";
import FilterSection from "./rail/FilterSection";
import RailToggleGroup, { type ToggleOption } from "./rail/RailToggleGroup";
import TagFilter from "./rail/TagFilter";
import SessionPicker from "./rail/SessionPicker";
import ListPicker from "./rail/ListPicker";
import TemporalScrubber from "./TemporalScrubber";

const MODES: { value: SearchMode; label: string }[] = [
  { value: "hybrid", label: "hybrid" },
  { value: "keyword", label: "keyword" },
  { value: "semantic", label: "semantic" },
];
const LIMITS = [20, 50, 100, 200];
const READ_OPTS: ToggleOption[] = [
  { value: "unread", label: "unread" },
  { value: "in_progress", label: "in progress" },
  { value: "read", label: "read" },
];
const CAP_OPTS: ToggleOption[] = [
  { value: "svg", label: "svg" },
  { value: "interactive", label: "interactive" },
  { value: "code", label: "code" },
  { value: "longread", label: "longread" },
];
const SINCE_OPTS: ToggleOption[] = [
  { value: "day", label: "day" },
  { value: "week", label: "week" },
  { value: "month", label: "month" },
  { value: "year", label: "year" },
];
const SINCE_FIELD_OPTS: ToggleOption[] = [
  { value: "modified", label: "modified" },
  { value: "created", label: "created" },
];

export type SearchRailProps = {
  mode: SearchMode;
  scope: SearchScope;
  category: string;
  folder: string;
  limit: number;
  read: string[];
  tags: string[];
  excludeTags: string[];
  status: string[];
  severity: string[];
  caps: string[];
  since: string;
  sinceField: "created" | "modified";
  session: string;
  list: string;
  // W1.search — "read during" window (unix seconds; a history-opens
  // window, distinct from the Date section's mtime/created recency).
  readFrom: number | null;
  readTo: number | null;
  activeKb?: string;
  // R0-opt-in — the active kb's configured `[kb.foo] default_search_category`
  // (architecture invariant #11), e.g. `"memory-session"` on a sessions
  // corpus. `undefined`/`null` when the kb has none configured. Drives the
  // "include session transcripts" toggle's checked state below.
  defaultSearchCategory?: string | null;
  onMode: (m: SearchMode) => void;
  onScope: (s: SearchScope) => void;
  onLimit: (n: number) => void;
  // Generic URL writers from the route's useUrl — leaf facets map straight
  // to params (single value via `set`, related pairs via `setMany`).
  set: (key: string, value: string | null) => void;
  setMany: (patch: Record<string, string | null>) => void;
};

// R0-opt-in checked-state derivation (architecture invariant #11): the
// "include session transcripts" toggle reflects the EFFECTIVE outcome, not
// merely "is `category` literally present" — true when the SPA explicitly
// asked for it (`category === "memory-session"`), OR when the request is
// single-kb, `category` is genuinely absent, and the active kb is
// configured with `default_search_category === "memory-session"` (the
// server is already applying it silently — showing unchecked here would
// misrepresent what's on screen). Exported as a pure function so the
// derivation can be unit-tested in isolation from rendering.
export function isSessionsIncluded(
  category: string,
  scope: SearchScope,
  defaultSearchCategory: string | null | undefined,
): boolean {
  if (category === "memory-session") return true;
  return (
    scope === "one" &&
    category === "" &&
    defaultSearchCategory === "memory-session"
  );
}

// FS2/FS3 — the search page's filter rail. Mode/scope/results stay plain
// always-visible sections; every real facet (read-state, tags, category,
// status, severity, date, capabilities, folder) is a collapsible
// FilterSection with an active-count badge. Tags + folder are single-kb
// (per-corpus), hidden under scope=all. The rail is controlled: the route
// owns useUrl and passes values + `set`/`setMany`; leaves never fetch URL
// state themselves (keeps the scope-reset batching in one place).
export default function SearchRail({
  mode,
  scope,
  category,
  folder,
  limit,
  read,
  tags,
  excludeTags,
  status,
  severity,
  caps,
  since,
  sinceField,
  session,
  list,
  readFrom,
  readTo,
  activeKb,
  defaultSearchCategory,
  onMode,
  onScope,
  onLimit,
  set,
  setMany,
}: SearchRailProps) {
  const [folders, setFolders] = useState<FolderNode[]>([]);
  useEffect(() => {
    if (!activeKb || scope === "all") {
      setFolders([]);
      return;
    }
    const ctl = new AbortController();
    fetchFolders(activeKb, ctl.signal)
      .then((r) => setFolders(r.folders))
      .catch((e) => {
        if (!isAbortError(e)) setFolders([]);
      });
    return () => ctl.abort();
  }, [activeKb, scope]);

  // Tag facet list is per-corpus → only fetch in single-kb scope.
  const allTags = useTags(scope === "one" ? activeKb : undefined);

  const readSet = new Set(read);
  const capsSet = new Set(caps);
  const includeSet = new Set(tags);
  const excludeSet = new Set(excludeTags);
  const sessionsChecked = isSessionsIncluded(
    category,
    scope,
    defaultSearchCategory,
  );
  // Include-only wire contract (architecture invariant #11): unchecking
  // just removes the explicit `category` param. If the active kb has
  // `default_search_category` configured, the server keeps applying it —
  // there is no "explicitly exclude a category" concept, so this can't
  // force sessions off, only stop asking for them explicitly.
  const onSessionsToggle = () =>
    set("category", sessionsChecked ? null : "memory-session");

  // Toggle a value in a csv-backed multi-select param (default-out at empty).
  const toggleCsv = (key: string, current: string[], value: string) => {
    const s = new Set(current);
    if (s.has(value)) s.delete(value);
    else s.add(value);
    set(key, s.size ? [...s].join(",") : null);
  };

  // Tristate tag chip: none → include → exclude → none. Writes both params
  // in one navigate so the pair never half-updates.
  const cycleTag = (name: string) => {
    const inc = new Set(tags);
    const exc = new Set(excludeTags);
    if (inc.has(name)) {
      inc.delete(name);
      exc.add(name);
    } else if (exc.has(name)) {
      exc.delete(name);
    } else {
      inc.add(name);
    }
    setMany({
      tags: inc.size ? [...inc].join(",") : null,
      exclude_tags: exc.size ? [...exc].join(",") : null,
    });
  };

  return (
    <aside className="kb-search-rail" aria-label="search filters">
      <div className="kb-search-rail__sect">Mode</div>
      <div className="kb-search-rail__seg">
        {MODES.map((m) => (
          <button
            key={m.value}
            type="button"
            className={mode === m.value ? "on" : ""}
            onClick={() => onMode(m.value)}
            aria-pressed={mode === m.value}
          >
            {m.label}
          </button>
        ))}
      </div>

      <div className="kb-search-rail__sect">Scope</div>
      <div className="kb-search-rail__seg">
        <button
          type="button"
          className={scope === "one" ? "on" : ""}
          onClick={() => onScope("one")}
          aria-pressed={scope === "one"}
        >
          this kb
        </button>
        <button
          type="button"
          className={scope === "all" ? "on" : ""}
          onClick={() => onScope("all")}
          aria-pressed={scope === "all"}
        >
          all corpora
        </button>
      </div>

      <FilterSection id="read" title="Read state" count={read.length} defaultOpen>
        <RailToggleGroup
          options={READ_OPTS}
          active={readSet}
          onToggle={(v) => toggleCsv("read", read, v)}
          ariaLabel="read state"
        />
      </FilterSection>

      {scope === "one" && (
        <FilterSection
          id="tags"
          title="Tags"
          count={tags.length + excludeTags.length}
          defaultOpen
        >
          <TagFilter
            tags={allTags}
            include={includeSet}
            exclude={excludeSet}
            onCycle={cycleTag}
          />
        </FilterSection>
      )}

      <FilterSection id="category" title="Category" count={category ? 1 : 0}>
        <RailToggleGroup
          options={[
            { value: "memory-session", label: "include session transcripts" },
          ]}
          active={sessionsChecked ? new Set(["memory-session"]) : new Set()}
          onToggle={onSessionsToggle}
          ariaLabel="include session transcripts"
        />
        <RailTextInput
          value={category}
          placeholder="exact kb-category…"
          ariaLabel="filter by category"
          onCommit={(v) => set("category", v || null)}
        />
      </FilterSection>

      <FilterSection id="status" title="Status" count={status.length}>
        <RailTextInput
          value={status.join(",")}
          placeholder="kb-status (csv)…"
          ariaLabel="filter by status"
          onCommit={(v) => set("status", v || null)}
        />
      </FilterSection>

      <FilterSection id="severity" title="Severity" count={severity.length}>
        <RailTextInput
          value={severity.join(",")}
          placeholder="kb-severity (csv)…"
          ariaLabel="filter by severity"
          onCommit={(v) => set("severity", v || null)}
        />
      </FilterSection>

      <FilterSection id="date" title="Date" count={since ? 1 : 0}>
        <RailToggleGroup
          options={SINCE_OPTS}
          active={new Set(since ? [since] : [])}
          onToggle={(v) => set("since", since === v ? null : v)}
          ariaLabel="date window"
        />
        {since && (
          <RailToggleGroup
            options={SINCE_FIELD_OPTS}
            active={new Set([sinceField])}
            onToggle={(v) => set("since_field", v === "modified" ? null : v)}
            ariaLabel="date field"
          />
        )}
      </FilterSection>

      <FilterSection
        id="read_during"
        title="Read during"
        count={readFrom != null || readTo != null ? 1 : 0}
      >
        <TemporalScrubber
          from={readFrom}
          to={readTo}
          onChange={(f, t) =>
            setMany({
              read_from: f != null ? String(f) : null,
              read_to: t != null ? String(t) : null,
            })
          }
        />
      </FilterSection>

      <FilterSection id="caps" title="Capabilities" count={caps.length}>
        <RailToggleGroup
          options={CAP_OPTS}
          active={capsSet}
          onToggle={(v) => toggleCsv("caps", caps, v)}
          ariaLabel="capabilities"
        />
      </FilterSection>

      {scope === "one" && (
        <FilterSection id="folder" title="Folder" count={folder ? 1 : 0}>
          <div className="kb-search-rail__folders">
            <FolderTree
              nodes={folders}
              active={folder || null}
              onSelect={(p) => set("folder", p || null)}
            />
          </div>
        </FilterSection>
      )}

      {scope === "one" && (
        <FilterSection id="list" title="Reading list" count={list ? 1 : 0}>
          <ListPicker
            activeKb={activeKb}
            value={list}
            onSelect={(id) => set("list", id)}
          />
        </FilterSection>
      )}

      {scope === "one" && (
        <FilterSection id="session" title="Session" count={session ? 1 : 0}>
          {(open) => (
            <SessionPicker
              activeKb={activeKb}
              value={session}
              onSelect={(id) => set("session", id)}
              enabled={open}
            />
          )}
        </FilterSection>
      )}

      <div className="kb-search-rail__sect">Results</div>
      <div className="kb-search-rail__seg">
        {LIMITS.map((n) => (
          <button
            key={n}
            type="button"
            className={limit === n ? "on" : ""}
            onClick={() => onLimit(n)}
            aria-pressed={limit === n}
          >
            {n}
          </button>
        ))}
      </div>
    </aside>
  );
}

// Local commit-on-Enter/blur text input for the free-text facets
// (category exact; status/severity csv). Buffers keystrokes so typing
// doesn't re-query per character.
function RailTextInput({
  value,
  placeholder,
  ariaLabel,
  onCommit,
}: {
  value: string;
  placeholder: string;
  ariaLabel: string;
  onCommit: (next: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const commit = () => {
    const next = draft.trim();
    if (next !== value) onCommit(next);
  };
  return (
    <input
      type="text"
      className="kb-search-rail__input"
      placeholder={placeholder}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
      }}
      onBlur={commit}
      aria-label={ariaLabel}
    />
  );
}
