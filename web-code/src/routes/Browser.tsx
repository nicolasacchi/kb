import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  fetchFile,
  fetchHierarchyCallees,
  fetchHierarchyCallers,
  fetchHierarchyTypes,
  fetchSymbolsSearch,
} from "../api/client";
import type { Symbol, SymbolMatch } from "../api/types";
import EmptyState from "../components/EmptyState";
import { ClassBadge } from "../components/hierarchy/HierarchyPanel";
import { Icon } from "../components/icons";
import { readerUrl } from "../lib/breadcrumbs";
import {
  browserUrl,
  parseBrowserSearch,
  type BrowserSymbolRef,
} from "../lib/browserUrl";
import { isTypeIshKind } from "../lib/hierarchyState";
import { speedFilterItems } from "../lib/speedSearch";
import { resolve as resolveCommand, tokenOf } from "../commands/dispatch";
import { useCommandScope } from "../commands/CommandRoot";
import type { BrowserHandlers } from "./browserCommands";
import "../styles/browser.css";

/** Kinds shown in the containers/types pane (classes, traits, free functions). */
function isContainerKind(kind: string): boolean {
  if (isTypeIshKind(kind)) return true;
  const k = kind.toLowerCase();
  // Callable kinds mirror the SERVER's vocabulary (extract.rs map_kind):
  // Rust "fn", Python "def", Go "func", JS/TS/Bash "function" — a bare
  // "function" check silently hides every Rust/Python/Go free function.
  return (
    k === "fn"
    || k === "def"
    || k === "func"
    || k === "function"
    || k === "const"
    || k === "static"
    || k === "macro"
  );
}

function sliceSymbolSource(content: string, lineStart: number, lineEnd: number): string {
  const lines = content.split("\n");
  const from = Math.max(0, lineStart - 1);
  const to = Math.min(lines.length, Math.max(from + 1, lineEnd));
  return lines.slice(from, to).join("\n");
}

function matchKey(m: { path: string; name: string; line_start: number }): string {
  return `${m.path}\0${m.name}\0${m.line_start}`;
}

function refMatches(
  ref: BrowserSymbolRef | null,
  m: { path: string; name: string; line_start?: number },
): boolean {
  if (!ref) return false;
  if (ref.path !== m.path || ref.name !== m.name) return false;
  if (ref.line != null && m.line_start != null && ref.line !== m.line_start) return false;
  return true;
}

type FocusPane = "containers" | "members" | "source" | "calls";

/**
 * V3.4-C3 — `/r/:repo/~browser`: Smalltalk-lens symbol-first browser.
 * Four linked panes over existing APIs (symbols, file, hierarchy).
 * Files stay primary: every row links out to the reader.
 */
export default function BrowserPage() {
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const { symbol: urlSymbol } = useMemo(
    () => parseBrowserSearch(searchParams),
    [searchParams],
  );

  const [filter, setFilter] = useState("");
  const [focusPane, setFocusPane] = useState<FocusPane>("containers");
  const [callTab, setCallTab] = useState<"callers" | "callees">("callers");
  const rootRef = useRef<HTMLDivElement>(null);

  // Repo-wide symbol index (substring "" matches all; server caps at 200).
  const symbolsQ = useQuery({
    queryKey: ["browser", "symbols", repo] as const,
    queryFn: () => fetchSymbolsSearch(repo, ""),
    enabled: !!repo,
    staleTime: 60_000,
  });

  const containers = useMemo(() => {
    const matches = symbolsQ.data?.matches ?? [];
    // Prefer type-ish; also keep top-level functions (no container).
    const list = matches.filter((m) => {
      if (isTypeIshKind(m.kind)) return true;
      if (isContainerKind(m.kind) && !m.container) return true;
      return false;
    });
    // Dedupe by path+name (first line wins).
    const seen = new Set<string>();
    const out: SymbolMatch[] = [];
    for (const m of list) {
      const k = `${m.path}\0${m.name}`;
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(m);
    }
    out.sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path));
    if (filter.trim() === "") return out;
    return speedFilterItems(out, filter, (m) => `${m.name} ${m.path} ${m.kind}`).map(
      (h) => h.item,
    );
  }, [symbolsQ.data, filter]);

  // Active container: URL symbol if it is a container, else its .container parent.
  const selectedContainer = useMemo((): BrowserSymbolRef | null => {
    if (!urlSymbol) return null;
    // If URL carries container name, the symbol is a member — resolve parent.
    if (urlSymbol.container) {
      const parent = containers.find(
        (c) => c.path === urlSymbol.path && c.name === urlSymbol.container,
      );
      if (parent) {
        return {
          path: parent.path,
          name: parent.name,
          line: parent.line_start,
        };
      }
      return {
        path: urlSymbol.path,
        name: urlSymbol.container,
      };
    }
    // URL points at a container/type itself.
    const hit = containers.find((c) => refMatches(urlSymbol, c));
    if (hit) {
      return { path: hit.path, name: hit.name, line: hit.line_start };
    }
    // Free function / member without parent in list — treat as container.
    return { path: urlSymbol.path, name: urlSymbol.name, line: urlSymbol.line };
  }, [urlSymbol, containers]);

  // Selected member = URL symbol when it has a container or is a non-type in the members list.
  const selectedMember = useMemo((): BrowserSymbolRef | null => {
    if (!urlSymbol) return null;
    if (urlSymbol.container) return urlSymbol;
    // If URL is a type-ish container, no member yet.
    const asContainer = containers.find((c) => refMatches(urlSymbol, c));
    if (asContainer && isTypeIshKind(asContainer.kind)) return null;
    // Free function selected as both container+member.
    return urlSymbol;
  }, [urlSymbol, containers]);

  const setSymbol = useCallback(
    (ref: BrowserSymbolRef | null) => {
      navigate(browserUrl(repo, ref), { replace: false });
    },
    [navigate, repo],
  );

  // File for members + source (container path or member path).
  const filePath = selectedMember?.path ?? selectedContainer?.path ?? null;
  const fileQ = useQuery({
    queryKey: ["file", repo, filePath, null] as const,
    queryFn: () => fetchFile(repo, filePath as string),
    enabled: !!repo && !!filePath,
    staleTime: 60_000,
  });

  const members = useMemo((): Symbol[] => {
    const syms = fileQ.data?.symbols ?? [];
    if (!selectedContainer) return [];
    const cName = selectedContainer.name;
    const cLine = selectedContainer.line;
    // Type container: children by container field or nested by line range.
    const containerSym = syms.find(
      (s) =>
        s.name === cName &&
        (cLine == null || s.line_start === cLine) &&
        isTypeIshKind(s.kind),
    );
    if (containerSym) {
      return syms
        .filter(
          (s) =>
            s !== containerSym &&
            (s.container === cName ||
              (s.line_start >= containerSym.line_start &&
                s.line_end <= containerSym.line_end &&
                s.name !== cName)),
        )
        .sort((a, b) => a.line_start - b.line_start);
    }
    // Free function / non-type: members pane lists the symbol itself.
    const self = syms.find(
      (s) =>
        s.name === cName && (cLine == null || s.line_start === cLine),
    );
    return self ? [self] : [];
  }, [fileQ.data, selectedContainer]);

  const memberSym: Symbol | null = useMemo(() => {
    if (!selectedMember) return null;
    const fromMembers = members.find(
      (s) =>
        s.name === selectedMember.name &&
        (selectedMember.line == null || s.line_start === selectedMember.line),
    );
    if (fromMembers) return fromMembers;
    const all = fileQ.data?.symbols ?? [];
    return (
      all.find(
        (s) =>
          s.name === selectedMember.name &&
          (selectedMember.line == null || s.line_start === selectedMember.line),
      ) ?? null
    );
  }, [selectedMember, members, fileQ.data]);

  const sourceText = useMemo(() => {
    if (!fileQ.data || !memberSym) return null;
    const content =
      fileQ.data.encoding === "base64"
        ? null
        : fileQ.data.content;
    if (content == null) return null;
    return sliceSymbolSource(content, memberSym.line_start, memberSym.line_end);
  }, [fileQ.data, memberSym]);

  // Hierarchy types for selected container (supertypes/subtypes).
  const typesQ = useQuery({
    queryKey: ["browser", "types", repo, selectedContainer?.name, selectedContainer?.path],
    queryFn: () =>
      fetchHierarchyTypes(
        repo,
        selectedContainer!.name,
        selectedContainer!.path,
      ),
    enabled:
      !!repo &&
      !!selectedContainer &&
      isTypeIshKind(
        containers.find((c) => refMatches(selectedContainer, c))?.kind ?? "class",
      ),
    staleTime: 120_000,
    retry: false,
  });

  // Callers / callees for selected member.
  const hierPos = memberSym
    ? {
        repo,
        path: selectedMember!.path,
        line: memberSym.line_start,
        col: memberSym.col_start ?? 0,
      }
    : null;

  const callersQ = useQuery({
    queryKey: ["browser", "callers", hierPos?.path, hierPos?.line],
    queryFn: () => fetchHierarchyCallers(hierPos!),
    enabled: !!hierPos,
    staleTime: 120_000,
    retry: false,
  });
  const calleesQ = useQuery({
    queryKey: ["browser", "callees", hierPos?.path, hierPos?.line],
    queryFn: () => fetchHierarchyCallees(hierPos!),
    enabled: !!hierPos,
    staleTime: 120_000,
    retry: false,
  });

  // Keyboard (V73-K6: kbc-cmd/1 handlers, scope `board`, `board == browser`)
  // — j/k within focused pane, Enter drills, h/l pane focus. Previously five
  // hard-coded `e.key === …` checks with no registry involvement at all — a
  // SECOND home for these keys (`board.pane-prev`/`board.pane-next`/
  // `board.row-next`/`board.row-prev`/`board.drill` shipped in
  // `registry.json` since before this unit, always with this real behaviour,
  // but nothing here ever asked what a keystroke MEANT). `onKey` below now
  // asks `dispatch.ts`'s `resolve()` — the same resolver `CommandRoot`
  // uses — and runs the returned id's handler from `handlers`; see
  // `browserCommands.ts` for the declaration↔handler contract and
  // `browserCommands.test.ts` for the bidirectional walk.
  useCommandScope("board", { board: "browser" });

  function panePrev() {
    setFocusPane((p) =>
      p === "calls"
        ? "source"
        : p === "source"
          ? "members"
          : p === "members"
            ? "containers"
            : "containers",
    );
  }
  function paneNext() {
    setFocusPane((p) =>
      p === "containers"
        ? "members"
        : p === "members"
          ? "source"
          : p === "source"
            ? "calls"
            : "calls",
    );
  }
  /// Shared by `board.row-next`/`board.row-prev` — `delta` is `+1`/`-1`.
  function moveRow(delta: number) {
    if (focusPane === "containers" && containers.length > 0) {
      const idx = Math.max(0, containers.findIndex((c) => refMatches(selectedContainer, c)));
      const next = Math.max(0, Math.min(containers.length - 1, idx + delta));
      const c = containers[next];
      if (c) setSymbol({ path: c.path, name: c.name, line: c.line_start });
      return;
    }
    if (focusPane === "members" && members.length > 0 && selectedContainer) {
      const idx = Math.max(
        0,
        members.findIndex(
          (m) =>
            selectedMember &&
            m.name === selectedMember.name &&
            (selectedMember.line == null || m.line_start === selectedMember.line),
        ),
      );
      const base = idx < 0 ? 0 : idx;
      const next = Math.max(0, Math.min(members.length - 1, base + delta));
      const m = members[next];
      if (m) {
        const contKind = containers.find((c) => refMatches(selectedContainer, c))?.kind ?? "";
        setSymbol({
          path: selectedContainer.path,
          name: m.name,
          line: m.line_start,
          container: isTypeIshKind(contKind) ? selectedContainer.name : undefined,
        });
      }
    }
  }
  function drill() {
    if (focusPane === "containers" && containers.length > 0) {
      const idx = Math.max(0, containers.findIndex((c) => refMatches(selectedContainer, c)));
      const c = containers[idx] ?? containers[0];
      if (c) {
        setSymbol({ path: c.path, name: c.name, line: c.line_start });
        setFocusPane("members");
      }
      return;
    }
    if (focusPane === "members" && members.length > 0 && selectedContainer) {
      const idx = Math.max(
        0,
        members.findIndex(
          (m) =>
            selectedMember &&
            m.name === selectedMember.name &&
            (selectedMember.line == null || m.line_start === selectedMember.line),
        ),
      );
      const m = members[idx >= 0 ? idx : 0];
      if (m) {
        setSymbol({
          path: selectedContainer.path,
          name: m.name,
          line: m.line_start,
          container: isTypeIshKind(
            containers.find((c) => refMatches(selectedContainer, c))?.kind ?? "",
          )
            ? selectedContainer.name
            : undefined,
        });
        setFocusPane("source");
      }
    }
  }

  // Every id `browserCommands.ts` declares, or this does not compile.
  const handlers: BrowserHandlers = {
    "board.pane-prev": panePrev,
    "board.pane-next": paneNext,
    "board.row-next": () => moveRow(1),
    "board.row-prev": () => moveRow(-1),
    "board.drill": drill,
  };

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) {
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const token = tokenOf(e);
      const ctx = { board: "browser" as const };
      // Pre-registry this page accepted BOTH the vim letter and the plain
      // arrow for every row (`e.key === "j" || e.key === "ArrowDown"`, one
      // literal check, no preset in sight) — `board.pane-prev`/
      // `board.pane-next`/`board.row-next`/`board.row-prev` all carry a
      // DIFFERENT `plain` key than their `vim`/`helix` one (`h`/`j` vs.
      // `ArrowLeft`/`ArrowDown`), so resolving under the live preset alone
      // would silently drop whichever column isn't selected — a real
      // regression from what shipped. Trying `vim` then `plain` keeps both
      // working regardless of the SPA's preset setting, matching the
      // original behaviour exactly (and, for pane switching, finally makes
      // the registry's own `plain` column — declared since before this
      // unit, never wired — actually do something).
      const cmd = resolveCommand(token, "board", ctx, "vim") ?? resolveCommand(token, "board", ctx, "plain");
      if (!cmd) return;
      const handler = (handlers as Record<string, (() => void) | undefined>)[cmd.id];
      if (!handler) return;
      e.preventDefault();
      handler();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    focusPane,
    containers,
    members,
    selectedContainer,
    selectedMember,
    setSymbol,
  ]);

  const langUnsupported =
    fileQ.data &&
    fileQ.data.lang == null &&
    (fileQ.data.symbols?.length ?? 0) === 0;

  return (
    <div className="kbc-browser" id="main" data-kbc-browser ref={rootRef}>
      <header className="kbc-browser__head">
        <div>
          <h1 className="kbc-browser__title">Browser — {repo}</h1>
          <p className="kbc-browser__hint" data-kbc-browser-hint>
            Symbol-first lens over containers, members, source, and callers.
            Files stay primary — open the reader from any row.{" "}
            <kbd>j</kbd>/<kbd>k</kbd> move · <kbd>h</kbd>/<kbd>l</kbd> panes ·{" "}
            <kbd>Enter</kbd> drills.
          </p>
        </div>
        <input
          className="kbc-browser__filter"
          type="search"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter containers…"
          aria-label="filter containers"
          data-kbc-browser-filter
        />
      </header>

      <div className="kbc-browser__panes">
        {/* Pane 1 — containers / types */}
        <section
          className={
            "kbc-browser__pane" +
            (focusPane === "containers" ? " is-focused" : "")
          }
          data-kbc-browser-pane="containers"
          onMouseDown={() => setFocusPane("containers")}
        >
          <h2 className="kbc-browser__pane-title">Containers / types</h2>
          {symbolsQ.isLoading && (
            <div className="kbc-browser__muted">Loading symbols…</div>
          )}
          {symbolsQ.error && (
            <div className="kbc-browser__error">
              {(symbolsQ.error as Error).message}
            </div>
          )}
          {!symbolsQ.isLoading && !symbolsQ.error && containers.length === 0 && (
            <EmptyState
              variant="rail"
              icon={<Icon.List />}
              title="No containers"
              hint={
                filter
                  ? "Nothing matches the filter."
                  : "No type or top-level function symbols indexed (unsupported languages show no outline)."
              }
            />
          )}
          <ul className="kbc-browser__list" role="listbox" aria-label="containers">
            {containers.map((c) => {
              const active = refMatches(selectedContainer, c);
              return (
                <li key={matchKey(c)}>
                  <button
                    type="button"
                    className={
                      "kbc-browser__row" + (active ? " is-active" : "")
                    }
                    data-kbc-browser-container={c.name}
                    data-kbc-browser-container-path={c.path}
                    aria-selected={active}
                    onClick={() => {
                      setSymbol({
                        path: c.path,
                        name: c.name,
                        line: c.line_start,
                      });
                      setFocusPane("members");
                    }}
                  >
                    <span className="kbc-browser__kind">{c.kind}</span>
                    <span className="kbc-browser__name">{c.name}</span>
                    <Link
                      to={readerUrl(repo, c.path, undefined, c.line_start)}
                      className="kbc-browser__open"
                      data-kbc-browser-open-file={c.path}
                      onClick={(e) => e.stopPropagation()}
                      title="Open in reader"
                      aria-label="Open in reader"
                    >
                      <Icon.OpenInReader />
                    </Link>
                    <span className="kbc-browser__path">{c.path}</span>
                  </button>
                </li>
              );
            })}
          </ul>
          {selectedContainer && typesQ.data && (
            <div className="kbc-browser__types" data-kbc-browser-types>
              {typesQ.data.supertypes.length > 0 && (
                <div>
                  <div className="kbc-browser__sublab">Supertypes</div>
                  <ul className="kbc-browser__typelist">
                    {typesQ.data.supertypes.map((e, i) => (
                      <li key={`sup-${e.name}-${i}`}>
                        <span className="kbc-browser__kind">{e.kind}</span>{" "}
                        {e.name} <ClassBadge className={e.class} />
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              {typesQ.data.subtypes.length > 0 && (
                <div>
                  <div className="kbc-browser__sublab">Subtypes</div>
                  <ul className="kbc-browser__typelist">
                    {typesQ.data.subtypes.map((e, i) => (
                      <li key={`sub-${e.name}-${i}`}>
                        <span className="kbc-browser__kind">{e.kind}</span>{" "}
                        {e.name} <ClassBadge className={e.class} />
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          )}
        </section>

        {/* Pane 2 — members */}
        <section
          className={
            "kbc-browser__pane" + (focusPane === "members" ? " is-focused" : "")
          }
          data-kbc-browser-pane="members"
          onMouseDown={() => setFocusPane("members")}
        >
          <h2 className="kbc-browser__pane-title">Members</h2>
          {!selectedContainer && (
            <div className="kbc-browser__muted" data-kbc-browser-members-empty>
              Select a container to list its members.
            </div>
          )}
          {selectedContainer && fileQ.isLoading && (
            <div className="kbc-browser__muted">Loading file…</div>
          )}
          {selectedContainer && langUnsupported && (
            <EmptyState
              variant="rail"
              title="No symbol extraction"
              hint={
                fileQ.data?.lang
                  ? `Language “${fileQ.data.lang}” returned no symbols.`
                  : "Unsupported or unindexed language for this path — open the file in the reader instead."
              }
            />
          )}
          {selectedContainer &&
            !fileQ.isLoading &&
            !langUnsupported &&
            members.length === 0 && (
              <div className="kbc-browser__muted" data-kbc-browser-members-empty>
                No members under {selectedContainer.name}.
              </div>
            )}
          <ul className="kbc-browser__list" role="listbox" aria-label="members">
            {members.map((m) => {
              const active =
                !!selectedMember &&
                selectedMember.name === m.name &&
                (selectedMember.line == null ||
                  selectedMember.line === m.line_start);
              return (
                <li key={`${m.name}:${m.line_start}`}>
                  <button
                    type="button"
                    className={
                      "kbc-browser__row" + (active ? " is-active" : "")
                    }
                    data-kbc-browser-member={m.name}
                    aria-selected={active}
                    onClick={() => {
                      const contKind =
                        containers.find((c) =>
                          refMatches(selectedContainer, c),
                        )?.kind ?? "";
                      setSymbol({
                        path: selectedContainer!.path,
                        name: m.name,
                        line: m.line_start,
                        container: isTypeIshKind(contKind)
                          ? selectedContainer!.name
                          : undefined,
                      });
                      setFocusPane("source");
                    }}
                  >
                    <span className="kbc-browser__kind">{m.kind}</span>
                    <span className="kbc-browser__name">{m.name}</span>
                    <Link
                      to={readerUrl(
                        repo,
                        selectedContainer!.path,
                        undefined,
                        m.line_start,
                      )}
                      className="kbc-browser__open"
                      onClick={(e) => e.stopPropagation()}
                      title="Open in reader"
                      aria-label="Open in reader"
                    >
                      <Icon.OpenInReader />
                    </Link>
                  </button>
                </li>
              );
            })}
          </ul>
        </section>

        {/* Pane 3 — source span */}
        <section
          className={
            "kbc-browser__pane kbc-browser__pane--source" +
            (focusPane === "source" ? " is-focused" : "")
          }
          data-kbc-browser-pane="source"
          onMouseDown={() => setFocusPane("source")}
        >
          <h2 className="kbc-browser__pane-title">
            Source
            {memberSym && (
              <Link
                to={readerUrl(
                  repo,
                  selectedMember!.path,
                  undefined,
                  memberSym.line_start,
                )}
                className="kbc-browser__source-link"
                data-kbc-browser-source-open
              >
                open full file
              </Link>
            )}
          </h2>
          {!memberSym && (
            <div className="kbc-browser__muted" data-kbc-browser-source-empty>
              Select a member to show its source span.
            </div>
          )}
          {memberSym && sourceText == null && fileQ.data?.encoding === "base64" && (
            <div className="kbc-browser__muted">
              Binary file — open in the reader to inspect.
            </div>
          )}
          {memberSym && sourceText != null && (
            <pre className="kbc-browser__source" data-kbc-browser-source>
              <code>{sourceText}</code>
            </pre>
          )}
        </section>

        {/* Pane 4 — callers / callees */}
        <section
          className={
            "kbc-browser__pane" + (focusPane === "calls" ? " is-focused" : "")
          }
          data-kbc-browser-pane="calls"
          onMouseDown={() => setFocusPane("calls")}
        >
          <h2 className="kbc-browser__pane-title">
            <button
              type="button"
              className={
                "kbc-browser__tab" + (callTab === "callers" ? " is-on" : "")
              }
              data-kbc-browser-call-tab="callers"
              onClick={() => setCallTab("callers")}
            >
              Callers
            </button>
            <button
              type="button"
              className={
                "kbc-browser__tab" + (callTab === "callees" ? " is-on" : "")
              }
              data-kbc-browser-call-tab="callees"
              onClick={() => setCallTab("callees")}
            >
              Callees
            </button>
          </h2>
          {!memberSym && (
            <div className="kbc-browser__muted" data-kbc-browser-calls-empty>
              Select a member to list callers and callees.
            </div>
          )}
          {memberSym && callTab === "callers" && (
            <div data-kbc-browser-callers>
              {callersQ.isLoading && (
                <div className="kbc-browser__muted">Loading callers…</div>
              )}
              {callersQ.error && (
                <div className="kbc-browser__error">
                  Hierarchy callers unavailable for this language or position.
                </div>
              )}
              {callersQ.data && callersQ.data.callers.length === 0 && (
                <div className="kbc-browser__muted">No callers found.</div>
              )}
              {callersQ.data?.truncated && (
                <div className="kbc-browser__trunc" role="status">
                  Callers truncated by server cap.
                </div>
              )}
              <ul className="kbc-browser__list">
                {(callersQ.data?.callers ?? []).map((g, i) => {
                  const cls = g.sites[0]?.class ?? "candidate";
                  const name = g.enclosing?.name ?? "(site)";
                  const line = g.enclosing?.line ?? g.sites[0]?.line;
                  return (
                    <li key={`${g.path}:${name}:${i}`}>
                      <div
                        className="kbc-browser__row kbc-browser__row--static"
                        data-kbc-browser-caller={name}
                      >
                        <span className="kbc-browser__kind">
                          {g.enclosing?.kind ?? "call"}
                        </span>
                        <span className="kbc-browser__name">{name}</span>
                        <ClassBadge className={cls} />
                        <Link
                          to={readerUrl(repo, g.path, undefined, line)}
                          className="kbc-browser__open"
                          title={g.path}
                          aria-label="Open in reader"
                        >
                          <Icon.OpenInReader />
                        </Link>
                        <span className="kbc-browser__path">{g.path}</span>
                      </div>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
          {memberSym && callTab === "callees" && (
            <div data-kbc-browser-callees>
              {calleesQ.isLoading && (
                <div className="kbc-browser__muted">Loading callees…</div>
              )}
              {calleesQ.error && (
                <div className="kbc-browser__error">
                  Hierarchy callees unavailable for this language or position.
                </div>
              )}
              {calleesQ.data && calleesQ.data.callees.length === 0 && (
                <div className="kbc-browser__muted">No callees found.</div>
              )}
              <ul className="kbc-browser__list">
                {(calleesQ.data?.callees ?? []).map((c, i) => (
                  <li key={`${c.name}:${c.line}:${i}`}>
                    <div
                      className="kbc-browser__row kbc-browser__row--static"
                      data-kbc-browser-callee={c.name}
                    >
                      <span className="kbc-browser__name">{c.name}</span>
                      <ClassBadge className={c.class} />
                      {c.target && (
                        <Link
                          to={readerUrl(
                            repo,
                            c.target.path,
                            undefined,
                            c.target.line,
                          )}
                          className="kbc-browser__open"
                          aria-label="Open in reader"
                        >
                          <Icon.OpenInReader />
                        </Link>
                      )}
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
