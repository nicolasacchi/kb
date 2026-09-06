import { useEffect, useRef } from "react";

// Y-track — full-size image viewer for an attachment. Native <dialog> via
// .showModal() with the dual cancel + keydown-Escape close, mirroring
// CommentModal (synthesized Escape from headless Chromium doesn't reliably
// fire `cancel`). Click the backdrop or the image to close.

type Props = {
  url: string;
  alt: string;
  onClose: () => void;
};

export default function AttachmentLightbox({ url, alt, onClose }: Props) {
  const ref = useRef<HTMLDialogElement | null>(null);

  useEffect(() => {
    const dlg = ref.current;
    if (dlg && !dlg.open) dlg.showModal();
    const close = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close(e);
    };
    dlg?.addEventListener("cancel", close);
    dlg?.addEventListener("keydown", onKey);
    return () => {
      dlg?.removeEventListener("cancel", close);
      dlg?.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  return (
    <dialog
      ref={ref}
      className="cp__lightbox"
      aria-label={alt || "image"}
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      <img
        className="cp__lightbox-img"
        src={url}
        alt={alt || "attachment"}
        onClick={onClose}
      />
    </dialog>
  );
}
