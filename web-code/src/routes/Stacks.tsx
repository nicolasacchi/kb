import { useEffect, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import type { StackLayer } from "../api/types";
import EmptyState from "../components/EmptyState";
import FileChangeRow from "../components/history/FileChangeRow";
import { Icon } from "../components/icons";
import { useStacks, useStacksLayerDiff } from "../hooks/useStacks";
import { shortSha } from "../lib/format";
import {
  layerDiffHeader,
  layerTags,
  stacksUrl,
} from "../lib/stacksFormat";
import "../styles/stacks.css";
import "../styles/history.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// V3.3-U1 — `/r/:repo/~stacks`: dependent-branch stacks + layer-diff.
export default function Stacks() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();

  const all = searchParams.get("all") === "1" || searchParams.get("all") === "true";
  const branchParam = searchParams.get("branch") ?? "";

  const [selectedBranch, setSelectedBranch] = useState(branchParam);

  useEffect(() => {
    setSelectedBranch(branchParam);
  }, [branchParam]);

  const stacksQ = useStacks({ repo, all });
  const layerDiffQ = useStacksLayerDiff(repo, selectedBranch || undefined, !!selectedBranch);

  function setAll(next: boolean) {
    navigate(stacksUrl(repo, { all: next, branch: selectedBranch || undefined }), {
      replace: true,
    });
  }

  function selectLayer(layer: StackLayer) {
    setSelectedBranch(layer.branch);
    navigate(stacksUrl(repo, { all, branch: layer.branch }), { replace: true });
  }

  const stacks = stacksQ.data?.stacks ?? [];
  const empty = stacksQ.isSuccess && stacks.length === 0;

  return (
    <div className="kbc-stacks" id="main" data-kbc-stacks>
      <header className="kbc-stacks__head">
        <h1 className="kbc-stacks__title">Stacks — {repo}</h1>
        <p className="kbc-stacks__hint" data-kbc-stacks-hint>
          Dependent-branch stacks detected from local refs. Each layer shows its
          incremental diff against its own base — not the whole chain vs main.
        </p>
      </header>

      <div className="kbc-stacks__filters">
        <label className="kbc-stacks__all-lab">
          <input
            type="checkbox"
            checked={all}
            onChange={(e) => setAll(e.target.checked)}
            data-kbc-stacks-all
          />
          Show single-layer branches
        </label>
        {stacksQ.data?.default_branch && (
          <span className="kbc-stacks__default" data-kbc-stacks-default>
            default: {stacksQ.data.default_branch}
          </span>
        )}
      </div>

      {stacksQ.data?.truncated && (
        <div className="kbc-stacks__banner" role="status" data-kbc-stacks-trunc>
          showing the capped set — branch enumeration hit the cap
        </div>
      )}

      {stacksQ.isLoading && <div className="kbc-stacks__muted">Loading…</div>}
      {stacksQ.error && (
        <div className="kbc-stacks__error">{(stacksQ.error as Error).message}</div>
      )}

      {empty && (
        <EmptyState
          icon={<Icon.List />}
          title="no dependent-branch stacks detected"
          hint={
            all
              ? "No local branches to stack (or none beyond the default)."
              : "hint: enable “Show single-layer branches” to include branches based on the default"
          }
        />
      )}

      <div className="kbc-stacks__body">
        <div className="kbc-stacks__list" data-kbc-stacks-list>
          {stacks.map((stack, si) => (
            <article
              key={si}
              className="kbc-stacks__card"
              data-kbc-stacks-card
              data-kbc-stacks-card-i={si}
            >
              <h2 className="kbc-stacks__card-title">
                Stack · {stack.layers.length} layer{stack.layers.length === 1 ? "" : "s"}
              </h2>
              <ol className="kbc-stacks__layers">
                {stack.layers.map((layer) => {
                  const active = selectedBranch === layer.branch;
                  const tags = layerTags(layer);
                  return (
                    <li key={layer.branch}>
                      <button
                        type="button"
                        className={
                          "kbc-stacks__layer" + (active ? " kbc-stacks__layer--active" : "")
                        }
                        onClick={() => selectLayer(layer)}
                        data-kbc-stacks-layer={layer.branch}
                      >
                        <span className="kbc-stacks__layer-branch">{layer.branch}</span>
                        <span className="kbc-stacks__layer-base">
                          {layer.unresolved ? "base ?" : `← ${layer.base || "?"}`}
                        </span>
                        <span className="kbc-stacks__layer-ab" data-kbc-stacks-ahead>
                          +{layer.ahead}/−{layer.behind}
                        </span>
                        {tags.map((t) => (
                          <span
                            key={t.kind}
                            className={`kbc-stacks__tag kbc-stacks__tag--${t.kind}`}
                            data-kbc-stacks-tag={t.kind}
                          >
                            {t.label}
                          </span>
                        ))}
                        <span className="kbc-stacks__layer-tip" data-kbc-stacks-tip>
                          {shortSha(layer.tip.sha)} · {layer.tip.subject}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ol>
            </article>
          ))}
        </div>

        {selectedBranch && (
          <div className="kbc-stacks__diff" data-kbc-stacks-diff>
            {layerDiffQ.isLoading && <div className="kbc-stacks__muted">Loading layer diff…</div>}
            {layerDiffQ.error && (
              <div className="kbc-stacks__error">{(layerDiffQ.error as Error).message}</div>
            )}
            {layerDiffQ.data && (
              <>
                <header className="kbc-stacks__diff-head" data-kbc-stacks-diff-head>
                  <h2 className="kbc-stacks__sub">{layerDiffQ.data.branch}</h2>
                  <p className="kbc-stacks__diff-meta">
                    {layerDiffHeader(
                      layerDiffQ.data.base,
                      layerDiffQ.data.base_tip,
                      layerDiffQ.data.stale,
                    )}
                  </p>
                  {layerDiffQ.data.stale && (
                    <div className="kbc-stacks__banner" role="status" data-kbc-stacks-stale>
                      base moved since cut
                    </div>
                  )}
                  {layerDiffQ.data.truncated && (
                    <div className="kbc-stacks__banner" role="status">
                      showing the capped set
                    </div>
                  )}
                  {layerDiffQ.data.commits_truncated && (
                    <div className="kbc-stacks__banner" role="status">
                      commit list truncated
                    </div>
                  )}
                </header>
                <div className="kbc-stacks__files" data-kbc-stacks-files>
                  {layerDiffQ.data.files.length === 0 ? (
                    <p className="kbc-stacks__muted">No files in this layer&apos;s incremental range.</p>
                  ) : (
                    layerDiffQ.data.files.map((f) => (
                      <FileChangeRow
                        key={f.path}
                        repo={repo}
                        file={f}
                        from={layerDiffQ.data.base_tip}
                        to={layerDiffQ.data.tip}
                        browseRef={layerDiffQ.data.tip}
                      />
                    ))
                  )}
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
