import { Link } from "react-router";
import { useAnnotations, usePatchAnnotation } from "../../hooks/useAnnotations";
import { groupThreads } from "../../lib/annotations";
import { commitUrl } from "../../lib/codeUrl";
import { formatUnixSeconds, shortSha } from "../../lib/format";
import { toast } from "../../lib/toast";
import IntentChip from "./IntentChip";

export interface DiffFileAnnotationsProps {
  repo: string;
  path: string;
  sha: string;
}

/// The compact comment strip under an expanded Commit/Compare file row
/// (Phase D deliverable 4): every EXISTING `anchor_kind: "diff"` annotation
/// anchored to THIS `(path, sha)`, client-filtered from the same `GET /api/
/// annotations?repo=&path=` fetch the reader's own `AnnotationsPanel`
/// already uses (`useAnnotations` — same query key, so a comment posted
/// from either surface shows up in both, no second index). Deliberately
/// lightweight: an intent chip + a resolve toggle, no reply composer, no
/// delete — this is a comment strip, not a review UI (Wave G builds the
/// full thing); reply/delete on a diff comment still work, just from the
/// reader's own AnnotationsPanel once the operator opens that file there.
export default function DiffFileAnnotations({ repo, path, sha }: DiffFileAnnotationsProps) {
  const { data } = useAnnotations(repo, path);
  const patch = usePatchAnnotation(repo, path);
  const forThisCommit = (data?.annotations ?? []).filter((a) => a.anchor_kind === "diff" && a.sha === sha);
  if (forThisCommit.length === 0) return null;
  const threads = groupThreads(forThisCommit);

  async function toggleResolved(id: string, resolved: boolean) {
    try {
      await patch.mutateAsync({ id, input: { resolved: !resolved } });
    } catch (e) {
      toast.err(`couldn't update comment: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return (
    <div className="kbc-diffannot" data-kbc-diffannot>
      {threads.map(({ parent, replies }) => (
        <div
          key={parent.id}
          className={"kbc-diffannot__thread" + (parent.resolved ? " is-resolved" : "")}
          data-kbc-diffannot-thread={parent.id}
        >
          <div className="kbc-diffannot__row">
            <Link
              to={commitUrl(repo, sha)}
              className="kbc-diffannot__sha"
              title={`Pinned at commit ${sha}`}
              data-kbc-diffannot-sha
            >
              {shortSha(sha)}
            </Link>
            <span className="kbc-diffannot__line">L{parent.line}</span>
            <IntentChip intent={parent.intent} />
            <p className="kbc-diffannot__body">{parent.body}</p>
            <span className="kbc-diffannot__meta">
              {parent.author} · {formatUnixSeconds(parent.updated_at)}
            </span>
            <button type="button" onClick={() => toggleResolved(parent.id, parent.resolved)} data-kbc-diffannot-resolve>
              {parent.resolved ? "Reopen" : "Resolve"}
            </button>
          </div>
          {replies.map((r) => (
            <div key={r.id} className="kbc-diffannot__reply">
              <p className="kbc-diffannot__body">{r.body}</p>
              <span className="kbc-diffannot__meta">
                {r.author} · {formatUnixSeconds(r.updated_at)}
              </span>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
