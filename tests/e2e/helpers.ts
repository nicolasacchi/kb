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
