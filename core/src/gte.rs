//! Geometry Transformation Engine (COP2).
//!
//! Fixed-point semantics follow psx-spx: 44-bit MAC1-3 with per-addition
//! overflow flags, 32-bit MAC0, IR saturation (lm), hardware UNR division,
//! and the well-known quirks (RTPS IR3 flag, H sign-extended on read, IRGB/ORGB,
//! LZCR, SXYP FIFO push, MVMVA FC bug, "garbage" matrix 3).

const UNR_TABLE: [u8; 257] = {
    let mut t = [0u8; 257];
    let mut i = 0;
    while i < 257 {
        let v = (0x40000 / (i as i32 + 0x100) + 1) / 2 - 0x101;
        t[i] = if v < 0 { 0 } else { v as u8 };
        i += 1;
    }
    t
};

pub fn unr_table(i: usize) -> u8 {
    UNR_TABLE[i]
}

type Mat = [[i16; 3]; 3];

#[derive(Debug, Clone, Default)]
pub struct Gte {
    // ---- data registers
    pub v: [[i16; 3]; 3],
    pub rgbc: [u8; 4],
    pub otz: u16,
    pub ir: [i16; 4],
    pub sxy: [(i16, i16); 3],
    pub sz: [u16; 4],
    pub rgb: [[u8; 4]; 3],
    pub res1: u32,
    pub mac: [i32; 4],
    pub lzcs: u32,
    pub lzcr: u32,
    // ---- control registers
    pub rt: Mat,
    pub tr: [i32; 3],
    pub llm: Mat,
    pub bk: [i32; 3],
    pub lcm: Mat,
    pub fc: [i32; 3],
    pub ofx: i32,
    pub ofy: i32,
    pub h: u16,
    pub dqa: i16,
    pub dqb: i32,
    pub zsf3: i16,
    pub zsf4: i16,
    pub flag: u32,
    // ---- current command
    shift: u32,
    lm: bool,
}

#[inline(always)]
fn pack(lo: i16, hi: i16) -> u32 {
    (lo as u16 as u32) | ((hi as u16 as u32) << 16)
}

impl Gte {
    pub fn new() -> Self {
        Self::default()
    }

    // =================================================================== regs
    pub fn read_data(&mut self, r: u32) -> u32 {
        match r {
            0 | 2 | 4 => {
                let v = self.v[(r / 2) as usize];
                pack(v[0], v[1])
            }
            1 | 3 | 5 => self.v[(r / 2) as usize][2] as i32 as u32,
            6 => u32::from_le_bytes(self.rgbc),
            7 => self.otz as u32,
            8..=11 => self.ir[(r - 8) as usize] as i32 as u32,
            12..=14 => {
                let (x, y) = self.sxy[(r - 12) as usize];
                pack(x, y)
            }
            15 => pack(self.sxy[2].0, self.sxy[2].1),
            16..=19 => self.sz[(r - 16) as usize] as u32,
            20..=22 => u32::from_le_bytes(self.rgb[(r - 20) as usize]),
            23 => self.res1,
            24..=27 => self.mac[(r - 24) as usize] as u32,
            28 | 29 => {
                let c = |v: i16| ((v as i32) >> 7).clamp(0, 0x1F) as u32;
                c(self.ir[1]) | (c(self.ir[2]) << 5) | (c(self.ir[3]) << 10)
            }
            30 => self.lzcs,
            _ => self.lzcr,
        }
    }

    pub fn write_data(&mut self, r: u32, v: u32) {
        match r {
            0 | 2 | 4 => {
                let i = (r / 2) as usize;
                self.v[i][0] = v as i16;
                self.v[i][1] = (v >> 16) as i16;
            }
            1 | 3 | 5 => self.v[(r / 2) as usize][2] = v as i16,
            6 => self.rgbc = v.to_le_bytes(),
            7 => self.otz = v as u16,
            8..=11 => self.ir[(r - 8) as usize] = v as i16,
            12..=14 => self.sxy[(r - 12) as usize] = (v as i16, (v >> 16) as i16),
            15 => {
                self.sxy[0] = self.sxy[1];
                self.sxy[1] = self.sxy[2];
                self.sxy[2] = (v as i16, (v >> 16) as i16);
            }
            16..=19 => self.sz[(r - 16) as usize] = v as u16,
            20..=22 => self.rgb[(r - 20) as usize] = v.to_le_bytes(),
            23 => self.res1 = v,
            24..=27 => self.mac[(r - 24) as usize] = v as i32,
            28 => {
                self.ir[1] = ((v & 0x1F) * 0x80) as i16;
                self.ir[2] = (((v >> 5) & 0x1F) * 0x80) as i16;
                self.ir[3] = (((v >> 10) & 0x1F) * 0x80) as i16;
            }
            29 | 31 => {}
            _ => {
                self.lzcs = v;
                self.lzcr = if (v as i32) < 0 { (!v).leading_zeros() } else { v.leading_zeros() };
            }
        }
    }

    fn mat_mut(&mut self, idx: u32) -> &mut Mat {
        match idx {
            0 => &mut self.rt,
            1 => &mut self.llm,
            _ => &mut self.lcm,
        }
    }

    fn vec_mut(&mut self, idx: u32) -> &mut [i32; 3] {
        match idx {
            0 => &mut self.tr,
            1 => &mut self.bk,
            _ => &mut self.fc,
        }
    }

    pub fn read_ctrl(&mut self, r: u32) -> u32 {
        match r {
            0..=4 | 8..=12 | 16..=20 => {
                let m = *self.mat_mut(r / 8);
                match r % 8 {
                    0 => pack(m[0][0], m[0][1]),
                    1 => pack(m[0][2], m[1][0]),
                    2 => pack(m[1][1], m[1][2]),
                    3 => pack(m[2][0], m[2][1]),
                    _ => m[2][2] as i32 as u32,
                }
            }
            5..=7 | 13..=15 | 21..=23 => self.vec_mut(r / 8)[(r % 8 - 5) as usize] as u32,
            24 => self.ofx as u32,
            25 => self.ofy as u32,
            26 => self.h as i16 as i32 as u32, // hardware quirk: sign-extended
            27 => self.dqa as i32 as u32,
            28 => self.dqb as u32,
            29 => self.zsf3 as i32 as u32,
            30 => self.zsf4 as i32 as u32,
            _ => self.flag,
        }
    }

    pub fn write_ctrl(&mut self, r: u32, v: u32) {
        match r {
            0..=4 | 8..=12 | 16..=20 => {
                let m = self.mat_mut(r / 8);
                let (lo, hi) = (v as i16, (v >> 16) as i16);
                match r % 8 {
                    0 => {
                        m[0][0] = lo;
                        m[0][1] = hi;
                    }
                    1 => {
                        m[0][2] = lo;
                        m[1][0] = hi;
                    }
                    2 => {
                        m[1][1] = lo;
                        m[1][2] = hi;
                    }
                    3 => {
                        m[2][0] = lo;
                        m[2][1] = hi;
                    }
                    _ => m[2][2] = lo,
                }
            }
            5..=7 | 13..=15 | 21..=23 => self.vec_mut(r / 8)[(r % 8 - 5) as usize] = v as i32,
            24 => self.ofx = v as i32,
            25 => self.ofy = v as i32,
            26 => self.h = v as u16,
            27 => self.dqa = v as i16,
            28 => self.dqb = v as i32,
            29 => self.zsf3 = v as i16,
            30 => self.zsf4 = v as i16,
            _ => {
                self.flag = v & 0x7FFF_F000;
                self.update_flag_summary();
            }
        }
    }

    fn update_flag_summary(&mut self) {
        if self.flag & 0x7F87_E000 != 0 {
            self.flag |= 1 << 31;
        } else {
            self.flag &= !(1 << 31);
        }
    }

    // =================================================================== helpers
    #[inline(always)]
    fn set_flag(&mut self, bit: u32) {
        self.flag |= 1 << bit;
    }

    /// Overflow flags only (no wrap) for a final MAC1..3 sum.
    #[inline(always)]
    fn flag_mac(&mut self, i: usize, v: i64) {
        if v > 0x7FF_FFFF_FFFF {
            self.set_flag(31 - i as u32);
        } else if v < -0x800_0000_0000 {
            self.set_flag(28 - i as u32);
        }
    }

    /// Overflow check + 44-bit wrap, used for *intermediate* partial sums.
    #[inline(always)]
    fn chk(&mut self, i: usize, v: i64) -> i64 {
        self.flag_mac(i, v);
        (v << 20) >> 20
    }

    /// Final sum -> MACi = low 32 bits of (v >> sf). Returns that 32-bit value;
    /// hardware saturates IR from the stored MAC, not the wide sum (verified
    /// against ps1-tests gte/test-all real-hardware results).
    #[inline(always)]
    fn set_mac(&mut self, i: usize, v: i64) -> i64 {
        self.flag_mac(i, v);
        let r = (v >> self.shift) as i32;
        self.mac[i] = r;
        r as i64
    }

    /// Final sum -> MACi and IRi (IR saturated from the 32-bit MAC).
    #[inline(always)]
    fn set_mac_ir(&mut self, i: usize, v: i64, lm: bool) {
        let r = self.set_mac(i, v);
        self.ir[i] = self.sat_ir(i, r, lm);
    }

    #[inline(always)]
    fn sat_ir(&mut self, i: usize, v: i64, lm: bool) -> i16 {
        let min = if lm { 0 } else { -0x8000 };
        if v < min {
            self.set_flag(25 - i as u32);
            min as i16
        } else if v > 0x7FFF {
            self.set_flag(25 - i as u32);
            0x7FFF
        } else {
            v as i16
        }
    }

    #[inline(always)]
    fn set_ir_from_mac(&mut self, lm: bool) {
        for i in 1..=3 {
            let m = self.mac[i] as i64;
            self.ir[i] = self.sat_ir(i, m, lm);
        }
    }

    #[inline(always)]
    fn set_mac0(&mut self, v: i64) -> i64 {
        if v > 0x7FFF_FFFF {
            self.set_flag(16);
        } else if v < -0x8000_0000 {
            self.set_flag(15);
        }
        self.mac[0] = v as i32;
        v
    }

    fn push_sz(&mut self, v: i64) {
        let z = if v < 0 {
            self.set_flag(18);
            0
        } else if v > 0xFFFF {
            self.set_flag(18);
            0xFFFF
        } else {
            v as u16
        };
        self.sz[0] = self.sz[1];
        self.sz[1] = self.sz[2];
        self.sz[2] = self.sz[3];
        self.sz[3] = z;
    }

    fn push_sxy(&mut self, x: i64, y: i64) {
        let sat = |g: &mut Self, v: i64, bit: u32| -> i16 {
            if v < -0x400 {
                g.set_flag(bit);
                -0x400
            } else if v > 0x3FF {
                g.set_flag(bit);
                0x3FF
            } else {
                v as i16
            }
        };
        let sx = sat(self, x, 14);
        let sy = sat(self, y, 13);
        self.sxy[0] = self.sxy[1];
        self.sxy[1] = self.sxy[2];
        self.sxy[2] = (sx, sy);
    }

    fn push_color(&mut self) {
        let mut c = [0u8; 4];
        for i in 0..3 {
            let v = self.mac[i + 1] >> 4;
            c[i] = if v < 0 {
                self.set_flag(21 - i as u32);
                0
            } else if v > 0xFF {
                self.set_flag(21 - i as u32);
                0xFF
            } else {
                v as u8
            };
        }
        c[3] = self.rgbc[3];
        self.rgb[0] = self.rgb[1];
        self.rgb[1] = self.rgb[2];
        self.rgb[2] = c;
    }

    fn set_ir0(&mut self, v: i64) {
        self.ir[0] = if v < 0 {
            self.set_flag(12);
            0
        } else if v > 0x1000 {
            self.set_flag(12);
            0x1000
        } else {
            v as i16
        };
    }

    /// Hardware Newton-Raphson division H/SZ3 (result 1.16 fixed point).
    pub fn divide(&mut self, h: u32, sz: u32) -> u32 {
        if h < sz * 2 {
            let z = (sz as u16).leading_zeros();
            let n = (h as i64) << z;
            let mut d = (sz as i64) << z;
            let u = UNR_TABLE[((d - 0x7FC0) >> 7) as usize] as i64 + 0x101;
            d = (0x200_0080 - d * u) >> 8;
            d = (0x000_0080 + d * u) >> 8;
            (((n * d) + 0x8000) >> 16).min(0x1FFFF) as u32
        } else {
            self.set_flag(17);
            0x1FFFF
        }
    }

    /// tr<<12 + M*v. The first two partial sums wrap to 44 bits (with flags);
    /// the final sum is returned unwrapped (flags are set by set_mac).
    fn mat_vec(&mut self, m: &Mat, v: [i64; 3], tr: [i64; 3]) -> [i64; 3] {
        let mut out = [0i64; 3];
        for i in 0..3 {
            let mut acc = self.chk(i + 1, (tr[i] << 12) + m[i][0] as i64 * v[0]);
            acc = self.chk(i + 1, acc + m[i][1] as i64 * v[1]);
            out[i] = acc + m[i][2] as i64 * v[2];
        }
        out
    }

    fn mat_vec_set(&mut self, m: &Mat, v: [i64; 3], tr: [i64; 3]) {
        let acc = self.mat_vec(m, v, tr);
        let lm = self.lm;
        for i in 0..3 {
            self.set_mac_ir(i + 1, acc[i], lm);
        }
    }

    fn vec(&self, i: usize) -> [i64; 3] {
        [self.v[i][0] as i64, self.v[i][1] as i64, self.v[i][2] as i64]
    }

    fn ir_vec(&self) -> [i64; 3] {
        [self.ir[1] as i64, self.ir[2] as i64, self.ir[3] as i64]
    }

    /// Execution time of a command in CPU cycles (psx-spx "GTE Command
    /// Summary"); the CPU only stalls if it touches the GTE again sooner.
    pub fn command_cycles(cmd: u32) -> u32 {
        match cmd & 0x3F {
            0x01 => 15,
            0x06 => 8,
            0x0C => 6,
            0x10 => 8,
            0x11 => 8,
            0x12 => 8,
            0x13 => 19,
            0x14 => 13,
            0x16 => 44,
            0x1B => 17,
            0x1C => 11,
            0x1E => 14,
            0x20 => 30,
            0x28 => 5,
            0x29 => 8,
            0x2A => 17,
            0x2D => 5,
            0x2E => 6,
            0x30 => 23,
            0x3D => 5,
            0x3E => 5,
            0x3F => 39,
            _ => 1,
        }
    }

    // =================================================================== commands
    pub fn command(&mut self, cmd: u32) {
        // Invalid opcodes leave every register, including FLAG, untouched
        // (verified against the ps1-tests hardware GTE fuzz log).
        const VALID: [u32; 22] = [
            0x01, 0x06, 0x0C, 0x10, 0x11, 0x12, 0x13, 0x14, 0x16, 0x1B, 0x1C, 0x1E, 0x20, 0x28, 0x29, 0x2A, 0x2D,
            0x2E, 0x30, 0x3D, 0x3E, 0x3F,
        ];
        if !VALID.contains(&(cmd & 0x3F)) {
            return;
        }
        self.flag = 0;
        self.shift = if cmd & (1 << 19) != 0 { 12 } else { 0 };
        self.lm = cmd & (1 << 10) != 0;
        match cmd & 0x3F {
            0x01 => self.rtp(0, true),
            0x06 => self.nclip(),
            0x0C => self.op(),
            0x10 => self.dpcs(false),
            0x11 => self.intpl(),
            0x12 => self.mvmva(cmd),
            0x13 => self.ncd(0),
            0x14 => self.cdp(),
            0x16 => {
                for i in 0..3 {
                    self.ncd(i)
                }
            }
            0x1B => self.ncc(0),
            0x1C => self.cc(),
            0x1E => self.nc(0),
            0x20 => {
                for i in 0..3 {
                    self.nc(i)
                }
            }
            0x28 => self.sqr(),
            0x29 => self.dcpl(),
            0x2A => {
                for _ in 0..3 {
                    self.dpcs(true)
                }
            }
            0x2D => self.avsz3(),
            0x2E => self.avsz4(),
            0x30 => {
                self.rtp(0, false);
                self.rtp(1, false);
                self.rtp(2, true);
            }
            0x3D => self.gpf(),
            0x3E => self.gpl(),
            0x3F => {
                for i in 0..3 {
                    self.ncc(i)
                }
            }
            _ => {}
        }
        self.update_flag_summary();
    }

    fn rtp(&mut self, vi: usize, last: bool) {
        let tr = [self.tr[0] as i64, self.tr[1] as i64, self.tr[2] as i64];
        let rt = self.rt;
        let acc = self.mat_vec(&rt, self.vec(vi), tr);
        let lm = self.lm;
        self.set_mac_ir(1, acc[0], lm);
        self.set_mac_ir(2, acc[1], lm);
        // IR3 quirk: value saturates from MAC3, but the flag is tested
        // against (sum >> 12) regardless of sf.
        let m3 = self.set_mac(3, acc[2]);
        // (sum >> 12) as a 32-bit value: feeds the IR3 flag and SZ3.
        let z12 = ((acc[2] >> 12) as i32) as i64;
        if !(-0x8000..=0x7FFF).contains(&z12) {
            self.set_flag(22);
        }
        let min = if lm { 0 } else { -0x8000 };
        self.ir[3] = m3.clamp(min, 0x7FFF) as i16;

        self.push_sz(z12);
        let div = self.divide(self.h as u32, self.sz[3] as u32) as i64;
        let x = self.set_mac0(div * self.ir[1] as i64 + self.ofx as i64);
        let y = self.set_mac0(div * self.ir[2] as i64 + self.ofy as i64);
        self.push_sxy(x >> 16, y >> 16);
        if last {
            let d = self.set_mac0(div * self.dqa as i64 + self.dqb as i64);
            self.set_ir0(d >> 12);
        }
    }

    fn nclip(&mut self) {
        let (x0, y0) = (self.sxy[0].0 as i64, self.sxy[0].1 as i64);
        let (x1, y1) = (self.sxy[1].0 as i64, self.sxy[1].1 as i64);
        let (x2, y2) = (self.sxy[2].0 as i64, self.sxy[2].1 as i64);
        self.set_mac0(x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1);
    }

    fn op(&mut self) {
        let d = [self.rt[0][0] as i64, self.rt[1][1] as i64, self.rt[2][2] as i64];
        let ir = self.ir_vec();
        let lm = self.lm;
        self.set_mac_ir(1, ir[2] * d[1] - ir[1] * d[2], lm);
        self.set_mac_ir(2, ir[0] * d[2] - ir[2] * d[0], lm);
        self.set_mac_ir(3, ir[1] * d[0] - ir[0] * d[1], lm);
    }

    /// Shared "depth cue" stage. `m` is the unshifted input (e.g. R<<16).
    ///   IR  = ((FC<<12) - m) >> sf          (saturated, lm=0)
    ///   MAC = (IR*IR0 + m) >> sf
    ///   color FIFO <- MAC/16 ; IR = MAC (saturated with lm)
    fn depth_cue(&mut self, m: [i64; 3]) {
        for i in 0..3 {
            let fc = (self.fc[i] as i64) << 12;
            self.set_mac_ir(i + 1, fc - m[i], false);
        }
        let ir0 = self.ir[0] as i64;
        for i in 0..3 {
            let v = self.ir[i + 1] as i64 * ir0 + m[i];
            self.set_mac(i + 1, v);
        }
        self.push_color();
        self.set_ir_from_mac(self.lm);
    }

    fn dpcs(&mut self, use_fifo: bool) {
        let c = if use_fifo { self.rgb[0] } else { self.rgbc };
        self.depth_cue([(c[0] as i64) << 16, (c[1] as i64) << 16, (c[2] as i64) << 16]);
    }

    fn intpl(&mut self) {
        let ir = self.ir_vec();
        self.depth_cue([ir[0] << 12, ir[1] << 12, ir[2] << 12]);
    }

    fn color_times_ir(&self) -> [i64; 3] {
        let ir = self.ir_vec();
        [
            ((self.rgbc[0] as i64) * ir[0]) << 4,
            ((self.rgbc[1] as i64) * ir[1]) << 4,
            ((self.rgbc[2] as i64) * ir[2]) << 4,
        ]
    }

    fn dcpl(&mut self) {
        let m = self.color_times_ir();
        self.depth_cue(m);
    }

    /// Light stages: IR = LLM*V ; IR = BK + LCM*IR
    fn light(&mut self, vi: usize) {
        let llm = self.llm;
        let v = self.vec(vi);
        self.mat_vec_set(&llm, v, [0; 3]);
        self.light_color();
    }

    fn light_color(&mut self) {
        let lcm = self.lcm;
        let bk = [self.bk[0] as i64, self.bk[1] as i64, self.bk[2] as i64];
        let ir = self.ir_vec();
        self.mat_vec_set(&lcm, ir, bk);
    }

    fn finish_ncc(&mut self) {
        let m = self.color_times_ir();
        let lm = self.lm;
        for i in 0..3 {
            self.set_mac_ir(i + 1, m[i], lm);
        }
        self.push_color();
    }

    fn nc(&mut self, vi: usize) {
        self.light(vi);
        self.push_color();
    }

    fn ncc(&mut self, vi: usize) {
        self.light(vi);
        self.finish_ncc();
    }

    fn ncd(&mut self, vi: usize) {
        self.light(vi);
        let m = self.color_times_ir();
        self.depth_cue(m);
    }

    fn cc(&mut self) {
        self.light_color();
        self.finish_ncc();
    }

    fn cdp(&mut self) {
        self.light_color();
        let m = self.color_times_ir();
        self.depth_cue(m);
    }

    fn mvmva(&mut self, cmd: u32) {
        let mx = (cmd >> 17) & 3;
        let vsel = ((cmd >> 15) & 3) as usize;
        let cv = (cmd >> 13) & 3;
        let v = if vsel < 3 { self.vec(vsel) } else { self.ir_vec() };
        let m: Mat = match mx {
            0 => self.rt,
            1 => self.llm,
            2 => self.lcm,
            _ => {
                let r = (self.rgbc[0] as i16) << 4;
                [[-r, r, self.ir[0]], [self.rt[0][2]; 3], [self.rt[1][1]; 3]]
            }
        };
        let t: [i64; 3] = match cv {
            0 => [self.tr[0] as i64, self.tr[1] as i64, self.tr[2] as i64],
            1 => [self.bk[0] as i64, self.bk[1] as i64, self.bk[2] as i64],
            2 => [self.fc[0] as i64, self.fc[1] as i64, self.fc[2] as i64],
            _ => [0; 3],
        };
        if cv == 2 {
            // Hardware bug: FC + column 0 only affects the flags; the result
            // is built from columns 1 and 2 alone.
            let lm = self.lm;
            for i in 0..3 {
                let partial = self.chk(i + 1, (t[i] << 12) + m[i][0] as i64 * v[0]);
                self.sat_ir(i + 1, partial >> self.shift, false);
                let acc = self.chk(i + 1, m[i][1] as i64 * v[1]) + m[i][2] as i64 * v[2];
                self.set_mac_ir(i + 1, acc, lm);
            }
        } else {
            self.mat_vec_set(&m, v, t);
        }
    }

    fn sqr(&mut self) {
        let ir = self.ir_vec();
        let lm = self.lm;
        for i in 0..3 {
            self.set_mac_ir(i + 1, ir[i] * ir[i], lm);
        }
    }

    fn avsz3(&mut self) {
        let s = self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
        let v = self.set_mac0(self.zsf3 as i64 * s);
        self.set_otz(v >> 12);
    }

    fn avsz4(&mut self) {
        let s = self.sz.iter().map(|&z| z as i64).sum::<i64>();
        let v = self.set_mac0(self.zsf4 as i64 * s);
        self.set_otz(v >> 12);
    }

    fn set_otz(&mut self, v: i64) {
        self.otz = if v < 0 {
            self.set_flag(18);
            0
        } else if v > 0xFFFF {
            self.set_flag(18);
            0xFFFF
        } else {
            v as u16
        };
    }

    fn gpf(&mut self) {
        let ir = self.ir_vec();
        let ir0 = self.ir[0] as i64;
        let lm = self.lm;
        for i in 0..3 {
            self.set_mac_ir(i + 1, ir0 * ir[i], lm);
        }
        self.push_color();
    }

    fn gpl(&mut self) {
        let ir = self.ir_vec();
        let ir0 = self.ir[0] as i64;
        let lm = self.lm;
        for i in 0..3 {
            let base = (self.mac[i + 1] as i64) << self.shift;
            self.set_mac_ir(i + 1, base + ir0 * ir[i], lm);
        }
        self.push_color();
    }
}

impl Gte {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("GTE ");
        for v in self.v.iter_mut() {
            s.arr_i16(v);
        }
        s.buf(&mut self.rgbc);
        s.u16(&mut self.otz);
        s.arr_i16(&mut self.ir);
        for p in self.sxy.iter_mut() {
            s.i16(&mut p.0);
            s.i16(&mut p.1);
        }
        s.arr_u16(&mut self.sz);
        for c in self.rgb.iter_mut() {
            s.buf(c);
        }
        s.u32(&mut self.res1);
        s.arr_i32(&mut self.mac);
        s.u32(&mut self.lzcs);
        s.u32(&mut self.lzcr);
        for m in [&mut self.rt, &mut self.llm, &mut self.lcm] {
            for row in m.iter_mut() {
                s.arr_i16(row);
            }
        }
        s.arr_i32(&mut self.tr);
        s.arr_i32(&mut self.bk);
        s.arr_i32(&mut self.fc);
        s.i32(&mut self.ofx);
        s.i32(&mut self.ofy);
        s.u16(&mut self.h);
        s.i16(&mut self.dqa);
        s.i32(&mut self.dqb);
        s.i16(&mut self.zsf3);
        s.i16(&mut self.zsf4);
        s.u32(&mut self.flag);
        s.u32(&mut self.shift);
        s.bool(&mut self.lm);
    }
}
