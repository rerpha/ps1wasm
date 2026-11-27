# ps1wasm

> **⚠️ Please don't take this seriously.** This project is purely the output
> of my personal experimenting with vibe coding (AI-assisted programming).
> It isn't maintained, reviewed or supported, and nobody should really use
> it or rely on it for anything.

A PS1 emulator written in Rust and compiled to WebAssembly, so it runs in a
web browser.

**Try it:** https://rerpha.github.io/ps1wasm/ — press **Try the demo** to see
the controller test, or **Use OpenBIOS** and drop in a homebrew game.

## Run it locally

Needs [Rust](https://rustup.rs) and Node.js 18+:

```sh
rustup target add wasm32-unknown-unknown
npm install
npm run build
npm run serve        # http://localhost:8080
```

`npm test` runs the Rust tests; `npm run test:e2e` runs the browser tests
(after `npm run build:demo`).

## Using it

- Drop a BIOS and a game anywhere on the page, or use the buttons. Games:
  a `.cue` with its `.bin` files, a `.bin`/`.iso`, or a PS-X executable.
- **Use OpenBIOS** loads a bundled open-source BIOS, which works for
  homebrew; commercial games may not boot with it.
- Controls: keyboard (remappable in Settings), gamepads, or on-screen touch
  controls on phones and tablets.
- Save states, memory cards and your last game are remembered in the
  browser.

## Legal

- Not affiliated with or endorsed by Sony Interactive Entertainment.
  “PlayStation” is a trademark of Sony Interactive Entertainment Inc.
- No Sony BIOS and no commercial games are included. If you use them, use
  only your own.
- [OpenBIOS](https://github.com/pcsx-redux/nugget/tree/main/openbios) is
  bundled unmodified in `web/static/openbios/` — © PCSX-Redux authors, MIT
  licence (text included there).
- Written from public documentation (psx-spx) and testing against hardware
  results; no code was copied from other emulators.

Licensed under the [MIT licence](LICENSE).
