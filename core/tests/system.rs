//! Whole-system tests: CPU programs and DMA transfers exercising devices
//! through the real bus and memory map.

mod common;
use common::*;

use ps1wasm_core::bus::Size;
use ps1wasm_core::gpu::VRAM_W;
use ps1wasm_core::pad::buttons;
use ps1wasm_core::Psx;

const BASE: u32 = 0x8001_0000;
const IO: u32 = 0x1F80_0000;

fn load(p: &mut Psx, addr: u32, prog: &[u32]) {
    for (k, w) in prog.iter().enumerate() {
        p.bus.write(addr + 4 * k as u32, *w, Size::Word);
    }
}

fn io_w(p: &mut Psx, off: u32, v: u32) {
    p.bus.write(0x1F80_0000 | off, v, Size::Word);
}
fn io_r(p: &mut Psx, off: u32) -> u32 {
    p.bus.read(0x1F80_0000 | off, Size::Word)
}

#[test]
fn cpu_program_draws_via_gp0_port() {
    let mut p = Psx::new();
    let prog = cat(&[
        &li(T1, IO),
        &li(T0, 0xE400_0000 | 1023 | (511 << 10)), &[sw(T0, 0x1810, T1)],
        &li(T0, 0x0200_00F8), &[sw(T0, 0x1810, T1)], // fill rect, red
        &li(T0, 0x0000_0000), &[sw(T0, 0x1810, T1)],
        &li(T0, 0x0010_0010), &[sw(T0, 0x1810, T1)],
        &[j(BASE + 4 * 15), NOP],
    ]);
    load(&mut p, BASE, &prog);
    p.cpu.set_pc(BASE);
    p.run_instructions(40);
    assert_eq!(p.bus.gpu.vram[5 * VRAM_W + 5], 0x1F);
    assert_ne!(io_r(&mut p, 0x1814) & (1 << 26), 0, "GPUSTAT readable via bus");
}

#[test]
fn otc_dma_builds_ordering_table() {
    let mut p = Psx::new();
    io_w(&mut p, 0x10F0, 0x0800_0000); // enable ch6
    io_w(&mut p, 0x10E0, 0x0010_0010); // MADR = end of table
    io_w(&mut p, 0x10E4, 4); // 4 entries
    io_w(&mut p, 0x10E8, 0x1100_0002); // start + trigger
    assert_eq!(p.bus.read(0x0010_0010, Size::Word), 0x0010_000C);
    assert_eq!(p.bus.read(0x0010_000C, Size::Word), 0x0010_0008);
    assert_eq!(p.bus.read(0x0010_0008, Size::Word), 0x0010_0004);
    assert_eq!(p.bus.read(0x0010_0004, Size::Word), 0x00FF_FFFF);
    assert_eq!(io_r(&mut p, 0x10E8) & (1 << 24), 0, "channel finished");
}

#[test]
fn gpu_linked_list_dma_draws() {
    let mut p = Psx::new();
    let gp0 = |p: &mut Psx, w: u32| io_w(p, 0x1810, w);
    gp0(&mut p, 0xE400_0000 | 1023 | (511 << 10));
    // node A at 0x1000 -> node B at 0x2000 (end). Each carries one packet.
    let a = [0x0300_2000u32, 0x6000_F800, 0x0000_0000, 0x0004_0004]; // green 4x4 rect at (0,0)
    let b = [0x02FF_FFFFu32, 0x6800_001F, 0x0000_0020]; // 1x1 red-ish at (32,0)
    for (k, w) in a.iter().enumerate() {
        p.bus.write(0x1000 + 4 * k as u32, *w, Size::Word);
    }
    for (k, w) in b.iter().enumerate() {
        p.bus.write(0x2000 + 4 * k as u32, *w, Size::Word);
    }
    io_w(&mut p, 0x10F0, 0x0000_0800); // enable ch2
    io_w(&mut p, 0x1814, 0x0400_0002); // GP1: DMA direction CPU->GP0
    io_w(&mut p, 0x10A0, 0x1000);
    io_w(&mut p, 0x10A8, 0x0100_0401); // linked list, from RAM, start
    assert_eq!(p.bus.gpu.vram[3 * VRAM_W + 3], 0x03E0, "green 0xF8 -> 0x1F << 5");
    assert_eq!(p.bus.gpu.vram[32], 0x1F >> 3);
    assert_eq!(io_r(&mut p, 0x10A0), 0x00FF_FFFF);
}

#[test]
fn gpu_block_dma_uploads_and_reads_vram() {
    let mut p = Psx::new();
    // CPU->VRAM command header via GP0, pixel data via DMA2 sync mode 1
    for w in [0xA000_0000u32, 0x0000_0010, 0x0002_0004] {
        io_w(&mut p, 0x1810, w);
    }
    for k in 0..4u32 {
        p.bus.write(0x3000 + 4 * k, 0x1111_0000 * (k + 1) + k, Size::Word);
    }
    io_w(&mut p, 0x10F0, 0x0000_0800);
    io_w(&mut p, 0x10A0, 0x3000);
    io_w(&mut p, 0x10A4, 0x0001_0004); // 1 block of 4 words
    io_w(&mut p, 0x10A8, 0x0100_0201);
    assert_eq!(p.bus.gpu.vram[16], 0);
    assert_eq!(p.bus.gpu.vram[17], 0x1111);
    assert_eq!(io_r(&mut p, 0x10A0), 0x3010, "MADR advanced");
    // read it back with DMA to RAM
    for w in [0xC000_0000u32, 0x0000_0010, 0x0002_0004] {
        io_w(&mut p, 0x1810, w);
    }
    io_w(&mut p, 0x10A0, 0x4000);
    io_w(&mut p, 0x10A4, 0x0001_0004);
    io_w(&mut p, 0x10A8, 0x0100_0200);
    for k in 0..4u32 {
        assert_eq!(p.bus.read(0x4000 + 4 * k, Size::Word), p.bus.read(0x3000 + 4 * k, Size::Word));
    }
}

#[test]
fn dma_interrupt_reaches_i_stat() {
    let mut p = Psx::new();
    io_w(&mut p, 0x10F4, (1 << 23) | (1 << (16 + 6)));
    io_w(&mut p, 0x10F0, 0x0800_0000);
    io_w(&mut p, 0x10E0, 0x100);
    io_w(&mut p, 0x10E4, 1);
    io_w(&mut p, 0x10E8, 0x1100_0002);
    assert_ne!(io_r(&mut p, 0x1070) & (1 << 3), 0);
    assert_ne!(io_r(&mut p, 0x10F4) & (1 << 30), 0, "ch6 flag");
}

#[test]
fn vblank_interrupt_runs_cpu_handler() {
    let mut p = Psx::new();
    let counter = 0x8002_0000u32;
    let handler = cat(&[
        &[lui(K0, counter >> 16), lw(K1, 0, K0), NOP, addiu(K1, K1, 1), sw(K1, 0, K0)],
        &[lui(K0, IO >> 16), sw(ZERO, 0x1070, K0)],
        &[mfc0(K0, 14), NOP, jr(K0), rfe()],
    ]);
    load(&mut p, 0x8000_0080, &handler);
    let main = cat(&[
        &li(T1, IO), &[addiu(T0, ZERO, 1), sw(T0, 0x1074, T1)],
        &[addiu(T0, ZERO, 0x401), mtc0(T0, 12)],
        &[j(BASE + 4 * 6), NOP],
    ]);
    load(&mut p, BASE, &main);
    p.cpu.cop0.sr = 0;
    p.cpu.set_pc(BASE);
    for _ in 0..5 {
        p.run_frame();
    }
    p.run_instructions(200);
    assert_eq!(p.bus.read(counter, Size::Word), 5);
}

#[test]
fn frame_length_matches_ntsc_timing() {
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    p.run_frame();
    let c0 = p.bus.cycles;
    p.run_frame();
    let frame = (p.bus.cycles - c0) as f64;
    assert!((frame - 571_212.0).abs() < 600.0, "{}", frame);
}

#[test]
fn timers_count_through_bus_and_icache_timing() {
    // Uncached RAM fetches cost the RAM access time (5 cycles) each.
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    io_w(&mut p, 0x1124, 0); // timer 2 mode reset
    p.run_instructions(500);
    let t = io_r(&mut p, 0x1120);
    assert!((t as i32 - 2500).abs() <= 20, "uncached: {}", t);

    // With the instruction cache enabled a loop costs 1 cycle per instruction.
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    p.bus.write(0xFFFE_0130, 0x800, Size::Word);
    io_w(&mut p, 0x1124, 0);
    p.run_instructions(500);
    let t = io_r(&mut p, 0x1120);
    assert!((t as i32 - 500).abs() <= 20, "cached: {}", t);

    // Same loop from KSEG1 (uncached segment) ignores the cache.
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(0xA001_0000), NOP]);
    p.cpu.set_pc(0xA001_0000);
    p.bus.write(0xFFFE_0130, 0x800, Size::Word);
    io_w(&mut p, 0x1124, 0);
    p.run_instructions(500);
    let t = io_r(&mut p, 0x1120);
    assert!((t as i32 - 2500).abs() <= 20, "kseg1: {}", t);
}

#[test]
fn load_and_muldiv_timing() {
    // RAM load = 5 cycles, scratchpad load = 1, DIV result stalls MFLO to 36.
    let run = |prog: &[u32]| -> u64 {
        let mut p = Psx::new();
        load(&mut p, BASE, prog);
        p.cpu.set_pc(BASE);
        p.bus.write(0xFFFE_0130, 0x800, Size::Word);
        p.run_instructions(prog.len() as u64); // warm the cache
        p.cpu.set_pc(BASE);
        let t0 = p.bus.now();
        p.run_instructions(prog.len() as u64);
        p.bus.now() - t0
    };
    let base = run(&[NOP, NOP, NOP, NOP]);
    assert_eq!(base, 4);
    let ram = run(&cat(&[&[lui(T1, 0x8002), lw(T0, 0, T1)], &[NOP, NOP]]));
    assert_eq!(ram, 4 + 4, "RAM load adds 4 cycles");
    let spad = run(&cat(&[&[lui(T1, 0x1F80), lw(T0, 0, T1)], &[NOP, NOP]]));
    assert_eq!(spad, 4, "scratchpad load is free");
    let div = run(&[addiu(T0, ZERO, 7), r(0x1A, T0, T0, 0, 0), r(0x12, 0, 0, T1, 0), NOP]);
    assert!((36..=40).contains(&div), "mflo waits for div: {}", div);
}

#[test]
fn spu_dma_write_lands_in_spu_ram() {
    let mut p = Psx::new();
    for k in 0..8u32 {
        p.bus.write(0x5000 + 4 * k, 0xA000_0000 + k, Size::Word);
    }
    p.bus.write(0x1F80_1DA6, 0x100, Size::Half); // transfer addr 0x800
    p.bus.write(0x1F80_1DAA, 0xC020, Size::Half); // DMA write mode
    io_w(&mut p, 0x10F0, 0x0008_0000);
    io_w(&mut p, 0x10C0, 0x5000);
    io_w(&mut p, 0x10C4, 0x0001_0008);
    io_w(&mut p, 0x10C8, 0x0100_0201);
    assert_eq!(&p.bus.spu.ram[0x800..0x804], &[0x00, 0x00, 0x00, 0xA0]);
    assert_eq!(&p.bus.spu.ram[0x81C..0x820], &[0x07, 0x00, 0x00, 0xA0]);
    // SPU registers via 16-bit bus accesses
    assert_eq!(p.bus.read(0x1F80_1DAA, Size::Half), 0xC020);
    assert_eq!(p.bus.read(0x1F80_1DAE, Size::Half) & 0x3F, 0x20);
}

#[test]
fn cdrom_command_through_bus_and_irq() {
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    p.bus.write(0x1F80_1800, 1, Size::Byte);
    p.bus.write(0x1F80_1802, 0x1F, Size::Byte); // IE
    p.bus.write(0x1F80_1800, 0, Size::Byte);
    p.bus.write(0x1F80_1802, 0x20, Size::Byte);
    p.bus.write(0x1F80_1801, 0x19, Size::Byte); // Test(20h)
    p.run_instructions(60_000);
    assert_ne!(io_r(&mut p, 0x1070) & (1 << 2), 0, "CDROM IRQ");
    let resp: Vec<u8> = (0..4).map(|_| p.bus.read(0x1F80_1801, Size::Byte) as u8).collect();
    assert_eq!(resp, vec![0x94, 0x09, 0x19, 0xC0]);
}

#[test]
fn controller_read_through_bus_with_cpu_running() {
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    p.bus.sio.ports[0].buttons = buttons::CIRCLE;
    p.bus.write(0x1F80_104A, 0x1003, Size::Half);
    let mut replies = vec![];
    for b in [0x01u8, 0x42, 0x00, 0x00, 0x00] {
        p.bus.write(0x1F80_1040, b as u32, Size::Byte);
        p.run_instructions(1000);
        replies.push(p.bus.read(0x1F80_1040, Size::Byte) as u8);
    }
    assert_eq!(replies, vec![0xFF, 0x41, 0x5A, 0xFF, !(buttons::CIRCLE >> 8) as u8]);
}

#[test]
fn mdec_via_dma_channels() {
    let mut p = Psx::new();
    // set luma quant table = all 1 via MDEC command port, then decode with DMA0/1
    io_w(&mut p, 0x1820, 0x4000_0000);
    for _ in 0..16 {
        io_w(&mut p, 0x1820, 0x0101_0101);
    }
    io_w(&mut p, 0x1820, 0x6000_0000);
    for _ in 0..32 {
        io_w(&mut p, 0x1820, 0x5A82_5A82); // flat scale rows: DC-only usable
    }
    // stream: DC 256, end
    p.bus.write(0x6000, (1 << 10) | 0x100 | (0xFE00 << 16), Size::Word);
    io_w(&mut p, 0x1820, 0x2800_0000 | (1 << 26) | 1);
    io_w(&mut p, 0x1824, 0x6000_0000);
    io_w(&mut p, 0x10F0, 0x0000_0088);
    // command already sent; feed the data word with DMA0
    io_w(&mut p, 0x1080, 0x6000);
    io_w(&mut p, 0x1084, 0x0001_0001);
    io_w(&mut p, 0x1088, 0x0100_0201);
    io_w(&mut p, 0x1090, 0x7000);
    io_w(&mut p, 0x1094, 0x0001_0010);
    io_w(&mut p, 0x1098, 0x0100_0200);
    let w = p.bus.read(0x7000, Size::Word);
    assert_ne!(w, 0, "decoded output reached RAM");
    assert_eq!(io_r(&mut p, 0x1824) & (1 << 31), 1 << 31, "output drained");
}

#[test]
fn unmapped_and_fallback_registers() {
    let mut p = Psx::new();
    io_w(&mut p, 0x1060, 0x0000_0B88); // RAM_SIZE
    assert_eq!(io_r(&mut p, 0x1060), 0x0B88);
    io_w(&mut p, 0x1000, 0x1F00_0000); // MEM_CTRL exp1 base
    assert_eq!(io_r(&mut p, 0x1000), 0x1F00_0000);
}

#[test]
fn reset_keeps_bios_disc_and_cards() {
    use ps1wasm_core::disc::{Disc, MemReader};
    let mut p = Psx::new();
    p.bus.bios[0] = 0xAB;
    let img = vec![0u8; 2352 * 4];
    p.insert_disc(Disc::from_single(img.len() as u64, true, Box::new(MemReader(vec![img]))).unwrap());
    let mut card = vec![0u8; 128 * 1024];
    card[5] = 0x77;
    p.set_memcard(1, card);
    p.bus.ram[0] = 1;
    p.reset();
    assert_eq!(p.bus.ram[0], 0, "RAM cleared");
    assert_eq!(p.bus.bios[0], 0xAB);
    assert!(p.bus.cdrom.disc.is_some());
    assert_eq!(p.memcard(1).unwrap()[5], 0x77);
    assert_eq!(p.cpu.pc, 0xBFC0_0000);
}

#[test]
fn audio_is_produced_and_drained() {
    let mut p = Psx::new();
    load(&mut p, BASE, &[j(BASE), NOP]);
    p.cpu.set_pc(BASE);
    p.run_frame();
    p.run_frame();
    let mut out = vec![];
    p.take_audio(&mut out);
    // ~735 stereo frames per NTSC frame (44100 / 59.29 = 744)
    let frames = out.len() / 2;
    assert!(frames > 1000 && frames < 1600, "{}", frames);
    let mut again = vec![];
    p.take_audio(&mut again);
    assert!(again.is_empty());
}
