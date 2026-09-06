import { useEffect, useRef, useState } from "react";
import type { ClipboardEvent as RClipboardEvent, DragEvent as RDragEvent } from "react";
import { useNavigate } from "react-router-dom";
import { captureUpload } from "../api/capture";
import { useKbs } from "../hooks/useKbs";
import { useActiveKb } from "../hooks/useActiveKb";
import { artifactHref } from "../lib/artifactHref";
import { DRAFT_TAG } from "../lib/draft";
import { buildPastedFile, type PasteFormat } from "../lib/pasteFile";
import { toast } from "../lib/toast";
import { Icon } from "./icons";

// U4 (v0.25 quick capture) — the SPA's "get a file into a kb" sheet. A
// native <dialog> via .showModal() (focus trap + ::backdrop + Escape),
// same shell as ShareModal/CommentModal (`cp__modal`) — already usable
// down to phone widths (`width: min(640px, 92vw)`, app.css) with no extra
// mobile-specific CSS needed.
//
// Files are staged CLIENT-SIDE (picker / drag-drop / paste all just push
// onto `picked`) and only sent as one multipart POST on submit — unlike
// useComposerAttachments' upload-per-file-on-drop (which stages server-side
// immediately because a comment/reply composer has nowhere else to hold an
// uploaded id). Capture's title/tags/sanitize apply to the whole batch, so
// there's nothing to submit until the user hits "capture". The paste-text
// box (C1) stages nothing itself — its contents are only synthesized into a
// `File` (`buildPastedFile`) and joined onto the batch at submit time.
//
// Deliberately does NOT filter memory-scope kbs out of the picker (unlike
// MemoryPromoteModal) — the capture endpoint has no such server-side
// restriction, so showing every configured kb matches what actually works.

const ACCEPT = ".md,.markdown,.html,.htm,.txt";

type PickedFile = { key: string; file: File };

export default function CaptureSheet({ onClose }: { onClose: () => void }) {
  const ref = useRef<HTMLDialogElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const navigate = useNavigate();
  const { data: kbs = [] } = useKbs();
  const defaultKb = useActiveKb();
  const [kb, setKb] = useState("");
  const [picked, setPicked] = useState<PickedFile[]>([]);
  const [pasteText, setPasteText] = useState("");
  const [pasteFormat, setPasteFormat] = useState<PasteFormat>("md");
  const [title, setTitle] = useState("");
  const [tags, setTags] = useState("");
  const [sanitize, setSanitize] = useState(false);
  // W2.8 — capture-to-draft (zero-daemon version, see lib/draft.ts). Stamps
  // the plain `draft` tag onto the batch; off by default so a plain capture
  // behaves exactly as before. Plain state (no sessionStorage) — like
  // `sanitize`, this sheet doesn't persist checkbox prefs across opens.
  // NOTE: the Android Web Share Target route (`POST /capture`, capture.rs)
  // bypasses this sheet entirely, so a share-sheet capture can never set
  // this — a recorded gap, not a bug.
  const [saveAsDraft, setSaveAsDraft] = useState(false);
  const [busy, setBusy] = useState(false);
  const seq = useRef(0);

  // Seed the kb picker from the active kb once it's known; leaves the
  // user's own pick alone thereafter.
  useEffect(() => {
    if (kb || !defaultKb) return;
    setKb(defaultKb);
  }, [defaultKb, kb]);

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

  function addFiles(fs: File[]) {
    if (fs.length === 0) return;
    setPicked((p) => [
      ...p,
      ...fs.map((file) => ({ key: `f${seq.current++}-${file.name}`, file })),
    ]);
  }

  function removeFile(key: string) {
    setPicked((p) => p.filter((x) => x.key !== key));
  }

  async function submit() {
    const trimmedPaste = pasteText.trim();
    if ((picked.length === 0 && trimmedPaste.length === 0) || !kb || busy) return;
    setBusy(true);
    try {
      const tagList = tags
        .split(",")
        .map((t) => t.trim())
        .filter((t) => t.length > 0);
      // The server's slugify only collapses non-alphanumerics (colons etc.)
      // — a bare "draft" tag round-trips untouched, so this is exactly the
      // tag the gallery's default `NOT tag:draft` exclusion looks for.
      if (saveAsDraft && !tagList.includes(DRAFT_TAG)) tagList.push(DRAFT_TAG);
      const files = picked.map((p) => p.file);
      if (trimmedPaste.length > 0) {
        files.push(buildPastedFile(pasteText, pasteFormat, title.trim() || undefined));
      }
      const resp = await captureUpload(kb, {
        files,
        title: title.trim() || undefined,
        tags: tagList,
        sanitize,
      });
      for (const item of resp.items) {
        // No auto-navigate — indexing is async (the watcher's next
        // debounce); the "Open" action lets the user jump once it's ready
        // instead of the sheet guessing when that is.
        toast.ok(`Captured "${item.title}"`, {
          label: "Open",
          onClick: () => navigate(artifactHref(item.kb, item.source_relative)),
        });
      }
      onClose();
    } catch (e) {
      // invariant #32 — a user-action failure surfaces via toast.err, never
      // a silent swallow. Left open (not onClose()) so the user can retry
      // without re-picking files.
      toast.err(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const canSubmit = !busy && (picked.length > 0 || pasteText.trim().length > 0) && !!kb;
  const hasPaste = pasteText.trim().length > 0;
  const submitLabel = busy
    ? "capturing…"
    : hasPaste && picked.length > 0
      ? `capture ${picked.length + 1} items`
      : hasPaste
        ? "capture text"
        : picked.length > 0
          ? `capture ${picked.length} file${picked.length === 1 ? "" : "s"}`
          : "capture";

  return (
    <dialog
      ref={ref}
      className="cp__modal capture-sheet"
      aria-label="capture files"
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
      onPaste={(e: RClipboardEvent<HTMLDialogElement>) => {
        const fs = Array.from(e.clipboardData?.files ?? []);
        if (fs.length) {
          e.preventDefault();
          addFiles(fs);
        }
      }}
    >
      <div className="cp__modal-inner">
        <header className="cp__modal-head">
          <div className="cp__row-meta">
            <span className="cp__row-author">Capture</span>
          </div>
          <button onClick={onClose} aria-label="close" className="cp__modal-x">
            <Icon.X />
          </button>
        </header>

        <div className="cp__modal-body capture-sheet__body">
          <div
            className="capture-sheet__drop"
            onDragOver={(e: RDragEvent<HTMLDivElement>) => e.preventDefault()}
            onDrop={(e: RDragEvent<HTMLDivElement>) => {
              e.preventDefault();
              addFiles(Array.from(e.dataTransfer?.files ?? []));
            }}
          >
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept={ACCEPT}
              className="capture-sheet__file-input"
              data-testid="capture-file-input"
              onChange={(e) => {
                addFiles(Array.from(e.target.files ?? []));
                // Reset so picking the same file again re-fires onChange.
                e.target.value = "";
              }}
            />
            <button
              type="button"
              className="capture-sheet__pick"
              onClick={() => fileInputRef.current?.click()}
            >
              Choose files…
            </button>
            <p className="capture-sheet__hint">
              or drag &amp; drop / paste .md, .html, or .txt files here
            </p>
          </div>

          <div className="capture-sheet__paste">
            <label className="capture-sheet__field">
              paste text (optional)
              <textarea
                value={pasteText}
                onChange={(e) => setPasteText(e.target.value)}
                placeholder="or paste markdown / HTML / plain text here"
                rows={6}
                data-testid="capture-text"
              />
            </label>
            <div className="capture-sheet__format" role="radiogroup" aria-label="paste format">
              {(
                [
                  { value: "md", label: "markdown" },
                  { value: "txt", label: "plain text" },
                  { value: "html", label: "html" },
                ] as const
              ).map((opt) => (
                <label key={opt.value} className="capture-sheet__format-opt">
                  <input
                    type="radio"
                    name="capture-paste-format"
                    checked={pasteFormat === opt.value}
                    onChange={() => setPasteFormat(opt.value)}
                    data-testid={`capture-format-${opt.value}`}
                  />
                  {opt.label}
                </label>
              ))}
            </div>
          </div>

          {picked.length > 0 && (
            <ul className="capture-sheet__files">
              {picked.map((p) => (
                <li key={p.key} className="capture-sheet__file">
                  <span className="capture-sheet__file-name">{p.file.name}</span>
                  <button
                    type="button"
                    className="capture-sheet__file-x"
                    aria-label={`remove ${p.file.name}`}
                    onClick={() => removeFile(p.key)}
                  >
                    <Icon.X />
                  </button>
                </li>
              ))}
            </ul>
          )}

          <label className="capture-sheet__field">
            kb
            <select
              value={kb}
              onChange={(e) => setKb(e.target.value)}
              data-testid="capture-kb"
            >
              {kbs.length === 0 && <option value="">—</option>}
              {kbs.map((k) => (
                <option key={k.name} value={k.name}>
                  {k.name}
                </option>
              ))}
            </select>
          </label>

          <label className="capture-sheet__field">
            title (optional)
            <input
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="derived from the filename if left blank"
              data-testid="capture-title"
            />
          </label>

          <label className="capture-sheet__field">
            tags (optional, comma-separated)
            <input
              type="text"
              value={tags}
              onChange={(e) => setTags(e.target.value)}
              placeholder="research, phone"
              data-testid="capture-tags"
            />
          </label>

          <label className="capture-sheet__sanitize">
            <input
              type="checkbox"
              checked={saveAsDraft}
              onChange={(e) => setSaveAsDraft(e.target.checked)}
              data-testid="capture-draft"
            />
            Save as draft (tags it "draft" — kept out of the gallery by
            default until you file it via its tags)
          </label>

          <label className="capture-sheet__sanitize">
            <input
              type="checkbox"
              checked={sanitize}
              onChange={(e) => setSanitize(e.target.checked)}
              data-testid="capture-sanitize"
            />
            Sanitize HTML (strip scripts/styles — off by default; the Web
            Share Target route defaults this on for saved pages)
          </label>
        </div>

        <footer className="cp__modal-foot">
          <button onClick={onClose}>cancel</button>
          <button
            className="cp__modal-save"
            disabled={!canSubmit}
            onClick={submit}
            data-testid="capture-submit"
          >
            {submitLabel}
          </button>
        </footer>
      </div>
    </dialog>
  );
}
