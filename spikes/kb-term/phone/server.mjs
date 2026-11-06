// Static server + fake tap WebSocket for the phone spike.
// /tap?from=<offset>&pace=<ms>&chunk=<bytes>&rewind=<bytes>
//   binary frame = 8-byte big-endian absolute offset of payload byte 0, then payload
//   final text frame = {"end":true,"total":N}
// POST /drop terminates every open tap socket abruptly. GET /stats gives connection log.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer } from 'ws';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const NM = path.join(HERE, '..', 'node_modules');
const PORT = Number(process.env.PHONE_PORT || 4173);
const FILES = {
  '/': ['page.html', 'text/html'],
  '/xterm.js': [path.join(NM, '@xterm/xterm/lib/xterm.js'), 'text/javascript'],
  '/xterm.css': [path.join(NM, '@xterm/xterm/css/xterm.css'), 'text/css'],
  '/addon-webgl.js': [path.join(NM, '@xterm/addon-webgl/lib/addon-webgl.js'), 'text/javascript'],
  '/data/seed.bin': [path.join(HERE, 'data/seed.bin'), 'application/octet-stream'],
  '/data/stream.bin': [path.join(HERE, 'data/stream.bin'), 'application/octet-stream'],
  '/data/stream-strip.bin': [path.join(HERE, 'data/stream-strip.bin'), 'application/octet-stream'],
  '/data/wide.bin': [path.join(HERE, 'data/wide.bin'), 'application/octet-stream'],
};
const streams = { raw: fs.readFileSync(path.join(HERE, 'data/stream.bin')), strip: fs.readFileSync(path.join(HERE, 'data/stream-strip.bin')) };
const log = [];
const open = new Set();

const server = http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/health') { res.end('ok'); return; }
  if (u.pathname === '/drop') {
    const n = open.size;
    for (const s of open) s.terminate();
    res.end(JSON.stringify({ dropped: n }));
    return;
  }
  if (u.pathname === '/stats') { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ log, totals: { raw: streams.raw.length, strip: streams.strip.length } })); return; }
  if (u.pathname === '/reset') { log.length = 0; res.end('ok'); return; }
  const f = FILES[u.pathname];
  if (!f) { res.statusCode = 404; res.end('nf'); return; }
  try {
    const p = path.isAbsolute(f[0]) ? f[0] : path.join(HERE, f[0]);
    res.setHeader('content-type', f[1]);
    res.setHeader('cache-control', 'no-store');
    res.end(fs.readFileSync(p));
  } catch (e) { res.statusCode = 500; res.end(String(e)); }
});

const wss = new WebSocketServer({ noServer: true });
server.on('upgrade', (req, sock, head) => {
  const u = new URL(req.url, 'http://x');
  if (u.pathname !== '/tap') { sock.destroy(); return; }
  wss.handleUpgrade(req, sock, head, (ws) => {
    const stream = streams[u.searchParams.get('s') === 'strip' ? 'strip' : 'raw'];
    const from = Math.max(0, Number(u.searchParams.get('from') || 0));
    const pace = Number(u.searchParams.get('pace') || 3);
    const chunk = Number(u.searchParams.get('chunk') || 1024);
    const rewind = Number(u.searchParams.get('rewind') || 0);
    const start = Math.max(0, from - (from > 0 ? rewind : 0));
    log.push({ from, start, t: Date.now() });
    open.add(ws);
    ws.on('close', () => open.delete(ws));
    ws.on('error', () => open.delete(ws));
    let off = start;
    const pump = () => {
      if (ws.readyState !== 1) return;
      if (off >= stream.length) { ws.send(JSON.stringify({ end: true, total: stream.length })); return; }
      const end = Math.min(stream.length, off + chunk);
      const b = Buffer.alloc(8 + end - off);
      b.writeBigUInt64BE(BigInt(off), 0);
      stream.copy(b, 8, off, end);
      ws.send(b);
      off = end;
      setTimeout(pump, pace);
    };
    pump();
  });
});
server.listen(PORT, '127.0.0.1', () => console.log('phone spike server on', PORT));
