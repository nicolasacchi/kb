// CMDPALETTE: shared jsdom rig for the palette and rail tests. Not a test file.

import type { ReactElement } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { vi } from "vitest";
import ConfirmProvider from "../../../components/ConfirmProvider";
import type { EffectiveCatalogue } from "../../../api/generated/EffectiveCatalogue";

type Item = EffectiveCatalogue["harnesses"][string]["groups"][number]["items"][number];
const it = (o: Partial<Item> & { id: string; label: string }): Item => ({ kind: "command", origin: "default", mode: "paste", ...o });

export const CATALOGUE: EffectiveCatalogue = {
  rev: 3,
  overlay: { harnesses: {}, hidden: {} },
  hidden: {},
  harnesses: {
    claude: {
      groups: [
        {
          id: "session",
          label: "Session",
          origin: "default",
          items: [
            it({ id: "compact", label: "/compact", text: "/compact", description: "Summarise the conversation" }),
            it({ id: "config", label: "/config", text: "/config" }),
            it({ id: "model", label: "/model", text: "/model", mode: "run" }),
            it({ id: "multi", label: "/multi", text: "first\nsecond" }),
            it({ id: "esc", label: "Rewind", kind: "chord", keys: ["Esc", "Esc"], text: undefined, mode: undefined }),
          ],
        },
        {
          id: "args",
          label: "With arguments",
          origin: "overlay",
          items: [
            it({ id: "add", label: "/add", text: "/add {path}", args: [{ name: "path", hint: "A file" }] }),
            it({ id: "runarg", label: "/runarg", text: "/runarg {x}", mode: "run", args: [{ name: "x", hint: "x" }] }),
          ],
        },
      ],
    },
    omp: { groups: [{ id: "omp", label: "omp", origin: "default", items: [it({ id: "shake", label: "/shake", text: "/shake", unverified: true })] }] },
  },
};

export interface Rig {
  calls: { url: string; body: Record<string, unknown> | null }[];
  /** The paste/keys/enter calls only. */
  drive(): { op: string; body: Record<string, unknown> | null }[];
  queueDrive(status: number, body: unknown): void;
}

/** Stub fetch: the commands route answers `commands` (or a 500), drive routes answer `{}`. */
export function rigFetch(opts: { commandsStatus?: number } = {}): Rig {
  const calls: Rig["calls"] = [];
  const queue: { status: number; body: unknown }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const body = typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
      calls.push({ url, body });
      if (url.endsWith("/api/terms/commands")) {
        const status = opts.commandsStatus ?? 200;
        return new Response(JSON.stringify(status === 200 ? CATALOGUE : {}), { status, headers: { "Content-Type": "application/json" } });
      }
      const q = queue.shift();
      return new Response(JSON.stringify(q?.body ?? {}), { status: q?.status ?? 200, headers: { "Content-Type": "application/json" } });
    }),
  );
  return {
    calls,
    drive: () => calls.filter((c) => /\/api\/terms\/[^/]+\/(paste|keys|enter)$/.test(c.url)).map((c) => ({ op: c.url.split("/").pop() as string, body: c.body })),
    queueDrive: (status, body) => void queue.push({ status, body }),
  };
}

export function wrap(ui: ReactElement): ReactElement {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return (
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <ConfirmProvider>{ui}</ConfirmProvider>
      </MemoryRouter>
    </QueryClientProvider>
  );
}

export function installDialogPolyfill(): void {
  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
    this.open = false;
  };
}
