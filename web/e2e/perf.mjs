// Browser performance check: worst-case CPU load (cached ALU loop).
import { chromium } from 'playwright-core';
import http from 'node:http'; import fs from 'node:fs'; import path from 'node:path'; import os from 'node:os';
import { fileURLToPath } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.join(here, '../../dist'), assets = path.join(here, 'assets');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const server = http.createServer((req, res) => { const p = path.join(dist, req.url === '/' ? 'index.html' : req.url.split('?')[0]);
  fs.readFile(p, (e, d) => { if (e) { res.writeHead(404); res.end(); return; } res.writeHead(200, { 'Content-Type': types[path.extname(p)] ?? 'application/octet-stream' }); res.end(d); }); }).listen(0);
const dir = path.join(os.homedir(), '.cache/ms-playwright');
const exe = fs.readdirSync(dir).filter(d => d.startsWith('chromium-')).map(d => path.join(dir, d, 'chrome-linux64/chrome')).find(fs.existsSync);
const b = await chromium.launch({ executablePath: exe }); const page = await b.newPage();
await page.goto(`http://localhost:${server.address().port}/`);
await page.setInputFiles('#game-input', path.join(assets, 'alu-bench.exe'));
await page.waitForTimeout(6000);
console.log('worst case (cached ALU loop):', await page.textContent('#fps'));
await b.close(); server.close();
