// V76-R2b — ONE tree for the review Files tab and the diff map column.
//
// The component renders rows it did not compute: `buildStatusSections`
// (lib/reviewFileTree.ts) is the projection; this file is the renderer
// (kbc-tree/1). Click/Enter on a FILE is `onPick` (the parent decides
// whether that expands an inline diff or opens the full-page center).
// Click on a FOLDER toggles collapse. Kind icons come from the cached
// syntax/1 registry (`useSyntax`); a generic file glyph is the fallback.

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MutableRefObject,
  type ReactNode,
} from "react";
import type { ReviewFileRow, ReviewRiskFile, SyntaxRowOut } from "../../api/types";
import { Icon } from "../icons";
import RiskBadge from "./RiskBadge";
import {
  buildStatusSections,
  countsText,
  flattenVisible,
  langIdFromSyntax,
  middleTruncate,
  type FileStatusKind,
  type FileTreeFolderNode,
  type StatusSection,
} from "../../lib/reviewFileTree";
import { mapRowTitle, type MapRowState } from "../../lib/reviewMapColumn";

const PATH_BUDGET = 28;

export interface ReviewFileTreeProps {
  files: readonly ReviewFileRow[];
  stateByPath?: ReadonlyMap<string, MapRowState>;
  currentPath: string;
  syntaxRows?: readonly SyntaxRowOut[] | null;
  onPick: (path: string) => void;
  /// Files-tab attrs (`data-kbc-review-file-row`) vs map attrs
  /// (`data-kbc-rdiff-map-row`). Both may be set.
  rowAttr?: "map" | "files" | "both";
  /// Optional body under the currently-open Files-tab row (inline diff).
  expandedPath?: string | null;
  expandedContent?: ReactNode;
  /// Imperative handle the route uses for `g f` / `z f` / `z m`.
  treeRef?: MutableRefObject<ReviewFileTreeHandle | null>;
  /// Files-tab only: B2 risk badges, one per file (null → "—", never 0).
  riskAvailable?: boolean;
  riskByPath?: ReadonlyMap<string, ReviewRiskFile>;
  onToggleViewed?: (file: ReviewFileRow) => void;
}

export interface ReviewFileTreeHandle {
  focus: () => void;
  toggleFocusedFolder: () => void;
  collapseAllFolders: () => void;
}

function Chip({
  cls,
  label,
  title,
  attr,
}: {
  cls: string;
  label: string;
  title: string;
  attr?: Record<string, string>;
}) {
  return (
    <span className={`kbc-rmap__chip ${cls}`} title={title} {...attr}>
      {label}
    </span>
  );
}

function FileKindIcon({ lang }: { lang: string | null }) {
  if (!lang) {
    return (
      <span className="kbc-ftree__kind" data-kbc-file-kind="generic" title="unknown type">
        <Icon.File />
      </span>
    );
  }
  const abbr = lang.length <= 2 ? lang : lang.slice(0, 2);
  return (
    <span className="kbc-ftree__kind" data-kbc-file-kind={lang} title={lang}>
      <span className="kbc-ftree__kind-abbr" aria-hidden>
        {abbr}
      </span>
    </span>
  );
}

export default function ReviewFileTree({
  files,
  stateByPath,
  currentPath,
  syntaxRows,
  onPick,
  rowAttr = "map",
  expandedPath,
  expandedContent,
  treeRef,
  riskAvailable,
  riskByPath,
  onToggleViewed,
}: ReviewFileTreeProps) {
  const sections = useMemo(() => buildStatusSections(files), [files]);
  const [collapsedSections, setCollapsedSections] = useState<Set<FileStatusKind>>(() => new Set());
  const [collapsedFolders, setCollapsedFolders] = useState<Set<string>>(() => new Set());
  const [treeCursor, setTreeCursor] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const currentRef = useRef<HTMLDivElement | null>(null);

  const visible = useMemo(
    () => flattenVisible(sections, collapsedFolders, collapsedSections),
    [sections, collapsedFolders, collapsedSections],
  );

  useEffect(() => {
    currentRef.current?.scrollIntoView({ block: "nearest" });
  }, [currentPath, treeCursor]);

  function toggleSection(s: FileStatusKind) {
    setCollapsedSections((prev) => {
      const next = new Set(prev);
      if (next.has(s)) next.delete(s);
      else next.add(s);
      return next;
    });
  }
  function toggleFolder(_status: FileStatusKind, path: string) {
    setCollapsedFolders((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }
  function folderCollapsed(_status: FileStatusKind, path: string): boolean {
    return collapsedFolders.has(path);
  }

  function collapseAllFolders() {
    const keys = new Set<string>();
    for (const row of flattenVisible(sections, new Set(), collapsedSections)) {
      if (row.node.kind === "folder") keys.add(row.node.path);
    }
    setCollapsedFolders(keys);
  }

  function toggleFocusedFolder() {
    const key = treeCursor ?? currentPath;
    const row = visible.find((r) => r.key === key || (r.node.kind === "file" && r.node.path === key));
    if (!row) return;
    if (row.node.kind === "folder") toggleFolder(row.status, row.node.path);
    else {
      const slash = row.node.path.lastIndexOf("/");
      if (slash > 0) toggleFolder(row.status, row.node.path.slice(0, slash));
    }
  }

  useEffect(() => {
    if (!treeRef) return;
    const handle: ReviewFileTreeHandle = {
      focus: () => rootRef.current?.focus(),
      toggleFocusedFolder,
      collapseAllFolders,
    };
    treeRef.current = handle;
    return () => {
      treeRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [treeRef, visible, treeCursor, currentPath, sections, collapsedSections]);

  function onTreeKey(e: KeyboardEvent<HTMLDivElement>) {
    // Tree navigation ONLY for keys that originate on the tree itself or
    // one of its rows. The Files tab mounts the inline diff (composer,
    // CM6 suggestion editor) UNDER the picked row as `expandedContent`,
    // so every keystroke typed there bubbles here — and an `Enter` that
    // re-picked the file remounted the editor mid-edit (suggestions.spec).
    const target = e.target as HTMLElement | null;
    if (target && target !== e.currentTarget && !target.closest(".kbc-ftree__row")) return;
    if (target && (target.isContentEditable || target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
    const keys = new Set(["j", "k", "ArrowDown", "ArrowUp", "h", "l", "Enter", "ArrowLeft", "ArrowRight"]);
    if (!keys.has(e.key)) return;
    e.preventDefault();
    e.stopPropagation();
    const idx = Math.max(
      0,
      visible.findIndex((r) => r.key === (treeCursor ?? currentPath) || (r.node.kind === "file" && r.node.path === currentPath)),
    );
    if (e.key === "j" || e.key === "ArrowDown") {
      const next = visible[Math.min(visible.length - 1, idx + 1)];
      if (next) setTreeCursor(next.node.kind === "file" ? next.node.path : next.key);
      return;
    }
    if (e.key === "k" || e.key === "ArrowUp") {
      const prev = visible[Math.max(0, idx - 1)];
      if (prev) setTreeCursor(prev.node.kind === "file" ? prev.node.path : prev.key);
      return;
    }
    const row = visible[idx];
    if (!row) return;
    if (e.key === "Enter") {
      if (row.node.kind === "file") onPick(row.node.path);
      else toggleFolder(row.status, row.node.path);
      return;
    }
    if (e.key === "h" || e.key === "ArrowLeft") {
      if (row.node.kind === "folder" && !folderCollapsed(row.status, row.node.path)) {
        toggleFolder(row.status, row.node.path);
      }
      return;
    }
    if (e.key === "l" || e.key === "ArrowRight") {
      if (row.node.kind === "folder" && folderCollapsed(row.status, row.node.path)) {
        toggleFolder(row.status, row.node.path);
      } else if (row.node.kind === "file") onPick(row.node.path);
    }
  }

  function fileRowAttrs(path: string, current: boolean): Record<string, string | undefined> {
    const attrs: Record<string, string | undefined> = {};
    if (rowAttr === "map" || rowAttr === "both") {
      attrs["data-kbc-rdiff-map-row"] = path;
      if (current) attrs["data-kbc-rdiff-map-current"] = "1";
    }
    if (rowAttr === "files" || rowAttr === "both") {
      attrs["data-kbc-review-file-row"] = path;
    }
    return attrs;
  }

  function renderFolder(node: FileTreeFolderNode, depth: number, status: FileStatusKind) {
    const collapsed = folderCollapsed(status, node.path);
    const cursor = treeCursor === `dir:${node.path}`;
    return (
      <li key={`dir:${status}:${node.path}`}>
        <button
          type="button"
          className={"kbc-ftree__row kbc-ftree__row--folder" + (cursor ? " is-cursor" : "")}
          style={{ paddingInlineStart: 8 + depth * 12 }}
          aria-expanded={!collapsed}
          title={`${node.path || "/"} · ${countsText(node.counts)}`}
          onClick={() => toggleFolder(status, node.path)}
          data-kbc-rdiff-map-folder={node.path}
        >
          <span className="kbc-ftree__twist" aria-hidden>
            {collapsed ? <Icon.Chevron /> : <Icon.ChevDown />}
          </span>
          <span className="kbc-ftree__kind" data-kbc-file-kind="folder" title="folder">
            <Icon.Folder />
          </span>
          <span className="kbc-ftree__name">{node.name}</span>
          <span className="kbc-ftree__counts">{countsText(node.counts)}</span>
        </button>
        {!collapsed && <ul className="kbc-ftree__list">{node.children.map((c) => renderNode(c, depth + 1, status))}</ul>}
      </li>
    );
  }

  function renderFile(file: ReviewFileRow, name: string, depth: number) {
    const st = stateByPath?.get(file.path);
    const current = file.path === currentPath;
    const lang = langIdFromSyntax(file.path, syntaxRows);
    const shown = middleTruncate(name, PATH_BUDGET);
    const title = st ? mapRowTitle(st) : file.path;
    return (
      <li key={file.path}>
        <div
          className={"kbc-review__file" + (rowAttr !== "map" ? "" : "")}
          data-kbc-review-file={rowAttr !== "map" ? file.path : undefined}
        >
          <div
            ref={current ? currentRef : undefined}
            role="button"
            tabIndex={0}
            className={"kbc-ftree__row kbc-ftree__row--file" + (current ? " is-current" : "")}
            style={{ paddingInlineStart: 8 + depth * 12 }}
            title={`${title} · ${file.path}`}
            onClick={() => onPick(file.path)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onPick(file.path);
              }
            }}
            {...fileRowAttrs(file.path, current)}
          >
            <span className="kbc-ftree__twist" aria-hidden />
            <FileKindIcon lang={lang} />
            <span className="kbc-ftree__name kbc-ftree__path" data-kbc-ftree-path={file.path}>
              {shown}
            </span>
            <span className="kbc-rmap__stats">
              <span className="kbc-review__file-add">+{file.additions}</span>{" "}
              <span className="kbc-review__file-del">−{file.deletions}</span>
            </span>
            {riskAvailable && <RiskBadge file={file} riskRow={riskByPath?.get(file.path)} />}
            {onToggleViewed && (
              <label className="kbc-review__file-viewed" onClick={(e) => e.stopPropagation()}>
                <input
                  type="checkbox"
                  checked={!!(file.viewed && !file.viewed_stale)}
                  onChange={() => onToggleViewed(file)}
                  aria-label={file.viewed && !file.viewed_stale ? "mark unviewed" : "mark viewed"}
                  data-kbc-review-viewed={file.path}
                />
              </label>
            )}
            <span className="kbc-rmap__chips">
              {st?.viewed && (
                <Chip
                  cls={"kbc-rmap__chip--viewed" + (st.viewedStale ? " kbc-rmap__chip--stale" : "")}
                  label={st.viewedStale ? "viewed?" : "viewed"}
                  title={
                    st.viewedStale
                      ? "marked viewed, but this file's blob has changed since"
                      : "marked viewed at this blob"
                  }
                  attr={{ "data-kbc-rdiff-map-viewed": st.viewedStale ? "stale" : "1" }}
                />
              )}
              {(st?.openComments ?? 0) > 0 && (
                <Chip
                  cls="kbc-rmap__chip--comments"
                  label={`${st?.openComments}`}
                  title={`${st?.openComments} open comment thread(s)`}
                  attr={{ "data-kbc-rdiff-map-comments": String(st?.openComments) }}
                />
              )}
              {(st?.findings ?? 0) > 0 && (
                <Chip
                  cls="kbc-rmap__chip--findings"
                  label={`f${st?.findings}`}
                  title={`${st?.findings} finding(s) anchored in this file`}
                  attr={{ "data-kbc-rdiff-map-findings": String(st?.findings) }}
                />
              )}
              {(st?.drafts ?? 0) > 0 && (
                <Chip
                  cls="kbc-rmap__chip--draft"
                  label={`${st?.drafts} draft`}
                  title={`${st?.drafts} unpublished draft(s) on this file`}
                  attr={{ "data-kbc-rdiff-map-drafts": String(st?.drafts) }}
                />
              )}
              {(st?.noise ?? []).map((n) => (
                <Chip
                  key={n}
                  cls={`kbc-rmap__chip--noise kbc-noise--${n}`}
                  label={n}
                  title={`noise label: ${n} — the rule is on the hunk chip`}
                  attr={{ "data-kbc-rdiff-map-noise": n }}
                />
              ))}
            </span>
          </div>
          {expandedPath === file.path && expandedContent}
        </div>
      </li>
    );
  }

  function renderNode(node: StatusSection["tree"][number], depth: number, status: FileStatusKind) {
    if (node.kind === "folder") return renderFolder(node, depth, status);
    return renderFile(node.file, node.name, depth);
  }

  if (files.length === 0) {
    return <p className="kbc-rmap__note">No files in this patchset.</p>;
  }

  return (
    <div
      ref={rootRef}
      className="kbc-ftree"
      tabIndex={0}
      onKeyDown={onTreeKey}
      data-kbc-ftree
      aria-label="Review files"
    >
      {sections.map((sec) => {
        const closed = collapsedSections.has(sec.status);
        return (
          <section
            key={sec.status}
            className="kbc-ftree__section"
            data-kbc-rdiff-map-status={sec.status}
            data-kbc-rdiff-map-chapter={sec.label}
          >
            <button
              type="button"
              className="kbc-ftree__section-head"
              aria-expanded={!closed}
              onClick={() => toggleSection(sec.status)}
              data-kbc-ftree-section={sec.status}
            >
              <span className="kbc-ftree__twist" aria-hidden>
                {closed ? <Icon.Chevron /> : <Icon.ChevDown />}
              </span>
              <h2 className="kbc-rmap__chapter-head">
                {sec.label}
                <span className="kbc-rmap__chapter-n">{countsText(sec.counts)}</span>
              </h2>
            </button>
            {!closed && <ul className="kbc-ftree__list">{sec.tree.map((n) => renderNode(n, 0, sec.status))}</ul>}
          </section>
        );
      })}
    </div>
  );
}
