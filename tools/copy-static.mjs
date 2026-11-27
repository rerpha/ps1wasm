// Copies the static page into dist/ after wasm-pack has built dist/pkg.
import fs from 'node:fs';
fs.mkdirSync('dist', { recursive: true });
for (const f of fs.readdirSync('web/static')) fs.copyFileSync(`web/static/${f}`, `dist/${f}`);
// wasm-pack drops its own .gitignore into the output; Pages doesn't need it
fs.rmSync('dist/pkg/.gitignore', { force: true });
const wasm = fs.statSync('dist/pkg/ps1wasm_web_bg.wasm').size;
console.log(`dist/ ready (${(wasm / 1024).toFixed(0)} KB wasm). Run: npm run serve`);
