import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  fetchDocsPage,
  fetchFacets,
  fetchTags,
  search,
  type DocSummary,
  type SearchMode,
  type SearchScope,
} from "../../api/client";
import { artifactHref } from "../../lib/artifactHref";

const SEARCH_MODES: SearchMode[] = ["hybrid", "keyword", "semantic"];
const PROBE_LIMIT = 8;
const SAMPLE_DECK_SIZE = 6;

export type ZeroHitFilters = {
  category?: string;
  folder?: string;
  tags?: string[];
  excludeTags?: string[];
  status?: string[];
  severity?: string[];
  caps?: string[];
  since?: string;
  sinceField?: "created" | "modified";
  session?: string;
  list?: string;
  readFrom?: number;
  readTo?: number;
};

export type ZeroHitRecoveryProps = {
  q: string;
  mode: SearchMode;
  scope: SearchScope;
  /// Wire kb (`undefined` on scope=all — federated fan-out).
  kb?: string;
  /// Resolved kb for facet/sample-deck reads, which are always single-kb
  /// even when the failing search itself was federated.
  activeKb?: string;
  filters: ZeroHitFilters;
  hasFilters: boolean;
  set: (key: string, value: string | null) => void;
  setMany: (patch: Record<string, string | null>) => void;
};

type ProbeChipDef = {
  key: string;
  label: string;
  count?: number;
  loading: boolean;
  onClick: () => void;
};

// One bounded, abortable, cached probe search — nested under the
// existing `["search", ...]` query-key prefix (see queryClient.ts's key
// contract) so the artifact-churn burst gate invalidates these for free,
// with no bridge changes needed. `dropFilters` runs the SAME query/mode/
// scope with every facet removed (the "without filters" probe); the
// other probes keep every filter and vary only mode or scope.
function useProbe(
  enabled: boolean,
  q: string,
  mode: SearchMode,
  scope: SearchScope,
  kb: string | undefined,
  filters: ZeroHitFilters,
  dropFilters: boolean,
) {
  return useQuery({
    queryKey: [
      "search",
      "probe",
      q,
      mode,
      scope,
      kb ?? null,
      dropFilters ? null : filters,
    ] as const,
    enabled,
    queryFn: ({ signal }) =>
      search(q, {
        mode,
        scope,
        kb,
        limit: PROBE_LIMIT,
        signal,
        ...(dropFilters
          ? {}
          : {
              category: filters.category,
              folder: filters.folder,
              tags: filters.tags,
              excludeTags: filters.excludeTags,
              status: filters.status,
              severity: filters.severity,
              caps: filters.caps,
              since: filters.since,
              sinceField: filters.sinceField,
              session: filters.session,
              list: filters.list,
              readFrom: filters.readFrom,
              readTo: filters.readTo,
            }),
      }),
    staleTime: Infinity,
  });
}

// W1.search — the never-empty search recovery surface (board B2 core +
// museum delta). Renders in place of the old dead-end zero-hit
// EmptyState: a probe strip with honest, bounded counts for nearby
// escapes (other modes / broader scope / dropped filters), plus a
// "try instead" block (top live facet values + a deterministic sample
// deck) so there is always something clickable, never a true dead end.
export default function ZeroHitRecovery({
  q,
  mode,
  scope,
  kb,
  activeKb,
  filters,
  hasFilters,
  set,
  setMany,
}: ZeroHitRecoveryProps) {
  const hasQuery = q.trim().length > 0;
  const otherModes = useMemo(
    () => SEARCH_MODES.filter((m) => m !== mode),
    [mode],
  );

  const modeAProbe = useProbe(hasQuery, q, otherModes[0], scope, kb, filters, false);
  const modeBProbe = useProbe(hasQuery, q, otherModes[1], scope, kb, filters, false);
  const scopeAllProbe = useProbe(
    hasQuery && scope === "one",
    q,
    mode,
    "all",
    undefined,
    filters,
    false,
  );
  const noFiltersProbe = useProbe(
    hasQuery && hasFilters,
    q,
    mode,
    scope,
    kb,
    filters,
    true,
  );

  const chips: ProbeChipDef[] = [];
  if (hasQuery) {
    chips.push({
      key: `mode:${otherModes[0]}`,
      label: otherModes[0],
      count: modeAProbe.data?.hits.length,
      loading: modeAProbe.isFetching,
      onClick: () => set("mode", otherModes[0] === "hybrid" ? null : otherModes[0]),
    });
    chips.push({
      key: `mode:${otherModes[1]}`,
      label: otherModes[1],
      count: modeBProbe.data?.hits.length,
      loading: modeBProbe.isFetching,
      onClick: () => set("mode", otherModes[1] === "hybrid" ? null : otherModes[1]),
    });
    if (scope === "one") {
      chips.push({
        key: "scope:all",
        label: "all corpora",
        count: scopeAllProbe.data?.hits.length,
        loading: scopeAllProbe.isFetching,
        // Mirrors SearchRail's onScope: switching to federated drops the
        // single-kb-only facets in the same navigate.
        onClick: () =>
          setMany({
            scope: "all",
            category: null,
            folder: null,
            tags: null,
            exclude_tags: null,
            session: null,
            list: null,
          }),
      });
    }
    if (hasFilters) {
      chips.push({
        key: "filters:clear",
        label: "without filters",
        count: noFiltersProbe.data?.hits.length,
        loading: noFiltersProbe.isFetching,
        onClick: () =>
          setMany({
            category: null,
            folder: null,
            tags: null,
            exclude_tags: null,
            status: null,
            severity: null,
            caps: null,
            since: null,
            since_field: null,
            session: null,
            list: null,
            read_from: null,
            read_to: null,
          }),
      });
    }
  }

  const tagsQ = useQuery({
    queryKey: ["tags", activeKb],
    enabled: !!activeKb,
    queryFn: ({ signal }) => fetchTags(activeKb as string, signal),
    staleTime: Infinity,
  });
  const facetsQ = useQuery({
    queryKey: ["facets", activeKb],
    enabled: !!activeKb,
    queryFn: ({ signal }) => fetchFacets(activeKb as string, signal),
    staleTime: Infinity,
  });
  const sampleQ = useQuery({
    queryKey: ["docs", activeKb, "zero-hit-sample", SAMPLE_DECK_SIZE] as const,
    enabled: !!activeKb,
    queryFn: ({ signal }) =>
      fetchDocsPage(activeKb as string, {
        offset: 0,
        limit: SAMPLE_DECK_SIZE,
        sort: "recent",
        signal,
      }),
    staleTime: Infinity,
  });

  const topTags = useMemo(
    () => [...(tagsQ.data ?? [])].sort((a, b) => b.count - a.count).slice(0, 3),
    [tagsQ.data],
  );
  const topCategories = useMemo(
    () =>
      [...(facetsQ.data?.categories ?? [])]
        .sort((a, b) => b.count - a.count)
        .slice(0, 2),
    [facetsQ.data],
  );
  const sampleDocs: DocSummary[] = sampleQ.data?.docs ?? [];
  const hasExamples = topTags.length > 0 || topCategories.length > 0;
  const hasMuseum = hasExamples || sampleDocs.length > 0;

  if (chips.length === 0 && !hasMuseum) return null;

  return (
    <div className="kb-zero">
      {chips.length > 0 && (
        <div
          className="kb-zero__probes"
          role="group"
          aria-label="try a different search"
        >
          {chips.map((c) => {
            const pending = c.loading && c.count == null;
            const noHits = c.count === 0;
            return (
              <button
                key={c.key}
                type="button"
                className="kb-zero__probe"
                disabled={pending || noHits}
                onClick={c.onClick}
              >
                <span className="kb-zero__probe-label">{c.label}</span>
                {pending ? (
                  <span className="kb-zero__probe-skel" aria-hidden="true" />
                ) : c.count != null ? (
                  <span className="kb-zero__probe-n">
                    {c.count}
                    {c.count === PROBE_LIMIT ? "+" : ""}
                  </span>
                ) : null}
              </button>
            );
          })}
        </div>
      )}

      {hasMuseum && activeKb && (
        <div className="kb-zero__museum">
          <div className="kb-zero__museum-h">Try instead</div>
          {hasExamples && (
            <div className="kb-zero__examples">
              {topTags.map((t) => (
                <button
                  key={`tag:${t.name}`}
                  type="button"
                  className="kb-zero__example"
                  onClick={() => setMany({ q: null, tags: t.name, category: null })}
                >
                  tag: {t.name}
                </button>
              ))}
              {topCategories.map((c) => (
                <button
                  key={`cat:${c.value}`}
                  type="button"
                  className="kb-zero__example"
                  onClick={() => setMany({ q: null, category: c.value, tags: null })}
                >
                  {c.value}
                </button>
              ))}
            </div>
          )}
          {sampleDocs.length > 0 && (
            <div className="kb-zero__deck">
              {sampleDocs.map((d) => (
                <Link
                  key={d.id}
                  to={artifactHref(activeKb, d.source_relative)}
                  className="kb-zero__deck-card"
                >
                  {d.title || "(untitled)"}
                </Link>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
