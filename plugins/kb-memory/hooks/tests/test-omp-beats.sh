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

mkdir -p "$TMPROOT/hooks" "$TMPROOT/repo" "$TMPROOT/bin"
export KB_BEAT_CALLS="$TMPROOT/calls.log"
: >"$KB_BEAT_CALLS"
for h in kb-recall.sh kb-wake.sh kb-capture-omp.sh kb-distill-nudge-omp.sh; do
  printf '#!/usr/bin/env bash\ncat >/dev/null 2>&1\nexit 0\n' >"$TMPROOT/hooks/$h"
  chmod +x "$TMPROOT/hooks/$h"
done
for h in kb-beat.sh kb-beat-throttle.sh; do
  cat >"$TMPROOT/hooks/$h" <<'STUB'
#!/usr/bin/env bash
{ printf 'CALL %s %s ' "$(basename "$0")" "$*"; cat | tr -d '\n'; printf '\n'; } >>"$KB_BEAT_CALLS"
exit 0
STUB
  chmod +x "$TMPROOT/hooks/$h"
done
# a fake kb so the slate lane cannot reach a real one
printf '#!/usr/bin/env bash\nexit 1\n' >"$TMPROOT/bin/kb"
chmod +x "$TMPROOT/bin/kb"

export KB_HOOKS_DIR="$TMPROOT/hooks"
export KB_TEST_CWD="$TMPROOT/repo"

cat >"$TMPROOT/omp-beats.test.ts" <<'TS'
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const M: any = await import(process.env.KB_OMP_TS!);
const CALLS = process.env.KB_BEAT_CALLS!;
const SID = "omp-beats-session";
const ctx: any = {
  cwd: process.env.KB_TEST_CWD!,
  hasUI: false,
  sessionManager: { getSessionId: () => SID, getCwd: () => process.env.KB_TEST_CWD!, getSessionFile: () => "" },
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
