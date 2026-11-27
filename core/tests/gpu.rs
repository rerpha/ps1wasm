//! GPU tests driven entirely through the GP0/GP1 command ports.

use ps1wasm_core::gpu::{Gpu, VRAM_W};
use ps1wasm_core::irq::Irq;

fn gpu() -> (Gpu, Irq) {
    let mut g = Gpu::new();
    let mut irq = Irq::default();
    g.gp0(0xE300_0000, &mut irq); // draw area TL (0,0)
    g.gp0(0xE400_0000 | 1023 | (511 << 10), &mut irq); // BR (1023,511)
    g.gp0(0xE500_0000, &mut irq); // offset 0
    (g, irq)
}

fn send(g: &mut Gpu, irq: &mut Irq, words: &[u32]) {
    for &w in words {
        g.gp0(w, irq);
    }
}

fn px(g: &Gpu, x: usize, y: usize) -> u16 {
    g.vram[y * VRAM_W + x]
}

fn xy(x: i32, y: i32) -> u32 {
    ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)
}

fn count_nonzero(g: &Gpu, x0: usize, y0: usize, w: usize, h: usize) -> usize {
    let mut n = 0;
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            if px(g, x, y) != 0 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn fill_rect_aligns_and_converts_color() {
    let (mut g, mut irq) = gpu();
    // color 0xFF8008 (B=FF,G=80,R=08) at x=17 -> aligned to 16, width 5 -> 16
    send(&mut g, &mut irq, &[0x02FF_8008, xy(17, 3), xy(5, 2)]);
    let expect = (0x08 >> 3) | ((0x80 >> 3) << 5) | ((0xFF >> 3) << 10);
    assert_eq!(px(&g, 16, 3), expect);
    assert_eq!(px(&g, 31, 4), expect);
    assert_eq!(px(&g, 32, 3), 0);
    assert_eq!(px(&g, 16, 5), 0);
}

#[test]
fn cpu_to_vram_and_back() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0xA000_0000, xy(100, 50), xy(3, 2)]);
    // 6 pixels -> 3 words
    send(&mut g, &mut irq, &[0x0002_0001, 0x0004_0003, 0x0006_0005]);
    assert_eq!(px(&g, 100, 50), 1);
    assert_eq!(px(&g, 102, 50), 3);
    assert_eq!(px(&g, 100, 51), 4);
    assert_eq!(px(&g, 102, 51), 6);
    assert_ne!(g.status() & (1 << 26), 0, "back to command mode");

    send(&mut g, &mut irq, &[0xC000_0000, xy(100, 50), xy(3, 2)]);
    assert_ne!(g.status() & (1 << 27), 0, "ready to send VRAM");
    assert_eq!(g.read_data(), 0x0002_0001);
    assert_eq!(g.read_data(), 0x0004_0003);
    assert_eq!(g.read_data(), 0x0006_0005);
    assert_eq!(g.status() & (1 << 27), 0);
}

#[test]
fn vram_to_vram_copy() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0xA000_0000, xy(0, 0), xy(2, 1), 0xBBBB_AAAA]);
    send(&mut g, &mut irq, &[0x8000_0000, xy(0, 0), xy(500, 300), xy(2, 1)]);
    assert_eq!(px(&g, 500, 300), 0xAAAA);
    assert_eq!(px(&g, 501, 300), 0xBBBB);
}

#[test]
fn flat_triangle_top_left_rule() {
    let (mut g, mut irq) = gpu();
    // (0,0) (4,0) (0,4): pixels with x+y<4 -> 10 pixels
    send(&mut g, &mut irq, &[0x2000_00FF, xy(10, 10), xy(14, 10), xy(10, 14)]);
    assert_eq!(count_nonzero(&g, 8, 8, 10, 10), 10);
    assert_ne!(px(&g, 10, 10), 0);
    assert_ne!(px(&g, 13, 10), 0);
    assert_eq!(px(&g, 14, 10), 0, "right vertex excluded");
    assert_eq!(px(&g, 10, 14), 0, "bottom vertex excluded");
}

#[test]
fn winding_does_not_matter() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0x2000_00FF, xy(10, 10), xy(10, 14), xy(14, 10)]);
    assert_eq!(count_nonzero(&g, 8, 8, 10, 10), 10);
}

#[test]
fn quad_covers_exact_rectangle_without_overlap() {
    let (mut g, mut irq) = gpu();
    // semi-transparent additive (mode 1) red quad: overlap would double values
    send(&mut g, &mut irq, &[0xE100_0020]); // semi mode 1
    send(&mut g, &mut irq, &[0x2A00_0040, xy(20, 20), xy(28, 20), xy(20, 26), xy(28, 26)]);
    assert_eq!(count_nonzero(&g, 18, 18, 14, 12), 8 * 6);
    for y in 20..26 {
        for x in 20..28 {
            assert_eq!(px(&g, x, y), 0x40 >> 3, "pixel ({},{})", x, y);
        }
    }
}

#[test]
fn draw_area_and_offset() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0xE300_0000 | 5 | (5 << 10), 0xE400_0000 | 9 | (9 << 10)]);
    send(&mut g, &mut irq, &[0xE500_0000 | 2 | (2 << 11)]); // offset (2,2)
    send(&mut g, &mut irq, &[0x6000_00FF, xy(0, 0), xy(20, 20)]);
    assert_eq!(count_nonzero(&g, 0, 0, 30, 30), 25);
    assert_ne!(px(&g, 5, 5), 0);
    assert_ne!(px(&g, 9, 9), 0);
    // negative offset
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0xE500_0000 | (0x7FF & (-3i32 as u32)) | ((0x7FF & (-3i32 as u32)) << 11)]);
    send(&mut g, &mut irq, &[0x6800_00FF, xy(10, 10)]);
    assert_ne!(px(&g, 7, 7), 0);
}

#[test]
fn gouraud_interpolates() {
    let (mut g, mut irq) = gpu();
    // red at left, blue at right, horizontal 32px wide strip triangle
    send(&mut g, &mut irq, &[0x3000_00F8, xy(0, 0), 0x00F8_0000, xy(32, 0), 0x0000_00F8, xy(0, 32)]);
    let p = px(&g, 0, 0);
    assert_eq!(p & 0x1F, 0x1F, "red at v0");
    let p = px(&g, 30, 0);
    assert!((p >> 10) & 0x1F >= 27, "mostly blue near v1: {:04x}", p);
    assert!(p & 0x1F <= 4);
}

#[test]
fn oversized_polygon_rejected() {
    let (mut g, mut irq) = gpu();
    // width 1024 (-1 .. 1023) is rejected
    send(&mut g, &mut irq, &[0x2000_00FF, xy(-1, 0), xy(1023, 0), xy(0, 5)]);
    assert_eq!(count_nonzero(&g, 0, 0, 1024, 8), 0);
    // height 512 is rejected
    send(&mut g, &mut irq, &[0x2000_00FF, xy(0, -1), xy(5, 0), xy(0, 511)]);
    assert_eq!(count_nonzero(&g, 0, 0, 8, 512), 0);
    // width 1023 is fine
    send(&mut g, &mut irq, &[0x2000_00FF, xy(0, 0), xy(1023, 0), xy(0, 5)]);
    assert!(count_nonzero(&g, 0, 0, 1024, 8) > 0);
}

fn upload(g: &mut Gpu, irq: &mut Irq, x: i32, y: i32, w: i32, h: i32, pixels: &[u16]) {
    send(g, irq, &[0xA000_0000, xy(x, y), xy(w, h)]);
    for pair in pixels.chunks(2) {
        let lo = pair[0] as u32;
        let hi = *pair.get(1).unwrap_or(&0) as u32;
        g.gp0(lo | (hi << 16), irq);
    }
}

#[test]
fn textured_rect_15bpp_raw_and_transparent_texels() {
    let (mut g, mut irq) = gpu();
    // 4x1 texture at texpage (64*1, 0): 15bpp
    upload(&mut g, &mut irq, 64, 0, 4, 1, &[0x001F, 0x0000, 0x03E0, 0x7C00]);
    send(&mut g, &mut irq, &[0xE100_0001 | (2 << 7)]); // texpage x=1, 15bpp
    send(&mut g, &mut irq, &[0x6500_0000, xy(200, 100), 0x0000_0000, xy(4, 1)]);
    assert_eq!(px(&g, 200, 100), 0x001F);
    assert_eq!(px(&g, 201, 100), 0, "texel 0 is transparent");
    assert_eq!(px(&g, 202, 100), 0x03E0);
    assert_eq!(px(&g, 203, 100), 0x7C00);
}

#[test]
fn textured_4bpp_with_clut_and_modulation() {
    let (mut g, mut irq) = gpu();
    // CLUT at (0, 480): idx1 = white-ish 0x7FFF, idx2 = 0x0010
    upload(&mut g, &mut irq, 0, 480, 4, 1, &[0, 0x7FFF, 0x0010, 0]);
    // 4bpp texture at page (128,0): one halfword = 4 texels: 1,2,1,0
    upload(&mut g, &mut irq, 128, 0, 1, 1, &[0x0121]);
    let clut = 480 << 6;
    send(&mut g, &mut irq, &[0xE100_0002]); // page x=2 (128), 4bpp
    // modulated by 0x80 (neutral)
    send(&mut g, &mut irq, &[0x6480_8080, xy(300, 10), (clut << 16), xy(4, 1)]);
    assert_eq!(px(&g, 300, 10), 0x7FFF);
    assert_eq!(px(&g, 301, 10), 0x0010);
    assert_eq!(px(&g, 302, 10), 0x7FFF);
    assert_eq!(px(&g, 303, 10), 0, "idx 0 -> transparent black");
    // modulation 0x40 halves
    send(&mut g, &mut irq, &[0x6440_4040, xy(300, 11), (clut << 16), xy(1, 1)]);
    assert_eq!(px(&g, 300, 11), 0x3DEF);
}

#[test]
fn textured_8bpp_triangle_maps_texels() {
    let (mut g, mut irq) = gpu();
    // CLUT row at y=490 with idx k -> value k+1
    let clut_vals: Vec<u16> = (0..256).map(|k| k as u16 + 1).collect();
    upload(&mut g, &mut irq, 0, 490, 256, 1, &clut_vals);
    // 8bpp texture page x=4 (256): 16x16 texels, texel(u,v) = u
    let mut tex = vec![];
    for _v in 0..16 {
        for u in (0..16).step_by(2) {
            tex.push((u as u16) | ((u as u16 + 1) << 8));
        }
    }
    upload(&mut g, &mut irq, 256, 0, 8, 16, &tex);
    let clut = 490 << 6;
    let page = 4 | (1 << 7);
    // raw-textured quad 16x16 at (400,0), uv 0..16
    send(&mut g, &mut irq, &[
        0x2D00_0000,
        xy(400, 0), (clut << 16),
        xy(416, 0), (page << 16) | 16,
        xy(400, 16), 16 << 8,
        xy(416, 16), 16 | (16 << 8),
    ]);
    for u in 0..16 {
        assert_eq!(px(&g, 400 + u, 5), u as u16 + 1, "u={}", u);
    }
}

#[test]
fn texture_window() {
    let (mut g, mut irq) = gpu();
    upload(&mut g, &mut irq, 64, 0, 2, 1, &[0x1111, 0x2222]);
    send(&mut g, &mut irq, &[0xE100_0001 | (2 << 7)]);
    // window: mask x = 0x1F (all bits above 3 forced to offset 0) -> u & 7
    send(&mut g, &mut irq, &[0xE200_001F]);
    send(&mut g, &mut irq, &[0x6500_0000, xy(0, 50), 0, xy(10, 1)]);
    // u=8 -> 0 , u=9 -> 1
    assert_eq!(px(&g, 8, 50), 0x1111);
    assert_eq!(px(&g, 9, 50), 0x2222);
}

#[test]
fn semi_transparency_modes() {
    let cases = [(0u32, 0x10, 0x08, 0x0C), (1, 0x10, 0x08, 0x18), (2, 0x10, 0x08, 0x08), (3, 0x10, 0x08, 0x12)];
    for (mode, bg, fg, expect) in cases {
        let (mut g, mut irq) = gpu();
        send(&mut g, &mut irq, &[0x0200_0000 | (bg << 3), xy(0, 0), xy(16, 1)]);
        send(&mut g, &mut irq, &[0xE100_0000 | (mode << 5)]);
        send(&mut g, &mut irq, &[0x6A00_0000 | (fg << 3), xy(0, 0)]);
        assert_eq!(px(&g, 0, 0) & 0x1F, expect as u16, "mode {}", mode);
    }
    // saturation
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0x0200_00F8, xy(0, 0), xy(16, 1)]);
    send(&mut g, &mut irq, &[0xE100_0020, 0x6A00_00F8, xy(0, 0)]);
    assert_eq!(px(&g, 0, 0) & 0x1F, 0x1F);
}

#[test]
fn mask_bit_set_and_check() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0xE600_0001]); // set mask
    send(&mut g, &mut irq, &[0x6800_00F8, xy(0, 0)]);
    assert_eq!(px(&g, 0, 0), 0x801F);
    send(&mut g, &mut irq, &[0xE600_0002]); // check mask
    send(&mut g, &mut irq, &[0x6800_F800, xy(0, 0), 0x6800_F800, xy(1, 0)]);
    assert_eq!(px(&g, 0, 0), 0x801F, "protected");
    assert_eq!(px(&g, 1, 0), 0x03E0);
}

#[test]
fn dithering_only_when_enabled_and_shaded() {
    let (mut g, mut irq) = gpu();
    // flat untextured: never dithered
    send(&mut g, &mut irq, &[0xE100_0200, 0x6000_0084, xy(0, 0), xy(4, 4)]);
    let flat: Vec<u16> = (0..4).map(|x| px(&g, x, 0)).collect();
    assert!(flat.iter().all(|&p| p == flat[0]));
    // gouraud with dither on: values vary across the 4x4 pattern
    // 0x87 = 135: dither offsets -4..+3 straddle the 136 rounding boundary
    send(&mut g, &mut irq, &[0x3800_0087, xy(10, 0), 0x87, xy(14, 0), 0x87, xy(10, 4), 0x87, xy(14, 4)]);
    let d: Vec<u16> = (10..14).map(|x| px(&g, x, 0)).collect();
    assert!(d.iter().any(|&p| p != d[0]), "{:?}", d);
}

#[test]
fn lines_and_polylines() {
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0x4000_00FF, xy(0, 0), xy(9, 0)]);
    assert_eq!(count_nonzero(&g, 0, 0, 20, 2), 10, "endpoints inclusive");
    // diagonal
    send(&mut g, &mut irq, &[0x4000_00FF, xy(0, 10), xy(5, 15)]);
    for k in 0..=5 {
        assert_ne!(px(&g, k, 10 + k), 0);
    }
    // polyline: 3 segments then terminator
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0x4800_00FF, xy(0, 0), xy(4, 0), xy(4, 4), xy(0, 4)]);
    assert_eq!(g.status() & (1 << 26), 0, "still in polyline mode");
    send(&mut g, &mut irq, &[0x5555_5555]);
    assert_eq!(g.status() & (1 << 26), 0, "busy rasterizing right after the draw");
    g.now = g.busy_until; // let the GPU finish (fill-rate timing model)
    assert_ne!(g.status() & (1 << 26), 0);
    assert_ne!(px(&g, 4, 2), 0);
    assert_ne!(px(&g, 2, 4), 0);
    // shaded polyline
    let (mut g, mut irq) = gpu();
    send(&mut g, &mut irq, &[0x5800_00FF, xy(0, 0), 0x00FF_0000, xy(8, 0), 0x0000_FF00, xy(8, 8), 0x5000_5000]);
    g.now = g.busy_until;
    assert_ne!(g.status() & (1 << 26), 0);
    assert_ne!(px(&g, 8, 4), 0);
}

#[test]
fn status_register_and_gp1() {
    let (mut g, mut irq) = gpu();
    let s = g.status();
    assert_ne!(s & (1 << 26), 0, "ready for command");
    assert_ne!(s & (1 << 28), 0, "ready for DMA");
    assert_ne!(s & (1 << 23), 0, "display disabled after reset");
    g.gp1(0x0300_0000, &mut irq);
    assert_eq!(g.status() & (1 << 23), 0);
    g.gp0(0xE100_0000 | 0x7FF, &mut irq);
    assert_eq!(g.status() & 0x7FF, 0x7FF);
    g.gp1(0x0800_0000 | 0x3F, &mut irq); // hres1=3 vres pal 24bit interlace
    let s = g.status();
    assert_eq!((s >> 17) & 3, 3);
    assert_ne!(s & (1 << 19), 0);
    assert_ne!(s & (1 << 20), 0);
    assert_ne!(s & (1 << 21), 0);
    assert_ne!(s & (1 << 22), 0);
    g.gp1(0x0400_0002, &mut irq);
    assert_eq!((g.status() >> 29) & 3, 2);
    // GPU info
    g.gp1(0x1000_0007, &mut irq);
    assert_eq!(g.read_data(), 2);
    g.gp0(0xE500_0000 | 5 | (7 << 11), &mut irq);
    g.gp1(0x1000_0005, &mut irq);
    assert_eq!(g.read_data(), 5 | (7 << 11));
    // GP1(00) resets
    g.gp1(0, &mut irq);
    assert_eq!(g.status() & 0x7FF, 0);
}

#[test]
fn gpu_irq_command() {
    let (mut g, mut irq) = gpu();
    g.gp0(0x1F00_0000, &mut irq);
    assert_ne!(g.status() & (1 << 24), 0);
    assert_ne!(irq.stat & 2, 0);
    g.gp1(0x0200_0000, &mut irq);
    assert_eq!(g.status() & (1 << 24), 0);
}

#[test]
fn vblank_timing_ntsc() {
    let (mut g, mut irq) = gpu();
    let mut cycles = 0u64;
    let mut vblanks = vec![];
    while vblanks.len() < 3 {
        let ev = g.tick(64, &mut irq);
        cycles += 64;
        if ev.vblank_start {
            vblanks.push(cycles);
        }
    }
    let frame = vblanks[2] - vblanks[1];
    let expect = 263.0 * 3413.0 * 7.0 / 11.0;
    assert!((frame as f64 - expect).abs() < 200.0, "frame = {} cycles, expected ~{}", frame, expect);
    assert_ne!(irq.stat & 1, 0);
    assert_eq!(g.frame_count, 3);
}

#[test]
fn dotclock_events() {
    let (mut g, mut irq) = gpu();
    g.gp1(0x0800_0001, &mut irq); // 320 wide -> divider 8
    let mut dots = 0;
    for _ in 0..700 {
        dots += g.tick(7, &mut irq).dots;
    }
    // 4900 cpu cycles * 11/7 = 7700 video clocks / 8 = 962
    assert!((dots as i32 - 962).abs() <= 1, "{}", dots);
}

#[test]
fn display_size() {
    let (mut g, mut irq) = gpu();
    assert_eq!(g.display_size(), (256, 240));
    g.gp1(0x0800_0001 | 0x24, &mut irq); // 320, vres + interlace
    assert_eq!(g.display_size(), (320, 480));
    g.gp1(0x0800_0040, &mut irq);
    assert_eq!(g.display_size().0, 368);
}

#[test]
fn render_display_15bit_and_24bit() {
    let (mut g, mut irq) = gpu();
    g.gp1(0x0300_0000, &mut irq); // display on
    g.gp1(0x0500_0000 | 100 | (20 << 10), &mut irq); // display start (100,20)
    g.vram[20 * VRAM_W + 100] = 0x7C1F; // magenta
    g.vram[20 * VRAM_W + 101] = 0x03E0; // green
    let mut out = vec![];
    let (w, h) = g.render_display(&mut out);
    assert_eq!((w, h), (256, 240));
    assert_eq!(&out[0..8], &[0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0xFF]);
    // 24-bit: bytes R0 G0 | B0 R1 | G1 B1
    g.gp1(0x0800_0010, &mut irq);
    g.vram[20 * VRAM_W + 100] = 0x2211;
    g.vram[20 * VRAM_W + 101] = 0x4433;
    g.vram[20 * VRAM_W + 102] = 0x6655;
    g.render_display(&mut out);
    assert_eq!(&out[0..8], &[0x11, 0x22, 0x33, 0xFF, 0x44, 0x55, 0x66, 0xFF]);
    // disabled display renders black
    g.gp1(0x0300_0001, &mut irq);
    g.render_display(&mut out);
    assert_eq!(&out[0..4], &[0, 0, 0, 0xFF]);
}

#[test]
fn texture_disable_requires_gp1_09() {
    let (mut g, mut irq) = gpu();
    g.gp0(0xE100_0800, &mut irq);
    assert_eq!(g.status() & (1 << 15), 0, "ignored when not allowed");
    g.gp1(0x0900_0001, &mut irq);
    g.gp0(0xE100_0800, &mut irq);
    assert_ne!(g.status() & (1 << 15), 0, "honoured once allowed");
    g.gp1(0x0000_0000, &mut irq);
    assert_eq!(g.status() & (1 << 15), 0, "reset clears it");
}

#[test]
fn fill_rate_timing_model() {
    // Drawing keeps the GPU busy in proportion to pixels drawn, at the rates
    // measured on hardware (ps1-tests gpu/bandwidth).
    let (mut g, mut irq) = gpu();
    g.gp0(0x6000_00FF, &mut irq);
    g.gp0(xy(0, 0), &mut irq);
    g.gp0(xy(100, 100), &mut irq);
    let flat = g.busy_until;
    assert!((flat as i64 - 10_000 * 560 / 1024).abs() <= 2, "flat rect: {}", flat);
    let (mut g, mut irq) = gpu();
    for w in [0x0200_00FF, xy(0, 0), xy(320, 240)] {
        g.gp0(w, &mut irq);
    }
    assert!((g.busy_until as i64 - 76_800 * 89 / 1024).abs() <= 2, "fill: {}", g.busy_until);
    // off-screen pixels cost nothing (clipped by the draw area)
    let (mut g, mut irq) = gpu();
    g.gp0(0xE400_0000 | 49 | (511 << 10), &mut irq); // draw area 50 wide
    g.gp0(0x6000_00FF, &mut irq);
    g.gp0(xy(0, 0), &mut irq);
    g.gp0(xy(100, 100), &mut irq);
    assert_eq!(g.busy_until, flat / 2);
}

#[test]
fn hblank_is_an_interval_outside_the_display_range() {
    let (mut g, mut irq) = gpu();
    g.gp1(0x0600_0000 | 0x260 | (0xC60 << 12), &mut irq); // display x 608..3168
    // walk one scanline in small steps; count events and time spent in hblank
    let (mut starts, mut ends, mut inside, mut total) = (0, 0, 0u32, 0u32);
    while total < 2172 * 3 {
        let ev = g.tick(4, &mut irq);
        starts += ev.hblanks;
        ends += ev.hblank_ends;
        if ev.in_hblank {
            inside += 4;
        }
        total += 4;
    }
    assert!((3..=4).contains(&starts) && (3..=4).contains(&ends), "one start/end per line: {} {}", starts, ends);
    // hblank = (3413 - 3168 + 608) video clocks per line = 853 -> ~543 CPU cycles
    let frac = inside as f64 / total as f64;
    assert!((frac - 853.0 / 3413.0).abs() < 0.02, "hblank fraction {}", frac);
}
