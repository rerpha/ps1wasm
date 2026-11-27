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

// ---------------------------------------------------------------- 1b. built-in controller demo + keyboard
// 0xBBGGRR -> canvas RGB as the emulator renders it (5-bit expand)
const rgb15 = (c) => { const e = (v) => { v >>= 3; return (v << 3) | (v >> 2); }; return [e(c & 0xFF), e((c >> 8) & 0xFF), e((c >> 16) & 0xFF)]; };
const canvasPx = (x, y) => page.evaluate(([x, y]) => { const c = document.querySelector('#screen'); return Array.from(c.getContext('2d').getImageData(x, y, 1, 1).data.slice(0, 3)); }, [x, y]);
const CROSS = { x: 246, y: 117, on: 0xF09040, off: 0x482818 };
const START = { x: 171, y: 99, on: 0xE0E0E0, off: 0x404040 };
const isColor = async (b, which) => JSON.stringify(await canvasPx(b.x, b.y)) === JSON.stringify(rgb15(b[which]));
if (fs.existsSync(path.join(dist, 'test-files', 'controller.exe'))) {
  await page.click('#demo-btn');
  check(await waitFor(() => isColor(CROSS, 'off'), 8000), '"Try the demo" runs the controller demo');
  await page.keyboard.down('KeyX');
  check(await waitFor(() => isColor(CROSS, 'on'), 2000), 'keyboard X lights ✕ in the controller demo');
  await page.keyboard.up('KeyX');
  check(await waitFor(() => isColor(CROSS, 'off'), 2000), 'releasing X turns ✕ off');
  await page.keyboard.down('Enter');
  check(await waitFor(() => isColor(START, 'on'), 2000), 'Enter lights Start');
  await page.keyboard.up('Enter');
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
// bundled OpenBIOS button (+ its licence is served alongside)
await page.click('#openbios-btn');
check(await waitFor(async () => /bundled, MIT licence\) — OpenBIOS/.test(await text('#bios-status')), 5000), `"Use OpenBIOS" loads the bundled BIOS: "${await text('#bios-status')}"`);
const lic = await page.evaluate(async () => (await fetch('openbios/LICENSE.txt')).text());
check(/MIT License/.test(lic) && /PCSX-Redux authors/.test(lic), 'OpenBIOS licence text is served with the binary');
check(/OpenBIOS<\/a> © PCSX-Redux authors/.test(await page.$eval('footer', e => e.innerHTML)), 'footer credits OpenBIOS');
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
// forgetting a remembered game must not crash (regression: RefCell borrow
// held across an await collided with the frame loop)
await page.click('#continue-forget');
check(await toastSeen(/Forgot the saved game/), 'forgetting the remembered game works');
await page.waitForTimeout(500);
check(!(await page.$$eval('.toast', ts => ts.some(t => /crashed/.test(t.textContent)))), 'no crash after forgetting');
await page.reload();
await page.waitForSelector('#start');
check(!(await visible('#continue')), 'Continue gone after forgetting');
// put it back for the remaining checks
await dropFiles([path.join(assets, 'demo.cue'), path.join(assets, 'demo.bin')]);
check(await waitFor(demoOnScreen, 60000), 'disc boots again after forgetting');
await page.click('#btn-eject');
await page.reload();
await page.waitForSelector('#start');
await waitFor(() => visible('#continue'), 5000);
await page.click('#continue-btn');
check(await waitFor(demoOnScreen, 60000), 'Continue boots the remembered game');
await page.click('#btn-load');
check(await toastSeen(/Loaded slot 1/), 'save state survives a reload (IndexedDB)');

// ---------------------------------------------------------------- 8. phone: touch controls
{
  const mctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true, deviceScaleFactor: 2 });
  const m = await mctx.newPage();
  m.on('pageerror', e => errors.push('mobile: ' + e));
  await m.goto(url);
  await m.waitForSelector('#start');
  const mvis = (sel) => m.evaluate(s => getComputedStyle(document.querySelector(s)).display !== 'none', sel);
  check(!(await mvis('#touch')), 'phone: touch controls hidden on the start screen');
  await m.click('#demo-btn');
  await m.waitForTimeout(1500);
  check(await mvis('#touch'), 'phone: touch controls shown automatically while running');
  const overflow = await m.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  check(overflow <= 0, `phone: no horizontal overflow (${overflow}px)`);
  const mpx = (x, y) => m.evaluate(([x, y]) => { const c = document.querySelector('#screen'); return Array.from(c.getContext('2d').getImageData(x, y, 1, 1).data.slice(0, 3)); }, [x, y]);
  const mIs = async (b, which) => JSON.stringify(await mpx(b.x, b.y)) === JSON.stringify(rgb15(b[which]));
  const crossBtn = m.locator('#touch [data-btn="16384"]');
  await crossBtn.dispatchEvent('pointerdown', { pointerId: 1, pointerType: 'touch', isPrimary: true });
  let ok = false;
  for (let t = 0; t < 20 && !ok; t++) { await m.waitForTimeout(100); ok = await mIs(CROSS, 'on'); }
  check(ok, 'phone: holding on-screen ✕ lights ✕ in the demo');
  await crossBtn.dispatchEvent('pointerup', { pointerId: 1, pointerType: 'touch', isPrimary: true });
  ok = false;
  for (let t = 0; t < 20 && !ok; t++) { await m.waitForTimeout(100); ok = await mIs(CROSS, 'off'); }
  check(ok, 'phone: releasing on-screen ✕ turns it off');
  // two fingers at once: Start + D-pad up
  const startBtn = m.locator('#touch [data-btn="8"]');
  await startBtn.dispatchEvent('pointerdown', { pointerId: 2, pointerType: 'touch' });
  await m.locator('#touch [data-btn="16"]').dispatchEvent('pointerdown', { pointerId: 3, pointerType: 'touch' });
  ok = false;
  for (let t = 0; t < 20 && !ok; t++) { await m.waitForTimeout(100); ok = (await mIs(START, 'on')) && JSON.stringify(await mpx(58, 73)) === JSON.stringify(rgb15(0xE0E0E0)); }
  check(ok, 'phone: multi-touch (Start + Up together)');
  await m.screenshot({ path: path.join(os.tmpdir(), 'ps1wasm-phone.png') });
  await mctx.close();
}

check(errors.length === 0, `no console errors ${errors.length ? JSON.stringify(errors) : ''}`);
await browser.close();
server.close();
process.exit(failed ? 1 : 0);
