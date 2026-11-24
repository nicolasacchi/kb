// PHIN0 - the input proxy's pure parts: the diff, the pref and the state machine.
// Node environment: the core never touches the DOM.

import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_VARIANT,
  ENTER_PAIR_MS,
  PHONE_DIRECT_KEY,
  PHONE_VARIANT_KEY,
  SENTINEL,
  attrsFor,
  createProxyCore,
  diffValue,
  encodeDiff,
  escapeCtl,
  phoneDirectEnabled,
  readVariant,
  writeVariant,
  type DebugEvent,
  type ProxyVariant,
} from "./inputProxy";

describe("diffValue / encodeDiff golden table", () => {
  const rows: Array<[string, string, string, number, string]> = [
    // [name, prev, next, del, ins]
    ["append", "hel", "hello", 0, "lo"],
    ["from empty", "", "a", 0, "a"],
    ["GBoard word replace helo -> hello", "helo", "hello", 1, "lo"],
    ["GBoard word replace wor -> world", "wor", "world", 0, "ld"],
    ["GBoard replace worl -> world keeps the prefix", "worl", "world", 0, "d"],
    ["backspace", "hello", "hell", 1, ""],
    ["clear", "abc", "", 3, ""],
    ["empty to emoji", "", "😀", 0, "😀"],
    ["deleting an emoji is ONE delete, not two", "😀", "", 1, ""],
    ["emoji after text", "a😀", "a", 1, ""],
    ["two different emoji never split a surrogate pair", "😀", "😁", 1, "😁"],
    ["precomposed e-acute vs e", "café", "cafe", 1, "e"],
    ["decomposed e + accent is two code points", "café", "café", 2, "é"],
    ["CJK replace", "你好", "你们", 1, "们"],
    ["CJK append", "你", "你好", 0, "好"],
    ["no change", "same", "same", 0, ""],
  ];
  for (const [name, prev, next, del, ins] of rows) {
    it(name, () => {
      expect(diffValue(prev, next)).toEqual({ del, ins });
    });
  }

  it("encodes DEL then the text, newlines as Enter", () => {
    expect(encodeDiff({ del: 0, ins: "a\nb" })).toBe("a\rb");
    expect(encodeDiff({ del: 0, ins: "a\r\nb" })).toBe("a\rb");
    expect(encodeDiff({ del: 2, ins: "xy" })).toBe("\x7f\x7fxy");
    expect(encodeDiff(diffValue("😀", ""))).toBe("\x7f");
    expect(encodeDiff(diffValue("helo", "hello"))).toBe("\x7flo");
  });

  it("escapes controls for display and never leaves a raw one", () => {
    const s = escapeCtl("a\r\n\t\x1b\x7f\x03\u200b\\");
    expect(s).toBe("a\\r\\n\\t\\e\\x7f\\x03\\u200b\\\\");
    // eslint-disable-next-line no-control-regex
    expect(/[\x00-\x1f\x7f]/.test(s)).toBe(false);
  });
});

function fakeStorage(init: Record<string, string> = {}, opts: { throws?: boolean } = {}) {
  const m = new Map(Object.entries(init));
  const calls: string[] = [];
  const guard = (): void => {
    if (opts.throws) throw new Error("denied");
  };
  return {
    m,
    calls,
    s: {
      getItem: (k: string) => {
        calls.push(`get ${k}`);
        guard();
        return m.get(k) ?? null;
      },
      setItem: (k: string, v: string) => {
        calls.push(`set ${k}`);
        guard();
        m.set(k, v);
      },
      removeItem: (k: string) => {
        calls.push(`remove ${k}`);
        guard();
        m.delete(k);
      },
    },
  };
}

describe("phoneDirectEnabled", () => {
  it("?direct=1 is on and remembered", () => {
    const f = fakeStorage();
    expect(phoneDirectEnabled("?direct=1", f.s)).toBe(true);
    expect(f.m.get(PHONE_DIRECT_KEY)).toBe("1");
  });
  it("?direct=0 is off and forgets", () => {
    const f = fakeStorage({ [PHONE_DIRECT_KEY]: "1" });
    expect(phoneDirectEnabled("?direct=0", f.s)).toBe(false);
    expect(f.m.has(PHONE_DIRECT_KEY)).toBe(false);
  });
  it("without a query it reads the remembered value", () => {
    expect(phoneDirectEnabled("", fakeStorage({ [PHONE_DIRECT_KEY]: "1" }).s)).toBe(true);
    expect(phoneDirectEnabled("?x=1", fakeStorage().s)).toBe(false);
    expect(phoneDirectEnabled("?direct=maybe", fakeStorage({ [PHONE_DIRECT_KEY]: "1" }).s)).toBe(true);
  });
  it("off writes nothing: the flag-off page touches no storage", () => {
    const f = fakeStorage();
    expect(phoneDirectEnabled("", f.s)).toBe(false);
    expect(f.calls.every((c) => c.startsWith("get"))).toBe(true);
  });
  it("a null or throwing storage never turns the remembered pref on", () => {
    expect(phoneDirectEnabled("", null)).toBe(false);
    expect(phoneDirectEnabled("", fakeStorage({ [PHONE_DIRECT_KEY]: "1" }, { throws: true }).s)).toBe(false);
    expect(phoneDirectEnabled("?direct=0", fakeStorage({}, { throws: true }).s)).toBe(false);
  });
  it("an explicit ?direct=1 still enables this page when the storage cannot remember it", () => {
    expect(phoneDirectEnabled("?direct=1", null)).toBe(true);
    expect(phoneDirectEnabled("?direct=1", fakeStorage({}, { throws: true }).s)).toBe(true);
  });
});

describe("variant persistence", () => {
  it("round-trips and falls back to the default on garbage", () => {
    const f = fakeStorage();
    const v: ProxyVariant = { send: "deferred", sentinel: "zwsp", attrs: "input-url" };
    writeVariant(f.s, v);
    expect(readVariant(f.s)).toEqual(v);
    f.m.set(PHONE_VARIANT_KEY, "{not json");
    expect(readVariant(f.s)).toEqual(DEFAULT_VARIANT);
    f.m.set(PHONE_VARIANT_KEY, JSON.stringify({ send: "x", sentinel: "none", attrs: "textarea" }));
    expect(readVariant(f.s)).toEqual(DEFAULT_VARIANT);
    expect(readVariant(null)).toEqual(DEFAULT_VARIANT);
  });
  it("attribute sets: every set carries the no-assist attributes; nofill adds the manager opt-outs", () => {
    for (const k of ["textarea", "input-text", "input-url", "textarea-nofill"] as const) {
      const { attrs } = attrsFor(k);
      expect(attrs).toMatchObject({
        autocapitalize: "off",
        autocorrect: "off",
        spellcheck: "false",
        autocomplete: "off",
        enterkeyhint: "enter",
        inputmode: "text",
      });
    }
    expect(attrsFor("input-url")).toMatchObject({ tag: "input", attrs: { type: "url" } });
    expect(attrsFor("input-text")).toMatchObject({ tag: "input", attrs: { type: "text" } });
    expect(attrsFor("textarea").attrs.name).toBeUndefined();
    expect(attrsFor("textarea-nofill").attrs).toMatchObject({ name: "kb-term-proxy", "data-lpignore": "true", "data-1p-ignore": "true" });
  });
});

/** A core wired to a fake element value and a recording socket. */
function rig(variant: ProxyVariant = DEFAULT_VARIANT, opts: { accept?: () => boolean } = {}) {
  const sent: string[] = [];
  const log: DebugEvent[] = [];
  const sets: string[] = [];
  let clock = 0;
  const core = createProxyCore({
    send: (d) => {
      if (opts.accept && !opts.accept()) return false;
      sent.push(d);
      return true;
    },
    variant,
    log: (e) => log.push(e),
    setValue: (v) => sets.push(v),
    now: () => clock,
  });
  const input = (value: string, inputType = "insertText", isComposing = false) =>
    core.onInput({ value, inputType, data: null, isComposing });
  const key = (k: string, extra: Partial<{ keyCode: number; ctrlKey: boolean; isComposing: boolean }> = {}) =>
    core.onKeyDown({ key: k, keyCode: 0, ctrlKey: false, altKey: false, metaKey: false, isComposing: false, ...extra });
  return { core, sent, log, sets, input, key, tick: (ms: number) => (clock += ms) };
}

describe("core: every vs deferred across a composition", () => {
  const compose = (r: ReturnType<typeof rig>) => {
    r.core.onCompositionStart();
    r.input("h", "insertCompositionText", true);
    r.input("he", "insertCompositionText", true);
    r.input("hel", "insertCompositionText", true);
    const during = [...r.sent];
    r.core.onCompositionEnd({ data: "hel", value: "hel" });
    return during;
  };

  it("every: each interim goes out as it is typed, nothing doubles at compositionend", () => {
    const r = rig({ send: "every", sentinel: "none", attrs: "textarea" });
    expect(compose(r)).toEqual(["h", "e", "l"]);
    expect(r.sent.join("")).toBe("hel");
    // the element is cleared only once the composition is over
    expect(r.sets).toEqual([""]);
  });

  it("deferred: nothing goes out until compositionend, then exactly one diff", () => {
    const r = rig({ send: "deferred", sentinel: "none", attrs: "textarea" });
    expect(compose(r)).toEqual([]);
    expect(r.sent).toEqual(["hel"]);
    expect(r.log.filter((e) => e.bytes === "interim")).toHaveLength(3);
  });

  it("a GBoard word swap in every mode is DEL + text and the terminal ends with the word", () => {
    const r = rig();
    r.core.onCompositionStart();
    r.input("helo", "insertCompositionText", true);
    r.input("hello", "insertReplacementText", true);
    expect(r.sent).toEqual(["helo", "\x7flo"]);
    r.core.onCompositionEnd({ data: "hello", value: "hello" });
    expect(r.sent).toHaveLength(2);
  });

  it("a non-composing input is diffed and the element is reset after the send", () => {
    const r = rig();
    r.input("a");
    expect(r.sent).toEqual(["a"]);
    expect(r.sets).toEqual([""]);
    r.input("b");
    expect(r.sent).toEqual(["a", "b"]);
  });

  it("none: the value is never reset while composing", () => {
    const r = rig();
    r.core.onCompositionStart();
    r.input("a", "insertCompositionText", true);
    r.input("ab", "insertCompositionText", true);
    expect(r.sets).toEqual([]);
  });
});

describe("core: sentinel", () => {
  const z: ProxyVariant = { send: "every", sentinel: "zwsp", attrs: "textarea" };

  it("starts from two zero-width spaces", () => {
    expect(rig(z).core.initialValue).toBe(SENTINEL);
    expect(rig().core.initialValue).toBe("");
  });

  it("losing a sentinel char sends exactly one DEL and the sentinel is restored", () => {
    const r = rig(z);
    r.input(SENTINEL.slice(1), "deleteContentBackward");
    expect(r.sent).toEqual(["\x7f"]);
    expect(r.sets).toEqual([SENTINEL]);
    r.input(SENTINEL.slice(1), "deleteContentBackward");
    expect(r.sent).toEqual(["\x7f", "\x7f"]);
  });

  it("typed text after the sentinel is sent without the sentinel", () => {
    const r = rig(z);
    r.input(`${SENTINEL}a`);
    expect(r.sent).toEqual(["a"]);
    expect(r.sets).toEqual([SENTINEL]);
  });

  it("the sentinel is never restored inside a composition, only after it", () => {
    const r = rig(z);
    r.core.onCompositionStart();
    r.input(`${SENTINEL}he`, "insertCompositionText", true);
    r.input(SENTINEL.slice(1), "deleteCompositionText", true);
    expect(r.sets).toEqual([]);
    expect(r.sent).toEqual(["he", "\x7f\x7f\x7f"]);
    r.core.onCompositionEnd({ data: "", value: SENTINEL.slice(1) });
    expect(r.sets).toEqual([SENTINEL]);
  });
});

describe("core: Enter and the key table", () => {
  it("beforeinput insertLineBreak is prevented and sends one \\r", () => {
    const r = rig();
    expect(r.core.onBeforeInput({ inputType: "insertLineBreak", data: null, isComposing: false })).toBe(true);
    expect(r.core.onBeforeInput({ inputType: "insertParagraph", data: null, isComposing: false })).toBe(true);
    expect(r.sent).toEqual(["\r", "\r"]);
  });

  it("a keydown Enter plus the beforeinput of the same press sends exactly one \\r", () => {
    const r = rig();
    expect(r.key("Enter", { keyCode: 13 })).toBe(true);
    r.tick(5);
    r.core.onBeforeInput({ inputType: "insertLineBreak", data: null, isComposing: false });
    expect(r.sent).toEqual(["\r"]);
    // the next press, well after, is its own
    r.tick(ENTER_PAIR_MS + 10);
    r.core.onBeforeInput({ inputType: "insertLineBreak", data: null, isComposing: false });
    expect(r.sent).toEqual(["\r", "\r"]);
  });

  it("a keydown 229 (an IME key) is left to the input events and sends nothing", () => {
    const r = rig();
    expect(r.key("Enter", { keyCode: 229 })).toBe(false);
    expect(r.key("Backspace", { isComposing: true })).toBe(false);
    expect(r.sent).toEqual([]);
  });

  it("maps the non-IME keys", () => {
    const r = rig();
    for (const k of ["Backspace", "Tab", "Escape", "ArrowUp", "ArrowDown", "ArrowRight", "ArrowLeft"]) {
      expect(r.key(k, { keyCode: 1 })).toBe(true);
    }
    expect(r.sent).toEqual(["\x7f", "\t", "\x1b", "\x1b[A", "\x1b[B", "\x1b[C", "\x1b[D"]);
    expect(r.key("c", { ctrlKey: true })).toBe(true);
    expect(r.key("Z", { ctrlKey: true })).toBe(true);
    expect(r.sent.slice(-2)).toEqual(["\x03", "\x1a"]);
    expect(r.key("a")).toBe(false);
    expect(r.sent).toHaveLength(9);
  });

  it("Backspace on an empty value is ours to send; with text it is the diff's", () => {
    const r = rig();
    expect(r.core.onBeforeInput({ inputType: "deleteContentBackward", data: null, isComposing: false })).toBe(true);
    expect(r.sent).toEqual(["\x7f"]);
    r.core.onCompositionStart();
    r.input("ab", "insertCompositionText", true);
    // mid-composition the delete belongs to the IME and the diff
    expect(r.core.onBeforeInput({ inputType: "deleteContentBackward", data: null, isComposing: true })).toBe(false);
    r.core.onCompositionEnd({ data: "ab", value: "ab" });
    // settled: the mirror is empty again, so the next delete is the empty case
    expect(r.core.onBeforeInput({ inputType: "deleteContentBackward", data: null, isComposing: false })).toBe(true);
    expect(r.sent).toEqual(["\x7f", "ab", "\x7f"]);
  });
});

describe("core: paste", () => {
  it("sends text only, newlines as Enter", () => {
    const r = rig();
    expect(r.core.onPaste("a\nb\r\nc", false)).toBe(true);
    expect(r.sent).toEqual(["a\rb\rc"]);
  });
  it("wraps in bracketed paste only when the terminal asked for it", () => {
    const r = rig();
    r.core.onPaste("x\ny", true);
    expect(r.sent).toEqual(["\x1b[200~x\ry\x1b[201~"]);
  });
});

describe("core: a refused send", () => {
  it("logs dropped, keeps the mirror, and the next diff resends what was lost", () => {
    let ok = false;
    const r = rig(DEFAULT_VARIANT, { accept: () => ok });
    r.input("a");
    expect(r.sent).toEqual([]);
    expect(r.log.at(-1)?.bytes).toBe("dropped");
    expect(r.sets).toEqual([]);
    ok = true;
    r.input("ab");
    expect(r.sent).toEqual(["ab"]);
  });
});

describe("core: typed text is never persisted", () => {
  it("touches no storage and no console while typing, pasting and composing", () => {
    const getItem = vi.fn();
    const setItem = vi.fn();
    const store = { getItem, setItem, removeItem: vi.fn() };
    vi.stubGlobal("localStorage", store);
    vi.stubGlobal("sessionStorage", store);
    const spies = (["log", "info", "warn", "error", "debug"] as const).map((m) => vi.spyOn(console, m).mockImplementation(() => {}));
    try {
      const r = rig({ send: "every", sentinel: "zwsp", attrs: "textarea" });
      r.core.onCompositionStart();
      r.input(`${SENTINEL}hunter2`, "insertCompositionText", true);
      r.core.onCompositionEnd({ data: "hunter2", value: `${SENTINEL}hunter2` });
      r.core.onPaste("s3cret\n", true);
      r.key("Enter", { keyCode: 13 });
      expect(r.sent.join("")).toContain("hunter2");
    } finally {
      for (const s of spies) {
        expect(s).not.toHaveBeenCalled();
        s.mockRestore();
      }
      vi.unstubAllGlobals();
    }
    expect(getItem).not.toHaveBeenCalled();
    expect(setItem).not.toHaveBeenCalled();
  });

  it("the log holds escaped text only", () => {
    const r = rig();
    r.core.onPaste("a\rb\x1b", false);
    for (const e of r.log) {
      // eslint-disable-next-line no-control-regex
      expect(/[\x00-\x1f\x7f]/.test(`${e.data}${e.bytes}`)).toBe(false);
    }
  });
});
