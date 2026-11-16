// @vitest-environment jsdom
// T18 — the native composer: which calls Send makes, in which order, and what it says.

import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "../../lib/toast";
import { Composer } from "./Composer";
import { UPLOAD_MAX_BYTES, type Drive } from "./driveApi";
import { CAPTION } from "./planSend";
import {
  byId,
  click,
  maybeById,
  mkDrive,
  mount,
  rigFetch,
  settle,
  typeInto,
  type FetchRig,
  type Mounted,
} from "./driveTestkit";

let m: Mounted | null = null;
let rig: FetchRig;

beforeEach(() => {
  rig = rigFetch();
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

interface HostProps {
  drive: Drive;
  bracketPaste?: boolean | null;
  blocked?: boolean;
  bound?: boolean;
  onTyped?: () => void;
}

/** The composer with the caption state a real dock would hold. */
function Host({ drive, bracketPaste = true, blocked = false, bound = true, onTyped }: HostProps) {
  const [caption, setCaption] = useState<string | null>(null);
  return (
    <Composer
      drive={drive}
      bracketPaste={bracketPaste}
      blocked={blocked}
      bound={bound}
      caption={caption}
      onCaption={setCaption}
      onTyped={onTyped}
    />
  );
}

const input = (): HTMLTextAreaElement => byId(m!.host, "kb-drive-input") as HTMLTextAreaElement;
const caption = (): string | null => maybeById(m!.host, "kb-drive-caption")?.textContent ?? null;
const send = async (): Promise<void> => {
  click(byId(m!.host, "kb-drive-send"));
  await settle();
};

describe("Composer", () => {
  it("is a real textarea with the phone IME attributes, not inputmode none", () => {
    m = mount(<Host drive={mkDrive()} />);
    const ta = input();
    expect(ta.tagName).toBe("TEXTAREA");
    expect(ta.getAttribute("enterkeyhint")).toBe("send");
    expect(ta.getAttribute("autocapitalize")).toBe("sentences");
    expect(ta.getAttribute("autocorrect")).toBe("on");
    expect(ta.spellcheck).toBe(true);
    expect(ta.inputMode).not.toBe("none");
  });

  it("(a) multi-line without bracketed paste: zero requests, the refusal caption, the text stays", async () => {
    m = mount(<Host drive={mkDrive()} bracketPaste={false} />);
    typeInto(input(), "line one\nline two");
    await send();
    expect(rig.calls).toHaveLength(0);
    expect(caption()).toBe(CAPTION.refuse);
    expect(input().value).toBe("line one\nline two");
  });

  it("(b) /compact is pasted and never entered", async () => {
    m = mount(<Host drive={mkDrive()} />);
    typeInto(input(), "/compact");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
    expect(rig.calls[0].json?.text).toBe("/compact");
    expect(caption()).toBe(CAPTION.pastedOnly);
    expect(input().value).toBe("");
  });

  it("@file is pasted and never entered either", async () => {
    m = mount(<Host drive={mkDrive()} />);
    typeInto(input(), "@src/main.rs");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
  });

  it("(c) a plain message is pasted, then Enter, in that order", async () => {
    const onTyped = vi.fn();
    m = mount(<Host drive={mkDrive()} onTyped={onTyped} />);
    typeInto(input(), "fix the failing test");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste", "/api/terms/abcd1234/enter"]);
    expect(input().value).toBe("");
    expect(caption()).toBeNull();
    expect(onTyped).toHaveBeenCalledTimes(1);
  });

  it("a blocked session pastes and holds: no Enter, and says to use the choice bar", async () => {
    m = mount(<Host drive={mkDrive()} blocked />);
    typeInto(input(), "go ahead");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
    expect(caption()).toBe(CAPTION.holding);
  });

  it("an unbound terminal pastes but keeps Enter off, and says so", async () => {
    m = mount(<Host drive={mkDrive()} bound={false} />);
    expect(caption()).toBe(CAPTION.unbound);
    typeInto(input(), "hello");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
    expect(caption()).toBe(CAPTION.unbound);
  });

  it("(d) an Enter refusal keeps 'pasted, not sent', clears the text and raises no error toast", async () => {
    const err = vi.spyOn(toast, "err");
    rig.reply(200);
    rig.reply(409, { type: "urn:kb:errors:term-blocked", detail: "the bound session is blocked" });
    m = mount(<Host drive={mkDrive()} />);
    typeInto(input(), "hello");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste", "/api/terms/abcd1234/enter"]);
    expect(caption()).toBe("Pasted, not sent: the bound session is blocked");
    expect(input().value).toBe("");
    expect(err).not.toHaveBeenCalled();
  });

  it("(d) a refused multi-line paste keeps the textarea and shows the refusal", async () => {
    rig.reply(422, { type: "urn:kb:errors:terminal-refused", detail: "multi-line paste refused" });
    m = mount(<Host drive={mkDrive()} bracketPaste={null} />);
    typeInto(input(), "a\nb");
    await send();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
    expect(input().value).toBe("a\nb");
    expect(caption()).toBe(CAPTION.refuse);
  });

  it("any other paste failure is a toast (never swallowed) and keeps the text", async () => {
    const err = vi.spyOn(toast, "err");
    rig.reply(503, { type: "urn:kb:errors:terminal-runner-unavailable", detail: "terminal runner unavailable" });
    m = mount(<Host drive={mkDrive()} />);
    typeInto(input(), "hello");
    await send();
    expect(err).toHaveBeenCalledWith("terminal runner unavailable");
    expect(input().value).toBe("hello");
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
  });

  it("(e) claude `/bg` asks before ANY request; declining sends nothing and keeps the text", async () => {
    const confirm = vi.fn(async () => {
      expect(rig.calls, "the guard must run before the first request").toHaveLength(0);
      return false;
    });
    m = mount(<Host drive={mkDrive({ harness: "claude", confirm })} />);
    typeInto(input(), "/bg");
    await send();
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(rig.calls).toHaveLength(0);
    expect(input().value).toBe("/bg");
  });

  it("(e) claude `/bg` is pasted (never entered) once confirmed", async () => {
    const confirm = vi.fn(async () => true);
    m = mount(<Host drive={mkDrive({ harness: "claude", confirm })} />);
    typeInto(input(), "/bg");
    await send();
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
  });

  it("(f) every request carries the viewer", async () => {
    m = mount(<Host drive={mkDrive({ viewer: "ws-zzzz000001" })} />);
    typeInto(input(), "hello");
    await send();
    expect(rig.calls).toHaveLength(2);
    for (const c of rig.calls) expect(c.json?.viewer).toBe("ws-zzzz000001");
  });

  it("Send and attach never take focus on press (the textarea keeps the soft keyboard)", () => {
    m = mount(<Host drive={mkDrive()} />);
    for (const id of ["kb-drive-send", "kb-drive-attach"]) {
      const ev = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
      byId(m.host, id).dispatchEvent(ev);
      expect(ev.defaultPrevented, id).toBe(true);
    }
  });

  describe("attach a picture", () => {
    const pick = async (file: File): Promise<void> => {
      const el = byId(m!.host, "kb-drive-file") as HTMLInputElement;
      Object.defineProperty(el, "files", { value: [file], configurable: true });
      el.dispatchEvent(new Event("change", { bubbles: true }));
      await settle(8);
    };
    const png = (size: number): File => new File([new Uint8Array(size)], "p.png", { type: "image/png" });
    const SHA = "0123456789abcdef".repeat(4);

    it("has no capture attribute and accepts exactly the four picture types", () => {
      m = mount(<Host drive={mkDrive()} />);
      const el = byId(m.host, "kb-drive-file");
      expect(el.getAttribute("capture")).toBeNull();
      expect(el.getAttribute("accept")).toBe("image/png,image/jpeg,image/gif,image/webp");
    });

    it("uploads once, pastes the returned PATH without Enter and shows a chip", async () => {
      rig.reply(200, { path: "/run/kb-term/s/abcd1234/up/x.png", bytes: 16, mime: "image/png", sha256: SHA });
      m = mount(<Host drive={mkDrive()} />);
      await pick(png(16));
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/upload", "/api/terms/abcd1234/paste"]);
      expect(rig.calls[1].json?.text).toBe("/run/kb-term/s/abcd1234/up/x.png");
      expect(byId(m.host, "kb-drive-chip").textContent).toBe("image-01234567.png");
    });

    it("rejects a 9 MiB picture client-side: no request, a toast", async () => {
      const err = vi.spyOn(toast, "err");
      m = mount(<Host drive={mkDrive()} />);
      await pick(png(9 * 1024 * 1024));
      expect(UPLOAD_MAX_BYTES).toBe(8 * 1024 * 1024);
      expect(rig.calls).toHaveLength(0);
      expect(err).toHaveBeenCalledWith(expect.stringMatching(/8 MiB/));
    });

    it("explains a server refusal with a toast and pastes nothing", async () => {
      const err = vi.spyOn(toast, "err");
      rig.reply(415, { type: "urn:kb:errors:unsupported-media", detail: "not a picture" });
      m = mount(<Host drive={mkDrive()} />);
      await pick(png(16));
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/upload"]);
      expect(err).toHaveBeenCalledWith("not a picture");
      expect(maybeById(m.host, "kb-drive-chip")).toBeNull();
    });
  });
});
