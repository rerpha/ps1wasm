// Minimal static server (no dependencies) with the right MIME type for .wasm.
// Usage: node tools/serve.mjs [dir] [port]     (also imported by the e2e tests)
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };

/** Serve `dir` on `port` (0 = any free port). Resolves to the server once listening. */
export function serve(dir, port = 0, host = '0.0.0.0') {
  const root = path.resolve(dir);
  const server = http.createServer((req, res) => {
    let p = path.join(root, decodeURIComponent(req.url.split('?')[0]));
    if (!p.startsWith(root)) { res.writeHead(403); return res.end(); }
    if (fs.existsSync(p) && fs.statSync(p).isDirectory()) p = path.join(p, 'index.html');
    fs.readFile(p, (err, data) => {
      if (err) { res.writeHead(404); return res.end('not found'); }
      res.writeHead(200, { 'Content-Type': types[path.extname(p)] ?? 'application/octet-stream' });
      res.end(data);
    });
  });
  return new Promise(resolve => server.listen(port, host, () => resolve(server)));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const dir = process.argv[2] ?? 'dist';
  const server = await serve(dir, Number(process.argv[3] ?? 8080));
  console.log(`Serving ${path.resolve(dir)} on http://localhost:${server.address().port}`);
}
