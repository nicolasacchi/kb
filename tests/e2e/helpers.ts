// Shared helpers for e2e specs. The daemon is spawned by global-setup.ts
// at this fixed port. Workers run in separate processes from globalSetup
// so we can't read globalThis state — the port is hardcoded here and
// the global-setup.ts uses the same constant.
export const PORT = 4737;
export const BASE = `http://127.0.0.1:${PORT}`;

// Track U — artifact permalinks are path-based (`/a/<kb>/<source_relative>`).
// Build a RegExp that matches such a URL, escaping the path's regex-special
// chars (`.html`, etc.). Trailing group allows an optional `?p=` page query.
export function artifactUrlRe(kb: string, sourceRelative: string): RegExp {
  const esc = sourceRelative.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`/a/${kb}/${esc}(\\?|$)`);
}

// AH4 — a controllable `CloseWatcher` for specs. Real Android Back cannot be
// driven by Playwright, so this replaces `window.CloseWatcher` with a class
// that has the platform's semantics (requestClose fires a cancelable `cancel`
// then `close`; close() fires `close`; both destroy) and exposes
// `window.__cw = { made, live, instances }`. Call BEFORE `page.goto`.
export async function installCloseWatcherStub(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    type W = {
      alive: boolean;
      oncancel: ((e: Event) => void) | null;
      onclose: ((e: Event) => void) | null;
      requestClose: (o?: { cancelable?: boolean }) => void;
      close: () => void;
      destroy: () => void;
    };
    const cw = {
      made: 0,
      instances: [] as W[],
      get live() {
        return this.instances.filter((w) => w.alive).length;
      },
    };
    class Stub implements W {
      alive = true;
      oncancel: ((e: Event) => void) | null = null;
      onclose: ((e: Event) => void) | null = null;
      constructor() {
        cw.made += 1;
        cw.instances.push(this);
      }
      requestClose(o: { cancelable?: boolean } = {}) {
        if (!this.alive) return;
        if (o.cancelable ?? true) {
          const ev = new Event("cancel", { cancelable: true });
          this.oncancel?.(ev);
          if (ev.defaultPrevented) return;
        }
        this.close();
      }
      close() {
        if (!this.alive) return;
        this.onclose?.(new Event("close"));
        this.destroy();
      }
      destroy() {
        this.alive = false;
      }
    }
    (window as unknown as { CloseWatcher: unknown }).CloseWatcher = Stub;
    (window as unknown as { __cw: unknown }).__cw = cw;
  });
}

// AH4 — the REAL close-request path, no stub. Playwright's Chromium ships
// `CloseWatcher`, and an Escape keydown that no handler prevented IS a close
// request there, so a typed Esc in an open sheet reaches the sheet's watcher
// and only the call site's `ignoreEscape` keeps the sheet open.
//
// `armTypingProbe` appends one INPUT, one TEXTAREA and one contentEditable DIV
// (`data-ah4-typing="input|textarea|editable"`) plus one plain BUTTON
// (`data-ah4-plain`) into `host`, an open sheet. Appended targets carry no
// handler of their own, so no editor's Esc handling can stand in for the
// guard. It also records, for every Escape keydown that reaches window's
// bubble phase, whether a handler prevented it (`escapeIn` returns it).
export async function hasRealCloseWatcher(page: import("@playwright/test").Page) {
  return page.evaluate(
    () => typeof (window as unknown as { CloseWatcher?: unknown }).CloseWatcher === "function",
  );
}

export async function armTypingProbe(page: import("@playwright/test").Page, host: string) {
  await page.evaluate((sel) => {
    const w = window as unknown as { __ah4Esc?: boolean[] };
    if (!w.__ah4Esc) {
      const log: boolean[] = [];
      w.__ah4Esc = log;
      window.addEventListener("keydown", (e) => {
        if (e.key === "Escape") log.push(e.defaultPrevented);
      });
    }
    const root = document.querySelector(sel);
    if (!root) throw new Error(`armTypingProbe: no ${sel}`);
    const input = document.createElement("input");
    input.setAttribute("data-ah4-typing", "input");
    const textarea = document.createElement("textarea");
    textarea.setAttribute("data-ah4-typing", "textarea");
    const editable = document.createElement("div");
    editable.contentEditable = "true";
    editable.textContent = "probe";
    editable.setAttribute("data-ah4-typing", "editable");
    const plain = document.createElement("button");
    plain.type = "button";
    plain.textContent = "probe";
    plain.setAttribute("data-ah4-plain", "");
    root.append(input, textarea, editable, plain);
  }, host);
}

// Focus `target`, press Escape, then give React long enough to commit any
// close the keypress caused (the platform's `close` event runs outside React,
// so its state update lands a task later). Returns whether a handler
// prevented the keydown; `false` means the platform ran a close request.
export async function escapeIn(
  page: import("@playwright/test").Page,
  target: string | import("@playwright/test").Locator,
) {
  await page.evaluate(() => {
    (window as unknown as { __ah4Esc: boolean[] }).__ah4Esc.length = 0;
  });
  await (typeof target === "string" ? page.locator(target) : target).focus();
  await page.keyboard.press("Escape");
  await page.waitForTimeout(250);
  return page.evaluate(() => {
    const log = (window as unknown as { __ah4Esc: boolean[] }).__ah4Esc;
    if (log.length !== 1) throw new Error(`escapeIn: saw ${log.length} Escape keydowns at window`);
    return log[0];
  });
}
