import type { Attachment } from "../api/client";
import { attachmentServeUrl, humanSize, isImageType } from "../lib/attachmentUrl";
import { Icon } from "./icons";

// Y-track — the attachment "strip" rendered under a comment/reply body:
// image thumbnails + file chips for every attachment the comment owns,
// independent of any inline `attachment:` ref in the body text. Detach (the
// × button) is shown only when `onDetach` is provided (own comments).

type Props = {
  kb: string;
  id: string;
  attachments: Attachment[] | undefined;
  /// Open the full image in the lightbox.
  onOpenImage?: (url: string, alt: string) => void;
  /// Remove the attachment (own comments/replies only). Omit → no × button.
  onDetach?: (aid: string) => void;
};

export default function AttachmentStrip({
  kb,
  id,
  attachments,
  onOpenImage,
  onDetach,
}: Props) {
  if (!attachments || attachments.length === 0) return null;
  return (
    <div className="cp__att-strip" aria-label="attachments">
      {attachments.map((a) => {
        const url = attachmentServeUrl(kb, id, a.id);
        return (
          <div key={a.id} className="cp__att-item" title={a.filename}>
            {isImageType(a.contentType) ? (
              <img
                className="cp__att-thumb"
                src={url}
                alt={a.filename}
                loading="lazy"
                onClick={() => onOpenImage?.(url, a.filename)}
              />
            ) : (
              <a
                className="cp__att-chip"
                href={url}
                target="_blank"
                rel="noopener noreferrer"
                download
              >
                <span className="cp__att-icon" aria-hidden="true">
                  <Icon.Paperclip />
                </span>
                <span className="cp__att-name">{a.filename}</span>
                <span className="cp__att-size">{humanSize(a.size)}</span>
              </a>
            )}
            {onDetach && (
              <button
                type="button"
                className="cp__att-detach"
                aria-label={`remove ${a.filename}`}
                title="remove attachment"
                onClick={() => onDetach(a.id)}
              >
                <Icon.X />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
