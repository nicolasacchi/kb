import { Fragment } from "react";
import { linkifyCitations } from "../lib/linkifyCitations";

// CT-B5 — renders a plain-text prose string (memory summary, session
// decision prompt/answer) with any sha/path citations turned into deep
// links into kb-code's `/search` page. `codeUrl` null/invalid (no `[kb.*]
// code_url` configured, or the daemon config is invalid) ⇒ renders `text`
// completely unchanged — this is a plain child, NOT a wrapper element, so a
// call site keeps its own surrounding `<span className=…>` byte-identical
// either way.
//
// This is the plain-text sibling of `CommentBody.tsx`'s `rehypeCitations`
// pass — that one walks a Markdown hast tree (skipping existing links/code
// spans); this one has no such tree to walk, so it always runs over the
// whole string. See `lib/linkifyCitations.ts` for the grammar + the
// no-repo/`/search`-not-`/r/{repo}` rationale.
export default function CitationText({
  text,
  codeUrl,
}: {
  text: string;
  codeUrl: string | null;
}) {
  const segments = linkifyCitations(text, codeUrl);
  if (segments.length === 1 && segments[0].kind === "text") {
    return <>{text}</>;
  }
  return (
    <>
      {segments.map((seg, i) =>
        seg.kind === "text" ? (
          <Fragment key={i}>{seg.text}</Fragment>
        ) : (
          <a
            key={i}
            className="kb-citation-link"
            href={seg.href}
            target="_blank"
            rel="noopener noreferrer"
            title={
              seg.kind === "sha"
                ? "look up this commit in kb-code"
                : "look up this file in kb-code (repo not pinned — pick one on the search page)"
            }
          >
            {seg.text}
          </a>
        ),
      )}
    </>
  );
}
