import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchPrompt, lookupArtifact } from "../api/client";
import { diffLines, type PromptDiffLine } from "../lib/promptDiff";
import { toast } from "../lib/toast";

// W2.11 — the generation prompt as first-class content, living at the head
// of the Story tab (see BiographyTab.tsx, which renders this in place of
// its old "deferred" placeholder). ONE home per invariant #30: this is a
// section inside the existing `story` inspector sub-tab, not a new rail
// icon — it therefore also appears for free inside the merged "all" tab
// (PreviewInspector's `show(tab) = tab === t || tab === "all"`).
//
// Query key `["prompt", kb, id]`, staleTime Infinity, no bridge wiring
// (queryClient.ts documents why: prompts only change on reindex, and this
// phase deliberately doesn't wire a new bridge invalidation for that rare
// case — see the key-contract comment there).
//
// `stripped: true` (a strip-configured corpus refusing a non-loopback
// reader) always renders as an honest inline sentence — never an error
// state, never a blank collapse.

// Mirrors `kb_core::parser::PROMPT_MAX_BYTES` — display-only (the daemon
// already applied this cap at index time; this is just so the byte count
// reads in context rather than as a bare, unexplained number).
const PROMPT_CAP_BYTES = 8 * 1024;

// A 12-hex canonical artifact id (`kb_core::ids::ArtifactId`'s shape) vs.
// anything else, which is treated as a source-relative path / filename and
// resolved via the same `/lookup` endpoint `kb versions`/`kb diff` use.
const HEX_ID_RE = /^[0-9a-f]{12}$/i;

export default function PromptPanel({
  kb,
  artifactId,
}: {
  kb: string;
  artifactId: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const [compareOpen, setCompareOpen] = useState(false);
  const [compareQuery, setCompareQuery] = useState("");
  const [compareTargetId, setCompareTargetId] = useState<string | null>(null);
  const [resolving, setResolving] = useState(false);
  const [resolveError, setResolveError] = useState<string | null>(null);

  const { data, isLoading } = useQuery({
    queryKey: ["prompt", kb, artifactId],
    queryFn: ({ signal }) => fetchPrompt(kb, artifactId, signal),
    staleTime: Infinity,
  });

  // Sentinel key when nothing is picked yet — `enabled: false` means it's
  // never actually fetched, but the queryKey shape stays stable across
  // renders (React Query wants a consistent key per hook call site).
  const cmp = useQuery({
    queryKey: ["prompt", kb, compareTargetId ?? "__kb-promptpanel-none__"],
    queryFn: ({ signal }) => fetchPrompt(kb, compareTargetId as string, signal),
    enabled: compareTargetId != null,
    staleTime: Infinity,
  });

  if (isLoading) return null;
  if (!data) return null;

  if (data.stripped) {
    return (
      <p className="kb-promptpanel__withheld">
        prompt withheld on this corpus for non-local readers
      </p>
    );
  }
  if (!data.prompt) return null;

  const promptText = data.prompt;

  async function onCompareSubmit() {
    const q = compareQuery.trim();
    if (!q) return;
    setResolveError(null);
    if (HEX_ID_RE.test(q)) {
      setCompareTargetId(q);
      return;
    }
    setResolving(true);
    try {
      const hit = await lookupArtifact(kb, q);
      if (hit.kind === "exact" || hit.kind === "unique_suffix") {
        setCompareTargetId(hit.id);
      } else if (hit.kind === "not_found") {
        setResolveError(`no artifact matches "${q}"`);
      } else {
        setResolveError(
          `"${q}" is ambiguous (${hit.candidates.length} matches) — try a fuller path`,
        );
      }
    } catch (e) {
      setResolveError(e instanceof Error ? e.message : String(e));
    } finally {
      setResolving(false);
    }
  }

  function onCopyAsTemplate() {
    const bundle = `<template id="kb-prompt">${promptText}</template>`;
    navigator.clipboard
      ?.writeText(bundle)
      .then(() => toast.ok('prompt copied as a <template id="kb-prompt"> block'))
      .catch(() => toast.err("couldn't copy prompt"));
  }

  const diff: PromptDiffLine[] | null =
    compareTargetId != null && cmp.data?.prompt != null
      ? diffLines(cmp.data.prompt, promptText)
      : null;

  return (
    <div className="kb-promptpanel">
      <button
        type="button"
        className="kb-promptpanel__toggle"
        data-kb-act="prompt-toggle"
        aria-expanded={expanded}
        onClick={() => setExpanded((v) => !v)}
      >
        <span aria-hidden>{expanded ? "▾" : "▸"}</span> prompt ·{" "}
        {data.size_bytes} / {PROMPT_CAP_BYTES} bytes
      </button>
      {expanded && (
        <div className="kb-promptpanel__body">
          <pre className="kb-promptpanel__text">{promptText}</pre>
          <div className="kb-promptpanel__actions">
            <button
              type="button"
              className="kb-promptpanel__act"
              data-kb-act="prompt-copy-template"
              onClick={onCopyAsTemplate}
            >
              copy as template
            </button>
            <button
              type="button"
              className="kb-promptpanel__act"
              data-kb-act="prompt-compare"
              onClick={() => setCompareOpen((v) => !v)}
            >
              compare…
            </button>
          </div>
          {compareOpen && (
            <div className="kb-promptpanel__compare">
              <div className="kb-promptpanel__comparerow">
                <input
                  type="text"
                  className="kb-promptpanel__compareinput"
                  placeholder="artifact id or source-rel path"
                  value={compareQuery}
                  onChange={(e) => setCompareQuery(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void onCompareSubmit();
                  }}
                />
                <button
                  type="button"
                  className="kb-promptpanel__act"
                  data-kb-act="prompt-compare-go"
                  disabled={resolving || !compareQuery.trim()}
                  onClick={() => void onCompareSubmit()}
                >
                  compare
                </button>
              </div>
              {resolving && <div className="kb-pinsp__hint">resolving…</div>}
              {resolveError && (
                <div className="kb-promptpanel__err">{resolveError}</div>
              )}
              {compareTargetId && cmp.isLoading && (
                <div className="kb-pinsp__hint">fetching…</div>
              )}
              {compareTargetId && !cmp.isLoading && cmp.data?.stripped && (
                <div className="kb-promptpanel__err">
                  {compareTargetId}'s prompt is withheld on this corpus for
                  non-local readers
                </div>
              )}
              {compareTargetId &&
                !cmp.isLoading &&
                cmp.data &&
                !cmp.data.stripped &&
                !cmp.data.prompt && (
                  <div className="kb-pinsp__hint">
                    {compareTargetId} has no stored prompt
                  </div>
                )}
              {diff && (
                <div className="kb-promptpanel__diff">
                  {diff.map((ln, i) => (
                    <div
                      key={i}
                      className={`kb-promptpanel__diffline kb-promptpanel__diffline--${ln.tag}`}
                    >
                      <span className="kb-promptpanel__diffsign" aria-hidden>
                        {ln.tag === "insert"
                          ? "+"
                          : ln.tag === "delete"
                            ? "-"
                            : " "}
                      </span>
                      <span className="kb-promptpanel__difftext">
                        {ln.text}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
