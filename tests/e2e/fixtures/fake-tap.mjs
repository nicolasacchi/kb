// T14 — fake kb-term tap for the SPA e2e tests.
//
// `installFakeTerminal(page, opts)` wires `page.route` for the card,
// the terms list and the ticket endpoint, and
// `page.routeWebSocket('**/api/terms/*/ws*', ...)` for the stream,
// serving the numbered-line generator (`L000001..LNNNNNN`) with the
// 8-byte big-endian offset framing from the phone spike
// (`spikes/kb-term/phone/server.mjs`). All content is synthetic.
//
// Served wire shape (what the browser sees after kb-server strips the
// viewer-id prefix): binary frames = 8-byte BE u64 offset + payload;
// text frames = `{"t": ...}` JSON. Join sends `hello`, `profile`,
// `seed{offset:0,len}` and the seed payload in chunks all at offset 0;
// resume sends `hello` only and replays from `?from=` (minus the
// optional rewind overlap). The fake records ticket requests, socket
// URLs/`from` values, client input frames and close codes for
// assertions.

export const TERM_ID = "t-e2e";
export const SID = "spec-t14-terminal";

const LINE_LEN = 8; // "L000001\r\n"

function numberedStream(total) {
  const lines = [];
  for (let i = 1; i <= total; i += 1) {
    lines.push(`L${String(i).padStart(6, "0")}\r\n`);
  }
  return Buffer.from(lines.join(""), "utf8");
}

// Terminal queries the guards must swallow (DA1, DA2, XTVERSION, DSR,
// XTWINOPS, kitty keyboard, OSC 11/52 queries).
const QUERY_BYTES = Buffer.from(
  "\u001b[c\u001b[>c\u001b[>q\u001b[5n\u001b[6n\u001b[?996n\u001b[?2026$p\u001b[16t\u001b[?u\u001b]11;?\u001b\\\u001b]52;c;?\u001b\\",
  "utf8",
);

function profileFrame(patch) {
  return JSON.stringify({
    t: "profile",
    xterm: "6.0.0",
    serialize: "0.14.0",
    width: "unicode-graphemes@0.4.0",
    image: "0.9.0",
    cols: 120,
    rows: 40,
    scrollback: 50000,
    scroll_on_erase_in_display: true,
    cell_px_w: 9,
    cell_px_h: 18,
    theme: null,
    ...(patch ?? {}),
  });
}

function helloFrame(epoch) {
  return JSON.stringify({
    t: "hello",
    proto: "kb-term/1",
    session: "s1",
    epoch: epoch ?? "E1",
    harness: "omp",
    bound_sid: SID,
  });
}

function binary(offset, payload) {
  const out = Buffer.alloc(8 + payload.length);
  out.writeBigUInt64BE(BigInt(offset), 0);
  payload.copy(out, 8);
  return out;
}

const OFF_LANE = {
  available: false,
  reason: "fixture",
  files: [],
  total: 0,
  decisions: [],
  commits: [],
  kind_counts: {},
  items: [],
  open_count: 0,
  raised_count: 0,
  open: [],
  raised: [],
  read: 0,
  edited: 0,
  wrote: 0,
};

function fixtureCard() {
  return {
    schema: "kb-session-card/1",
    session_id: SID,
    kb: "mem",
    artifact_id: "a1",
    text: "",
    summary: {
      available: true,
      display_name: "Synthetic T14 session",
      harness: "omp",
      active_secs: 10,
      started_at: 1760000000,
      ended_at: 1760000010,
      segment_chain: [],
      child_total: 0,
      resume: "",
    },
    outcome: { available: false, reason: "fixture", verdict: "unknown", checks: [], not_committed: null },
    files: { ...OFF_LANE, files: [] },
    decisions: { ...OFF_LANE, decisions: [] },
    commits: { ...OFF_LANE, commits: [] },
    research: { ...OFF_LANE, items: [], kind_counts: {} },
    comments: { ...OFF_LANE, open: [], raised: [] },
    readings: { ...OFF_LANE, items: [] },
    memory: { ...OFF_LANE, items: [] },
    live: { available: true, state: "working", holder: "omp", idle_secs: 0 },
    slate: { available: false, reason: "fixture", items: [], total: 0 },
    children: { available: false, reason: "fixture", total: 0, items: [], next_after: null },
    terminal: {
      available: true,
      term_id: TERM_ID,
      harness: "omp",
      state: "working",
      admitted: true,
    },
  };
}

/**
 * Install the fake terminal. Returns a stats object:
 * `{ tickets, ticketRequests, sockets, froms, inputs, closes }`.
 */
export async function installFakeTerminal(page, opts = {}) {
  const total = opts.total ?? 2000;
  const seedLines = opts.seedLines ?? Math.min(total, 500);
  const pace = opts.pace ?? 1;
  const rewind = opts.rewind ?? 0;
  const gapAt = opts.gapAt ?? null;
  const exitCode = opts.exitCode ?? null;
  const ticketStatus = opts.ticketStatus ?? null;
  const profilePatch = opts.profilePatch ?? null;
  const seedByteLen = LINE_LEN * seedLines;

  // The numbered-line stream, optionally with reply-guard queries
  // spliced in right after the seed region. The query bytes are real
  // pane-output bytes occupying real stream offsets (they would have
  // lived on the wire between the seed and the rest of the lines).
  let stream = numberedStream(total);
  if (opts.injectQueries) {
    stream = Buffer.concat([
      stream.subarray(0, seedByteLen),
      QUERY_BYTES,
      stream.subarray(seedByteLen),
    ]);
  }
  if (opts.widthTest) {
    // 40 kitty placeholder cells (U+10EEEE + combining acute) on line 1.
    const cells = Buffer.from("\u{10EEEE}\u0301".repeat(40), "utf8");
    stream = Buffer.concat([cells, Buffer.from("\r\n", "utf8"), stream]);
  }

  const stats = {
    tickets: 0,
    ticketRequests: [],
    sockets: 0,
    froms: [],
    inputs: [],
    closes: [],
  };
  let dropsLeft = opts.drops ?? 0;
  let restartDone = false;
  let gapDone = false;
  const dropEvery = opts.dropEvery ?? null;

  const card = fixtureCard();
  await page.route("**/api/sessions/*/card", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(card),
    }),
  );
  await page.route("**/api/terms", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          terms: [
            {
              id: TERM_ID,
              harness: "omp",
              cwd: "/synthetic",
              started_unix: 1760000000,
              resume_sid: null,
              pane_pid: 1,
              cols: 120,
              rows: 40,
              bracket_paste_flag: false,
              pane_dead: false,
              exit_code: null,
              grid_owner: null,
              conf_version: 1,
              anchor_pid: null,
              stale: false,
              session_id: SID,
              state: "working",
              blocked: false,
            },
          ],
        }),
      });
    }
    return route.continue();
  });
  await page.route("**/api/terms/*/ticket", (route) => {
    stats.tickets += 1;
    stats.ticketRequests.push({
      method: route.request().method(),
      headers: route.request().headers(),
    });
    if (ticketStatus !== null) {
      return route.fulfill({ status: ticketStatus, body: "" });
    }
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ ticket: "a".repeat(32), expires_in: 30, term: TERM_ID }),
    });
  });

  await page.routeWebSocket("**/api/terms/*/ws*", (ws) => {
    stats.sockets += 1;
    const url = new URL(ws.url());
    const fromParam = url.searchParams.get("from");
    const from = fromParam === null ? 0 : Number(fromParam);
    stats.froms.push(fromParam === null ? null : from);
    let closed = false;
    ws.onMessage((msg) => {
      stats.inputs.push(String(msg.toString()));
    });
    // Playwright's onClose handler receives (code, reason) positionally.
    ws.onClose((code) => {
      stats.closes.push(code);
      closed = true;
    });
    const sendText = (t) => {
      if (!closed) ws.send(t);
    };
    const sendBinary = (off, payload) => {
      if (!closed) ws.send(binary(off, payload));
    };
    sendText(helloFrame(opts.epoch));
    sendText(profileFrame(profilePatch));
    const full = from === 0;
    let cursor;
    if (full) {
      const seed = stream.subarray(0, seedByteLen);
      sendText(JSON.stringify({ t: "seed", offset: 0, len: seed.length }));
      for (let i = 0; i < seed.length; i += 4096) {
        sendBinary(0, seed.subarray(i, Math.min(seed.length, i + 4096)));
      }
      cursor = seed.length;
    } else {
      cursor = Math.max(0, from - rewind);
    }

    const connStart = cursor;
    const pump = () => {
      if (closed) return;
      if (exitCode !== null && cursor >= stream.length) {
        sendText(JSON.stringify({ t: "exit", seq: 1, code: exitCode, offset: stream.length }));
        closed = true;
        setTimeout(() => ws.close({ code: 1000 }), 10);
        return;
      }
      if (gapAt !== null && !gapDone && cursor >= gapAt && cursor < stream.length) {
        gapDone = true;
        sendBinary(cursor + 100, Buffer.from("GAPGAPGAPG", "utf8"));
        setTimeout(() => pump(), pace);
        return;
      }
      if (opts.restartOnce && !restartDone && cursor > 0) {
        restartDone = true;
        closed = true;
        ws.close({ code: 1012 });
        return;
      }
      if (cursor >= stream.length) {
        // Idle open socket (browser keeps it; onclose settles liveness).
        setTimeout(() => pump(), 50);
        return;
      }
      if (dropsLeft > 0 && dropEvery !== null && cursor - connStart >= dropEvery) {
        dropsLeft -= 1;
        closed = true;
        ws.close({ code: 1001 });
        return;
      }
      const end = Math.min(stream.length, cursor + 2048);
      sendBinary(cursor, stream.subarray(cursor, end));
      cursor = end;
      setTimeout(() => pump(), pace);
    };
    pump();
  });
  return stats;
}
