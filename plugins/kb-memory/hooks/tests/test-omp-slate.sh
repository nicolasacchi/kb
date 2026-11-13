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

test("the wake text names the ask, tags [you], and carries the operator carve-out only for [you] answers", () => {
  const human = M.slateResolutionText(JSON.stringify({ kind: "answer", re: 40, line: "go with B", body: "because C", prov: { origin: "human", harness: "claude", session_id: "op" } }));
  expect(human).toContain("Your slate ask #40 was answered by [you]: go with B");
  expect(human).toContain("  because C");
  expect(human).toContain("operator's answer to that question");
  expect(human).toContain("kb_slate_post done #40");
  const agent = M.slateResolutionText(JSON.stringify({ kind: "answer", re: 40, line: "B", prov: { harness: "claude", session_id: "abcd1234" } }));
  expect(agent).toContain("[claude/abcd]");
  expect(agent).not.toContain("operator's answer");
  expect(M.slateResolutionText(JSON.stringify({ kind: "done", re: 40, line: "chat: B", prov: { origin: "human" } })))
    .toContain("was closed by [you]: chat: B");
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

const M: any = await import(process.env.KB_OMP_TS!);
const CWD = process.env.KB_TEST_CWD!;

function boot(sid: string) {
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
  const sent: { m: any; opts: any; at: number }[] = [];
  M.default({
    zod: z,
    on: (e: string, h: any) => handlers.set(e, h),
    registerTool: () => {},
    registerCommand: () => {},
    registerFlag: () => {},
    getFlag: () => undefined,
    appendEntry: () => {},
    sendMessage: (m: any, opts: any) => sent.push({ m, opts, at: Date.now() }),
    sendUserMessage: () => {},
  });
  return { ctx, handlers, sent };
}

test(
  "own-ask answers and dones wake with triggerTurn at once; others' answers keep the cadence; an ask to: you is not pushed",
  async () => {
    process.env.KB_FAKE_WATCH_MODE = "resolve";
    const { ctx, handlers, sent } = boot("omp-session-wake");
    const t0 = Date.now();
    await handlers.get("session_start")({}, ctx);
    const deadline = Date.now() + 25_000;
    while (Date.now() < deadline && sent.length < 3) await Bun.sleep(100);
    await handlers.get("session_shutdown")({}, ctx);

    const wakes = sent.filter((s) => s.opts?.triggerTurn === true);
    const plain = sent.filter((s) => !s.opts);
    expect(wakes.length).toBe(2);
    expect(wakes[0].m.customType).toBe("kb-slate");
    expect(wakes[0].m.content).toContain("Your slate ask #40 was answered by [you]: go with B");
    expect(wakes[1].m.content).toContain("Your slate ask #40 was closed by [you]: chat: B");
    // Both resolutions delivered inside one rate window: the 30 s floor is bypassed.
    expect(wakes[1].at - wakes[0].at).toBeLessThan(5_000);
    // Someone else's answer (#41) still rides the ordinary path: no options, debounced >= 2 s.
    expect(plain.length).toBe(1);
    expect(plain[0].at - t0).toBeGreaterThanOrEqual(1_900);
    // The ask to: you never produced a message of its own.
    expect(sent.every((s) => !String(s.m.content).includes("for the operator?"))).toBe(true);
  },
  60_000,
);

test(
  "asks addressed to you or claude never start the push cadence",
  async () => {
    process.env.KB_FAKE_WATCH_MODE = "elsewhere";
    const { ctx, handlers, sent } = boot("omp-session-elsewhere");
    await handlers.get("session_start")({}, ctx);
    await Bun.sleep(6_000);
    await handlers.get("session_shutdown")({}, ctx);
    expect(sent.length).toBe(0);
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
if [ "$(grep -c 'CALL slate show 40 ' "$KB_FAKE_LOG")" = "1" ]; then
  ok "the own-ask lookup is cached per ask (show 40 ran once for two resolutions)"
else
  bad "show 40 was not cached"
  grep 'CALL slate show' "$KB_FAKE_LOG"
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
