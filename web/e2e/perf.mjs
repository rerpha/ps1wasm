// Browser performance check: worst-case CPU load (cached ALU loop).
import { start, testFile } from './harness.mjs';
const { url, browser, close } = await start();
const page = await browser.newPage();
await page.goto(url);
await page.setInputFiles('#game-input', testFile('alu-bench.exe'));
await page.waitForTimeout(6000);
console.log('worst case (cached ALU loop):', await page.textContent('#fps'));
await close();
