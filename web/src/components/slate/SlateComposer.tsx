import { useEffect, useId, useMemo, useRef, useState } from "react";
import LazyMarkdownEditor from "../LazyMarkdownEditor";
import { Icon } from "../icons";
import { glyphForKind } from "../../lib/slateGlyphs";
import type { SlateKind, SlatePostBody } from "../../api/slateTypes";
import { SPA_PROV } from "../../hooks/useSlates";

// The composer (§10 "Composer"). One form for every authored kind; the
// board's edit action opens it PREFILLED and submits with `supersedes`, so
// "rub out and rewrite" is the same code path as "write" — there is no
// in-place rewrite anywhere in this design.
//
// The wire caps are the form's caps: `line` ≤ 200, `body` ≤ 2,000, `refs`
// ≤ 8 (§9 / the rules matrix "Hand packet"). We show the counter rather
// than silently truncating — a truncated coordination line is worse than a
// refused one.
//
// MOBILE (rules matrix "Mobile"): kind, line and topic ONLY. No body
// editor, no refs, no subject — the phone is a reader with a capture slot,
// and the non-goal that says so is load-bearing, not an omission.

/// The kinds a human authors from the board. `drop`, `mark`, `done` and
/// `answer` are ACTIONS on an existing card, not things you compose;
/// `answer` gets here through the ask card's own button (which prefills
/// `re`).
export const COMPOSER_KINDS: readonly SlateKind[] = [
  "found",
  "idea",
  "now",
  "warn",
  "ask",
  "hand",
  "take",
  "tried",
  "answer",
];

export const LINE_MAX = 200;
export const BODY_MAX = 2000;
export const REFS_MAX = 8;

export type ComposerSeed = {
  kind?: SlateKind;
  line?: string;
  body?: string;
  topic?: string;
  subject?: string;
  refs?: string[];
  failed?: string;
  /// Set by "edit" — the post this one replaces. A post carrying
  /// `supersedes` MUST carry the target's kind (400 `kind-mismatch`), so the
  /// kind select is locked while it is set.
  supersedes?: number;
  /// Set by "answer" — the ask this answers.
  re?: number;
};

type Props = {
  open: boolean;
  seed?: ComposerSeed;
  topics: readonly string[];
  mobile?: boolean;
  closed?: boolean;
  onClose: () => void;
  onSubmit: (body: SlatePostBody) => Promise<unknown>;
};

export default function SlateComposer({
  open,
  seed,
  topics,
  mobile = false,
  closed = false,
  onClose,
  onSubmit,
}: Props) {
  const uid = useId();
  // A found needs a ref and the mobile sheet has no ref input, so the phone
  // defaults to idea (rules matrix "Mobile").
  const [kind, setKind] = useState<SlateKind>(seed?.kind ?? (mobile ? "idea" : "found"));
  const [line, setLine] = useState(seed?.line ?? "");
  const [body, setBody] = useState(seed?.body ?? "");
  const [topic, setTopic] = useState(seed?.topic ?? "");
  const [subject, setSubject] = useState(seed?.subject ?? "");
  const [failed, setFailed] = useState(seed?.failed ?? "");
  const [refsText, setRefsText] = useState((seed?.refs ?? []).join(" "));
  const [busy, setBusy] = useState(false);
  const lineRef = useRef<HTMLInputElement>(null);

  // Re-seed on every OPEN (an edit of a different card must not inherit the
  // last one's text). Keyed on the seed identity, not its fields, so typing
  // never gets clobbered mid-compose.
  useEffect(() => {
    if (!open) return;
    setKind(seed?.kind ?? "found");
    setLine(seed?.line ?? "");
    setBody(seed?.body ?? "");
    setTopic(seed?.topic ?? "");
    setSubject(seed?.subject ?? "");
    setFailed(seed?.failed ?? "");
    setRefsText((seed?.refs ?? []).join(" "));
    const t = setTimeout(() => lineRef.current?.focus(), 0);
    return () => clearTimeout(t);
  }, [open, seed]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const refs = useMemo(
    () => refsText.split(/\s+/).map((s) => s.trim()).filter(Boolean),
    [refsText],
  );
  const needsSubject = kind === "take" || kind === "hand";
  const overLine = line.length > LINE_MAX;
  const overBody = body.length > BODY_MAX;
  const overRefs = refs.length > REFS_MAX;
  // `ask` without a `?` is a 400 `ask-needs-question`; say so here rather
  // than making the daemon say it.
  const askNoQuestion = kind === "ask" && line.trim().length > 0 && !line.includes("?");
  // `found` without a ref is a 400 (§4: "post it as idea if it is a guess");
  // say so here too, and never offer `found` where refs cannot be typed.
  const foundNoRef = kind === "found" && (mobile || refs.length === 0);
  const canSubmit =
    !busy &&
    !closed &&
    line.trim().length > 0 &&
    !overLine &&
    !overBody &&
    !overRefs &&
    !askNoQuestion &&
    !foundNoRef &&
    (!needsSubject || subject.trim().length > 0);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    try {
      const post: SlatePostBody = {
        kind,
        line: line.trim(),
        prov: SPA_PROV,
      };
      if (!mobile && body.trim()) post.body = body;
      if (topic.trim()) post.topic = topic.trim();
      if (!mobile && needsSubject && subject.trim()) post.subject = subject.trim();
      if (!mobile && kind === "tried" && failed.trim()) post.failed = failed.trim();
      if (!mobile && refs.length) post.refs = refs;
      if (seed?.supersedes !== undefined) post.supersedes = seed.supersedes;
      if (seed?.re !== undefined) post.re = seed.re;
      await onSubmit(post);
      onClose();
    } finally {
      setBusy(false);
    }
  }

  const editing = seed?.supersedes !== undefined;

  return (
    <>
      {mobile && (
        <div
          className={`kb-pinsp-scrim${open ? " is-open" : ""}`}
          onClick={onClose}
          aria-hidden="true"
        />
      )}
      <form
        className={`slate-composer${open ? " is-open" : ""}`}
        id="slate-composer"
        hidden={!open}
        onSubmit={submit}
        {...(mobile ? { role: "dialog" as const, "aria-modal": true } : {})}
        aria-label={editing ? "Rewrite a post" : "Post to the slate"}
      >
        <header className="slate-composer__head">
          <h2 className="slate-composer__title">
            {editing ? `Rewrite #${seed!.supersedes}` : "Post"}
          </h2>
          <button
            type="button"
            className="slate-composer__close"
            onClick={onClose}
            aria-label="Close composer"
          >
            <Icon.X />
          </button>
        </header>

        {closed && (
          <p className="slate-composer__note" role="alert">
            This slate is closed — reopen it before posting.
          </p>
        )}

        <div className="slate-composer__row">
          <label className="slate-composer__lbl" htmlFor={`${uid}-kind`}>
            Kind
          </label>
          <select
            id={`${uid}-kind`}
            className="slate-composer__kind"
            value={kind}
            // An edit must carry the target's kind (400 `kind-mismatch`).
            disabled={editing}
            onChange={(e) => setKind(e.target.value as SlateKind)}
          >
            {(mobile ? COMPOSER_KINDS.filter((k) => k !== "found") : COMPOSER_KINDS).map((k) => {
              const g = glyphForKind(k);
              return (
                <option key={k} value={k}>
                  {g.glyph} {g.word.toLowerCase()}
                </option>
              );
            })}
          </select>
        </div>

        <div className="slate-composer__row">
          <label className="slate-composer__lbl" htmlFor={`${uid}-line`}>
            Line
          </label>
          <input
            id={`${uid}-line`}
            ref={lineRef}
            className="slate-composer__line"
            value={line}
            maxLength={LINE_MAX * 2}
            placeholder={
              kind === "ask" ? "what you need to know?" : "one line, 200 characters"
            }
            onChange={(e) => setLine(e.target.value)}
          />
          <span
            className={`slate-composer__count${overLine ? " is-over" : ""}`}
            aria-live="polite"
          >
            {line.length}/{LINE_MAX}
          </span>
        </div>
        {foundNoRef && line.trim().length > 0 && (
          <p className="slate-composer__hint" role="alert">
            a found needs a ref (path:… · kb:… · mem:… · post:#n); post it as an idea if it is a guess
          </p>
        )}
        {askNoQuestion && (
          <p className="slate-composer__hint" role="alert">
            An ask has to be a question — end it with a “?”.
          </p>
        )}

        <div className="slate-composer__row">
          <label className="slate-composer__lbl" htmlFor={`${uid}-topic`}>
            Topic
          </label>
          <input
            id={`${uid}-topic`}
            className="slate-composer__topic"
            value={topic}
            list={`${uid}-topics`}
            placeholder="optional — one per post"
            onChange={(e) => setTopic(e.target.value)}
          />
          <datalist id={`${uid}-topics`}>
            {topics.map((t) => (
              <option key={t} value={t} />
            ))}
          </datalist>
        </div>

        {/* Everything below is DESKTOP-ONLY (rules matrix "Mobile"). */}
        {!mobile && needsSubject && (
          <div className="slate-composer__row">
            <label className="slate-composer__lbl" htmlFor={`${uid}-subject`}>
              Subject
            </label>
            <input
              id={`${uid}-subject`}
              className="slate-composer__subject"
              value={subject}
              placeholder="the path or area you are claiming"
              onChange={(e) => setSubject(e.target.value)}
            />
          </div>
        )}

        {!mobile && kind === "tried" && (
          <div className="slate-composer__row">
            <label className="slate-composer__lbl" htmlFor={`${uid}-failed`}>
              Failed
            </label>
            <input
              id={`${uid}-failed`}
              className="slate-composer__failed"
              value={failed}
              placeholder="what went wrong — the do-not-retry cue"
              onChange={(e) => setFailed(e.target.value)}
            />
          </div>
        )}

        {!mobile && (
          <div className="slate-composer__row">
            <label className="slate-composer__lbl" htmlFor={`${uid}-refs`}>
              Refs
            </label>
            <input
              id={`${uid}-refs`}
              className="slate-composer__refs"
              value={refsText}
              placeholder="path:… kb:<kb>/<id> post:#12 session:… (space separated, max 8)"
              onChange={(e) => setRefsText(e.target.value)}
            />
            <span className={`slate-composer__count${overRefs ? " is-over" : ""}`}>
              {refs.length}/{REFS_MAX}
            </span>
          </div>
        )}
        {!mobile && refs.length > 0 && (
          <ul className="slate-composer__chips">
            {refs.map((r) => (
              <li key={r} className="slate-ref">
                {r}
              </li>
            ))}
          </ul>
        )}

        {!mobile && (
          <div className="slate-composer__body">
            {/* Invariant #22 — the CM6 composer keeps a hidden mirror
                <textarea> carrying this aria label + the value; the e2e
                spec drives the composer through it. */}
            <LazyMarkdownEditor
              value={body}
              onChange={setBody}
              ariaLabel="slate post body"
              textareaClassName="slate-composer__mirror"
              placeholder="optional — 2,000 characters. A ```mermaid fence draws."
              onSubmit={() => {
                if (canSubmit) void submit(new Event("submit") as unknown as React.FormEvent);
              }}
            />
            <span className={`slate-composer__count${overBody ? " is-over" : ""}`}>
              {body.length}/{BODY_MAX}
            </span>
          </div>
        )}

        <footer className="slate-composer__acts">
          <button type="button" className="slate-composer__cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            type="submit"
            className="slate-composer__go"
            disabled={!canSubmit}
          >
            {editing ? "Rewrite" : "Post"}
          </button>
        </footer>
      </form>
    </>
  );
}
