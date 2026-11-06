// Tiny static server: / -> renderer/web, /nm/<pkg path> -> spikes/kb-term/node_modules (only @xterm).
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const webRoot = path.join(here, 'web');
const nmRoot = path.join(here, '..', 'node_modules');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.map': 'application/json' };

http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  let rel = decodeURIComponent(u.pathname);
  let file;
  if (rel.startsWith('/nm/@xterm/')) file = path.join(nmRoot, rel.slice(4));
  else file = path.join(webRoot, rel === '/' ? 'index.html' : rel);
  const root = rel.startsWith('/nm/') ? nmRoot : webRoot;
  if (!path.resolve(file).startsWith(root)) { res.writeHead(403); return res.end('no'); }
  fs.readFile(file, (err, buf) => {
    if (err) { res.writeHead(404); return res.end('not found'); }
    res.writeHead(200, { 'content-type': types[path.extname(file)] || 'application/octet-stream', 'cache-control': 'no-store' });
    res.end(buf);
  });
}).listen(4173, '127.0.0.1', () => console.log('renderer spike server on 4173'));
