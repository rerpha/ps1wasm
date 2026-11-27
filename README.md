# ps1wasm — a PS1 emulator in Rust, running in the browser

The whole emulator — CPU, GTE, GPU, SPU, CD-ROM, MDEC, DMA, timers, pads,
memory cards **and the browser UI** — is Rust compiled to WebAssembly. The only
JavaScript is the two-line module loader browsers require in `index.html`.

## Quick start

Needs [Rust](https://rustup.rs) with the wasm target and Node.js 18+:

```sh
rustup target add wasm32-unknown-unknown
npm install          # fetches wasm-pack (which fetches wasm-bindgen)
npm run build        # static export -> dist/ (index.html + pkg/*.wasm)
npm run serve        # http://localhost:8080
```

`dist/` is a fully static site: host it anywhere. Pushing to `master`
deploys it to **GitHub Pages** via `.github/workflows/pages.yml` (tests →
build → deploy). One-time setup: repository *Settings → Pages → Source:
GitHub Actions*. Note: Pages on a **private** repository requires a paid
GitHub plan; on a free plan make the repository public or host `dist/`
elsewhere.

Open the page, then **drop your BIOS and your game anywhere on it** (or use
the two “Choose…” buttons). The BIOS is recognised by its size (512 KB), a
game by its extension: select a `.cue` together with its `.bin` file(s), or a
single `.bin`/`.iso`, or a PS-X `.exe`. The game starts automatically once
both are present. Nothing is uploaded anywhere.

* **Remembered between visits:** BIOS, settings, key bindings, memory cards,
  save states, and the last game (“Continue” button).
* **Save states:** 4 slots per game (💾/📂 or <kbd>F5</kbd>/<kbd>F9</kbd>); a
  dropped `.psxstate` file also loads.
* **Controls:** keyboard (remappable in ⚙ Settings) or any standard gamepad,
  including analog sticks. DualShock emulation with the Analog button
  (<kbd>F2</kbd>); switch to a digital pad in Settings.
* **Memory cards:** export/import/format in Settings (`.mcr`/`.mcd`, DexDrive
  `.gme`, PSP `.vmp`); dropping a card file imports it into slot 1.
* <kbd>Esc</kbd> pause · hold <kbd>Tab</kbd> fast-forward · ⛶ fullscreen ·
  volume slider · smooth scaling / stretch options.

You must supply your own BIOS and games; none are included.

## Layout

| Path | What |
|---|---|
| `core/` | Emulator core (no external dependencies, platform-neutral) |
| `core/src/cpu.rs` | R3000A interpreter: delay slots, load delays, COP0, exceptions |
| `core/src/gte.rs` | Geometry engine (COP2) with hardware UNR division and flags |
| `core/src/gpu.rs` | Software rasterizer, GP0/GP1, VRAM transfers, video timing |
| `core/src/spu.rs` | 24 ADPCM voices, ADSR, sweeps, noise, PMON, reverb, IRQ |
| `core/src/cdrom.rs` + `disc.rs` | CD controller, XA-ADPCM, CD-DA, CUE parsing |
| `core/src/mdec.rs` | Macroblock decoder (FMV) |
| `core/src/dma.rs`, `timers.rs`, `pad.rs`, `irq.rs`, `bus.rs` | the rest |
| `web/` | Browser frontend in Rust (`wasm-bindgen` + `web-sys`); page in `web/static/` |
| `cli/` | `ps1wasm-run` headless runner, `ps1wasm-mktest`, test kit (demo EXE + ISO9660 builder) |
| `tools/` | test-asset fetcher, hardware test-suite runners, static build/serve helpers |

## Testing and verification

```sh
npm test                           # cargo test --workspace --release: 212 tests
tools/fetch-test-assets.sh         # OpenBIOS + ps1-tests + PeterLemon/PSX
tools/run-vram-tests.sh            # VRAM vs real-hardware dumps (detailed report)
tools/run-ps1-tests.sh             # TTY logs vs real-hardware logs
tools/diff-redux-cpu.sh 1 50 800   # differential CPU fuzzing vs PCSX-Redux
npm run build && npm run build:demo && npm run test:e2e   # 35 browser UI checks
```

Checked against independent sources, from most to least authoritative:

| Source | What | Result |
|---|---|---|
| ps1-tests **real-hardware VRAM dumps** | 11 GPU tests | 9 pixel-perfect (`clipping`, `clut-cache`, `quad`, `rectangles`, `texture-flip`, `texture-overflow`, `triangle`, `uv-interpolation`, `vram-to-vram-overlap`); `transparency` exact in drawn area (dump from older test build); `lines` 64 px differ |
| ps1-tests real-hardware MDEC dumps | full 320x240 frame | correct picture; only 1-LSB rounding differences |
| ps1-tests **hardware GTE fuzz log** | 1150 random GTE ops, all 64 regs | **1150/1150 bit-exact** |
| ps1-tests hardware logs | CPU/GTE/DMA/GPU assertion tests | `gte/test-all` 1150/1150, `cpu/cop`, `dma/dpcr`, `dma/otc-test`, `gpu/gp0-e1`, `gpu/mask-bit` |
| **PCSX-Redux** (differential) | 150 random CPU programs × 800 ops | **150/150 identical** registers + memory |
| PeterLemon/PSX (emulator screenshots) | 72 programs | all 31 CPU/GTE tests + 9 rect/texture-window tests structurally identical; LZ77/Huffman/image decoders byte-exact (4×307200 px) |
| OpenBIOS (MIT) boot | kernel, EXE, CD boot via ISO9660 | pass, natively and in Chromium |

Bugs found by this cross-checking and fixed: COP0/1/3 exception rules,
GTE MAC/IR/SZ precision and invalid-opcode FLAG handling, texture-disable
gating, scratchpad fetch faults, X-flipped sprite texel offset, line stepping,
dithering of flat lines, polygon attribute interpolation, CLUT cache,
MDEC block termination (FMVs desynced), MDEC output order, and load-delay
cancellation.

Timing vs hardware measurements (`cli/tests/timing.rs`):

| Measurement | Result |
|---|---|
| CPU access time, 17 regions × 3 widths (`cpu/access-time`) | integer cycle counts match |
| Timer clock sources + delay loops (`timers`) | within 1.2% (1013 vs 1011 cycles) |
| GPU DMA, 130 block/chopping configurations (`dma/chopping`) | mean error 2.0%, 129/130 within 10% |
| GPU fill rates, 14 primitive types (`gpu/bandwidth`) | all within 6.2% |
| CPU loop with RAM stores, DMA-hogged bus (`dma/chain-looping`) | exact / 1.6%, flags exact |
| SPU DMA speed (`spu/memory-transfer`) | all assertions pass |
| Timer counting sequence (`timer-dump`, per-clock hardware dump) | period target+1 reproduced |

Not matching hardware: MDEC IDCT rounding (±1 LSB), `code-in-io`,
`io-access-bitwidth`, MDEC data-register read-out timing; timer sync-mode
sample phases (depend on boot timing: Sony BIOS vs OpenBIOS); `cdrom/*`
need a specific disc.

## Known limitations (honest list)

* **Not yet verified with a Sony BIOS or a commercial game** — those can't
  be distributed. OpenBIOS + homebrew pass; real games will surface bugs.
* **Timing is modelled, not cycle-exact.** The CPU has an instruction-cache
  model, per-region memory costs, a write buffer and mult/div/GTE latencies.
  DMA has real durations, stalls and chopping, and the GPU has a fill rate.
  All of it is calibrated against hardware measurements (see the table
  above). Not modelled: data-cache/scratchpad conflicts, exact GPU command
  FIFO behaviour, CD-ROM seek/read timing beyond simple constants (the
  `cdrom/timing` test needs a disc), and GTE timings are from documentation,
  not measured.
* MDEC IDCT differs from hardware by 1–2 LSB in some pixels (invisible).
* SPU uses linear instead of Gaussian interpolation; no SPU "sound map".
* No rumble output, multitap or light guns.
* Browser loads the whole disc image into memory (~700 MB for a full CD).
* Interlaced 480i output shows both fields without deinterlacing.

Performance: worst case (tight cached CPU loop, every cycle an instruction)
runs at 3.2× real time natively and costs 7.5 ms of the 16.9 ms frame budget
in Chromium (`node web/e2e/perf.mjs`).

## Legal

ps1wasm is an independent project written from public documentation
(psx-spx) and black-box testing of hardware behaviour. It is **not affiliated
with or endorsed by Sony Interactive Entertainment**; “PlayStation” is a
trademark of Sony Interactive Entertainment Inc., used here only to describe
what the software is compatible with.

* **No copyrighted Sony material is included or distributed** — no Sony
  BIOS, no commercial games. To use a Sony BIOS or commercial games you must
  supply your own dumps, and use only ones you own.
* **OpenBIOS** (the “Use OpenBIOS” button) is bundled in `web/static/openbios/`:
  an open-source BIOS replacement, © PCSX-Redux authors, **MIT licence**
  ([source](https://github.com/pcsx-redux/nugget/tree/main/openbios)),
  shipped unmodified with its licence text. All of its source files are MIT
  and it links no other libraries. It runs homebrew; commercial games may
  not boot with it.
* The test tooling downloads further third-party material at test time
  (never committed): the ps1-tests and PeterLemon/PSX homebrew suites with
  their reference results.
* Some algorithms (GPU attribute interpolation and line stepping) follow
  hardware behaviour as documented by and measured against Mednafen and
  DuckStation; no code was copied from them.

Licensed under the [MIT licence](LICENSE).

