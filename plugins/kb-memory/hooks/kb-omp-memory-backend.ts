// kb-omp-memory-backend.ts — kb as a first-class omp MemoryBackend, via the
// PROPOSED `pi.registerMemoryBackend(id, factory)` extension API (branch
// feat/register-memory-backend on a local oh-my-pi worktree,
// packages/coding-agent/src/extensibility/extensions/types.ts:1563; not yet
// in any installed omp release — the installed omp on this box is 18.0.8,
// which has no such method).
//
// WHY THIS FILE EXISTS: `kb-omp.ts` (the sibling in this directory) already
// gives omp kb memory through the EXTENSION lane — tools (kb_recall/
// kb_remember/…), a hidden per-prompt injection, and beats. That lane works
// on every omp release. This file is the OTHER integration point: if/when
// `registerMemoryBackend` lands (on the branch above, or upstream once
// accepted), an operator can instead set `memory.backend: "kb"` in omp
// settings and have kb answer the agent's own native `recall`/`retain`
// tools, `/memory` slash commands, and system-prompt append — the same
// surface `local`/`hindsight`/`mnemopi` occupy. Until that API ships, this
// file is a byte-silent no-op: see the feature-detect in the default export
// below. No settings, no CLAUDE.md, no installer step depends on this
// activating — it is pure upside on a host that has the API, pure inert on
// one that doesn't.
//
// DOUBLE-COVERAGE NOTE: kb-omp.ts's wake/recall injection lane and this
// backend are NOT mutually exclusive at the omp level — nothing here
// disables kb-omp.ts, and nothing in kb-omp.ts checks `memory.backend`. If
// an operator both keeps kb-omp.ts installed (the default) AND sets
// `memory.backend: "kb"` once this API exists, kb memory arrives through
// BOTH lanes on every turn: kb-omp.ts's hidden `before_agent_start`
// injection AND this backend's `buildDeveloperInstructions`/recall tool.
// That is redundant, not wrong (both read the same corpus), but doubles the
// token cost of memory context for no benefit. Prefer ONE lane: the
// extension lane (kb-omp.ts) is today's default and needs nothing further;
// switching to `memory.backend: "kb"` is an explicit opt-in an operator
// should pair with turning off kb-omp.ts's injection (or accept the
// duplication knowingly).
//
// HOOKS-NEVER-BLOCK: every method below is fail-open and bounded. `start()`
// is a synchronous no-op (never throws, per the interface contract — a
// throwing factory/start degrades the WHOLE session's memory to `off`, not
// just this backend, per the worktree's registry.ts:29-30/156-168).
// `search()`/`save()`/`status()` shell out to the INSTALLED `kb` CLI with a
// hard timeout and return an honest empty/failure shape on any error —
// never a thrown exception, never a hang. `clear()`/`enqueue()` are true
// no-ops (see their own comments below for why that is the correct, not
// merely cheapest, behaviour for kb).
//
// Install (idempotent): plugins/kb-memory/hooks/install-omp-hooks.sh
// symlinks this file into ~/.omp/agent/extensions/kb-memory-backend.ts
// alongside kb-omp.ts's own kb-memory.ts symlink — omp's native
// auto-discovery loads every *.ts in that directory, so both extensions run
// side by side regardless of which lane ends up in use.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/** Prefer the operator's ~/.local/bin binary, else let PATH resolve it. */
function whichBin(name: string): string {
  try {
    const local = join(homedir(), ".local", "bin", name);
    if (existsSync(local)) return local;
  } catch {}
  return name;
}

const KB = whichBin("kb");

// ---------------------------------------------------------------------------
// Structural contract types — a plain COPY of the shapes in the worktree
// /tmp/omp-mb-worktree's packages/coding-agent/src/memory-backend/types.ts
// (branch feat/register-memory-backend), not an import: this file has no
// build-time dependency on omp's own package — exactly like kb-omp.ts
// (sibling) never imports omp's ExtensionAPI type either. Both speak to omp
// structurally, so this file loads unmodified whether or not the host omp
// build ships the memory-backend module at all. Cited line numbers are
// against the worktree checkout at the time this was written; re-verify
// against types.ts if the branch's shapes move before it lands upstream.
// ---------------------------------------------------------------------------

/** types.ts:29-45 */
interface MemoryBackendStatus {
  backend: string;
  active: boolean;
  writable: boolean;
  searchable: boolean;
  scope?: string;
  retainBank?: string;
  recallBanks?: string[];
  workingCount?: number;
  episodicCount?: number;
  tripleCount?: number;
  lastMemory?: string;
  lastRecall?: boolean;
  database?: string;
  message?: string;
  error?: string;
}

/** types.ts:47-51 */
interface MemoryBackendSearchOptions {
  limit?: number;
  /** Best-effort — observed only before/after the recall child process, never wired into it (types.ts:49). */
  signal?: AbortSignal;
}

/** types.ts:53-59 */
interface MemoryBackendSearchItem {
  id?: string;
  content: string;
  source?: string;
  timestamp?: string;
  score?: number;
}

/** types.ts:61-67 */
interface MemoryBackendSearchResult {
  backend: string;
  query: string;
  count: number;
  items: MemoryBackendSearchItem[];
  message?: string;
}

/** types.ts:69-74 */
interface MemoryBackendSaveInput {
  content: string;
  context?: string;
  source?: string;
  importance?: number;
}

/** types.ts:76-82 */
interface MemoryBackendSaveResult {
  backend: string;
  stored: number;
  ids?: string[];
  queued?: boolean;
  message?: string;
}

/** types.ts:84-88 */
interface MemoryBackendOperationContext {
  agentDir: string;
  cwd: string;
  session?: unknown;
}

/** types.ts:106-177 — only the members this backend implements are typed strictly; the rest ride `unknown` since we never read them. */
interface MemoryBackend {
  readonly id: string;
  start(options: unknown): void | Promise<void>;
  buildDeveloperInstructions(agentDir: string, settings: unknown, session?: unknown): Promise<string | undefined>;
  clear(agentDir: string, cwd: string, session?: unknown): Promise<void>;
  enqueue(agentDir: string, cwd: string, session?: unknown): Promise<void>;
  status?(context: MemoryBackendOperationContext): Promise<MemoryBackendStatus>;
  search?(
    context: MemoryBackendOperationContext,
    query: string,
    options?: MemoryBackendSearchOptions,
  ): Promise<MemoryBackendSearchResult>;
  save?(context: MemoryBackendOperationContext, input: MemoryBackendSaveInput): Promise<MemoryBackendSaveResult>;
}

/** registry.ts:65-72 — what the factory is handed at (lazy, cheap) construction time. */
interface MemoryBackendFactoryInit {
  settings: unknown;
  cwd: string;
}

// ===================================================================== run()
// Structural copy of kb-omp.ts's run()/timeout/fail-open child-process
// pattern (same file, HEAD) — not imported, so this file has no dependency
// on its sibling either. Trimmed to what this backend actually needs: no
// stdin-JSON input (every call here is a plain argv invocation) and no
// grandchild-pipe workaround (kb-omp.ts's version guards against shell
// hooks that spawn curl as a grandchild inheriting stdout; `kb` itself
// speaks HTTP in-process via reqwest, so SIGTERM-ing it directly is enough).
type RunResult = { code: number; stdout: string; stderr: string };

function run(cmd: string, args: string[], timeoutMs: number): Promise<RunResult> {
  const { promise, resolve } = Promise.withResolvers<RunResult>();
  let child: ReturnType<typeof spawn>;
  try {
    // argv array, no shell — arguments are never re-parsed by a shell.
    child = spawn(cmd, args, { stdio: ["ignore", "pipe", "pipe"] });
  } catch {
    resolve({ code: 1, stdout: "", stderr: "" });
    return promise;
  }
  let stdout = "";
  let stderr = "";
  let done = false;
  const finish = (code: number) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    resolve({ code, stdout, stderr });
  };
  const timer = setTimeout(() => {
    try {
      child.kill("SIGTERM");
    } catch {}
    finish(124);
    try {
      child.stdout?.destroy();
      child.stderr?.destroy();
      child.unref();
    } catch {}
  }, timeoutMs);
  child.stdout?.on("data", (d) => {
    if (stdout.length < 262_144) stdout += String(d);
  });
  child.stderr?.on("data", (d) => {
    if (stderr.length < 8_192) stderr += String(d);
  });
  child.on("close", (code) => finish(code ?? 1));
  child.on("error", () => finish(1));
  return promise;
}

// ==================================================================== caps
/** Max recall hits mapped into search results (kb's own recall default is 5; this only bounds an explicit large `--limit`). */
const SEARCH_ITEM_CAP = 25;
/** Per-item `content` cap — a search result line, not a tool-result page (kb-omp.ts's TOOL_RESULT_CAP is 30 000 for the latter). */
const SEARCH_ITEM_CONTENT_CAP = 600;
/** `--summary` cap — mirrors kb_remember's own UI convention of a short one-line gloss. */
const SUMMARY_CAP = 100;

function capString(s: string, cap: number): string {
  return s.length <= cap ? s : `${s.slice(0, cap - 1)}…`;
}

// ============================================================ dev instructions
// Static, ~10 lines, no shell-out: buildDeveloperInstructions() is called on
// every system-prompt rebuild and must be fast (per the interface contract,
// types.ts:118-121).
const KB_DEVELOPER_INSTRUCTIONS = [
  "## kb memory",
  "- Durable facts (decisions, gotchas, hard-won knowledge) persist via `kb remember`.",
  "  ALWAYS pass a one-line `--summary` distinct from the memory text — it is",
  "  what every recall hit shows first.",
  "- Recall (`kb recall <query>`) is ranked and DETERMINISTIC — no LLM in the",
  "  ranking loop — combining relevance, salience, and time-decay.",
  "- Episodic history is separate from curated facts: `kb why <file>` explains",
  "  why a file is the way it is; `kb recollect <query>` searches past sessions.",
  "- Recall before you remember: check for an existing memory before writing",
  "  a near-duplicate.",
].join("\n");

// ======================================================================= kb
function makeKbBackend(): MemoryBackend {
  return {
    id: "kb",

    start() {
      // Intentionally does no work. kb talks to an already-running daemon
      // purely through the CLI; there is no per-session connection or
      // subscription to wire up here, and the interface requires this to be
      // fast and non-throwing (types.ts:112-115) — a synchronous no-op
      // trivially satisfies both.
    },

    async buildDeveloperInstructions() {
      return KB_DEVELOPER_INSTRUCTIONS;
    },

    async clear() {
      // A true no-op, not merely the cheapest option. kb's memory corpus is
      // a durable store SHARED across every harness and every session (the
      // whole point of kb-as-memory) — this backend does not privately own
      // any of it. Wiping it from a per-session `/memory clear` would be
      // both out of scope (kb has no per-backend-instance state to drop)
      // and needlessly destructive (kb's own deletion path, `kb forget`, is
      // deliberately explicit and per-memory). Mirrors the builtin
      // off-backend's true no-op `clear()` (worktree off-backend.ts:14).
    },

    async enqueue() {
      // No local consolidation queue to flush: `kb remember` writes
      // synchronously (a POST that either lands or fails), unlike
      // hindsight's batched retain queue. Nothing to force early.
    },

    async status(): Promise<MemoryBackendStatus> {
      const r = await run(KB, ["--version"], 5_000).catch(() => null);
      if (!r || r.code !== 0) {
        return {
          backend: "kb",
          active: false,
          writable: false,
          searchable: false,
          error: r ? `kb --version exited ${r.code}` : "kb: could not be started (is it on PATH?).",
        };
      }
      return {
        backend: "kb",
        active: true,
        writable: true,
        searchable: true,
        message: capString(r.stdout.trim() || "kb", 200),
      };
    },

    async search(context, query, options): Promise<MemoryBackendSearchResult> {
      if (options?.signal?.aborted) return { backend: "kb", query, count: 0, items: [] };

      const args = ["recall", query, "--json"];
      const limit = options?.limit;
      if (typeof limit === "number" && Number.isFinite(limit) && limit > 0) {
        args.push("--limit", String(Math.min(SEARCH_ITEM_CAP, Math.max(1, Math.trunc(limit)))));
      }
      // Project-aware recall (mirrors kb-omp.ts's kb_recall/kb_context
      // tools): --cwd lets the CLI derive its "auto" default scope
      // (global corpora + this repo's own memory-<slug> corpus) instead
      // of guessing from the daemon's own process cwd.
      if (context?.cwd) args.push("--cwd", context.cwd);

      const r = await run(KB, args, 20_000).catch(() => null);
      if (options?.signal?.aborted) return { backend: "kb", query, count: 0, items: [] };
      if (!r || r.code !== 0) {
        return {
          backend: "kb",
          query,
          count: 0,
          items: [],
          message: r
            ? `kb recall exited ${r.code}${r.stderr ? `: ${r.stderr.trim().slice(0, 200)}` : ""}.`
            : "kb: could not be started (is it on PATH?).",
        };
      }

      let parsed: unknown;
      try {
        parsed = JSON.parse(r.stdout);
      } catch {
        return { backend: "kb", query, count: 0, items: [], message: "kb recall: unparseable JSON output." };
      }
      const hits = Array.isArray((parsed as { hits?: unknown })?.hits) ? (parsed as { hits: unknown[] }).hits : [];

      const items: MemoryBackendSearchItem[] = hits.slice(0, SEARCH_ITEM_CAP).map((raw) => {
        const h = raw as Record<string, unknown>;
        const title = typeof h.title === "string" ? h.title : "";
        const summary = typeof h.summary === "string" ? h.summary : "";
        // kb's recall hit carries title + a curated one-line summary but NOT
        // the memory's full body (crates/kb-core/src/memory.rs:353-420,
        // RecallHit) — combining the two is the honest maximum we can give
        // without a second fetch per hit.
        const content = summary && summary !== title ? `${title} — ${summary}` : title || summary || "(untitled memory)";
        const item: MemoryBackendSearchItem = { content: capString(content, SEARCH_ITEM_CONTENT_CAP) };
        if (typeof h.id === "string") item.id = h.id;
        if (typeof h.kb === "string") item.source = h.kb;
        if (typeof h.mtime_unix === "number") item.timestamp = new Date(h.mtime_unix * 1000).toISOString();
        if (typeof h.score === "number") item.score = h.score;
        return item;
      });

      return { backend: "kb", query, count: items.length, items };
    },

    async save(_context, input): Promise<MemoryBackendSaveResult> {
      const content = String(input?.content ?? "").trim();
      if (!content) return { backend: "kb", stored: 0, message: "kb: empty content, nothing saved." };

      // `input.context` ("source context", tools/memory-retain.ts) is the
      // caller's own gloss when it has one; kb_remember requires a summary
      // distinct from the body, so prefer that over re-deriving one from
      // the content itself.
      const summarySource = (input.context && input.context.trim()) || content;
      const args = ["remember", content, "--summary", capString(summarySource, SUMMARY_CAP), "--json"];

      // `importance` (0..1) maps cleanly onto kb's own --salience. NOTE:
      // `input.source` is deliberately NOT forwarded to kb's `--source` —
      // that flag is a CLOSED enum (fetched-web|user-dictated|
      // agent-inference, MI-W3.4) validated locally before any network
      // call, while omp's `source` is free text (e.g. "coding-agent-
      // retain"); forwarding it would fail nearly every save.
      if (typeof input.importance === "number" && Number.isFinite(input.importance)) {
        args.push("--salience", String(Math.min(1, Math.max(0, input.importance))));
      }

      const r = await run(KB, args, 30_000).catch(() => null);
      if (!r || r.code !== 0) {
        return {
          backend: "kb",
          stored: 0,
          message: r
            ? `kb remember exited ${r.code}${r.stderr ? `: ${r.stderr.trim().slice(0, 200)}` : ""}.`
            : "kb: could not be started (is it on PATH?).",
        };
      }

      let id: string | undefined;
      try {
        const parsed = JSON.parse(r.stdout) as { id?: unknown };
        if (typeof parsed.id === "string") id = parsed.id;
      } catch {}
      return { backend: "kb", stored: 1, ids: id ? [id] : undefined };
    },
  };
}

// =================================================================== install
export default function kbMemoryBackendExtension(pi: {
  registerMemoryBackend?: (id: string, factory: (init: MemoryBackendFactoryInit) => MemoryBackend) => void;
}): void {
  // Feature-detect and get out silently: no log, no error, no side effect at
  // all on any omp build that lacks this API (every release up to and
  // including the installed 18.0.8). This is what makes the file safe to
  // symlink into every omp install unconditionally.
  if (typeof pi.registerMemoryBackend !== "function") return;

  // The factory itself must be cheap (registry.ts:27-30): it does no I/O and
  // opens nothing — `makeKbBackend()` only builds closures. Any real
  // start-up work belongs in `start()` above, which is itself a no-op here.
  pi.registerMemoryBackend("kb", () => makeKbBackend());
}
