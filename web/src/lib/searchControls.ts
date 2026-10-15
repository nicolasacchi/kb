import type { Cluster } from "@kb/ui/ViewBar";
import type { SearchMode, SearchScope } from "../api/client";
import { SEARCH_SORTS, type SearchSort } from "./searchSort";

export const SEARCH_LIMITS = [20, 50, 100, 200] as const;

// The daemon refuses hybrid/semantic on a kb with no embedding model with a
// 400 whose message names `embedding_model`. That message is the only signal
// the SPA has (KbSummary carries no model field), so the page reads it here.
export function isNoEmbeddingModelError(error: string | null | undefined): boolean {
  return !!error && /embedding_model/.test(error);
}

export const NO_MODEL_REASON = "needs an embedding model; this kb has none configured";

export type SearchControlsArgs = {
  mode: SearchMode;
  scope: SearchScope;
  limit: number;
  sort: SearchSort;
  /** True once the active kb has refused a model-backed mode. */
  noModel: boolean;
  onMode: (m: SearchMode) => void;
  onScope: (s: SearchScope) => void;
  onLimit: (n: number) => void;
  onSort: (s: SearchSort) => void;
};

// The search page's controls as ONE ViewBar: Mode, Scope, Results, Sort.
// When the kb has no embedding model, hybrid and semantic are disabled with a
// visible reason (keyword stays available).
export function buildSearchClusters(a: SearchControlsArgs): Cluster[] {
  return [
    {
      label: "Mode",
      controls: a.noModel
        ? [
            {
              id: "mode-keyword",
              label: "Mode",
              kind: "segment",
              value: a.mode,
              options: [{ value: "keyword", label: "keyword" }],
              onSelect: () => a.onMode("keyword"),
            },
            {
              id: "mode-model",
              label: "Model modes",
              kind: "segment",
              value: a.mode,
              disabled: true,
              reason: NO_MODEL_REASON,
              options: [
                { value: "hybrid", label: "hybrid" },
                { value: "semantic", label: "semantic" },
              ],
            },
          ]
        : [
            {
              id: "mode",
              label: "Mode",
              kind: "segment",
              value: a.mode,
              options: [
                { value: "hybrid", label: "hybrid" },
                { value: "keyword", label: "keyword" },
                { value: "semantic", label: "semantic" },
              ],
              onSelect: (v) => v && a.onMode(v as SearchMode),
            },
          ],
    },
    {
      label: "Scope",
      controls: [
        {
          id: "scope",
          label: "Scope",
          kind: "segment",
          value: a.scope,
          options: [
            { value: "one", label: "this kb" },
            { value: "all", label: "all corpora" },
          ],
          onSelect: (v) => v && a.onScope(v as SearchScope),
        },
      ],
    },
    {
      label: "Results",
      controls: [
        {
          id: "limit",
          label: "Results",
          kind: "select",
          value: String(a.limit),
          options: SEARCH_LIMITS.map((n) => ({ value: String(n), label: String(n) })),
          onSelect: (v) => v && a.onLimit(Number(v)),
        },
      ],
    },
    {
      label: "Sort",
      controls: [
        {
          id: "sort",
          label: "Sort",
          kind: "select",
          value: a.sort,
          options: SEARCH_SORTS.map((o) => ({ value: o.key, label: o.label })),
          onSelect: (v) => v && a.onSort(v as SearchSort),
        },
      ],
    },
  ];
}
