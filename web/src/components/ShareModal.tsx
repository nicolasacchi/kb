import { useEffect, useRef, useState } from "react";
import {
  createShare,
  exportShareBundle,
  exportSharePage,
  type ShareResult,
} from "../api/client";
import { saveBlob } from "../lib/download";
import { toast } from "../lib/toast";
import { Icon } from "./icons";

// kb share — one entry point ("Share"), three methods:
//   • Publish online — to a static host (Cloudflare Pages + Access, gated; or
//     GitHub Pages, public) via POST /api/kb/{kb}/share → a live URL.
//   • Download bundle — a self-contained OFFLINE .zip via POST .../share/export,
//     with in-share cross-artifact links rewritten to relative paths.
//   • Download page — a single UNCOMPRESSED artifact in its native format via
//     POST .../share/export/page (scrubbed standalone .html, or raw .md
//     source). Only offered for a single artifact (`allowPage`).
// A native <dialog> (focus trap + ::backdrop + Escape), mirroring CommentModal.
//
// Host API tokens live in the daemon's environment, not the browser — the SPA
// only POSTs the target + choice; the daemon runs the engine.

type Host = "cloudflare-pages" | "github-pages";
type GateMode = "email" | "google" | "github" | "public";
type Method = "publish" | "bundle" | "page";

// Unified "download complete" state for the bundle (.zip) and single-page paths.
type DownloadDone = {
  kind: "bundle" | "page";
  filename: string;
  /// File count — bundle only.
  files?: number;
  danglers: string[];
};

export default function ShareModal({
  kb,
  target,
  folder,
  allowPage = false,
  onClose,
}: {
  kb: string;
  /// Source-relative file/folder to share.
  target: string;
  /// The artifact's source-relative parent folder. When non-empty, the modal
  /// offers a "share the whole folder instead of just this page" scope toggle
  /// — the one-click path from a single artifact to publishing its folder.
  /// Empty/undefined ⇒ the artifact is at the kb root (no folder scope).
  folder?: string | null;
  /// Offer the single-page (uncompressed, native-format) download. Set by
  /// callers that share a single artifact (not a folder).
  allowPage?: boolean;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  // Scope — "page" shares the open artifact, "folder" shares its whole parent
  // directory (folders flow through the same engine; the share walks every
  // file + picks an entry page). Offered only when the artifact has a folder.
  const hasFolder = !!folder && folder.trim().length > 0 && folder.trim() !== ".";
  const [scope, setScope] = useState<"page" | "folder">("page");
  const effectiveTarget =
    scope === "folder" && hasFolder ? folder!.trim() : target;
  const pageName = target.includes("/")
    ? target.slice(target.lastIndexOf("/") + 1)
    : target;
  const [method, setMethod] = useState<Method>("publish");
  const [host, setHost] = useState<Host>("cloudflare-pages");
  const [gateMode, setGateMode] = useState<GateMode>("email");
  const [emailDomain, setEmailDomain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ShareResult | null>(null);
  const [done, setDone] = useState<DownloadDone | null>(null);
  const [copied, setCopied] = useState(false);
  const [withComments, setWithComments] = useState(false);

  // Native single-page format follows the artifact's extension (the server
  // decides authoritatively; this only drives the label).
  const isMd = /\.(md|markdown)$/i.test(target);
  const pageExt = isMd ? ".md" : ".html";

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const dlg = ref.current;
    if (dlg && !dlg.open) dlg.showModal();
    return () => trigger?.focus?.();
  }, []);

  useEffect(() => {
    const dlg = ref.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  // GitHub Pages can't gate — it's the public lane.
  const isGithub = host === "github-pages";
  const effectiveGate: GateMode = isGithub ? "public" : gateMode;
  const needsDomain = method === "publish" && effectiveGate === "email";
  const canSubmit = !busy && (!needsDomain || emailDomain.trim().length > 0);
  const showForm = !result && !done;
  // A folder can't be downloaded as a single uncompressed page.
  const canDownloadPage = allowPage && scope === "page";

  async function submit() {
    setError(null);
    setBusy(true);
    try {
      if (method === "bundle") {
        const r = await exportShareBundle(kb, {
          target: effectiveTarget,
          include_comments: withComments,
        });
        saveBlob(r.filename, r.blob);
        setDone({
          kind: "bundle",
          files: r.files,
          danglers: r.danglers,
          filename: r.filename,
        });
        return;
      }
      if (method === "page") {
        const r = await exportSharePage(kb, { target: effectiveTarget });
        saveBlob(r.filename, r.blob);
        setDone({ kind: "page", filename: r.filename, danglers: r.danglers });
        return;
      }
      const input =
        effectiveGate === "public"
          ? {
              target: effectiveTarget,
              host,
              public: true,
              include_comments: withComments,
            }
          : {
              target: effectiveTarget,
              host,
              gate:
                effectiveGate === "email"
                  ? [`email:${emailDomain.trim()}`]
                  : [effectiveGate],
              include_comments: withComments,
            };
      const r = await createShare(kb, input);
      setResult(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  function copyUrl() {
    if (!result) return;
    navigator.clipboard
      .writeText(result.url)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => toast.err("Couldn't copy link"));
  }

  return (
    <dialog
      ref={ref}
      className="cp__modal share-modal"
      aria-label="share artifact"
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      <div className="cp__modal-inner">
        <header className="cp__modal-head">
          <div className="cp__row-meta">
            <span className="cp__row-author">Share</span>
            <span className="cp__row-anchor">{effectiveTarget}</span>
          </div>
          <button onClick={onClose} aria-label="close" className="cp__modal-x">
            <Icon.X />
          </button>
        </header>

        <div className="cp__modal-body share-modal__body">
          {result ? (
            <div className="share-modal__result">
              <p className="share-modal__done">
                {result.updated ? "Updated" : "Published"} ·{" "}
                {result.gate ? `gated (${result.gate})` : "public"} ·{" "}
                {result.files} file{result.files === 1 ? "" : "s"}
              </p>
              <div className="share-modal__url">
                <input
                  type="text"
                  readOnly
                  value={result.url}
                  aria-label="share URL"
                  onFocus={(e) => e.currentTarget.select()}
                />
                <button onClick={copyUrl} className="share-modal__copy">
                  {copied ? "copied ✓" : "copy"}
                </button>
                <a
                  href={result.url}
                  target="_blank"
                  rel="noreferrer noopener"
                  className="share-modal__open"
                  title="open in new tab"
                >
                  ↗
                </a>
              </div>
              {result.danglers.length > 0 && (
                <p className="share-modal__warn">
                  ⚠ {result.danglers.length} cross-artifact link
                  {result.danglers.length === 1 ? "" : "s"} point outside the
                  share — readers will hit dead links unless you also share
                  those artifacts.
                </p>
              )}
            </div>
          ) : done ? (
            <div className="share-modal__result">
              <p className="share-modal__done">
                Downloaded {done.filename}
                {done.kind === "bundle"
                  ? ` · ${done.files} file${done.files === 1 ? "" : "s"}`
                  : ""}
              </p>
              <p className="share-modal__hint">
                {done.kind === "bundle"
                  ? "A self-contained bundle — open its entry page offline, or host the folder anywhere. In-share links work without the daemon."
                  : "A single uncompressed page in its native format. The kb-prompt is stripped and your outbound redactions applied."}
              </p>
              {done.danglers.length > 0 && (
                <p className="share-modal__warn">
                  ⚠ {done.danglers.length} cross-artifact link
                  {done.danglers.length === 1 ? "" : "s"} point outside the{" "}
                  {done.kind === "bundle" ? "bundle" : "page"} —{" "}
                  {done.kind === "bundle"
                    ? "those will be dead links offline unless you bundle a folder that includes them."
                    : "they'll be dead links in the standalone file."}
                </p>
              )}
            </div>
          ) : (
            <>
              {hasFolder && (
                <fieldset className="share-modal__field">
                  <legend>What to share</legend>
                  <label>
                    <input
                      type="radio"
                      name="share-scope"
                      checked={scope === "page"}
                      onChange={() => setScope("page")}
                    />
                    This page{" "}
                    <span className="share-modal__hint">{pageName}</span>
                  </label>
                  <label>
                    <input
                      type="radio"
                      name="share-scope"
                      checked={scope === "folder"}
                      onChange={() => {
                        setScope("folder");
                        // A folder can't ship as a single uncompressed page.
                        if (method === "page") setMethod("publish");
                      }}
                    />
                    Whole folder{" "}
                    <span className="share-modal__hint">{folder}/</span>
                  </label>
                </fieldset>
              )}

              <fieldset className="share-modal__field">
                <legend>How to share</legend>
                <label>
                  <input
                    type="radio"
                    name="share-method"
                    checked={method === "publish"}
                    onChange={() => setMethod("publish")}
                  />
                  Publish online{" "}
                  <span className="share-modal__hint">(live URL)</span>
                </label>
                <label>
                  <input
                    type="radio"
                    name="share-method"
                    checked={method === "bundle"}
                    onChange={() => setMethod("bundle")}
                  />
                  Download bundle{" "}
                  <span className="share-modal__hint">(offline .zip)</span>
                </label>
                {canDownloadPage && (
                  <label>
                    <input
                      type="radio"
                      name="share-method"
                      checked={method === "page"}
                      onChange={() => setMethod("page")}
                    />
                    Download page{" "}
                    <span className="share-modal__hint">
                      (uncompressed {pageExt})
                    </span>
                  </label>
                )}
              </fieldset>

              {method === "publish" && (
                <>
                  <fieldset className="share-modal__field">
                    <legend>Host</legend>
                    <label>
                      <input
                        type="radio"
                        name="share-host"
                        checked={host === "cloudflare-pages"}
                        onChange={() => setHost("cloudflare-pages")}
                      />
                      Cloudflare Pages{" "}
                      <span className="share-modal__hint">(gated)</span>
                    </label>
                    <label>
                      <input
                        type="radio"
                        name="share-host"
                        checked={host === "github-pages"}
                        onChange={() => setHost("github-pages")}
                      />
                      GitHub Pages{" "}
                      <span className="share-modal__hint">(public)</span>
                    </label>
                  </fieldset>

                  {!isGithub && (
                    <fieldset className="share-modal__field">
                      <legend>Who can open it</legend>
                      {(
                        [
                          ["email", "Email (one-time PIN)"],
                          ["google", "Google sign-in"],
                          ["github", "GitHub sign-in"],
                          ["public", "Anyone with the link"],
                        ] as [GateMode, string][]
                      ).map(([mode, label]) => (
                        <label key={mode}>
                          <input
                            type="radio"
                            name="share-gate"
                            checked={gateMode === mode}
                            onChange={() => setGateMode(mode)}
                          />
                          {label}
                        </label>
                      ))}
                      {needsDomain && (
                        <input
                          type="text"
                          className="share-modal__domain"
                          placeholder="allowed email domain, e.g. example.com"
                          value={emailDomain}
                          onChange={(e) => setEmailDomain(e.target.value)}
                          aria-label="allowed email domain"
                        />
                      )}
                    </fieldset>
                  )}

                  {isGithub && (
                    <p className="share-modal__hint">
                      GitHub Pages serves the artifact world-readable
                      (secret-URL only). The export scrub still runs.
                    </p>
                  )}
                </>
              )}

              {method === "bundle" && (
                <p className="share-modal__hint">
                  A self-contained <code>.zip</code> with cross-artifact links
                  rewritten to relative paths — opens offline with no daemon, and
                  you can host the folder anywhere. Share a folder target to
                  bundle a whole set of linked artifacts.
                </p>
              )}

              {method === "page" && (
                <p className="share-modal__hint">
                  The single open page, uncompressed, in its native format —{" "}
                  {isMd ? (
                    <>
                      the raw <code>.md</code> source
                    </>
                  ) : (
                    <>
                      a self-contained <code>.html</code>
                    </>
                  )}
                  . The kb-prompt is stripped and your outbound redactions
                  applied; links to other artifacts won't resolve offline.
                </p>
              )}

              {method !== "page" && (
                <label className="share-modal__comments">
                  <input
                    type="checkbox"
                    checked={withComments}
                    onChange={(e) => setWithComments(e.target.checked)}
                  />
                  Include comments + attachments
                </label>
              )}
              {method !== "page" && withComments && (
                <p className="share-modal__error">
                  ⚠ This{" "}
                  {method === "bundle"
                    ? "writes your review comments AND their attached files into the bundle"
                    : `publishes your review comments AND their attached files onto the ${
                        isGithub ? "public" : "gated"
                      } site`}
                  .
                </p>
              )}

              {error && <p className="share-modal__error">{error}</p>}
            </>
          )}
        </div>

        <footer className="cp__modal-foot">
          {showForm ? (
            <>
              <button onClick={onClose}>cancel</button>
              <button
                className="cp__modal-save"
                disabled={!canSubmit}
                onClick={submit}
              >
                {busy
                  ? method === "publish"
                    ? "publishing…"
                    : "preparing…"
                  : method === "publish"
                    ? "publish"
                    : method === "bundle"
                      ? "download .zip"
                      : `download ${pageExt}`}
              </button>
            </>
          ) : (
            <button onClick={onClose}>done</button>
          )}
        </footer>
      </div>
    </dialog>
  );
}
