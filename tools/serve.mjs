// Minimal static server (no dependencies) with the right MIME type for .wasm.
// Usage: node tools/serve.mjs [dir] [port]
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
const root = path.resolve(process.argv[2] ?? 'dist');
const port = Number(process.argv[3] ?? 8080);
const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };
http.createServer((req, res) => {
  const url = decodeURIComponent(req.url.split('?')[0]);
  let p = path.join(root, url);
  if (!p.startsWith(root)) { res.writeHead(403); return res.end(); }
  if (fs.existsSync(p) && fs.statSync(p).isDirectory()) p = path.join(p, 'index.html');
  fs.readFile(p, (err, data) => {
    if (err) { res.writeHead(404); return res.end('not found'); }
    res.writeHead(200, { 'Content-Type': types[path.extname(p)] ?? 'application/octet-stream' });
    res.end(data);
  });
}).listen(port, '0.0.0.0', () => console.log(`Serving ${root} on http://localhost:${port}`));
