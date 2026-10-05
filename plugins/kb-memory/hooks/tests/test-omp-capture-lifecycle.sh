#!/usr/bin/env bash
# test-omp-capture-lifecycle.sh - kb-omp.ts's CAPTURE spawn path (v0.45 OC):
# the capture script is spawned OWNED (detached: its own session/process group,
# never omp's), a timeout or an aborted session_stop terminates it - SIGTERM to
# its pid first (so its trap can reap what it owns), then SIGKILL of its OWN
# process group as a backstop - and run()'s plain behaviour for every other
# hook is unchanged.
#
# The extension is driven WITHOUT omp (the fake `pi` of test-omp-slate.sh); the
# capture script is a stub that records its pids. Every pid this test sees is
# one it started; the cleanup kills only those.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-omp-capture-lifecycle.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OMP_TS="$HOOKS_DIR/kb-omp.ts"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

echo "== kb-omp.ts capture spawn path (v0.45 OC) =="

if ! command -v bun >/dev/null 2>&1; then
  if [ "${KB_REQUIRE_BUN:-}" = "1" ]; then
    echo "not ok  - bun is required (KB_REQUIRE_BUN=1) but is not installed"
    exit 1
  fi
  echo "SKIP: bun is not installed - kb-omp.ts is a bun/TS extension."
  echo "passed=0 failed=0 skipped=1"
  exit 0
fi

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-capture-lifecycle.XXXXXX")"
cleanup() {
  # Only pids the stubs recorded in OUR temp dir, and only while they are
  # still the bash/sleep processes the stub started.
  local f p c
  for f in "$TMPROOT"/stub-*/script.pid "$TMPROOT"/stub-*/grandchild.pid; do
    [ -f "$f" ] || continue
    p="$(cat "$f" 2>/dev/null)"
    c="$(cat "/proc/$p/comm" 2>/dev/null)"
    case "$c" in bash | sleep) kill -KILL "$p" 2>/dev/null ;; esac
  done
  rm -rf "$TMPROOT"
}
trap cleanup EXIT

mkdir -p "$TMPROOT/hooks" "$TMPROOT/repo"
for h in kb-recall.sh kb-wake.sh kb-beat.sh kb-beat-throttle.sh kb-distill-nudge-omp.sh; do
  printf '#!/usr/bin/env bash\ncat >/dev/null 2>&1\nexit 0\n' >"$TMPROOT/hooks/$h"
  chmod +x "$TMPROOT/hooks/$h"
done
# The capture stub: records its own pid, starts a same-group grandchild, and
# (STUB_MODE=trap) behaves like the real adapter - on TERM it notes it, kills
# its grandchild and exits.
cat >"$TMPROOT/hooks/kb-capture-omp.sh" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
echo $$ >"$STUB_DIR/script.pid"
ps -o pgid= -p $$ | tr -d ' ' >"$STUB_DIR/script.pgid"
ps -o sid= -p $$ | tr -d ' ' >"$STUB_DIR/script.sid"
sleep 300 &
echo $! >"$STUB_DIR/grandchild.pid"
if [ "${STUB_MODE:-}" = "trap" ]; then
  trap 'echo term >"$STUB_DIR/got-term"; kill "$(cat "$STUB_DIR/grandchild.pid")" 2>/dev/null; exit 0' TERM
fi
wait
STUB
chmod +x "$TMPROOT/hooks/kb-capture-omp.sh"
export KB_HOOKS_DIR="$TMPROOT/hooks" KB_TEST_CWD="$TMPROOT/repo" TMPROOT

if bun build "$OMP_TS" --target=bun --outfile "$TMPROOT/bundle.js" >"$TMPROOT/build.log" 2>&1; then
  ok "kb-omp.ts parses and bundles (bun build)"
else
  bad "kb-omp.ts does not parse"
  sed -n '1,20p' "$TMPROOT/build.log"
  echo "passed=$PASS failed=$FAIL"
  exit 1
fi

cat >"$TMPROOT/capture-lifecycle.test.ts" <<'TS'
import { expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";

const M: any = await import(process.env.KB_OMP_TS!);
const ROOT = process.env.TMPROOT!;
const SID = "omp-capture-lifecycle-sid";
const FILE = `${ROOT}/session.jsonl`;
writeFileSync(FILE, "{}\n");

const ctx: any = {
  cwd: process.env.KB_TEST_CWD,
  hasUI: false,
  sessionManager: { getSessionId: () => SID, getCwd: () => process.env.KB_TEST_CWD, getSessionFile: () => FILE },
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

const alive = (pid: number) => {
  try { process.kill(pid, 0); } catch { return false; }
  try { // a zombie is dead for our purposes
    return !readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1]?.startsWith("Z");
  } catch { return false; }
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(cond: () => boolean, ms: number) {
  const t0 = Date.now();
  while (!cond() && Date.now() - t0 < ms) await sleep(25);
  return cond();
}
function stub(name: string) {
  const dir = `${ROOT}/stub-${name}`;
  mkdirSync(dir, { recursive: true });
  process.env.STUB_DIR = dir;
  const rd = (f: string) => (existsSync(`${dir}/${f}`) ? readFileSync(`${dir}/${f}`, "utf8").trim() : "");
  return { dir, rd };
}
function ownGroup(): string {
  return readFileSync("/proc/self/stat", "utf8").split(") ")[1].split(" ")[2];
}

test("a capture TIMEOUT terminates the owned script and its group, never omp's", async () => {
  // A sentinel in OUR group stands in for the rest of omp's process group.
  const sentinel = spawn("sleep", ["300"], { stdio: "ignore" });
  try {
    const s = stub("timeout");
    delete process.env.STUB_MODE;
    process.env.KB_CAPTURE_TIMEOUT_MS = "600";
    const handlers = makePi();
    const t0 = Date.now();
    await handlers.get("session_shutdown")({}, ctx);
    expect(Date.now() - t0).toBeLessThan(5_000); // the 600 ms deadline, not the stub's 300 s
    expect(await until(() => s.rd("grandchild.pid") !== "", 2000)).toBe(true);
    const script = Number(s.rd("script.pid"));
    const grand = Number(s.rd("grandchild.pid"));
    // owned: its own session and process group, distinct from the extension host's
    expect(s.rd("script.pgid")).toBe(String(script));
    expect(s.rd("script.pgid")).not.toBe(ownGroup());
    // TERM first (the stub has no trap, so it dies), the group SIGKILL after the grace
    expect(await until(() => !alive(script) && !alive(grand), 8_000)).toBe(true);
    expect(alive(sentinel.pid!)).toBe(true); // omp's own group was never signalled

  } finally {
    sentinel.kill("SIGKILL");
  }
});

test("the script is asked to stop with SIGTERM first so its trap can reap its own session", async () => {
  const s = stub("trap");
  process.env.STUB_MODE = "trap";
  process.env.KB_CAPTURE_TIMEOUT_MS = "600";
  const handlers = makePi();
  await handlers.get("session_shutdown")({}, ctx);
  expect(await until(() => s.rd("got-term") === "term", 3_000)).toBe(true);
  const script = Number(s.rd("script.pid"));
  const grand = Number(s.rd("grandchild.pid"));
  expect(await until(() => !alive(script) && !alive(grand), 3_000)).toBe(true);
  delete process.env.STUB_MODE;
});

test("an aborted session_stop (event.signal) cancels the capture too", async () => {
  const s = stub("abort");
  process.env.STUB_MODE = "trap";
  process.env.KB_CAPTURE_TIMEOUT_MS = "120000";
  const handlers = makePi();
  const ac = new AbortController();
  const p = handlers.get("session_stop")({ signal: ac.signal, session_file: FILE }, ctx);
  expect(await until(() => s.rd("grandchild.pid") !== "", 4_000)).toBe(true);
  const script = Number(s.rd("script.pid"));
  expect(alive(script)).toBe(true);
  ac.abort();
  expect(await until(() => s.rd("got-term") === "term", 3_000)).toBe(true);
  expect(await until(() => !alive(script), 4_000)).toBe(true);
  await p;
  delete process.env.STUB_MODE;
});

test("a capture that finishes normally is left alone", async () => {
  const dir = `${ROOT}/stub-fast`;
  mkdirSync(dir, { recursive: true });
  process.env.STUB_DIR = dir;
  writeFileSync(`${ROOT}/hooks/kb-capture-omp.sh`, `#!/usr/bin/env bash\ncat >/dev/null\necho $$ >"$STUB_DIR/done.pid"\nexit 0\n`);
  process.env.KB_CAPTURE_TIMEOUT_MS = "5000";
  const handlers = makePi();
  const t0 = Date.now();
  await handlers.get("session_shutdown")({}, ctx);
  expect(Date.now() - t0).toBeLessThan(3_000);
  expect(existsSync(`${dir}/done.pid`)).toBe(true);
});
TS

if ( cd "$TMPROOT" && HOME="$TMPROOT" KB_OMP_TS="$OMP_TS" bun test "$TMPROOT/capture-lifecycle.test.ts" ) >"$TMPROOT/bun.log" 2>&1; then
  ok "bun test: owned spawn, TERM-then-group-KILL on timeout and abort, normal completion untouched"
else
  bad "bun test failed"
  sed -n '1,80p' "$TMPROOT/bun.log"
fi
grep -E '^\s*[0-9]+ (pass|fail)' "$TMPROOT/bun.log" | sed 's/^/        /'

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
