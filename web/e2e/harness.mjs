// Shared setup for the browser scripts: serve dist/ and launch Chromium.
// Test content comes from the build: dist/test-files (npm run build:demo)
// and the bundled dist/openbios/openbios.bin.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { serve } from '../../tools/serve.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
export const dist = path.resolve(process.env.DIST ?? path.join(here, '../../dist'));
export const testFile = (name) => path.join(dist, 'test-files', name);
export const openbios = path.join(dist, 'openbios', 'openbios.bin');

/** Start the server and browser. Returns { url, browser, close }. */
export async function start(launchArgs = []) {
  const server = await serve(dist, 0, '127.0.0.1');
  const url = `http://localhost:${server.address().port}/`;
  // Use Playwright's downloaded Chromium if present, else its default lookup.
  const cache = path.join(os.homedir(), '.cache/ms-playwright');
  const executablePath = fs.existsSync(cache)
    ? fs.readdirSync(cache).filter(d => d.startsWith('chromium-'))
        .map(d => path.join(cache, d, 'chrome-linux64/chrome')).find(fs.existsSync)
    : undefined;
  const browser = await chromium.launch({ executablePath, args: launchArgs });
  return { url, browser, close: async () => { await browser.close(); server.close(); } };
}
