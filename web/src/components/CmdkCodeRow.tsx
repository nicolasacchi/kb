import { Icon } from "@kb/ui/icons";

// OL S1 — Cmd+K's single "Search code for “q” in kb-code →" row. A real
// anchor (new tab, noopener), so middle/cmd-click and "copy link" work; a
// plain click also closes the palette.
export default function CmdkCodeRow({
  href,
  query,
  active,
  onHover,
  onOpen,
}: {
  href: string;
  query: string;
  active: boolean;
  onHover: () => void;
  onOpen: () => void;
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      role="option"
      aria-selected={active}
      data-cmdk-code
      className={`cmdk__hit cmdk__hit--code ${active ? "is-active" : ""}`}
      onMouseEnter={onHover}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        onOpen();
      }}
    >
      <span className="cmdk__hit-title">
        <Icon.Code aria-hidden="true" /> Search code for “{query.trim()}” in kb-code →
      </span>
    </a>
  );
}
