//! GTE tests: values derived by hand from the psx-spx fixed-point formulas.

use ps1wasm_core::gte::{unr_table, Gte};

fn cmd(op: u32, sf: bool, lm: bool) -> u32 {
    op | ((sf as u32) << 19) | ((lm as u32) << 10)
}
fn mvmva(sf: bool, mx: u32, v: u32, cv: u32) -> u32 {
    0x12 | ((sf as u32) << 19) | (mx << 17) | (v << 15) | (cv << 13)
}

fn identity(g: &mut Gte) {
    g.write_ctrl(0, 0x1000);
    g.write_ctrl(1, 0);
    g.write_ctrl(2, 0x1000);
    g.write_ctrl(3, 0);
    g.write_ctrl(4, 0x1000);
}

fn sxy2(g: &mut Gte) -> (i16, i16) {
    let v = g.read_data(14);
    (v as i16, (v >> 16) as i16)
}

#[test]
fn unr_table_endpoints() {
    assert_eq!(unr_table(0), 0xFF);
    assert_eq!(unr_table(256), 0x00);
    assert_eq!(unr_table(1), 0xFD);
}

#[test]
fn division_is_accurate() {
    let mut g = Gte::new();
    for &sz in &[1u32, 2, 3, 7, 100, 999, 1000, 4096, 0x7FFF, 0x8000, 0xFFFF] {
        for &h in &[0u32, 1, sz / 2, sz, (2 * sz).saturating_sub(1)] {
            if h >= 2 * sz {
                continue;
            }
            let got = g.divide(h, sz) as i64;
            let exact = ((h as i64) << 16) / sz as i64;
            let tol = 2.max(exact >> 14);
            assert!((got - exact).abs() <= tol, "h={} sz={} got={} exact={}", h, sz, got, exact);
        }
    }
    g.flag = 0;
    assert_eq!(g.divide(2000, 1000), 0x1FFFF);
    assert_ne!(g.flag & (1 << 17), 0, "divide overflow flag");
}

#[test]
fn rtps_perspective_projection() {
    let mut g = Gte::new();
    identity(&mut g);
    g.write_ctrl(7, 1000); // TRZ
    g.write_ctrl(24, 160 << 16); // OFX
    g.write_ctrl(25, 120 << 16); // OFY
    g.write_ctrl(26, 1000); // H
    g.write_ctrl(28, 0x0080_0000); // DQB
    g.write_data(0, (50 << 16) | 100); // VX=100 VY=50
    g.write_data(1, 0);
    g.command(cmd(0x01, true, false));
    assert_eq!(g.mac[1..], [100, 50, 1000]);
    assert_eq!(g.ir[1..], [100, 50, 1000]);
    assert_eq!(g.read_data(19), 1000, "SZ3");
    let (sx, sy) = sxy2(&mut g);
    assert!((sx as i32 - 260).abs() <= 1 && (sy as i32 - 170).abs() <= 1, "({},{})", sx, sy);
    assert_eq!(g.read_data(8), 0x800, "IR0 from DQB");
    assert_eq!(g.read_ctrl(31), 0, "no flags");
}

#[test]
fn rtpt_fills_fifos() {
    let mut g = Gte::new();
    identity(&mut g);
    g.write_ctrl(7, 500);
    g.write_ctrl(26, 500);
    for i in 0..3u32 {
        g.write_data(i * 2, i * 10);
        g.write_data(i * 2 + 1, 0);
    }
    g.command(cmd(0x30, true, false));
    let xs: Vec<i16> = (12..15).map(|r| g.read_data(r) as i16).collect();
    for (i, x) in xs.iter().enumerate() {
        assert!((*x as i32 - 10 * i as i32).abs() <= 1, "{:?}", xs);
    }
    assert_eq!(g.read_data(17), 500);
    assert_eq!(g.read_data(18), 500);
    assert_eq!(g.read_data(19), 500);
}

#[test]
fn rtps_screen_saturation_and_z_flags() {
    let mut g = Gte::new();
    identity(&mut g);
    g.write_ctrl(7, 10);
    g.write_ctrl(26, 1);
    g.write_data(0, 30000);
    g.write_data(1, 0);
    g.command(cmd(0x01, true, false));
    let (sx, _) = sxy2(&mut g);
    assert_eq!(sx, 0x3FF);
    let f = g.read_ctrl(31);
    assert_ne!(f & (1 << 14), 0, "SX2 saturated");
    assert_ne!(f & (1 << 31), 0, "summary bit");
    // negative Z -> SZ3 saturates to 0
    g.write_ctrl(7, -10i32 as u32);
    g.command(cmd(0x01, true, false));
    assert_eq!(g.read_data(19), 0);
    assert_ne!(g.read_ctrl(31) & (1 << 18), 0);
}

#[test]
fn nclip_winding() {
    let mut g = Gte::new();
    g.write_data(12, 0);
    g.write_data(13, 10);
    g.write_data(14, 10 << 16);
    g.command(cmd(0x06, false, false));
    assert_eq!(g.mac[0], 100);
    g.write_data(13, 10 << 16);
    g.write_data(14, 10);
    g.command(cmd(0x06, false, false));
    assert_eq!(g.mac[0], -100);
}

#[test]
fn avsz3_avsz4() {
    let mut g = Gte::new();
    g.write_data(16, 50);
    g.write_data(17, 100);
    g.write_data(18, 200);
    g.write_data(19, 300);
    g.write_ctrl(29, 0x555);
    g.command(cmd(0x2D, false, false));
    assert_eq!(g.read_data(7), (0x555 * 600) >> 12);
    g.write_ctrl(30, 0x400);
    g.command(cmd(0x2E, false, false));
    assert_eq!(g.read_data(7), (0x400 * 650) >> 12);
}

#[test]
fn sqr_and_op() {
    let mut g = Gte::new();
    g.write_data(9, 0x1000);
    g.write_data(10, (-0x800i32) as u32);
    g.write_data(11, 0);
    g.command(cmd(0x28, true, false));
    assert_eq!(g.mac[1..], [0x1000, 0x400, 0]);

    let mut g = Gte::new();
    g.write_ctrl(0, 1);
    g.write_ctrl(2, 2);
    g.write_ctrl(4, 3);
    g.write_data(9, 4);
    g.write_data(10, 5);
    g.write_data(11, 6);
    g.command(cmd(0x0C, false, false));
    assert_eq!(g.mac[1..], [-3, 6, -3]);
    assert_eq!(g.ir[1..], [-3, 6, -3]);
}

#[test]
fn ir_saturation_lm() {
    let mut g = Gte::new();
    g.write_ctrl(0, 1);
    g.write_ctrl(2, 2);
    g.write_ctrl(4, 3);
    g.write_data(9, 4);
    g.write_data(10, 5);
    g.write_data(11, 6);
    g.command(cmd(0x0C, false, true));
    assert_eq!(g.ir[1..], [0, 6, 0], "lm clamps negatives to 0");
    let f = g.read_ctrl(31);
    assert_ne!(f & (1 << 24), 0);
    assert_ne!(f & (1 << 22), 0);
    assert_eq!(f & (1 << 23), 0);
}

#[test]
fn mvmva_translation_variants() {
    let mut g = Gte::new();
    identity(&mut g);
    g.write_data(0, (2 << 16) | 1);
    g.write_data(1, 3);
    g.write_ctrl(5, 10);
    g.write_ctrl(6, 20);
    g.write_ctrl(7, 30);
    g.command(mvmva(true, 0, 0, 0));
    assert_eq!(g.mac[1..], [11, 22, 33]);
    g.command(mvmva(true, 0, 0, 3));
    assert_eq!(g.mac[1..], [1, 2, 3]);
    // FC bug: column 0 and FC dropped from the result
    g.write_ctrl(21, 100);
    g.write_ctrl(22, 100);
    g.write_ctrl(23, 100);
    g.command(mvmva(true, 0, 0, 2));
    assert_eq!(g.mac[1..], [0, 2, 3]);
    // IR vector input
    g.write_data(9, 7);
    g.write_data(10, 8);
    g.write_data(11, 9);
    g.command(mvmva(true, 0, 3, 3));
    assert_eq!(g.mac[1..], [7, 8, 9]);
}

#[test]
fn dpcs_interpolates_to_far_color() {
    let mut g = Gte::new();
    g.write_data(6, 0x30C8_9664); // R=100 G=150 B=200 code=0x30
    g.write_data(8, 0);
    g.command(cmd(0x10, true, false));
    assert_eq!(g.read_data(22), 0x30C8_9664, "IR0=0 keeps the color");
    g.write_ctrl(21, 10 << 4);
    g.write_ctrl(22, 20 << 4);
    g.write_ctrl(23, 30 << 4);
    g.write_data(8, 0x1000);
    g.command(cmd(0x10, true, false));
    assert_eq!(g.read_data(22), 0x301E_140A, "IR0=1.0 gives the far color");
    // halfway
    g.write_ctrl(21, 0);
    g.write_ctrl(22, 0);
    g.write_ctrl(23, 0);
    g.write_data(8, 0x800);
    g.command(cmd(0x10, true, false));
    assert_eq!(g.read_data(22) & 0xFF_FFFF, (100 << 16) | (75 << 8) | 50);
    assert_eq!(g.read_data(21), 0x301E_140A, "FIFO shifted");
}

#[test]
fn normal_color_family() {
    let mut g = Gte::new();
    g.write_data(6, 0x0040_8020); // R=0x20 G=0x80 B=0x40
    // LLM = LCM = 0, BK = 1.0 -> lighting result IR = 1.0
    g.write_ctrl(13, 0x1000);
    g.write_ctrl(14, 0x1000);
    g.write_ctrl(15, 0x1000);
    g.write_data(8, 0);
    g.command(cmd(0x13, true, true)); // NCDS
    assert_eq!(g.read_data(22) & 0xFF_FFFF, 0x0040_8020);
    g.command(cmd(0x1B, true, true)); // NCCS
    assert_eq!(g.read_data(22) & 0xFF_FFFF, 0x0040_8020);
    // NCS outputs the light color directly: 0x800 -> 0x80
    g.write_ctrl(13, 0x800);
    g.write_ctrl(14, 0x400);
    g.write_ctrl(15, 0x200);
    g.command(cmd(0x1E, true, true));
    assert_eq!(g.read_data(22) & 0xFF_FFFF, 0x0020_4080);
    // NCT processes 3 vectors -> 3 FIFO entries
    g.command(cmd(0x20, true, true));
    for r in 20..23 {
        assert_eq!(g.read_data(r) & 0xFF_FFFF, 0x0020_4080);
    }
    // LLM*V path: LLM row0 = 1.0, V0 = (0x1000,0,0), LCM identity, BK=0
    let mut g = Gte::new();
    g.write_data(6, 0x00FF_FFFF);
    g.write_ctrl(8, 0x1000);
    identity_lcm(&mut g);
    g.write_data(0, 0x800);
    g.write_data(1, 0);
    g.command(cmd(0x1E, true, true));
    assert_eq!(g.ir[1..], [0x800, 0, 0]);
    assert_eq!(g.read_data(22) & 0xFF, 0x80);
}

fn identity_lcm(g: &mut Gte) {
    g.write_ctrl(16, 0x1000);
    g.write_ctrl(17, 0);
    g.write_ctrl(18, 0x1000);
    g.write_ctrl(19, 0);
    g.write_ctrl(20, 0x1000);
}

#[test]
fn color_fifo_saturation_flags() {
    let mut g = Gte::new();
    g.write_data(8, 0x1000);
    g.write_data(9, 0x2000);
    g.write_data(10, 0);
    g.write_data(11, (-16i32) as u32);
    g.command(cmd(0x3D, true, false)); // GPF
    assert_eq!(g.read_data(22) & 0xFF_FFFF, 0x0000_00FF);
    let f = g.read_ctrl(31);
    assert_ne!(f & (1 << 21), 0, "R saturated");
    assert_ne!(f & (1 << 19), 0, "B saturated");
    assert_eq!(f & (1 << 20), 0);
}

#[test]
fn gpl_accumulates() {
    let mut g = Gte::new();
    g.write_data(25, 0x100);
    g.write_data(26, 0x200);
    g.write_data(27, 0x300);
    g.write_data(8, 0x1000);
    g.write_data(9, 0x10);
    g.write_data(10, 0x20);
    g.write_data(11, 0x30);
    g.command(cmd(0x3E, true, false));
    assert_eq!(g.mac[1..], [0x110, 0x220, 0x330]);
}

#[test]
fn register_quirks() {
    let mut g = Gte::new();
    g.write_ctrl(26, 0x8000);
    assert_eq!(g.read_ctrl(26), 0xFFFF_8000, "H reads sign-extended");
    g.write_data(9, 0xFFFF);
    assert_eq!(g.read_data(9), 0xFFFF_FFFF, "IR sign-extended");
    g.write_data(16, 0xFFFF_FFFF);
    assert_eq!(g.read_data(16), 0xFFFF, "SZ zero-extended");
    g.write_data(1, 0x8000);
    assert_eq!(g.read_data(1), 0xFFFF_8000, "VZ sign-extended");
    for (v, n) in [(0x0000_F000u32, 16), (0xFFFF_0000, 16), (0, 32), (0xFFFF_FFFF, 32), (1, 31)] {
        g.write_data(30, v);
        assert_eq!(g.read_data(31), n, "LZCR for {:08x}", v);
    }
    g.write_data(28, 0x7FFF);
    assert_eq!(g.ir[1..], [0xF80, 0xF80, 0xF80]);
    assert_eq!(g.read_data(29), 0x7FFF);
    g.write_data(9, (-5i32) as u32);
    assert_eq!(g.read_data(29) & 0x1F, 0, "ORGB clamps negatives");
    g.write_data(29, 0);
    assert_eq!(g.ir[2], 0xF80, "ORGB is read-only");
    // SXYP push
    g.write_data(12, 1);
    g.write_data(13, 2);
    g.write_data(14, 3);
    g.write_data(15, 4);
    assert_eq!([g.read_data(12), g.read_data(13), g.read_data(14), g.read_data(15)], [2, 3, 4, 4]);
    // FLAG write mask + summary
    g.write_ctrl(31, 0xFFFF_FFFF);
    assert_eq!(g.read_ctrl(31), 0xFFFF_F000);
    g.write_ctrl(31, 1 << 12);
    assert_eq!(g.read_ctrl(31), 1 << 12, "IR0 flag doesn't set summary");
    // matrix pair packing and sign extension of RT33
    g.write_ctrl(1, 0x1234_FFFE);
    assert_eq!(g.read_ctrl(1), 0x1234_FFFE);
    g.write_ctrl(4, 0x0000_8001);
    assert_eq!(g.read_ctrl(4), 0xFFFF_8001);
}

#[test]
fn mac_overflow_flags() {
    let mut g = Gte::new();
    // TR huge -> 44-bit overflow in MAC1
    g.write_ctrl(0, 0x7FFF);
    g.write_ctrl(5, 0x7FFF_FFFF);
    g.write_data(0, 0x7FFF);
    g.write_data(1, 0);
    g.command(mvmva(false, 0, 0, 0));
    let f = g.read_ctrl(31);
    assert_ne!(f & (1 << 30), 0, "MAC1 positive overflow");
    g.write_ctrl(5, 0x8000_0000);
    g.write_ctrl(0, 0x7FFF);
    g.write_data(0, 0x8000);
    g.command(mvmva(false, 0, 0, 0));
    assert_ne!(g.read_ctrl(31) & (1 << 27), 0, "MAC1 negative overflow");
}
