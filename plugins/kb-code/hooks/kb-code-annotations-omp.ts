// kb-code-annotations-omp.ts — kb-code operator-flag surfacing for Oh My
// Pi (omp). V71-X1, recon `cli-agent-surface.md` open question 9: "should
// plugins/kb-code ship a Codex/omp adapter for the annotations hook, so
// the human->agent flag dialogue isn't Claude-Code-only?"
//
// Mirrors kb-code-omp.ts (kb-code's existing omp adapter, for
// kb-code-why.sh) field for field — same `tool_result` seam (omp's
// `tool_call` can only block/revise input, never inject content; see that
// file's header for the full ToolCallEventResult/ToolResultEventResult
// citation), same `edit`/`write` targeting, same synthetic
// Claude-Code-shaped stdin payload, same fail-open/never-throw posture.
// The only thing that differs is WHICH shell script owns the lookup:
// kb-code-annotations.sh (D4's open-flags surface) instead of
// kb-code-why.sh (provenance) — this file is a thin adapter over that
// script, never a second implementation of its repos-cache / per-session
// cap / per-file cap / dedupe logic.
//
// SESSION-START HAS NO ADAPTER HERE, on purpose (same posture as the
// Codex sibling, kb-code-annotations-codex.sh): kb-code-annotations.sh's
// `--session-start` mode is a ONE-SHOT-per-session landing summary, and
// this repo's own kb-memory omp integration (kb-omp.ts) shows that its
// literal analogue, omp's `session_start` event, does NOT inject content
// — kb-memory defers its own session-start work (kb-wake.sh) to the
// FIRST `before_agent_start` instead, gated by a `waked` per-session set
// it privately owns. Reimplementing that same defer-and-gate dance in a
// SEPARATELY installed extension file, racing kb-memory's own bookkeeping
// and omp's per-handler timeout budget, is exactly the kind of
// unverifiable-without-a-live-omp-session guess this unit's evidence bar
// refuses to make (see the module doc's sibling files for the same
// refusal). The per-file PreToolUse injection below still discloses every
// open flag eventually — the first Edit/Write of a flagged file surfaces
// it — so omp users lose only the one-shot arrival summary, never the
// underlying flags.
//
// A SEPARATE OPEN QUESTION this file does NOT resolve: omp may load
// several `extensions/*.ts` files that each independently subscribe to
// `tool_result` for the SAME edit/write (this file plus kb-code-omp.ts,
// once both are installed). Whether omp composes their `{content}`
// returns (each handler seeing the previous one's edit) or lets only the
// LAST-registered handler's return value win is not verified anywhere in
// this repo (kb-code-omp.ts's own handler is the only precedent, and
// nothing here has ever run two `tool_result` subscribers at once). If
// composition turns out to drop one adapter's footer, the fix is to
// merge both delegates' output into ONE handler (as
// kb-code-annotations.sh already is merged with kb-code-why.sh under a
// single Claude Code PreToolUse matcher) — flagged here rather than
// silently assumed.
//
// Install (idempotent): plugins/kb-code/hooks/install-omp-hook.sh
// Symlinks this file into ~/.omp/agent/extensions/kb-code-annotations.ts
// (a SEPARATE target from kb-code-omp.ts's own kb-code.ts) — omp's native
// auto-discovery loads every *.ts extension it finds.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";

const HOOKS =
  process.env.KB_CODE_HOOKS_DIR &&
  existsSync(join(process.env.KB_CODE_HOOKS_DIR, "kb-code-annotations.sh"))
    ? process.env.KB_CODE_HOOKS_DIR
    : typeof import.meta.dir === "string" &&
      existsSync(join(import.meta.dir, "kb-code-annotations.sh"))
    ? import.meta.dir
    : null;

type RunOptions = {
  /** Object payloads are JSON-encoded for kb-code-annotations.sh's stdin contract. */
  input?: unknown;
  timeoutMs?: number;
};

// Structural copy of kb-code-omp.ts's own run() helper (itself a
// structural copy of kb-memory/hooks/kb-omp.ts's) — same child-process/
// timeout/fail-silent shape everywhere this repo shells out to a hook
// script from an omp extension. Not imported (kb-code-omp.ts's own header
// gives the same reasoning: these files are simple enough that a shared
// module would cost more in indirection than the duplication costs).
function run(cmd: string, args: string[], opts: RunOptions = {}): Promise<{
  code: number;
  stdout: string;
}> {
  const { promise, resolve: resolveRun } = Promise.withResolvers<{ code: number; stdout: string }>();
  let child: ReturnType<typeof spawn>;
  try {
    child = spawn(cmd, args, { stdio: ["pipe", "pipe", "pipe"] });
  } catch {
    resolveRun({ code: 1, stdout: "" });
    return promise;
  }
  let stdout = "";
  let done = false;
  const finish = (code: number) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    resolveRun({ code, stdout });
  };
  const timer = setTimeout(() => {
    try {
      child.kill("SIGTERM");
    } catch {}
  }, opts.timeoutMs ?? 20_000);
  child.stdout?.on("data", (d) => {
    stdout += String(d);
  });
  child.stderr?.on("data", () => {});
  child.on("close", (code) => finish(code ?? 1));
  child.on("error", () => finish(1));
  try {
    if (opts.input === undefined) child.stdin.end();
    else if (typeof opts.input === "string") child.stdin.end(opts.input);
    else child.stdin.end(JSON.stringify(opts.input));
  } catch {}
  return promise;
}

/** Extract `.hookSpecificOutput.additionalContext` from a Claude-format hook stdout. */
function claudeContextOf(stdout: string): string {
  try {
    const parsed = JSON.parse(stdout) as {
      hookSpecificOutput?: { additionalContext?: unknown };
    };
    const ctx = parsed.hookSpecificOutput?.additionalContext;
    return typeof ctx === "string" && ctx.trim() ? ctx : "";
  } catch {
    return "";
  }
}

/**
 * Resolve the touched file's path out of an omp `tool_result` event for
 * `edit`/`write` — byte-identical to kb-code-omp.ts's own
 * `resolveTouchedPath` (see that file's doc for the field citations).
 * Duplicated rather than imported for the same reason `run()` above is.
 */
function resolveTouchedPath(event: {
  toolName?: unknown;
  details?: unknown;
  input?: unknown;
}): string | null {
  const details = event.details as Record<string, unknown> | null | undefined;

  if (event.toolName === "edit") {
    if (details) {
      const p = details.path;
      if (typeof p === "string" && p) return p;
      const perFile = details.perFileResults;
      if (Array.isArray(perFile) && perFile.length === 1) {
        const only = perFile[0] as { path?: unknown } | undefined;
        if (only && typeof only.path === "string" && only.path) return only.path;
      }
    }
  } else if (event.toolName === "write") {
    if (details) {
      const p = details.resolvedPath;
      if (typeof p === "string" && p) return p;
    }
  } else {
    return null;
  }

  const input = event.input as Record<string, unknown> | null | undefined;
  const inputPath = input?.path;
  return typeof inputPath === "string" && inputPath ? inputPath : null;
}

function toAbsolute(p: string | null, cwd: string): string | null {
  if (!p) return null;
  try {
    return isAbsolute(p) ? p : resolve(cwd, p);
  } catch {
    return null;
  }
}

/** Append a footer to the last text block, or add one if the result has none. */
function appendFooter(
  content: Array<{ type: string; text?: string; [k: string]: unknown }>,
  footer: string,
): Array<{ type: string; text?: string; [k: string]: unknown }> {
  const out = content.slice();
  for (let i = out.length - 1; i >= 0; i--) {
    if (out[i]?.type === "text" && typeof out[i]?.text === "string") {
      out[i] = { ...out[i], text: `${out[i]!.text}${footer}` };
      return out;
    }
  }
  out.push({ type: "text", text: footer.replace(/^\n+/, "") });
  return out;
}

export default function kbCodeAnnotationsOmp(pi: {
  on(event: string, handler: (event: any, ctx: any) => Promise<unknown>): void;
}) {
  pi.on("tool_result", async (event, ctx) => {
    if (!HOOKS) return;
    try {
      const toolName = event?.toolName;
      if (toolName !== "edit" && toolName !== "write") return;
      // A failed edit/write never landed on disk — nothing new to flag
      // against (mirrors kb-code-omp.ts's own `isError` guard).
      if (event?.isError) return;

      let sid = "";
      try {
        sid = String(ctx?.sessionManager?.getSessionId?.() ?? "");
      } catch {}
      if (!sid) return;

      const cwd = (typeof ctx?.cwd === "string" && ctx.cwd) || process.cwd();
      const filePath = toAbsolute(resolveTouchedPath(event), cwd);
      if (!filePath) return;

      const synthetic = {
        tool_name: "Edit",
        tool_input: { file_path: filePath },
        session_id: sid,
      };
      const res = await run(join(HOOKS, "kb-code-annotations.sh"), [], {
        input: synthetic,
        timeoutMs: 2_000,
      });
      const block = claudeContextOf(res.stdout);
      if (!block) return;

      const content = Array.isArray(event.content) ? event.content : [];
      return { content: appendFooter(content, `\n\n[kb-code operator flags]\n${block}`) };
    } catch {
      return;
    }
  });
}
