// Copies the static page (and its assets, e.g. openbios/) into dist/ after
// wasm-pack has built dist/pkg.
import fs from 'node:fs';
fs.mkdirSync('dist', { recursive: true });
fs.cpSync('web/static', 'dist', { recursive: true });
// wasm-pack drops its own .gitignore into the output; Pages doesn't need it
fs.rmSync('dist/pkg/.gitignore', { force: true });
const wasm = fs.statSync('dist/pkg/ps1wasm_web_bg.wasm').size;
console.log(`dist/ ready (${(wasm / 1024).toFixed(0)} KB wasm). Run: npm run serve`);
