// T14 — terminal reply guards: no query the terminal sends is ever
// answered by the browser.
//
// The CLI/tmux side of kb-term/1 answers terminal queries (DA, DSR,
// XTWINOPS, …) itself. The SPA viewer must NEVER emit a reply: every
// CSI/OSC/DCS reporting query the pane program issues is swallowed
// (the parser handler returns `true`). Plain state sets (non-query
// forms) return `false` so xterm keeps its job.

export interface ReplyParser {
  registerCsiHandler(
    prefix: string,
    intermediates: string,
    final: string,
    handler: (params: number[]) => boolean,
  ): { dispose(): void };
  registerOscHandler(
    code: number,
    handler: (data: string) => boolean,
  ): { dispose(): void };
  registerDcsHandler(
    prefix: string,
    intermediates: string,
    final: string,
    handler: (params: number[], data: string) => boolean,
  ): { dispose(): void };
}

export interface InstalledGuards {
  dispose(): void;
}

const SWALLOW = (): boolean => true;

// XTWINOPS `t` report params the pane may ask us about (winops report
// numbers 11,13,14,15,16,18,19,20,21). Params 22/23 (window/edge
// pixel size) and everything else pass through untouched.
const XTWINOPS_REPORTS = new Set([11, 13, 14, 15, 16, 18, 19, 20, 21]);

function xtwinops(params: number[]): boolean {
  return params.length > 0 && params.every((p) => XTWINOPS_REPORTS.has(p));
}

// OSC queries: a payload containing `?` asks the terminal to report
// (OSC 4, 5, 10–19, 21, 52). Non-query sets return false.
function isOscQuery(data: string): boolean {
  return data.includes("?");
}

/** Install the reply guards on `term.parser`. Returns disposables. */
export function installReplyGuards(parser: ReplyParser): InstalledGuards {
  const disposables: Array<{ dispose(): void }> = [];
  const csi = (
    prefix: string,
    intermediates: string,
    final: string,
    handler: (params: number[]) => boolean = SWALLOW,
  ): void => {
    disposables.push(parser.registerCsiHandler(prefix, intermediates, final, handler));
  };
  const dcs = (
    prefix: string,
    intermediates: string,
    final: string,
  ): void => {
    disposables.push(
      parser.registerDcsHandler(prefix, intermediates, final, () => true),
    );
  };
  const osc = (code: number): void => {
    disposables.push(
      parser.registerOscHandler(code, (data) => isOscQuery(data)),
    );
  };

  // CSI: DA1 (`c`), DA2 (`>c`), DA3 (`=c`), DSR plain and `?` (`n`),
  // DECRQM with/without `?` (`$p`), XTVERSION (`>q`), kitty keyboard
  // query (`?u`), DECREQTPARM (`x`), XTWINOPS (`t`, allowlist only),
  // XTSMGRAPHICS (`?S`).
  csi("", "", "c");
  csi(">", "", "c");
  csi("=", "", "c");
  csi("", "", "n");
  csi("?", "", "n");
  csi("", "$", "p");
  csi("?", "$", "p");
  csi(">", "", "q");
  csi("?", "", "u");
  csi("", "", "x");
  csi("", "", "t", xtwinops);
  csi("?", "", "S");
  // DCS `+q` (XTGETTCAP-shaped) and `$q` (DECRQSS): the `+`/`$` are
  // intermediate bytes (0x20-0x2f), never prefixes (0x3c-0x3f) — xterm's
  // parser rejects a non-prefix byte in the prefix slot.
  dcs("", "+", "q");
  dcs("", "$", "q");

  // OSC: 4, 5, 10-19, 21, 52 — query payloads (`?` present) swallowed.
  for (const code of [4, 5, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 52]) {
    osc(code);
  }

  return {
    dispose(): void {
      for (const d of disposables) d.dispose();
    },
  };
}
