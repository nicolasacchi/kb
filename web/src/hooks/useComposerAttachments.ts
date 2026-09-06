import { useCallback, useRef, useState } from "react";
import { uploadAttachment, type Attachment } from "../api/client";
import { attachmentToken } from "../lib/attachmentUrl";

// Y-track — composer-side staging state for attachments. Each picked file is
// staged via `POST …/review/{id}/attachments` (one request per file, so a
// failure is isolated); on success it's added to `staged` and its markdown
// token is inserted into the draft via the `onToken` callback (the composer
// wires this to MarkdownEditor.insertAtCursor). On submit the composer passes
// `attachmentIds` to addComment/addReply, which adopt them. Abandoning a
// compose leaves the staged blobs server-side, GC-reaped after the grace
// window — the SPA does nothing.

export type StagedAttachment = Pick<
  Attachment,
  "id" | "filename" | "contentType"
>;

export type PendingUpload = {
  key: string;
  filename: string;
  file: File;
  error?: string;
};

export type ComposerAttachments = {
  staged: StagedAttachment[];
  pending: PendingUpload[];
  /// Ids to pass to addComment/addReply on submit.
  attachmentIds: string[];
  /// `true` while any upload is still in flight (submit should wait).
  uploading: boolean;
  /// Stage `files`; insert each one's markdown token via `onToken` on success.
  upload: (files: File[], onToken: (token: string) => void) => void;
  /// Re-stage a previously-failed upload.
  retry: (item: PendingUpload, onToken: (token: string) => void) => void;
  /// Drop a staged attachment from the draft (does NOT detach server-side —
  /// the comment doesn't exist yet; the orphan is GC-reaped).
  removeStaged: (aid: string) => void;
  reset: () => void;
};

export function useComposerAttachments(
  kb: string,
  id: string,
): ComposerAttachments {
  const [staged, setStaged] = useState<StagedAttachment[]>([]);
  const [pending, setPending] = useState<PendingUpload[]>([]);
  const seq = useRef(0);

  const uploadOne = useCallback(
    (file: File, onToken: (t: string) => void) => {
      const key = `u${seq.current++}-${file.name}`;
      setPending((p) => [...p, { key, filename: file.name, file }]);
      uploadAttachment(kb, id, [file])
        .then((atts) => {
          const att = atts[0];
          setPending((p) => p.filter((x) => x.key !== key));
          if (!att) return;
          setStaged((s) => [
            ...s,
            { id: att.id, filename: att.filename, contentType: att.contentType },
          ]);
          onToken(attachmentToken(att.filename, att.id, att.contentType));
        })
        .catch((e) => {
          setPending((p) =>
            p.map((x) => (x.key === key ? { ...x, error: String(e) } : x)),
          );
        });
    },
    [kb, id],
  );

  const upload = useCallback(
    (files: File[], onToken: (t: string) => void) => {
      for (const f of files) uploadOne(f, onToken);
    },
    [uploadOne],
  );

  const retry = useCallback(
    (item: PendingUpload, onToken: (t: string) => void) => {
      setPending((p) => p.filter((x) => x.key !== item.key));
      uploadOne(item.file, onToken);
    },
    [uploadOne],
  );

  const removeStaged = useCallback((aid: string) => {
    setStaged((s) => s.filter((x) => x.id !== aid));
  }, []);

  const reset = useCallback(() => {
    setStaged([]);
    setPending([]);
  }, []);

  return {
    staged,
    pending,
    attachmentIds: staged.map((s) => s.id),
    uploading: pending.some((p) => !p.error),
    upload,
    retry,
    removeStaged,
    reset,
  };
}
