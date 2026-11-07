#!/usr/bin/env bash
# test-omp-beats.sh - kb-omp.ts's live-session beats (OL PRES4): what the
# extension hands the beat scripts for tool_call (tool name + the call's `path`
# only, never an edit body), tool_approval_requested (blocked) and
# tool_approval_resolved (unblocked).
#
# Driven WITHOUT omp (the fake `pi` of test-omp-slate.sh); the beat scripts are
# stubs that append their argv and stdin to a log, so nothing reaches a daemon.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-omp-beats.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OMP_TS="$HOOKS_DIR/kb-omp.ts"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

echo "== kb-omp.ts live-session beats (PRES4) =="

if ! command -v bun >/dev/null 2>&1; then
  if [ "${KB_REQUIRE_BUN:-}" = "1" ]; then
    echo "not ok  - bun is required (KB_REQUIRE_BUN=1) but is not installed"
    exit 1
  fi
  echo "SKIP: bun is not installed - kb-omp.ts is a bun/TS extension."
  echo "passed=0 failed=0 skipped=1"
  exit 0
fi

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-beats-test.XXXXXX")"
trap 'rm -rf "$TMPROOT"' EXIT

mkdir -p "$TMPROOT/hooks" "$TMPROOT/repo" "$TMPROOT/bin" "$TMPROOT/code-hooks"
export KB_BEAT_CALLS="$TMPROOT/calls.log"
export KB_CAPTURE_CALLS="$TMPROOT/capture.log"
: >"$KB_BEAT_CALLS"
: >"$KB_CAPTURE_CALLS"
for h in kb-recall.sh kb-wake.sh kb-distill-nudge-omp.sh; do
  printf '#!/usr/bin/env bash\ncat >/dev/null 2>&1\nexit 0\n' >"$TMPROOT/hooks/$h"
  chmod +x "$TMPROOT/hooks/$h"
done
cat >"$TMPROOT/hooks/kb-capture-omp.sh" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null 2>&1 || true
printf 'CALL kb-capture-omp.sh\n' >>"$KB_CAPTURE_CALLS"
exit 0
STUB
chmod +x "$TMPROOT/hooks/kb-capture-omp.sh"
for h in kb-beat.sh kb-beat-throttle.sh; do
  cat >"$TMPROOT/hooks/$h" <<'STUB'
#!/usr/bin/env bash
# one atomic append: two stubs run concurrently and piecewise writes interleave
body="$(cat | tr -d '\n')"
printf 'CALL %s %s %s\n' "$(basename "$0")" "$*" "$body" >>"$KB_BEAT_CALLS"
exit 0
STUB
  chmod +x "$TMPROOT/hooks/$h"
done
for h in kb-code-why.sh kb-code-annotations.sh; do
  printf '#!/usr/bin/env bash\ncat >/dev/null 2>&1\nexit 0\n' >"$TMPROOT/code-hooks/$h"
  chmod +x "$TMPROOT/code-hooks/$h"
done
# a fake kb so the slate lane cannot reach a real one
printf '#!/usr/bin/env bash\nexit 1\n' >"$TMPROOT/bin/kb"
chmod +x "$TMPROOT/bin/kb"

export KB_HOOKS_DIR="$TMPROOT/hooks"
export KB_CODE_HOOKS_DIR="$TMPROOT/code-hooks"
export KB_TEST_CWD="$TMPROOT/repo"
export KB_CODE_OMP_TS="$(cd "$SCRIPT_DIR/../../../kb-code/hooks" && pwd)/kb-code-omp.ts"
export KB_CODE_ANN_TS="$(cd "$SCRIPT_DIR/../../../kb-code/hooks" && pwd)/kb-code-annotations-omp.ts"

cat >"$TMPROOT/omp-beats.test.ts" <<'TS'
import { expect, test } from "bun:test";
import { readFileSync, writeFileSync } from "node:fs";

const M: any = await import(process.env.KB_OMP_TS!);
const CALLS = process.env.KB_BEAT_CALLS!;
const CAPTURE_CALLS = process.env.KB_CAPTURE_CALLS!;
const SID = "omp-beats-session";
const ctx: any = {
  cwd: process.env.KB_TEST_CWD!,
  hasUI: false,
  sessionManager: { getSessionId: () => SID, getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "" },
};
const mainCtx: any = {
  cwd: process.env.KB_TEST_CWD!,
  hasUI: false,
  agent: { kind: "main", id: "m", name: "main", depth: 0 },
  sessionManager: { getSessionId: () => "omp-main-1", getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "" },
};
const subCtx: any = {
  cwd: process.env.KB_TEST_CWD!,
  hasUI: false,
  agent: { kind: "sub", id: "s", name: "explore", depth: 1, parentId: "m" },
  sessionManager: { getSessionId: () => "omp-sub-1", getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "" },
};

function makePi() {
  const chain: any = {};
  chain.optional = () => chain;
  chain.describe = () => chain;
  chain.nullable = () => chain;
  const z: any = {
    object: (o: any) => ({ ...chain, shape: o }),
    string: () => chain, number: () => chain, boolean: () => chain, array: () => chain,
  };
  const handlers = new Map<string, any>();
  M.default({
    zod: z,
    on: (e: string, h: any) => handlers.set(e, h),
    registerTool: () => {},
    registerCommand: () => {},
    registerFlag: () => {},
    getFlag: () => undefined,
    appendEntry: () => {},
    sendMessage: () => {},
    sendUserMessage: () => {},
  });
  return handlers;
}

async function calls(): Promise<string[]> {
  await Bun.sleep(600); // the beat scripts are spawned fire-and-forget
  return readFileSync(CALLS, "utf8").split("\n").filter((l) => l.startsWith("CALL "));
}
function payload(line: string): any {
  return JSON.parse(line.slice(line.indexOf("{")));
}

test("tool_call hands the throttle the tool name and the path, never the edit body", async () => {
  const h = makePi();
  await h.get("tool_call")(
    { type: "tool_call", toolName: "edit", input: { path: "src/a.rs", old_string: "SECRET-OLD", new_string: "SECRET-NEW" } },
    ctx,
  );
  const lines = (await calls()).filter((l) => l.includes("kb-beat-throttle.sh omp"));
  expect(lines.length).toBe(1);
  const p = payload(lines[0]);
  expect(p.tool_name).toBe("edit");
  expect(p.tool_input).toEqual({ path: "src/a.rs" });
  expect(p.session_id).toBe(SID);
  expect(lines[0]).not.toContain("SECRET");
});

test("tool_call without a string path sends the tool name and no tool_input", async () => {
  const h = makePi();
  await h.get("tool_call")({ type: "tool_call", toolName: "edit", input: { input: "*** patch text ***" } }, ctx);
  const line = (await calls()).filter((l) => l.includes("kb-beat-throttle.sh omp")).pop()!;
  const p = payload(line);
  expect(p.tool_name).toBe("edit");
  expect("tool_input" in p).toBe(false);
  expect(line).not.toContain("patch text");
});

test("an approval request is a blocked beat, its resolution an unblocked beat", async () => {
  const h = makePi();
  await h.get("tool_approval_requested")({ type: "tool_approval_requested", toolName: "bash", reason: "needs approval" }, ctx);
  await h.get("tool_approval_resolved")({ type: "tool_approval_resolved", toolName: "bash", approved: true }, ctx);
  const lines = await calls();
  const blocked = lines.filter((l) => l.includes("kb-beat.sh omp blocked"));
  const unblocked = lines.filter((l) => l.includes("kb-beat.sh omp unblocked"));
  expect(blocked.length).toBe(1);
  expect(payload(blocked[0]).message).toBe("bash: needs approval");
  expect(unblocked.length).toBe(1);
});

test("a subagent never takes the process identity", async () => {
  delete process.env.KB_SESSION_ID;
  const h = makePi();
  await h.get("session_start")({}, mainCtx);
  await h.get("session_start")({}, subCtx);
  await h.get("before_agent_start")({ prompt: "x" }, subCtx);
  await h.get("tool_call")({ type: "tool_call", toolName: "edit", input: { path: "a.rs" } }, subCtx);
  expect(process.env.KB_SESSION_ID).toBe("omp-main-1");
  expect(process.env.KB_HARNESS).toBe("omp");
});

test("a bash child after a subagent ran sees the main sid", () => {
  const r = Bun.spawnSync(["bash", "-c", 'printf %s "$KB_SESSION_ID"'], { env: process.env });
  expect(r.stdout.toString()).toBe("omp-main-1");
});

test("beat is not called for kind sub", async () => {
  await Bun.sleep(600); // let the previous test's fire-and-forget main beat land BEFORE truncating
  writeFileSync(CALLS, "");
  const h = makePi();
  await h.get("session_start")({}, subCtx);
  await h.get("before_agent_start")({ prompt: "x" }, subCtx);
  await h.get("tool_call")({ type: "tool_call", toolName: "edit", input: { path: "a.rs" } }, subCtx);
  await h.get("tool_approval_requested")({ type: "tool_approval_requested", toolName: "bash", reason: "needs approval" }, subCtx);
  await h.get("tool_approval_resolved")({ type: "tool_approval_resolved", toolName: "bash", approved: true }, subCtx);
  await h.get("session_stop")({ session_file: "" }, subCtx);
  await h.get("session_shutdown")({}, subCtx);
  const subLines = await calls();
  expect(subLines.filter((l) => l.includes("kb-beat.sh") || l.includes("kb-beat-throttle.sh")).length).toBe(0);
  // Positive control: the same events for the main agent do beat.
  await h.get("session_start")({}, mainCtx);
  await h.get("before_agent_start")({ prompt: "x" }, mainCtx);
  await h.get("tool_approval_requested")({ type: "tool_approval_requested", toolName: "bash", reason: "needs approval" }, mainCtx);
  await h.get("tool_approval_resolved")({ type: "tool_approval_resolved", toolName: "bash", approved: true }, mainCtx);
  await h.get("session_stop")({ session_file: "" }, mainCtx);
  await h.get("session_shutdown")({}, mainCtx);
  const mainLines = await calls();
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp start")).length).toBe(1);
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp prompt")).length).toBe(1);
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp blocked")).length).toBe(1);
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp unblocked")).length).toBe(1);
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp turn_end")).length).toBe(1);
  expect(mainLines.filter((l) => l.includes("kb-beat.sh omp end")).length).toBe(1);
});

test("advisor tool_call on the main session does not beat", async () => {
  writeFileSync(CALLS, "");
  const advisorCtx: any = {
    cwd: process.env.KB_TEST_CWD!,
    hasUI: false,
    agent: { kind: "sub", id: "advisor", name: "advisor", depth: 0 },
    sessionManager: { getSessionId: () => "omp-main-1", getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "" },
  };
  const h = makePi();
  await h.get("tool_call")({ type: "tool_call", toolName: "edit", input: { path: "a.rs" } }, advisorCtx);
  const lines = await calls();
  expect(lines.filter((l) => l.includes("kb-beat-throttle.sh")).length).toBe(0);
});

test("a sub still captures", async () => {
  writeFileSync(CAPTURE_CALLS, "");
  const subFileCtx: any = {
    cwd: process.env.KB_TEST_CWD!,
    hasUI: false,
    agent: { kind: "sub", id: "s", name: "explore", depth: 1, parentId: "m" },
    sessionManager: { getSessionId: () => "omp-sub-1", getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "/tmp/sub.jsonl" },
  };
  const h = makePi();
  await h.get("session_shutdown")({}, subFileCtx);
  await Bun.sleep(600);
  const lines = readFileSync(CAPTURE_CALLS, "utf8").split("\n").filter((l) => l.startsWith("CALL "));
  expect(lines.length).toBe(1);
});

test("no ctx.agent counts as main", async () => {
  delete process.env.KB_SESSION_ID;
  delete process.env.KB_HARNESS;
  writeFileSync(CALLS, "");
  const h = makePi();
  await h.get("session_start")({}, ctx);
  expect(process.env.KB_SESSION_ID).toBe(SID);
  expect(process.env.KB_HARNESS).toBe("omp");
  const lines = await calls();
  expect(lines.filter((l) => l.includes("kb-beat.sh omp start")).length).toBe(1);
});

test("the kb-code omp twins only publish identity for the main agent", async () => {
  const codeOmp: any = await import(process.env.KB_CODE_OMP_TS!);
  const codeAnn: any = await import(process.env.KB_CODE_ANN_TS!);
  function makeToolPi() {
    const handlers = new Map<string, any>();
    return { handlers, pi: { on: (e: string, h: any) => handlers.set(e, h) } };
  }
  const sub: any = { cwd: process.env.KB_TEST_CWD!, agent: { kind: "sub", id: "s", name: "explore", depth: 1, parentId: "m" }, sessionManager: { getSessionId: () => "omp-sub-9" } };
  const main: any = { cwd: process.env.KB_TEST_CWD!, agent: { kind: "main", id: "m", name: "main", depth: 0 }, sessionManager: { getSessionId: () => "omp-main-9" } };
  const evt: any = { toolName: "edit", isError: false, input: { path: "a.rs" }, content: [] };
  for (const [mod, label] of [[codeOmp, "kb-code-omp"], [codeAnn, "kb-code-annotations"]] as const) {
    delete process.env.KB_SESSION_ID;
    const t1 = makeToolPi();
    mod.default(t1.pi);
    await t1.handlers.get("tool_result")(evt, sub);
    expect(process.env.KB_SESSION_ID).toBeUndefined();
    const t2 = makeToolPi();
    mod.default(t2.pi);
    await t2.handlers.get("tool_result")(evt, main);
    expect(process.env.KB_SESSION_ID).toBe("omp-main-9");
  }
});
TS

if ( cd "$TMPROOT" && HOME="$TMPROOT" PATH="$TMPROOT/bin:$PATH" KB_OMP_TS="$OMP_TS" \
       bun test "$TMPROOT/omp-beats.test.ts" ) >"$TMPROOT/bun.log" 2>&1; then
  ok "bun test: tool_call path hand-off, blocked/unblocked beats"
else
  bad "bun test failed"
  sed -n '1,80p' "$TMPROOT/bun.log"
fi
grep -E '^\s*[0-9]+ (pass|fail)' "$TMPROOT/bun.log" | sed 's/^/        /'

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
