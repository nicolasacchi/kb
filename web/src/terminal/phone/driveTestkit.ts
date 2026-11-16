// T18 — shared helpers for the Drive component tests (jsdom). Not a test file.

import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { vi } from "vitest";
import type { Drive } from "./driveApi";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

export interface Mounted {
  host: HTMLElement;
  root: Root;
  unmount(): void;
}

export function mount(el: ReactElement): Mounted {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  act(() => root.render(el));
  return {
    host,
    root,
    unmount: () => {
      act(() => root.unmount());
      host.remove();
    },
  };
}

export interface FetchCall {
  url: string;
  init: RequestInit;
  json: Record<string, unknown> | null;
}

export interface FetchRig {
  calls: FetchCall[];
  /** Queue the next response (defaults to `200 {}`). */
  reply(status: number, body?: unknown): void;
  urls(): string[];
}

/** Stub `fetch`; every call is recorded, replies are queued with `reply`. */
export function rigFetch(): FetchRig {
  const calls: FetchCall[] = [];
  const queue: Response[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit) => {
      let json: Record<string, unknown> | null = null;
      if (typeof init.body === "string") json = JSON.parse(init.body) as Record<string, unknown>;
      calls.push({ url, init, json });
      return queue.shift() ?? new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
    }),
  );
  return {
    calls,
    reply: (status, body = {}) => {
      queue.push(
        new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }),
      );
    },
    urls: () => calls.map((c) => c.url),
  };
}

export const mkDrive = (over: Partial<Drive> = {}): Drive => ({
  termId: "abcd1234",
  harness: "omp",
  viewer: "ws-abc1234567",
  confirm: vi.fn(async () => true),
  ...over,
});

/** Let pending promise continuations run (real timers). */
export async function settle(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/** A cancelable pointer/mouse event on `el`; returns it so the caller can read `defaultPrevented`. */
export function press(el: Element, type: "pointerdown" | "pointerup" | "mousedown" = "pointerdown"): Event {
  const ev =
    type === "mousedown"
      ? new MouseEvent(type, { bubbles: true, cancelable: true })
      : new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, pointerType: "touch" });
  act(() => {
    el.dispatchEvent(ev);
  });
  return ev;
}

export const byId = (host: ParentNode, id: string): HTMLElement => {
  const el = host.querySelector<HTMLElement>(`[data-testid="${id}"]`);
  if (!el) throw new Error(`no [data-testid="${id}"]`);
  return el;
};

export const maybeById = (host: ParentNode, id: string): HTMLElement | null =>
  host.querySelector<HTMLElement>(`[data-testid="${id}"]`);

/** Type into a React-controlled textarea. */
export function typeInto(el: HTMLTextAreaElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  if (!setter) throw new Error("no textarea value setter");
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

export function click(el: Element): void {
  act(() => {
    (el as HTMLElement).click();
  });
}
