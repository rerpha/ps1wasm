// Screenshots of the UI states for visual review.  Usage: node web/e2e/shots.mjs [outDir]
import { start, testFile, openbios } from './harness.mjs';
const out = process.argv[2] ?? '/tmp';
const { url, browser, close } = await start();
const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
await page.goto(url); await page.waitForTimeout(800);
await page.screenshot({ path: `${out}/ui-1-start.png` });
await page.setInputFiles('#bios-input', openbios); await page.waitForTimeout(500);
await page.screenshot({ path: `${out}/ui-2-bios.png` });
await page.setInputFiles('#game-input', [testFile('demo.cue'), testFile('demo.bin')]);
await page.waitForTimeout(9000); await page.screenshot({ path: `${out}/ui-3-running.png` });
await page.click('#btn-settings'); await page.waitForTimeout(400); await page.screenshot({ path: `${out}/ui-4-settings.png` });
await close();
