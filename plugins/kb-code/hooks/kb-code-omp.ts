// kb-code-omp.ts — kb-code provenance for Oh My Pi (omp).
//
// omp's PreToolUse-equivalent (`tool_call`) can only block/revise a tool's
// INPUT before it runs — it has no context-injection return value (see
// ToolCallEventResult in extensibility/shared-events.ts: `block`/`reason`/
// `input` only). There is no seam there for kb-code-why.sh's job. `tool_result`
// is the closest match: middleware over every tool's RESULT, able to return
// `{content}` to replace what the model sees (ToolResultEventResult). This
// extension subscribes there, targeting omp's `edit`/`write` tools (the
// analogues of Claude Code's `Edit`/`Write`), and — on a successful result —
// appends kb-code-why.sh's provenance block as a footer on the tool's own
// text content, instead of injecting it "next to" the tool call the way
// Claude Code's PreToolUse hook does. Net effect is the same: the model sees
// "who wrote this and why" right after touching a file kb-code can attribute.
//
// kb-code-why.sh itself is NOT reimplemented or duplicated here — this
// extension is a thin adapter, exactly like kb-code-why-codex.sh, that
// builds a synthetic Claude-Code-shaped stdin payload
// ({tool_name:"Edit", tool_input:{file_path}, session_id}) and pipes it
// through the shell script UNMODIFIED. kb-code-why.sh owns the daemon
// lookup, the confidence gate (trailer/exact only), the 3-per-session
// injection cap, and the per-(repo,path) dedupe — all keyed off the
// `session_id` we pass through, so omp shares that budget with Claude Code/
// Codex sessions ONLY if they happen to reuse the same session id (they
// don't; omp's session ids are its own uuid space) — in practice this gives
// omp its OWN independent 3-per-session budget, which is the intended
// behavior (each harness's session is its own conversation).
//
// tool_name is hardcoded to the string "Edit" for BOTH omp's `edit` and
// `write` results (matching kb-code-why-codex.sh's own synthetic payload
// shape) — kb-code-why.sh's matcher only cares that it's "Edit"|"Write" and
// neither branch reads tool_input beyond `file_path`, so the distinction is
// immaterial to the delegate. No `old_string` is threaded through (omp's
// edit/write tools don't expose one on the RESULT event in a directly
// reusable shape), so every lookup is file-grade, never line-grade — an
// accepted, honest degradation matching the Codex adapter's own posture.
//
// Fails open exactly like every other kb hook in this repo: any error,
// missing HOOKS dir, timeout, or empty additionalContext -> the tool_result
// handler returns nothing (undefined), leaving the tool's result untouched.
// Never throws — every awaited call and every property access below is
// wrapped so a throw inside this handler (which the omp extensibility
// gotchas single out as PROCESS-FATAL — see docs/extensions.md +
// extensibility/extensions/types.ts) can't tear down the session.
//
// Install (idempotent): plugins/kb-code/hooks/install-omp-hook.sh
// Symlinks this file into ~/.omp/agent/extensions/kb-code.ts — omp's native
// auto-discovery loads it into every session; the loader resolves the
// symlink's realpath first (docs/extension-loading.md), so import.meta.dir
// below IS this hooks directory and kb-code-why.sh resolves regardless of
// repo location.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";

const HOOKS =
  process.env.KB_CODE_HOOKS_DIR && existsSync(join(process.env.KB_CODE_HOOKS_DIR, "kb-code-why.sh"))
    ? process.env.KB_CODE_HOOKS_DIR
    : typeof import.meta.dir === "string" &&
      existsSync(join(import.meta.dir, "kb-code-why.sh"))
    ? import.meta.dir
    : null;

type RunOptions = {
  /** Object payloads are JSON-encoded for kb-code-why.sh's stdin contract. */
  input?: unknown;
  timeoutMs?: number;
};

// Structural copy of kb-omp.ts's run() helper (kb-memory/hooks/kb-omp.ts) —
// same child-process/timeout/fail-silent shape, so both omp extensions
// behave identically under load/timeout/spawn-failure. Not imported (the
// sibling is being rewritten concurrently); duplicated on purpose.
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
 * `edit`/`write`, preferring the ABSOLUTE path the tool itself resolved to
 * (edit: `details.path`, single-file result — "Absolute file path for
 * single-file edit results", edit/renderer.ts `EditToolDetails.path`; write:
 * `details.resolvedPath`, write.ts `WriteToolDetails.resolvedPath`) over the
 * raw, possibly cwd-relative, model-supplied `input.path`. A multi-file edit
 * result (`details.perFileResults.length !== 1`) is deliberately left
 * unresolved — kb-code-why.sh attributes exactly one file per call and there
 * is no honest single choice among several.
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

  // Fallback: the model-supplied `path` on the tool's input (write's typed
  // schema always has one; edit's untyped Record carries the same key for
  // every edit mode this repo has observed — hashline/patch/replace/sloppy).
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

export default function kbCodeOmp(pi: {
  on(event: string, handler: (event: any, ctx: any) => Promise<unknown>): void;
}) {
  pi.on("tool_result", async (event, ctx) => {
    if (!HOOKS) return;
    try {
      const toolName = event?.toolName;
      if (toolName !== "edit" && toolName !== "write") return;
      // A failed edit/write leaves the on-disk file's provenance unrelated
      // to what the model just attempted — don't decorate an error result
      // with an unrelated "who wrote this" footer.
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
      const res = await run(join(HOOKS, "kb-code-why.sh"), [], {
        input: synthetic,
        timeoutMs: 2_000,
      });
      const block = claudeContextOf(res.stdout);
      if (!block) return;

      const content = Array.isArray(event.content) ? event.content : [];
      return { content: appendFooter(content, `\n\n[kb-code provenance]\n${block}`) };
    } catch {
      return;
    }
  });
}
