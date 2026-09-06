// MI-W4.2d — THE AGENT'S-EYE SIMULATOR: type a draft prompt, see EXACTLY
// what the `kb-recall` UserPromptSubmit hook would inject into a new turn
// — same ranking, same limit (5), same rendering including the "↳ summary"
// continuation lines. This is the single most explanatory surface in the
// wave: it makes recall's behaviour inspectable instead of mysterious.
//
// Drives off the REAL `/api/memory/recall` endpoint (never reimplements
// ranking client-side) and formats the response with
// `lib/injectionPreview.ts`'s pure renderer, which ports the hook's own jq
// filter line-for-line.

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchRecall } from "../api/client";
import { formatInjectionBlock, INJECTION_LIMIT } from "../lib/injectionPreview";

const DEBOUNCE_MS = 300;

export default function AgentEyeSimulator() {
  const [draft, setDraft] = useState("");
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    const t = setTimeout(() => setDebounced(draft), DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [draft]);

  const trimmed = debounced.trim();
  const q = useQuery({
    queryKey: ["agent-eye-simulator", trimmed],
    enabled: trimmed.length > 0,
    queryFn: ({ signal }) =>
      fetchRecall({ q: trimmed, scope: "all", limit: INJECTION_LIMIT }, signal),
  });

  const block = q.data ? formatInjectionBlock(q.data.hits) : null;

  return (
    <section className="kb-mem__simulator" data-testid="agent-eye-simulator">
      <h4>Agent&rsquo;s-eye simulator</h4>
      <p className="kb-mem__simulator-hint">
        Type a draft prompt to see exactly what the <code>kb-recall</code>{" "}
        hook would inject into a new turn — same ranking, same limit (
        {INJECTION_LIMIT}), same rendering.
      </p>
      <textarea
        className="kb-mem__simulator-input"
        placeholder="type a draft prompt…"
        aria-label="draft prompt"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        data-testid="agent-eye-simulator-input"
      />
      {trimmed.length === 0 && (
        <p className="kb-mem__simulator-empty" data-testid="agent-eye-simulator-idle">
          nothing injects for an empty prompt — the hook exits before ever
          calling recall.
        </p>
      )}
      {trimmed.length > 0 && q.isPending && (
        <p className="kb-mem__simulator-loading">recalling…</p>
      )}
      {trimmed.length > 0 && q.isError && (
        <p className="kb-mem__simulator-error" role="alert">
          recall failed: {String(q.error)}
        </p>
      )}
      {trimmed.length > 0 && q.isSuccess && block === null && (
        <p className="kb-mem__simulator-empty" data-testid="agent-eye-simulator-nohits">
          no hits — nothing would be injected.
        </p>
      )}
      {block !== null && (
        <pre className="kb-mem__simulator-block" data-testid="agent-eye-simulator-block">
          {block}
        </pre>
      )}
    </section>
  );
}
