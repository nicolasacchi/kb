// FXUI — the Files tab of the terminal's right rail: Upload (queue, progress,
// cancel), Browse (the working tree and the uploads over `ls`), Download and
// Recent (this browser session's uploads). Desktop-first; single column so it
// also sits in the phone sheet. File names, paths and server messages are DATA:
// every one renders as a React text node through `displayName`, never as HTML
// and never as an href. Nothing is read from terminal bytes. No xterm import.

import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useRef, useState, type DragEvent, type ReactElement } from "react";
import { StateCard } from "@kb/ui/StateCard";
import type { FileUploadAnswer } from "../../api/generated/FileUploadAnswer";
import type { FsEntry } from "../../api/generated/FsEntry";
import { toast } from "../../lib/toast";
import { DriveError } from "../phone/driveApi";
import { useTermRail } from "../rail/context";
import { displayName } from "./displayName";
import { absPath, downloadFile, FILE_CAP_MESSAGE, FILE_MAX_BYTES, pastePath, type FilesRoot } from "./filesApi";
import { useRecent } from "./recentStore";
import { cancelUpload, clearUploads, enqueueFiles, useUploadQueue } from "./uploadRunner";
import { useFilesListing } from "./useFilesListing";
import type { QueueItem } from "./uploadQueue";
import "./filesRail.css";

/** FXSAVE seam: a sibling server unit adds the route; no button in FXUI. */
export function KeepFromWorkingTree(): null {
  return null;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

function fmtTime(unix: number): string {
  return unix > 0 ? new Date(unix * 1000).toISOString().slice(0, 16).replace("T", " ") : "";
}

const errText = (e: unknown, fallback: string): string => (e instanceof DriveError ? e.detail : fallback);

/** Paste a path into the terminal prompt, no Enter. A refusal is a toast with the server's words. */
function PasteButton({ termId, text }: { termId: string; text: string }): ReactElement {
  const [busy, setBusy] = useState(false);
  const go = async (): Promise<void> => {
    setBusy(true);
    try {
      await pastePath(termId, text);
      toast.ok("Path pasted. Press Enter in the terminal to use it.");
    } catch (e) {
      toast.err(errText(e, "Could not paste the path."));
    } finally {
      setBusy(false);
    }
  };
  return (
    <button type="button" className="kb-filesrail__btn" data-act="paste" disabled={busy} onClick={() => void go()}>
      Paste path into terminal
    </button>
  );
}

/** The kb keep status of an upload, shown honestly. */
function KeepStatus({ kb }: { kb: FileUploadAnswer["kb"] }): ReactElement {
  const copy = (): void => {
    if (kb.stub_rel && navigator.clipboard) void navigator.clipboard.writeText(kb.stub_rel).catch(() => {});
  };
  if (kb.status === "stored") {
    // The files corpus' kb name is not on the wire (KbKeep has id + stub_rel), so the
    // open-in-kb link cannot be built without guessing: show the stub path with a copy action.
    return (
      <p className="kb-filesrail__keep" data-keep="stored">
        Kept in kb{kb.stub_rel ? <>: <code className="kb-filesrail__path">{displayName(kb.stub_rel)}</code></> : null}
        {kb.stub_rel ? (
          <button type="button" className="kb-filesrail__btn kb-filesrail__btn--quiet" data-act="copy-stub" onClick={copy}>
            Copy
          </button>
        ) : null}
      </p>
    );
  }
  const why = kb.reason ? `: ${displayName(kb.reason)}` : "";
  return (
    <p className={`kb-filesrail__keep${kb.status === "failed" ? " kb-filesrail__keep--bad" : ""}`} data-keep={kb.status}>
      {kb.status === "skipped" ? "Not kept in kb" : kb.status === "failed" ? "kb keep failed" : displayName(kb.status)}
      {why}
    </p>
  );
}

function ResultBlock({ termId, r }: { termId: string; r: FileUploadAnswer }): ReactElement {
  return (
    <div className="kb-filesrail__result">
      <code className="kb-filesrail__path" data-testid="files-runner-path">
        {displayName(r.path)}
      </code>
      <PasteButton termId={termId} text={r.path} />
      <KeepStatus kb={r.kb} />
    </div>
  );
}

function QueueRow({ termId, item }: { termId: string; item: QueueItem }): ReactElement {
  const pct = item.total > 0 ? Math.min(100, Math.round((item.loaded / item.total) * 100)) : 0;
  return (
    <li className="kb-filesrail__row" data-testid="files-upload-row" data-status={item.status}>
      <div className="kb-filesrail__rowhead">
        <span className="kb-filesrail__name">{displayName(item.file.name)}</span>
        <span className="kb-filesrail__meta">{fmtBytes(item.file.size)}</span>
        {item.status === "queued" || item.status === "uploading" ? (
          <button type="button" className="kb-filesrail__btn kb-filesrail__btn--quiet" data-act="cancel" onClick={() => cancelUpload(termId, item.id)}>
            Cancel
          </button>
        ) : null}
      </div>
      {item.status === "queued" ? <p className="kb-filesrail__meta">Waiting</p> : null}
      {item.status === "uploading" ? (
        <progress className="kb-filesrail__bar" max={100} value={pct} aria-label={`Uploading ${displayName(item.file.name)}`} data-testid="files-progress">
          {pct}%
        </progress>
      ) : null}
      {item.status === "cancelled" ? <p className="kb-filesrail__meta" data-testid="files-cancelled">cancelled</p> : null}
      {item.status === "error" ? (
        <p className="kb-filesrail__err" role="alert" data-testid="files-row-error">
          {displayName(item.error ?? "Upload failed")}
        </p>
      ) : null}
      {item.status === "done" && item.result ? <ResultBlock termId={termId} r={item.result} /> : null}
    </li>
  );
}

function UploadSection({ termId }: { termId: string }): ReactElement {
  const qc = useQueryClient();
  const q = useUploadQueue(termId);
  const [index, setIndex] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);
  const idxId = useId();
  const finished = q.items.some((i) => i.status === "done" || i.status === "error" || i.status === "cancelled");
  return (
    <section className="kb-filesrail__section" aria-labelledby={`${idxId}-h`}>
      <h3 className="kb-filesrail__h" id={`${idxId}-h`}>
        Upload
      </h3>
      <div className="kb-filesrail__tools">
        <button type="button" className="kb-filesrail__btn" data-testid="files-upload-btn" onClick={() => input.current?.click()}>
          Upload files
        </button>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          data-testid="files-input"
          onChange={(e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            if (files.length > 0) enqueueFiles(termId, files, index, qc);
          }}
        />
        {finished ? (
          <button type="button" className="kb-filesrail__btn kb-filesrail__btn--quiet" data-act="clear" onClick={() => clearUploads(termId)}>
            Clear
          </button>
        ) : null}
      </div>
      <label className="kb-filesrail__check" htmlFor={idxId}>
        <input id={idxId} type="checkbox" checked={index} data-testid="files-index" onChange={(e) => setIndex(e.target.checked)} />
        Index content in kb
      </label>
      <p className="kb-filesrail__hint">Session files may contain unscrubbed content. Leave this off unless you want the text searchable in kb.</p>
      <p className="kb-filesrail__hint">Drop files anywhere on this tab. Up to 32 MiB each.</p>
      {q.items.length > 0 ? (
        <ul className="kb-filesrail__list">
          {q.items.map((i) => (
            <QueueRow key={i.id} termId={termId} item={i} />
          ))}
        </ul>
      ) : null}
    </section>
  );
}

function Crumbs({ root, path, onGo }: { root: FilesRoot; path: string; onGo: (p: string) => void }): ReactElement {
  const segs = path === "" ? [] : path.split("/");
  return (
    <nav className="kb-filesrail__crumbs" aria-label="Directory path" data-testid="files-crumbs">
      <button type="button" className="kb-filesrail__crumb" onClick={() => onGo("")}>
        {root === "cwd" ? "Working tree" : "Uploads"}
      </button>
      {segs.map((s, i) => {
        const to = segs.slice(0, i + 1).join("/");
        return (
          <span key={to} className="kb-filesrail__crumbwrap">
            <span aria-hidden="true"> / </span>
            <button type="button" className="kb-filesrail__crumb" onClick={() => onGo(to)}>
              {displayName(s)}
            </button>
          </span>
        );
      })}
    </nav>
  );
}

function EntryRow({
  termId,
  root,
  dir,
  path,
  e,
  onOpen,
}: {
  termId: string;
  root: FilesRoot;
  dir: string;
  path: string;
  e: FsEntry;
  onOpen: (p: string) => void;
}): ReactElement {
  const [pct, setPct] = useState<number | null>(null);
  const rel = path === "" ? e.name : `${path}/${e.name}`;
  const download = async (): Promise<void> => {
    setPct(0);
    try {
      await downloadFile(termId, root, rel, e.name, {
        bytes: e.bytes,
        onProgress: (l, t) => setPct(t > 0 ? Math.min(100, Math.round((l / t) * 100)) : 0),
      });
    } catch (err) {
      toast.err(`${displayName(e.name)}: ${errText(err, "Could not download the file.")}`);
    } finally {
      setPct(null);
    }
  };
  const tooBig = e.kind === "file" && e.bytes > FILE_MAX_BYTES;
  return (
    <li className="kb-filesrail__row" data-testid="files-entry" data-kind={e.kind}>
      <div className="kb-filesrail__rowhead">
        {e.kind === "dir" ? (
          <button type="button" className="kb-filesrail__link" data-act="open" onClick={() => onOpen(rel)}>
            {displayName(e.name)}/
          </button>
        ) : (
          <span className="kb-filesrail__name">{displayName(e.name)}</span>
        )}
        <span className="kb-filesrail__meta">
          {e.kind === "file" ? `${fmtBytes(e.bytes)} ` : ""}
          {fmtTime(e.mtime_unix)}
        </span>
      </div>
      <div className="kb-filesrail__tools">
        {e.kind === "file" ? (
          <button type="button" className="kb-filesrail__btn" data-act="download" disabled={pct !== null} title={tooBig ? FILE_CAP_MESSAGE : undefined} onClick={() => void download()}>
            Download
          </button>
        ) : null}
        <PasteButton termId={termId} text={absPath(dir, e.name)} />
      </div>
      {tooBig ? <p className="kb-filesrail__hint">{FILE_CAP_MESSAGE}</p> : null}
      {pct !== null ? <progress className="kb-filesrail__bar" max={100} value={pct} aria-label="Downloading" data-testid="files-dl-progress" /> : null}
    </li>
  );
}

function BrowseSection({ termId }: { termId: string }): ReactElement {
  const [root, setRoot] = useState<FilesRoot>("cwd");
  const [path, setPath] = useState("");
  const [hidden, setHidden] = useState(false);
  const q = useFilesListing(termId, root, path);
  const switchRoot = (r: FilesRoot): void => {
    setRoot(r);
    setPath("");
  };
  const data = q.data;
  const entries = (data?.entries ?? []).filter((e) => hidden || !e.hidden);
  let body: ReactElement;
  if (q.isPending) {
    body = <StateCard kind="loading" size="inline" title="Loading the directory" />;
  } else if (q.isError) {
    const err = q.error;
    const retry = [{ label: "Retry", key: "retry", onClick: () => void q.refetch() }];
    if (err instanceof DriveError && err.status === 403) {
      body = <StateCard kind="refused" size="inline" title="Outside the allowed folder" reason={displayName(err.detail)} actions={retry} />;
    } else if (err instanceof DriveError && err.status === 422) {
      body = <StateCard kind="refused" size="inline" title="Not a regular file or folder" reason={displayName(err.detail)} actions={retry} />;
    } else if (err instanceof DriveError && err.status === 503) {
      body = <StateCard kind="degraded" size="inline" title="The terminal's runner is not reachable" reason={displayName(err.detail)} actions={retry} />;
    } else {
      body = <StateCard kind="degraded" size="inline" title="Could not list the directory" reason={displayName(errText(err, "Request failed"))} actions={retry} />;
    }
  } else if (entries.length === 0) {
    body = <StateCard kind="empty" size="inline" title={data && data.entries.length > 0 ? "Only hidden files here" : "This folder is empty"} />;
  } else {
    body = (
      <ul className="kb-filesrail__list">
        {entries.map((e) => (
          <EntryRow key={e.name} termId={termId} root={root} dir={data?.dir ?? ""} path={path} e={e} onOpen={setPath} />
        ))}
      </ul>
    );
  }
  return (
    <section className="kb-filesrail__section" aria-label="Browse">
      <h3 className="kb-filesrail__h">Browse</h3>
      <div className="kb-filesrail__tools" role="group" aria-label="Root">
        <button type="button" className="kb-filesrail__btn" data-testid="files-root-cwd" aria-pressed={root === "cwd"} onClick={() => switchRoot("cwd")}>
          Working tree
        </button>
        <button type="button" className="kb-filesrail__btn" data-testid="files-root-up" aria-pressed={root === "up"} onClick={() => switchRoot("up")}>
          Uploads
        </button>
        <button type="button" className="kb-filesrail__btn kb-filesrail__btn--quiet" data-testid="files-refresh" disabled={q.isFetching} onClick={() => void q.refetch()}>
          Refresh
        </button>
      </div>
      <label className="kb-filesrail__check">
        <input type="checkbox" checked={hidden} data-testid="files-hidden" onChange={(e) => setHidden(e.target.checked)} />
        Show hidden files
      </label>
      <Crumbs root={root} path={path} onGo={setPath} />
      {data?.truncated ? (
        <p className="kb-filesrail__hint" data-testid="files-truncated">
          The list is cut off: more entries exist than are shown.
        </p>
      ) : null}
      {data && data.skipped > 0 ? (
        <p className="kb-filesrail__hint" data-testid="files-skipped">
          {data.skipped} item{data.skipped === 1 ? "" : "s"} left out (links, special files or unreadable names).
        </p>
      ) : null}
      {body}
      <KeepFromWorkingTree />
    </section>
  );
}

function RecentSection({ termId }: { termId: string }): ReactElement | null {
  const recent = useRecent(termId);
  if (recent.length === 0) return null;
  return (
    <section className="kb-filesrail__section" aria-label="Recent uploads">
      <h3 className="kb-filesrail__h">Recent</h3>
      <p className="kb-filesrail__hint">Uploads from this browser session only.</p>
      <ul className="kb-filesrail__list">
        {recent.map((r) => (
          <li key={`${r.sha256}:${r.path}`} className="kb-filesrail__row" data-testid="files-recent-row">
            <span className="kb-filesrail__name">{displayName(r.name)}</span>
            <ResultBlock termId={termId} r={r} />
          </li>
        ))}
      </ul>
    </section>
  );
}

export function FilesRail(): ReactElement {
  const { termId } = useTermRail();
  const qc = useQueryClient();
  const [over, setOver] = useState(false);
  const hasFiles = (e: DragEvent): boolean => Array.from(e.dataTransfer?.types ?? []).includes("Files");
  const onDragOver = useCallback((e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    setOver(true);
  }, []);
  const onDrop = (e: DragEvent): void => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    setOver(false);
    const files = Array.from(e.dataTransfer?.files ?? []);
    if (files.length > 0) enqueueFiles(termId, files, readIndex(e.currentTarget), qc);
  };
  return (
    <div
      className="kb-filesrail"
      data-testid="files-rail"
      data-dragover={over ? "1" : "0"}
      onDragOver={onDragOver}
      onDragLeave={() => setOver(false)}
      onDrop={onDrop}
    >
      <UploadSection termId={termId} />
      <BrowseSection termId={termId} />
      <RecentSection termId={termId} />
    </div>
  );
}

/** The "Index content in kb" tickbox is read from the DOM at drop time (one source of truth). */
function readIndex(root: Element): boolean {
  return root.querySelector<HTMLInputElement>('[data-testid="files-index"]')?.checked ?? false;
}
