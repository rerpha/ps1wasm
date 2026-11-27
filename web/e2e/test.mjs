// End-to-end tests of the browser UI, driven like a user would.
// Usage: npm run test:e2e   (or node web/e2e/test.mjs [distDir] [assetsDir])
import { chromium } from 'playwright-core';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));

const dist = path.resolve(process.argv[2] ?? path.join(here, '../../dist'));
const assets = path.resolve(process.argv[3] ?? path.join(here, 'assets'));
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const server = http.createServer((req, res) => {
  const p = path.join(dist, req.url === '/' ? 'index.html' : req.url.split('?')[0]);
  fs.readFile(p, (err, data) => {
    if (err) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { 'Content-Type': types[path.extname(p)] ?? 'application/octet-stream' });
    res.end(data);
  });
}).listen(0);
const url = `http://localhost:${server.address().port}/`;

const chromeDir = path.join(os.homedir(), '.cache/ms-playwright');
const exePath = fs.readdirSync(chromeDir).filter(d => d.startsWith('chromium-'))
  .map(d => path.join(chromeDir, d, 'chrome-linux64/chrome')).find(fs.existsSync);
const browser = await chromium.launch({ executablePath: exePath, args: ['--autoplay-policy=no-user-gesture-required'] });
const context = await browser.newContext({ acceptDownloads: true });
const page = await context.newPage();
const errors = [];
page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
page.on('pageerror', e => errors.push(String(e)));
page.on('dialog', d => d.accept());

let failed = 0;
const check = (cond, msg) => { console.log((cond ? 'PASS ' : 'FAIL ') + msg); if (!cond) failed++; };
const visible = (sel) => page.evaluate(s => !document.querySelector(s).classList.contains('hidden'), sel);
const text = (sel) => page.textContent(sel);
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await page.waitForTimeout(200); }
  return false;
}
const toastSeen = (re) => waitFor(async () => (await page.$$eval('.toast', ts => ts.map(t => t.textContent))).some(t => re.test(t)), 5000);

// Same expectations as ps1wasm_cli::testkit::DEMO_PIXELS
const EXPECT = [[300, 10, 0x000084], [160, 30, 0xF70000], [50, 214, 0x08F700], [270, 214, 0x0800F7], [12, 12, 0xFFFF00]];
const demoOnScreen = () => page.evaluate((expect) => {
  const c = document.querySelector('#screen');
  if (c.width !== 320 || c.height !== 240) return false;
  const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
  return expect.every(([x, y, rgb]) => {
    const i = (y * c.width + x) * 4;
    return ((d[i] << 16) | (d[i + 1] << 8) | d[i + 2]) === rgb;
  });
}, EXPECT);

// Simulate dropping files onto the page (real DragEvent + DataTransfer).
async function dropFiles(paths) {
  const files = paths.map(p => ({ name: path.basename(p), b64: fs.readFileSync(p).toString('base64') }));
  await page.evaluate((files) => {
    const dt = new DataTransfer();
    for (const f of files) {
      const bin = Uint8Array.from(atob(f.b64), c => c.charCodeAt(0));
      dt.items.add(new File([bin], f.name));
    }
    window.dispatchEvent(new DragEvent('dragover', { dataTransfer: dt, bubbles: true, cancelable: true }));
    window.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }));
  }, files);
}

// ---------------------------------------------------------------- 1. first visit
await page.goto(url);
await page.waitForSelector('#start');
check(await visible('#start'), 'start card shown on first visit');
check(!(await visible('#toolbar')), 'toolbar hidden until a game runs');
check(await page.$eval('#start-btn', b => b.disabled), 'Start disabled without files');
check(/BIOS dump/.test(await text('#bios-status')), 'BIOS step explains what to provide');

// ---------------------------------------------------------------- 1b. built-in demo button
if (fs.existsSync(path.join(dist, 'test-files', 'demo.exe'))) {
  await page.click('#demo-btn');
  check(await waitFor(demoOnScreen, 8000), '"Try the demo" runs the built-in demo');
  await page.click('#btn-eject');
} else {
  console.log('SKIP demo button (run npm run build:demo first)');
}

// ---------------------------------------------------------------- 2. EXE without BIOS (file picker)
await page.setInputFiles('#game-input', path.join(assets, 'demo.exe'));
check(await waitFor(demoOnScreen, 8000), 'EXE auto-starts without BIOS and renders the expected picture');
check(await visible('#toolbar') && !(await visible('#start')), 'toolbar shown, start card hidden while running');
await page.waitForTimeout(1200);
const fps = await text('#fps');
check(/\d+ fps/.test(fps), `fps indicator: ${fps}`);

// pause / resume via Esc
await page.keyboard.press('Escape');
check(await visible('#paused'), 'Esc pauses (overlay shown)');
await page.keyboard.press('Escape');
check(!(await visible('#paused')), 'Esc resumes');

// ---------------------------------------------------------------- 3. close, then BIOS + disc by drag & drop
await page.click('#btn-eject');
check(await visible('#start'), 'Close returns to the start card');
await dropFiles([path.join(assets, 'openbios.bin')]);
check(await waitFor(async () => /OpenBIOS/.test(await text('#bios-status')), 5000), `dropped BIOS recognised: "${await text('#bios-status')}"`);
check(await page.$eval('#step-bios', e => e.classList.contains('done')), 'BIOS step marked done');
await dropFiles([path.join(assets, 'demo.cue'), path.join(assets, 'demo.bin')]);
check(await waitFor(async () => (await visible('#toolbar')), 8000), 'dropping .cue + .bin auto-starts the game');
check(await waitFor(demoOnScreen, 60000), 'disc booted through the BIOS and rendered');

// ---------------------------------------------------------------- 4. save states
await page.click('#btn-save');
check(await toastSeen(/Saved to slot 1/), 'save state to slot 1');
await page.click('#btn-reset');
check(await waitFor(async () => !(await demoOnScreen()), 5000), 'reset clears the screen (BIOS restarting)');
await page.click('#btn-load');
check(await toastSeen(/Loaded slot 1/), 'load state from slot 1');
check(await waitFor(demoOnScreen, 2000), 'loaded state restores the game screen instantly (no reboot)');
await page.selectOption('#slot', '3');
await page.click('#btn-load');
check(await toastSeen(/empty/), 'empty slot reports clearly');
await page.selectOption('#slot', '1');
await page.keyboard.press('F5');
check(await toastSeen(/Saved to slot 1/), 'F5 quick-saves');

// ---------------------------------------------------------------- 5. analog button
const ledBefore = await page.$eval('#led', e => e.classList.contains('on'));
await page.click('#btn-analog');
check(await waitFor(async () => (await page.$eval('#led', e => e.classList.contains('on'))) !== ledBefore, 2000), 'Analog button toggles the LED');

// ---------------------------------------------------------------- 6. settings: key remap, memory cards
await page.click('#btn-settings');
check(await page.$eval('#settings', d => d.open), 'settings dialog opens');
const crossBtn = page.locator('#keymap tr', { hasText: 'Cross' }).locator('button');
await crossBtn.click();
check(/Press a key/.test(await crossBtn.textContent()), 'remap: waiting for a key');
await page.keyboard.press('KeyK');
check((await crossBtn.textContent()).trim() === 'K', `remap: Cross now on K ("${await crossBtn.textContent()}")`);
check(/K<\/kbd> ✕/.test(await page.$eval('#help', e => e.innerHTML)), 'controls help reflects the new binding');

const [dl] = await Promise.all([page.waitForEvent('download'), page.click('#mc1-export')]);
const cardPath = path.join(os.tmpdir(), 'psx-e2e-card.mcr');
await dl.saveAs(cardPath);
const card = fs.readFileSync(cardPath);
check(card.length === 131072 && card.subarray(0, 2).toString() === 'MC', `memory card export: ${card.length} bytes, MC header`);
await page.setInputFiles('#mc2-import', cardPath);
check(await toastSeen(/imported into slot 2/), 'memory card import into slot 2');
await page.click('#settings-close');

// ---------------------------------------------------------------- 7. return visit
await page.reload();
await page.waitForSelector('#start');
check(/remembered/.test(await text('#bios-status')), `BIOS remembered: "${await text('#bios-status')}"`);
check(await waitFor(() => visible('#continue'), 5000), 'Continue offered for the last game');
check(/demo\.cue/.test(await text('#continue-name')), `Continue names the game: ${await text('#continue-name')}`);
await page.click('#btn-settings-top'); // settings reachable before starting a game
const crossPersisted = (await page.locator('#keymap tr', { hasText: 'Cross' }).locator('button').textContent()).trim();
check(crossPersisted === 'K', `key binding persisted across reload ("${crossPersisted}")`);
await page.click('#settings-close');
await page.click('#continue-btn');
check(await waitFor(demoOnScreen, 60000), 'Continue boots the remembered game');
await page.click('#btn-load');
check(await toastSeen(/Loaded slot 1/), 'save state survives a reload (IndexedDB)');

check(errors.length === 0, `no console errors ${errors.length ? JSON.stringify(errors) : ''}`);
await browser.close();
server.close();
process.exit(failed ? 1 : 0);
