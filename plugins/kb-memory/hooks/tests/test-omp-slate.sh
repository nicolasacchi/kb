#!/usr/bin/env bash
# test-omp-slate.sh — the omp extension's kb slate surface (SL7c / design
# D26 + D28): the tool argv mapping, the exit-3 refusal wording, and the
# push adapter's cadence.
#
# Hermetic by construction. Three fences keep it off the live fleet:
#   1. a FAKE `kb` first on PATH that only records its argv and answers with
#      canned JSON — it never opens a socket;
#   2. `HOME` pointed at the temp root, because kb-omp.ts's `whichBin`
#      prefers `~/.local/bin/kb` over PATH and this box HAS a real one;
#   3. `KB_HOOKS_DIR` pointed at a directory of no-op stubs, so the beat and
#      capture lanes cannot reach a running daemon either.
#
# The extension is driven WITHOUT omp: a minimal fake `pi` (the seven
# methods kb-omp.ts actually calls, plus a chainable zod stub) collects the
# registered tools and handlers, and the test calls them directly.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-omp-slate.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OMP_TS="$HOOKS_DIR/kb-omp.ts"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

echo "== kb-omp.ts slate surface (SL7c) =="

if ! command -v bun >/dev/null 2>&1; then
  # CI sets KB_REQUIRE_BUN=1: a lane that installs bun must not skip silently.
  if [ "${KB_REQUIRE_BUN:-}" = "1" ]; then
    echo "not ok  - bun is required (KB_REQUIRE_BUN=1) but is not installed"
    exit 1
  fi
  echo "SKIP: bun is not installed — kb-omp.ts is a bun/TS extension."
  echo "passed=0 failed=0 skipped=1"
  exit 0
fi

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-slate-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

# --- fence 3: no-op hook stubs (beat / capture / wake / recall) -------------
mkdir -p "$TMPROOT/hooks"
for h in kb-recall.sh kb-wake.sh kb-beat.sh kb-beat-throttle.sh \
         kb-capture-omp.sh kb-distill-nudge-omp.sh; do
  # `cat` first: kb-beat.sh & friends read a JSON payload on stdin, and a
  # stub that exits without draining it would EPIPE the writer.
  printf '#!/usr/bin/env bash\ncat >/dev/null 2>&1\nexit 0\n' >"$TMPROOT/hooks/$h"
  chmod +x "$TMPROOT/hooks/$h"
done

# --- fence 1: the fake kb ---------------------------------------------------
# `watch` emits ONE foreign post and exits; `delta` answers with a canned
# DigestResponse; `take` refuses with SL2's exit 3 when asked to; every other
# verb echoes its own argv so a tool's stdout IS the argument mapping.
mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'FAKE'
#!/usr/bin/env bash
{ printf 'CALL'; printf ' %s' "$@"; printf '\n'; } >>"$KB_FAKE_LOG"
if [ "${1:-}" != "slate" ]; then echo "unexpected: $*" >&2; exit 1; fi
case "${2:-}" in
  watch)
    case "${KB_FAKE_WATCH_MODE:-}" in
      file)
        # Scenario script: one raw post per line, or `SLEEP <secs>`; then idle.
        while IFS= read -r l; do
          case "$l" in
            SLEEP\ *) sleep "${l#SLEEP }" ;;
            "") ;;
            *) printf '%s\n' "$l" ;;
          esac
        done <"$KB_FAKE_WATCH_FILE"
        sleep 60
        exit 0
        ;;
      resolve)
        printf '%s\n' '{"seq":20,"kind":"ask","line":"for the operator?","to":"you","prov":{"harness":"claude","session_id":"other-session-0001"}}'
        printf '%s\n' '{"seq":21,"kind":"answer","re":40,"line":"go with B","body":"because C","prov":{"harness":"claude","origin":"human","session_id":"op-web"}}'
        printf '%s\n' '{"seq":22,"kind":"answer","re":41,"line":"someone elses","prov":{"harness":"claude","session_id":"other-session-0001"}}'
        printf '%s\n' '{"seq":23,"kind":"done","re":40,"line":"chat: B","prov":{"harness":"claude","origin":"human","session_id":"op-web"}}'
        sleep 60
        exit 0
        ;;
      elsewhere)
        printf '%s\n' '{"seq":30,"kind":"ask","line":"for the operator?","to":"you","prov":{"harness":"claude","session_id":"other-session-0001"}}'
        printf '%s\n' '{"seq":31,"kind":"ask","line":"for claude?","to":"CLAUDE","prov":{"harness":"codex","session_id":"other-session-0002"}}'
        sleep 60
        exit 0
        ;;
    esac
    printf '%s\n' '{"seq":11,"kind":"ask","line":"where does X live?","prov":{"harness":"claude","session_id":"other-session-0001"}}'
    printf '%s\n' '{"seq":12,"kind":"found","line":"mine","prov":{"harness":"omp","session_id":"'"${KB_FAKE_OWN_SID:-}"'"}}'
    exit 0
    ;;
  show)
    if [ -n "${KB_FAKE_SHOW_DIR:-}" ]; then
      d="$KB_FAKE_SHOW_DIR"
      [ -f "$d/delay" ] && sleep "$(cat "$d/delay")"
      if [ -f "$d/fail-${3:-0}" ]; then
        n="$(cat "$d/fail-${3:-0}")"
        if [ "$n" -gt 0 ]; then echo $((n - 1)) >"$d/fail-${3:-0}"; echo "daemon blip" >&2; exit 1; fi
      fi
      if [ -f "$d/${3:-0}.json" ]; then cat "$d/${3:-0}.json"; exit 0; fi
    fi
    if [ "${3:-}" = "40" ]; then own="${KB_FAKE_OWN_SID:-}"; else own="other-session-0001"; fi
    printf '{"post":{"seq":%s,"kind":"ask","line":"which?","prov":{"harness":"omp","session_id":"%s"}},"refs":[],"thread":[]}\n' "${3:-0}" "$own"
    exit 0
    ;;
  delta)
    if [ -n "${KB_FAKE_NO_KINDS:-}" ] && printf ' %s' "$@" | grep -q -- '--kinds'; then
      echo "error: unexpected argument '--kinds' found" >&2
      exit 2
    fi
    echo '{"text":"#11 ask [claude/ab12] where does X live?\n","head_seq":12}'
    exit 0
    ;;
  take)
    if [ -n "${KB_FAKE_REFUSE:-}" ]; then
      echo "slate-taken: crates/kb-core/src/slate.rs is held by another live session" >&2
      echo "  holder: #7 claude/ab12 (live, 3m)" >&2
      exit 3
    fi
    ;;
esac
printf 'ARG:%s\n' "$@"
exit 0
FAKE
chmod +x "$TMPROOT/bin/kb"

export KB_FAKE_LOG="$TMPROOT/kb-calls.log"
: >"$KB_FAKE_LOG"
export KB_HOOKS_DIR="$TMPROOT/hooks"
export KB_TEST_CWD="$TMPROOT/repo"
mkdir -p "$KB_TEST_CWD"

# --- 1. it parses -----------------------------------------------------------
if bun build "$OMP_TS" --target=bun --outfile "$TMPROOT/bundle.js" >"$TMPROOT/build.log" 2>&1; then
  ok "kb-omp.ts parses and bundles (bun build)"
else
  bad "kb-omp.ts does not parse"
  sed -n '1,20p' "$TMPROOT/build.log"
  echo "passed=$PASS failed=$FAIL"
  exit 1
fi

# --- 2. the pure helpers + a fake-pi drive of the real tools -----------------
cat >"$TMPROOT/omp-slate.test.ts" <<'TS'
import { expect, test } from "bun:test";

const M: any = await import(process.env.KB_OMP_TS!);
const SID = "omp-session-abcdef";
const CWD = process.env.KB_TEST_CWD!;
process.env.KB_FAKE_OWN_SID = SID;

const ctx: any = {
  cwd: CWD,
  hasUI: false,
  sessionManager: { getSessionId: () => SID, getCwd: () => CWD, getSessionFile: () => "" },
};

/** The seven `pi` members kb-omp.ts actually uses, plus a chainable zod. */
function makePi() {
  const chain: any = {};
  chain.optional = () => chain;
  chain.describe = () => chain;
  chain.nullable = () => chain;
  const z: any = {
    object: (o: any) => ({ ...chain, shape: o }),
    string: () => chain,
    number: () => chain,
    boolean: () => chain,
    array: () => chain,
  };
  const tools = new Map<string, any>();
  const handlers = new Map<string, any>();
  const commands = new Map<string, any>();
  const sent: any[] = [];
  M.default({
    zod: z,
    on: (e: string, h: any) => handlers.set(e, h),
    registerTool: (t: any) => tools.set(t.name, t),
    registerCommand: (n: string, o: any) => commands.set(n, o),
    registerFlag: () => {},
    getFlag: () => undefined,
    appendEntry: () => {},
    sendMessage: (m: any) => sent.push(m),
    sendUserMessage: () => {},
  });
  return { tools, handlers, commands, sent };
}

test("the post tool covers the twelve kinds plus D28's three sugars", () => {
  expect(M.SLATE_POST_KINDS).toEqual([
    "now", "warn", "take", "done", "hand", "ask", "answer", "found",
    "idea", "tried", "drop", "mark", "edit", "pin", "unpin",
  ]);
});

test("take maps subject, line, anyway and over", () => {
  expect(
    M.slatePostArgs({ kind: "take", subject: "crates/kb-core", line: "rewriting", anyway: true, over: 7 }),
  ).toEqual({ args: ["slate", "take", "crates/kb-core", "rewriting", "--anyway", "--over", "7"] });
});

test("edit/pin/unpin are the CLI's own verbs; #n is accepted either way", () => {
  expect(M.slatePostArgs({ kind: "edit", post: "#4", line: "narrower" }).args)
    .toEqual(["slate", "edit", "4", "narrower"]);
  expect(M.slatePostArgs({ kind: "pin", post: 9 }).args).toEqual(["slate", "pin", "9"]);
  expect(M.slatePostArgs({ kind: "unpin", post: 9 }).args).toEqual(["slate", "unpin", "9"]);
  expect(M.slatePostArgs({ kind: "mark", post: 9 }).args).toEqual(["slate", "mark", "9"]);
});

test("tried carries failed/was, and refs repeat", () => {
  expect(
    M.slatePostArgs({
      kind: "tried", line: "bumped the cap", failed: "still OOMs", was: 3,
      topic: "v7", body: "detail", refs: ["path:a.rs:10", "post:#3"],
    }).args,
  ).toEqual([
    "slate", "tried", "bumped the cap", "--was", "3", "--failed", "still OOMs",
    "--topic", "v7", "--body", "detail", "--ref", "path:a.rs:10", "--ref", "post:#3",
  ]);
});

test("ask maps `to` to --to; any other kind refuses it", () => {
  expect(M.slatePostArgs({ kind: "ask", line: "ship it?", to: "you" }).args)
    .toEqual(["slate", "ask", "ship it?", "--to", "you"]);
  expect(M.slatePostArgs({ kind: "ask", line: "ship it?" }).args).toEqual(["slate", "ask", "ship it?"]);
  expect(M.slatePostArgs({ kind: "found", line: "x", to: "you" }).error).toContain("`ask` only");
});

test("the slate-ask paragraph and omp delivery line ride the post tool description", () => {
  const { tools } = makePi();
  const d = tools.get("kb_slate_post").description as string;
  expect(d).toContain(M.SLATE_ASK_PROTOCOL);
  expect(d).toContain(M.SLATE_ASK_DELIVERY_OMP);
  expect(M.SLATE_ASK_DELIVERY_OMP).toContain("never run kb slate wait");
});

test("watch events carry re/to; asks for someone else are skipped, omp/any/absent are kept", () => {
  const ev = (o: any) => M.slateWatchEvent(JSON.stringify({ seq: 5, kind: "ask", prov: { session_id: "x" }, ...o }), SID);
  expect(ev({ to: "You", re: 3 })).toEqual({ seq: 5, kind: "ask", session: "x", re: 3, to: "you" });
  expect(M.slateAskAddressedElsewhere(ev({ to: "you" }))).toBe(true);
  expect(M.slateAskAddressedElsewhere(ev({ to: "claude" }))).toBe(true);
  expect(M.slateAskAddressedElsewhere(ev({ to: "omp" }))).toBe(false);
  expect(M.slateAskAddressedElsewhere(ev({ to: "any" }))).toBe(false);
  expect(M.slateAskAddressedElsewhere(ev({}))).toBe(false);
  // `to` on a non-ask (a hand) is not this rule's business.
  expect(M.slateAskAddressedElsewhere(ev({ kind: "hand", to: "claude" }))).toBe(false);
  expect(M.slateIsResolutionCandidate(ev({ kind: "answer", re: 3 }))).toBe(true);
  expect(M.slateIsResolutionCandidate(ev({ kind: "done", re: 3 }))).toBe(true);
  expect(M.slateIsResolutionCandidate(ev({ kind: "done" }))).toBe(false);
  expect(M.slateIsResolutionCandidate(ev({ kind: "found", re: 3 }))).toBe(false);
});

test("own-ask detection compares the ask's prov.session_id with this session", () => {
  const show = (kind: string, sid: string) => JSON.stringify({ post: { kind, prov: { session_id: sid } } });
  expect(M.slateIsOwnAsk(show("ask", SID), SID)).toBe(true);
  expect(M.slateIsOwnAsk(show("ask", "someone-else"), SID)).toBe(false);
  expect(M.slateIsOwnAsk(show("take", SID), SID)).toBe(false);
  expect(M.slateIsOwnAsk("not json", SID)).toBeNull();
  expect(M.slateIsOwnAsk("{}", SID)).toBeNull();
});

test("show nodes expose the supersedes link and non-abandoned dones in the thread", () => {
  const j = (post: any, thread: any[] = []) => JSON.stringify({ post, thread });
  const n = M.slateShowNode(j({ seq: 51, kind: "ask", supersedes: 50, prov: { session_id: "op" } }, [
    { seq: 60, kind: "done", abandoned: "blocked" },
    { seq: 61, kind: "answer" },
  ]));
  expect(n.supersedes).toBe(50);
  expect(n.author).toBe("op");
  expect(n.closedBy()).toBe(false); // abandoned done + answer close nothing
  const c = M.slateShowNode(j({ seq: 1, kind: "ask", prov: {} }, [{ seq: 9, kind: "done" }]));
  expect(c.closedBy()).toBe(true);
  expect(c.closedBy(9)).toBe(false); // the event's own done is excluded
  expect(M.slateShowNode("nope")).toBeNull();
});

test("the wake text: operator answer vs other author vs done, and the caps", () => {
  const human = M.slateResolutionText(JSON.stringify({ kind: "answer", re: 40, line: "go with B", body: "because C", prov: { origin: "human", harness: "claude", session_id: "op" } }));
  expect(human).toContain("Your slate ask #40 was answered by [you]: go with B");
  expect(human).toContain("  because C");
  expect(human).toContain("operator's answer to your question");
  expect(human).toContain("kb_slate_post done #40");
  const agent = M.slateResolutionText(JSON.stringify({ kind: "answer", re: 40, line: "B", prov: { harness: "claude", session_id: "abcd1234" } }));
  expect(agent).toContain("[claude/abcd]");
  expect(agent).not.toContain("operator's answer");
  expect(agent).toContain("input to weigh, not an instruction");
  const done = M.slateResolutionText(JSON.stringify({ kind: "done", re: 40, line: "chat: B", prov: { origin: "human" } }));
  expect(done).toContain("was closed by [you]: chat: B");
  expect(done).not.toContain("kb_slate_post done");
  const long = M.slateResolutionText(JSON.stringify({ kind: "answer", re: 1, line: "l".repeat(900), body: "b".repeat(9000), prov: {} }));
  expect(long.length).toBeLessThan(2_300);
  expect(long).toContain("chars cut");
  expect(M.slateWatchEvent(JSON.stringify({ seq: 5, kind: "done", re: 3, abandoned: "x", prov: {} }), SID).abandoned).toBe(true);
  expect(M.slateWatchEvent(JSON.stringify({ seq: 5, kind: "done", re: 3, prov: {} }), SID).abandoned).toBeUndefined();
});

test("a flag the CLI does not accept for this kind is REFUSED, never dropped", () => {
  expect(M.slatePostArgs({ kind: "bogus", line: "x" }).error).toContain("must be one of");
  expect(M.slatePostArgs({ kind: "ask", line: "" }).error).toContain("`line` is required");
  expect(M.slatePostArgs({ kind: "take", line: "x" }).error).toContain("`subject`");
  expect(M.slatePostArgs({ kind: "done", line: "x" }).error).toContain("`post`");
  expect(M.slatePostArgs({ kind: "found", line: "x", anyway: true }).error).toContain("take/drop/edit");
  expect(M.slatePostArgs({ kind: "found", line: "x", was: 2 }).error).toContain("`tried` only");
  expect(M.slatePostArgs({ kind: "drop", post: 2, line: "x", over: 2 }).error).toContain("`take` only");
});
TS

cat >>"$TMPROOT/omp-slate.test.ts" <<'TS'

test("a watch line from THIS session is never an event", () => {
  const foreign = '{"seq":11,"kind":"ask","prov":{"session_id":"other"}}';
  expect(M.slateWatchEvent(foreign, SID)).toEqual({ seq: 11, kind: "ask", session: "other" });
  expect(M.slateWatchEvent('{"seq":12,"kind":"found","prov":{"session_id":"' + SID + '"}}', SID)).toBeNull();
  expect(M.slateWatchEvent("[kb slate watch] kb from #3", SID)).toBeNull();
  expect(M.slateWatchEvent("", SID)).toBeNull();
  expect(M.slateWatchEvent('{"kind":"ask"}', SID)).toBeNull();
});

test("the push cadence debounces 2 s and delivers at most once per 30 s", () => {
  const idle = { pendingSince: null, lastEventAt: 0, lastDeliveredAt: 0 };
  expect(M.slatePushPlan(idle, 10_000)).toEqual({ action: "idle", waitMs: 0 });

  // An event 500 ms old still coalesces: wait out the rest of the window.
  expect(M.slatePushPlan({ pendingSince: 9_500, lastEventAt: 9_500, lastDeliveredAt: 0 }, 10_000))
    .toEqual({ action: "wait", waitMs: 1_500 });
  // Quiet for the whole window and never delivered → go.
  expect(M.slatePushPlan({ pendingSince: 5_000, lastEventAt: 7_000, lastDeliveredAt: 0 }, 10_000))
    .toEqual({ action: "deliver", waitMs: 0 });
  // Quiet, but a delivery 10 s ago → wait out the remaining 20 s.
  expect(M.slatePushPlan({ pendingSince: 5_000, lastEventAt: 7_000, lastDeliveredAt: 100_000 }, 110_000))
    .toEqual({ action: "wait", waitMs: 20_000 });
  // …and past the floor, go.
  expect(M.slatePushPlan({ pendingSince: 5_000, lastEventAt: 7_000, lastDeliveredAt: 100_000 }, 131_000))
    .toEqual({ action: "deliver", waitMs: 0 });
  expect(M.SLATE_PUSH_KINDS).toBe("now,warn,hand,ask,answer");
});

test("every slate verb is registered as a tool", () => {
  const { tools } = makePi();
  for (const n of [
    "kb_slate_open", "kb_slate_delta", "kb_slate_post",
    "kb_slate_show", "kb_slate_history", "kb_slate_stats", "kb_slate_ls",
  ]) {
    expect(tools.has(n)).toBe(true);
  }
});

test("kb_slate_post shells the mapped argv plus --harness/--cwd/--session-id", async () => {
  const { tools } = makePi();
  const out = await tools
    .get("kb_slate_post")
    .execute("t", { kind: "found", line: "the cap is 32 KiB", refs: ["path:a.rs:1"] }, null, null, ctx);
  const text = out.content[0].text;
  expect(out.isError).toBeFalsy();
  for (const a of ["ARG:slate", "ARG:found", "ARG:the cap is 32 KiB", "ARG:--ref",
    "ARG:path:a.rs:1", "ARG:--harness", "ARG:omp", "ARG:--cwd", `ARG:${CWD}`,
    "ARG:--session-id", `ARG:${SID}`]) {
    expect(text).toContain(a);
  }
});

test("exit 3 is surfaced as a REFUSAL that quotes the holder line", async () => {
  const { tools } = makePi();
  process.env.KB_FAKE_REFUSE = "1";
  const out = await tools
    .get("kb_slate_post")
    .execute("t", { kind: "take", subject: "a.rs", line: "claiming" }, null, null, ctx);
  delete process.env.KB_FAKE_REFUSE;
  expect(out.isError).toBe(true);
  const text = out.content[0].text;
  expect(text).toContain("REFUSED");
  expect(text).toContain("slate-taken");
  expect(text).toContain("holder: #7 claude/ab12 (live, 3m)");
  expect(text).toContain("anyway");
});

test("session_start exports KB_SESSION_ID/KB_HARNESS for the MAIN agent so shell kb writes are attributed", async () => {
  delete process.env.KB_SESSION_ID;
  delete process.env.KB_HARNESS;
  const { handlers } = makePi();
  await handlers.get("session_start")({}, ctx);
  expect(process.env.KB_SESSION_ID).toBe(SID);
  expect(process.env.KB_HARNESS).toBe("omp");
  await handlers.get("session_shutdown")({}, ctx);
});

test(
  "session_start pushes ONE foreign post, and /kb-slate prints the counters",
  async () => {
    const { handlers, commands, sent } = makePi();
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline && sent.length === 0) await Bun.sleep(200);
    expect(sent.length).toBe(1);
    expect(sent[0].customType).toBe("kb-slate");
    expect(sent[0].display).toBe(true);
    expect(sent[0].attribution).toBe("agent");
    expect(sent[0].content).toContain("where does X live?");

    await commands.get("kb-slate").handler("", ctx);
    const stats = sent[sent.length - 1].content as string;
    expect(stats).toMatch(/push: watching|push: reconnecting/);
    expect(stats).toMatch(/events seen [1-9]/);
    expect(stats).toContain("delivered 1");

    await handlers.get("session_shutdown")({}, ctx);
    await commands.get("kb-slate").handler("", ctx);
    expect(sent[sent.length - 1].content).toContain("push: not running for this session.");
  },
  40_000,
);
TS

cat >"$TMPROOT/omp-slate-wake.test.ts" <<'TS'
import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const M: any = await import(process.env.KB_OMP_TS!);
const CWD = process.env.KB_TEST_CWD!;
const LOG = process.env.KB_FAKE_LOG!;

const mkPost = (o: any) => JSON.stringify(o);
const op = { harness: "claude", origin: "human", session_id: "op-web" };
const agent = { harness: "claude", session_id: "other-agent-0001" };
const show = (seq: number, prov: any, extra: any = {}, thread: any[] = []) =>
  JSON.stringify({ post: { seq, kind: "ask", line: "q", prov, ...extra }, refs: [], thread });

/** Write a scenario (watch script lines + `show` fixtures) and point the fake kb at it. */
function scenario(watch: string[], shows: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), "kb-omp-wake-"));
  writeFileSync(join(dir, "watch.txt"), watch.join("\n") + "\n");
  for (const [k, v] of Object.entries(shows)) writeFileSync(join(dir, k), v);
  process.env.KB_FAKE_WATCH_MODE = "file";
  process.env.KB_FAKE_WATCH_FILE = join(dir, "watch.txt");
  process.env.KB_FAKE_SHOW_DIR = dir;
  return dir;
}

const calls = (re: RegExp) => readFileSync(LOG, "utf8").split("\n").filter((l) => re.test(l)).length;

function boot(initialSid: string) {
  let sid = initialSid;
  process.env.KB_FAKE_OWN_SID = sid;
  const ctx: any = {
    cwd: CWD,
    hasUI: false,
    sessionManager: { getSessionId: () => sid, getCwd: () => CWD, getSessionFile: () => "" },
  };
  const chain: any = {};
  chain.optional = () => chain;
  chain.describe = () => chain;
  const z: any = {
    object: (o: any) => ({ ...chain, shape: o }),
    string: () => chain, number: () => chain, boolean: () => chain, array: () => chain,
  };
  const handlers = new Map<string, any>();
  const commands = new Map<string, any>();
  const sent: { m: any; opts: any; at: number }[] = [];
  M.default({
    zod: z,
    on: (e: string, h: any) => handlers.set(e, h),
    registerTool: () => {},
    registerCommand: (n: string, o: any) => commands.set(n, o),
    registerFlag: () => {},
    getFlag: () => undefined,
    appendEntry: () => {},
    sendMessage: (m: any, opts: any) => sent.push({ m, opts, at: Date.now() }),
    sendUserMessage: () => {},
  });
  const setSid = (s: string) => { sid = s; };
  const wakes = () => sent.filter((s) => s.opts?.triggerTurn === true);
  const stats = async () => {
    const before = sent.length;
    await commands.get("kb-slate").handler("", ctx);
    return String(sent[before]?.m?.content ?? "");
  };
  return { ctx, handlers, sent, setSid, wakes, stats };
}

test(
  "own-ask answers and dones wake with triggerTurn at once; others' answers keep the cadence; an ask to: you queues nothing",
  async () => {
    const A = "omp-session-wake";
    scenario(
      [
        mkPost({ seq: 20, kind: "ask", line: "for the operator?", to: "you", prov: agent }),
        "SLEEP 3",
        mkPost({ seq: 21, kind: "answer", re: 40, line: "go with B", body: "because C", prov: op }),
        mkPost({ seq: 22, kind: "answer", re: 41, line: "someone elses", prov: agent }),
        mkPost({ seq: 23, kind: "done", re: 40, line: "chat: B", prov: op }),
      ],
      { "40.json": show(40, { harness: "omp", session_id: A }), "41.json": show(41, agent) },
    );
    const { ctx, handlers, sent, wakes } = boot(A);
    const t0 = Date.now();
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 25_000;
    while (Date.now() < deadline && sent.length < 3) await Bun.sleep(100);
    await handlers.get("session_shutdown")({}, ctx);

    const w = wakes();
    const plain = sent.filter((s) => !s.opts);
    expect(w.length).toBe(2);
    expect(w[0].m.customType).toBe("kb-slate");
    expect(w[0].m.content).toContain("Your slate ask #40 was answered by [you]: go with B");
    expect(w[0].m.content).toContain("operator's answer to your question");
    expect(w[0].m.content).toContain("kb_slate_post done #40");
    // A done means the ask is already closed: no close instruction.
    expect(w[1].m.content).toContain("Your slate ask #40 was closed by [you]: chat: B");
    expect(w[1].m.content).not.toContain("kb_slate_post done");
    expect(w[1].at - w[0].at).toBeLessThan(5_000);
    // Someone else's answer (#41) still rides the ordinary path: no options, debounced >= 2 s.
    expect(plain.length).toBe(1);
    // The `to: you` ask arrived 3 s BEFORE the resolutions; had it queued a
    // push, a plain delivery would have landed at ~2 s, ahead of every wake.
    expect(sent[0].opts?.triggerTurn).toBe(true);
    expect(plain[0].at).toBeGreaterThan(w[0].at);
    expect(plain[0].at - t0).toBeGreaterThanOrEqual(4_900);
  },
  60_000,
);

test(
  "asks addressed to you or claude never start the push cadence",
  async () => {
    process.env.KB_FAKE_WATCH_MODE = "elsewhere";
    delete process.env.KB_FAKE_SHOW_DIR;
    const { ctx, handlers, sent } = boot("omp-session-elsewhere");
    await handlers.get("session_start")({}, ctx);
    await Bun.sleep(6_000);
    await handlers.get("session_shutdown")({}, ctx);
    expect(sent.length).toBe(0);
  },
  30_000,
);

test(
  "session switch stops the old pusher: its asks wake nobody, the new session gets its own pusher",
  async () => {
    const A = "omp-sw-A", B = "omp-sw-B";
    scenario(
      ["SLEEP 2.5", mkPost({ seq: 30, kind: "answer", re: 40, line: "late", prov: op })],
      { "40.json": show(40, { harness: "omp", session_id: A }) },
    );
    const { ctx, handlers, setSid, sent, wakes } = boot(A);
    await handlers.get("session_start")({}, ctx);
    expect(typeof handlers.get("session_switch")).toBe("function");
    expect(typeof handlers.get("session_branch")).toBe("function");
    await Bun.sleep(300);
    setSid(B);
    await handlers.get("session_switch")({ reason: "new" }, ctx);
    await Bun.sleep(5_500);
    await handlers.get("session_shutdown")({}, ctx);
    expect(wakes().length).toBe(0);
    expect(sent.some((s) => String(s.m.content).includes("Your slate ask"))).toBe(false);
    const log = readFileSync(LOG, "utf8");
    expect(log).toContain(`--session-id ${A}`);
    expect(log).toContain(`slate watch --json --harness omp --cwd ${CWD} --session-id ${B}`);
  },
  30_000,
);

test(
  "a lookup that resolves after the session changed (or shut down) sends nothing",
  async () => {
    const A = "omp-live-A";
    const dir = scenario(
      [mkPost({ seq: 31, kind: "answer", re: 40, line: "late", prov: op })],
      { "40.json": show(40, { harness: "omp", session_id: A }), delay: "1.5" },
    );
    // (a) the live session id changes while `show` is in flight, with no handler call at all.
    let b = boot(A);
    await b.handlers.get("session_start")({}, b.ctx);
    await Bun.sleep(600);
    b.setSid("omp-live-B");
    await Bun.sleep(3_500);
    expect(b.wakes().length).toBe(0);
    b.setSid(A);
    await b.handlers.get("session_shutdown")({}, b.ctx);
    // (b) shutdown while `show` is in flight.
    b = boot(A);
    await b.handlers.get("session_start")({}, b.ctx);
    await Bun.sleep(600);
    await b.handlers.get("session_shutdown")({}, b.ctx);
    await Bun.sleep(3_500);
    expect(b.wakes().length).toBe(0);
    // control: with the delay and no interference the same scenario DOES wake.
    b = boot(A);
    await b.handlers.get("session_start")({}, b.ctx);
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && b.wakes().length === 0) await Bun.sleep(100);
    await b.handlers.get("session_shutdown")({}, b.ctx);
    expect(b.wakes().length).toBe(1);
    void dir;
  },
  40_000,
);

test(
  "ownership follows the supersedes chain to its ROOT author (operator rewrite, older version, foreign ask rewritten by us)",
  async () => {
    const A = "omp-chain-A";
    const mine = { harness: "omp", session_id: A };
    scenario(
      [
        // operator rewrote omp's #50 into #51 and answered #51
        mkPost({ seq: 52, kind: "answer", re: 51, line: "B", prov: op }),
        // an answer to the OLDER version #50 itself
        mkPost({ seq: 53, kind: "answer", re: 50, line: "B again", prov: op }),
        // omp rewrote a FOREIGN ask #60 into #61 and someone answered #61
        mkPost({ seq: 54, kind: "answer", re: 61, line: "not for us", prov: op }),
      ],
      {
        "50.json": show(50, mine),
        "51.json": show(51, op, { supersedes: 50 }),
        "60.json": show(60, agent),
        "61.json": show(61, mine, { supersedes: 60 }),
      },
    );
    const { ctx, handlers, wakes, sent } = boot(A);
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && wakes().length < 2) await Bun.sleep(100);
    await Bun.sleep(1_500);
    await handlers.get("session_shutdown")({}, ctx);
    const w = wakes();
    expect(w.length).toBe(2);
    // the two lookups race (the chain walk takes an extra show), so order is not asserted
    const texts = w.map((x) => String(x.m.content)).join("\n");
    expect(texts).toContain("Your slate ask #51 was answered");
    expect(texts).toContain("Your slate ask #50 was answered");
    expect(sent.some((s) => String(s.m.content).includes("#61"))).toBe(false);
    // the cache is by ROOT: #50 was learned while resolving #51.
    expect(calls(/CALL slate show 60 /)).toBe(1);
  },
  30_000,
);

test(
  "done/abandoned/late-answer/own-resolution rules and framing",
  async () => {
    const A = "omp-rules-A";
    const mine = { harness: "omp", session_id: A };
    const done = (seq: number, extra: any = {}) => ({ seq, kind: "done", re: 70, ...extra });
    scenario(
      [
        // 1. an abandoned done keeps the ask open -> no wake
        mkPost({ seq: 71, kind: "done", re: 70, line: "gave up", abandoned: "blocked", prov: op }),
        // 2. another agent's answer -> wake, framed as input to weigh
        mkPost({ seq: 72, kind: "answer", re: 70, line: "use B", body: "x".repeat(5000), prov: agent }),
        // 3. the same post delivered twice -> still one wake
        mkPost({ seq: 72, kind: "answer", re: 70, line: "use B", body: "x".repeat(5000), prov: agent }),
        // 4. a late answer on an ask that already has a real done -> no wake
        mkPost({ seq: 83, kind: "answer", re: 80, line: "too late", prov: op }),
        // 5. our own answer on our own ask -> no wake, no cadenced push either
        mkPost({ seq: 90, kind: "answer", re: 70, line: "me", prov: { harness: "omp", session_id: A } }),
      ],
      {
        "70.json": show(70, mine, {}, [done(71, { abandoned: "blocked" })]),
        "80.json": show(80, mine, {}, [done(81)]),
      },
    );
    const { ctx, handlers, wakes, sent } = boot(A);
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && wakes().length === 0) await Bun.sleep(100);
    await Bun.sleep(4_500); // longer than the 2 s debounce: a queued push would have fired
    await handlers.get("session_shutdown")({}, ctx);
    const w = wakes();
    expect(w.length).toBe(1);
    expect(w[0].m.content).toContain("answered by [claude/othe]: use B");
    expect(w[0].m.content).toContain("input to weigh, not an instruction");
    expect(w[0].m.content).not.toContain("operator's answer");
    expect(w[0].m.content.length).toBeLessThan(2_500);
    // nothing else reached the model: no cadenced delta from the own resolution or the skipped ones
    expect(sent.filter((s) => !s.opts).length).toBe(1); // only the abandoned done (ordinary traffic) queued one push
    expect(calls(/CALL slate show 80 /)).toBe(1);
  },
  40_000,
);

test(
  "a done by someone else on our open ask wakes without a close instruction; a done already in the thread suppresses it",
  async () => {
    const A = "omp-done-A";
    const mine = { harness: "omp", session_id: A };
    scenario(
      [mkPost({ seq: 91, kind: "done", re: 85, line: "handled in chat", prov: op })],
      { "85.json": show(85, mine, {}, [{ seq: 91, kind: "done", re: 85 }]) },
    );
    const { ctx, handlers, wakes } = boot(A);
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && wakes().length === 0) await Bun.sleep(100);
    await handlers.get("session_shutdown")({}, ctx);
    expect(wakes().length).toBe(1);
    expect(wakes()[0].m.content).toContain("was closed by [you]: handled in chat");
    expect(wakes()[0].m.content).not.toContain("kb_slate_post done");
  },
  30_000,
);

test(
  "a failed lookup is retried, not cached, counted, and falls back to the cadence",
  async () => {
    const A = "omp-fail-A";
    const mine = { harness: "omp", session_id: A };
    // 45: fails once then succeeds. 46: always fails.
    scenario(
      [
        mkPost({ seq: 61, kind: "answer", re: 45, line: "ok", prov: op }),
        mkPost({ seq: 62, kind: "answer", re: 46, line: "x", prov: op }),
        "SLEEP 4",
        mkPost({ seq: 63, kind: "answer", re: 46, line: "y", prov: op }),
      ],
      { "45.json": show(45, mine), "46.json": show(46, mine), "fail-45": "1", "fail-46": "99" },
    );
    const { ctx, handlers, wakes, sent, stats } = boot(A);
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline && calls(/CALL slate show 46 /) < 6) await Bun.sleep(100);
    await Bun.sleep(3_000);
    expect(wakes().length).toBe(1);
    expect(wakes()[0].m.content).toContain("#45");
    expect(calls(/CALL slate show 45 /)).toBe(2); // one failure, one retry
    expect(calls(/CALL slate show 46 /)).toBe(6); // 3 attempts x 2 events: the failure was not cached
    // fell back to the cadenced delta without triggerTurn
    expect(sent.filter((s) => !s.opts).length).toBeGreaterThanOrEqual(1);
    expect(await stats()).toMatch(/failures 2\b/);
    await handlers.get("session_shutdown")({}, ctx);
  },
  60_000,
);

test(
  "KB_SLATE_PUSH=0 blocks wakes, also when set mid-session",
  async () => {
    const A = "omp-off-A";
    scenario(
      ["SLEEP 2", mkPost({ seq: 64, kind: "answer", re: 40, line: "x", prov: op })],
      { "40.json": show(40, { harness: "omp", session_id: A }) },
    );
    const { ctx, handlers, sent } = boot(A);
    await handlers.get("session_start")({}, ctx);
    process.env.KB_SLATE_PUSH = "0";
    try {
      await Bun.sleep(5_000);
    } finally {
      delete process.env.KB_SLATE_PUSH;
    }
    await handlers.get("session_shutdown")({}, ctx);
    expect(sent.length).toBe(0);
  },
  30_000,
);

test(
  "a seq that goes backwards (ledger purge) clears the seq-keyed caches",
  async () => {
    const A = "omp-purge-A";
    scenario(
      [
        mkPost({ seq: 100, kind: "answer", re: 40, line: "first life", prov: op }),
        "SLEEP 1",
        mkPost({ seq: 3, kind: "found", line: "after the purge", prov: agent }),
        mkPost({ seq: 100, kind: "answer", re: 40, line: "second life", prov: op }),
      ],
      { "40.json": show(40, { harness: "omp", session_id: A }) },
    );
    const { ctx, handlers, wakes } = boot(A);
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline && wakes().length < 2) await Bun.sleep(100);
    await handlers.get("session_shutdown")({}, ctx);
    expect(wakes().length).toBe(2);
    expect(wakes()[1].m.content).toContain("second life");
  },
  30_000,
);
TS

# The `--kinds` fallback lives in a SECOND process: `kindsFlagOk` is probed
# once per process, so a CLI that rejects the flag has to be met with a fresh
# module load (this is also exactly how it behaves in a real session).
cat >"$TMPROOT/omp-slate-nokinds.test.ts" <<'TS'
import { expect, test } from "bun:test";

const M: any = await import(process.env.KB_OMP_TS!);
const SID = "omp-session-nokinds";
const CWD = process.env.KB_TEST_CWD!;
process.env.KB_FAKE_OWN_SID = SID;
const ctx: any = {
  cwd: CWD,
  hasUI: false,
  sessionManager: { getSessionId: () => SID, getCwd: () => CWD, getSessionFile: () => "" },
};

test(
  "an older kb that rejects delta --kinds is retried once without it",
  async () => {
    const chain: any = {};
    chain.optional = () => chain;
    chain.describe = () => chain;
    const z: any = {
      object: (o: any) => ({ ...chain, shape: o }),
      string: () => chain, number: () => chain, boolean: () => chain, array: () => chain,
    };
    const handlers = new Map<string, any>();
    const sent: any[] = [];
    M.default({
      zod: z,
      on: (e: string, h: any) => handlers.set(e, h),
      registerTool: () => {},
      registerCommand: () => {},
      registerFlag: () => {},
      getFlag: () => undefined,
      appendEntry: () => {},
      sendMessage: (m: any) => sent.push(m),
      sendUserMessage: () => {},
    });
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline && sent.length === 0) await Bun.sleep(200);
    expect(sent.length).toBe(1);
    expect(sent[0].content).toContain("where does X live?");
    await handlers.get("session_shutdown")({}, ctx);
  },
  40_000,
);
TS

run_bun_test() {
  # HOME → the temp root so `whichBin` cannot find the operator's real
  # ~/.local/bin/kb; PATH → the fake first.
  ( cd "$TMPROOT" && HOME="$TMPROOT" PATH="$TMPROOT/bin:$PATH" \
      KB_OMP_TS="$OMP_TS" bun test "$1" ) >"$2" 2>&1
}

if run_bun_test "$TMPROOT/omp-slate.test.ts" "$TMPROOT/bun.log"; then
  ok "bun test: tool argv mapping, refusal wording, watch filter, push cadence"
else
  bad "bun test failed"
  sed -n '1,80p' "$TMPROOT/bun.log"
fi
grep -E '^\s*[0-9]+ (pass|fail)' "$TMPROOT/bun.log" | sed 's/^/        /'

if grep -q 'CALL slate watch --json --harness omp --cwd' "$KB_FAKE_LOG"; then
  ok "the push child is spawned as \`kb slate watch --json\` with --cwd/--session-id"
else
  bad "no kb slate watch call recorded"
fi
if grep -q 'CALL slate delta --json --harness omp --budget 1500 .* --kinds now,warn,hand,ask,answer' "$KB_FAKE_LOG"; then
  ok "the delta fetch asks for D26's hybrid subset (--kinds now,warn,hand,ask,answer)"
else
  bad "delta was not fetched with --kinds now,warn,hand,ask,answer"
  grep 'CALL slate delta' "$KB_FAKE_LOG" | head -3
fi

: >"$KB_FAKE_LOG"
if run_bun_test "$TMPROOT/omp-slate-wake.test.ts" "$TMPROOT/bun-wake.log"; then
  ok "bun test: own-ask wake (triggerTurn, no cadence), others keep the cadence, to:you not pushed"
else
  bad "bun test (own-ask wake) failed"
  sed -n '1,80p' "$TMPROOT/bun-wake.log"
fi
grep -E '^\s*[0-9]+ (pass|fail)' "$TMPROOT/bun-wake.log" | sed 's/^/        /'
if grep -q 'CALL slate show 40 --json --harness omp' "$KB_FAKE_LOG"; then
  ok "own-ask detection resolves the ask with \`kb slate show <re> --json\`"
else
  bad "no kb slate show call recorded"
fi

: >"$KB_FAKE_LOG"
export KB_FAKE_NO_KINDS=1
if run_bun_test "$TMPROOT/omp-slate-nokinds.test.ts" "$TMPROOT/bun-nokinds.log"; then
  ok "bun test: the --kinds probe falls back on an older CLI"
else
  bad "bun test (--kinds fallback) failed"
  sed -n '1,60p' "$TMPROOT/bun-nokinds.log"
fi
unset KB_FAKE_NO_KINDS
if grep -q -- '--kinds' "$KB_FAKE_LOG" &&
   grep 'CALL slate delta' "$KB_FAKE_LOG" | grep -qv -- '--kinds'; then
  ok "the fallback retries the SAME delta without --kinds (probed once)"
else
  bad "no --kinds-less retry recorded"
  grep 'CALL slate delta' "$KB_FAKE_LOG" | head -4
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
