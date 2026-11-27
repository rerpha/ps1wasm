//! Sound Processing Unit (0x1F801C00-0x1F801FFF).
//!
//! 24 ADPCM voices with ADSR envelopes, volume sweeps, pitch modulation,
//! noise, loop/ENDX handling, the reverb unit (psx-spx algorithm at 22.05 kHz),
//! SPU IRQ, manual/DMA RAM transfers, capture buffers, and CD audio mixing.
//! Output: 44.1 kHz interleaved stereo i16 (one frame per 768 CPU cycles).
//!
//! Known simplification: sample interpolation is linear instead of the
//! hardware's 4-tap Gaussian table.

use crate::cdrom::CdRom;
use crate::irq::{Interrupt, Irq};

pub const SPU_RAM_SIZE: usize = 512 * 1024;
pub const CYCLES_PER_SAMPLE: u32 = 768;

const ADPCM_POS: [i32; 5] = [0, 60, 115, 98, 122];
const ADPCM_NEG: [i32; 5] = [0, 0, -52, -55, -60];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    Attack,
    Decay,
    Sustain,
    Release,
    #[default]
    Off,
}

/// One envelope step per psx-spx "ADSR / volume sweep" rules.
/// `rate` is 7 bits; returns the new level.
fn envelope_step(level: i32, counter: &mut u32, rate: u32, decrease: bool, exponential: bool) -> i32 {
    if rate == 0x7F {
        return level; // "never changes"
    }
    let shift = (rate >> 2) as i32;
    let base = (rate & 3) as i32;
    let mut step = if decrease { -8 + base } else { 7 - base };
    let mut cycles = 1u32 << (shift - 11).max(0);
    step <<= (11 - shift).max(0);
    if exponential && !decrease && level > 0x6000 {
        cycles *= 4;
    }
    if exponential && decrease {
        step = (step * level) >> 15;
    }
    *counter += 1;
    if *counter < cycles {
        return level;
    }
    *counter = 0;
    (level + step).clamp(0, 0x7FFF)
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Sweep {
    pub raw: u16,
    pub level: i32, // signed, -0x8000..0x7FFF
    counter: u32,
}

impl Sweep {
    fn set(&mut self, v: u16) {
        self.raw = v;
        if v & 0x8000 == 0 {
            self.level = ((v << 1) as i16) as i32;
        }
    }
    fn tick(&mut self) {
        if self.raw & 0x8000 == 0 {
            return;
        }
        let exp = self.raw & 0x4000 != 0;
        let decrease = self.raw & 0x2000 != 0;
        let negative = self.raw & 0x1000 != 0;
        let rate = (self.raw & 0x7F) as u32;
        let mag = self.level.abs();
        let new = envelope_step(mag, &mut self.counter, rate, decrease, exp);
        self.level = if negative { -new } else { new };
    }
}

#[derive(Debug, Clone, Default)]
pub struct Voice {
    pub vol_l: Sweep,
    pub vol_r: Sweep,
    pub pitch: u16,
    pub start: u16,
    pub adsr1: u16,
    pub adsr2: u16,
    pub env: i32,
    pub repeat: u16,
    pub phase: Phase,
    env_counter: u32,
    addr: u32,
    counter: u32,
    buf: [i16; 31], // [0..3] tail of previous block, [3..31] current block
    hist: (i32, i32),
    flags: u8,
    ignore_loop: bool,
    needs_decode: bool,
    pub last_out: i32,
}

impl Voice {
    fn key_on(&mut self) {
        self.addr = (self.start as u32) * 8;
        self.counter = 0;
        self.env = 0;
        self.env_counter = 0;
        self.phase = Phase::Attack;
        self.hist = (0, 0);
        self.buf = [0; 31];
        self.ignore_loop = false;
        self.needs_decode = true;
    }

    fn key_off(&mut self) {
        if self.phase != Phase::Off {
            self.phase = Phase::Release;
            self.env_counter = 0;
        }
    }

    fn tick_envelope(&mut self) {
        let sustain_level = (((self.adsr1 & 0xF) as i32) + 1) * 0x800;
        match self.phase {
            Phase::Attack => {
                let rate = ((self.adsr1 >> 8) & 0x7F) as u32;
                let exp = self.adsr1 & 0x8000 != 0;
                self.env = envelope_step(self.env, &mut self.env_counter, rate, false, exp);
                if self.env >= 0x7FFF {
                    self.phase = Phase::Decay;
                    self.env_counter = 0;
                }
            }
            Phase::Decay => {
                let rate = (((self.adsr1 >> 4) & 0xF) as u32) << 2;
                self.env = envelope_step(self.env, &mut self.env_counter, rate, true, true);
                if self.env <= sustain_level {
                    self.phase = Phase::Sustain;
                    self.env_counter = 0;
                }
            }
            Phase::Sustain => {
                let rate = ((self.adsr2 >> 6) & 0x7F) as u32;
                let dec = self.adsr2 & 0x4000 != 0;
                let exp = self.adsr2 & 0x8000 != 0;
                self.env = envelope_step(self.env, &mut self.env_counter, rate, dec, exp);
            }
            Phase::Release => {
                let rate = ((self.adsr2 & 0x1F) as u32) << 2;
                let exp = self.adsr2 & 0x20 != 0;
                self.env = envelope_step(self.env, &mut self.env_counter, rate, true, exp);
                if self.env == 0 {
                    self.phase = Phase::Off;
                }
            }
            Phase::Off => self.env = 0,
        }
    }
}

#[derive(Debug)]
pub struct Spu {
    pub ram: Vec<u8>,
    pub voices: [Voice; 24],
    regs: Vec<u16>, // raw register mirror for read-back (0x1C00..0x2000)/2
    main_l: Sweep,
    main_r: Sweep,
    reverb_out_l: i16,
    reverb_out_r: i16,
    pmon: u32,
    non: u32,
    eon: u32,
    pub endx: u32,
    reverb_base: u32,
    reverb_cur: u32,
    reverb: [u16; 32],
    irq_addr: u32,
    transfer_addr: u32,
    pub cnt: u16,
    transfer_ctrl: u16,
    irq_flag: bool,
    cd_vol: (i16, i16),
    ext_vol: (i16, i16),
    noise_level: i32,
    noise_timer: i32,
    capture_idx: u32,
    cycle_acc: u32,
    reverb_phase: bool,
    reverb_last: (i32, i32),
    /// Generated audio (interleaved L,R), drained by the frontend.
    pub output: Vec<i16>,
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline(always)]
fn sat16(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

#[inline(always)]
fn mul(a: i32, b: i32) -> i32 {
    (a * b) >> 15
}

impl Spu {
    pub fn new() -> Self {
        Spu {
            ram: vec![0; SPU_RAM_SIZE],
            voices: Default::default(),
            regs: vec![0; 0x200],
            main_l: Sweep::default(),
            main_r: Sweep::default(),
            reverb_out_l: 0,
            reverb_out_r: 0,
            pmon: 0,
            non: 0,
            eon: 0,
            endx: 0,
            reverb_base: 0,
            reverb_cur: 0,
            reverb: [0; 32],
            irq_addr: 0,
            transfer_addr: 0,
            cnt: 0,
            transfer_ctrl: 0,
            irq_flag: false,
            cd_vol: (0, 0),
            ext_vol: (0, 0),
            noise_level: 1,
            noise_timer: 0,
            capture_idx: 0,
            cycle_acc: 0,
            reverb_phase: false,
            reverb_last: (0, 0),
            output: Vec::with_capacity(4096),
        }
    }

    // ------------------------------------------------------------- registers
    pub fn read16(&mut self, off: u32) -> u16 {
        let off = off & 0x3FE;
        match off {
            0x000..=0x17F => {
                let v = &self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0xC => v.env as u16,
                    0xE => v.repeat,
                    _ => self.regs[(off / 2) as usize],
                }
            }
            0x19C => self.endx as u16,
            0x19E => (self.endx >> 16) as u16,
            0x1A6 => (self.transfer_addr / 8) as u16,
            0x1AA => self.cnt,
            0x1AE => self.status(),
            0x1B8 => self.main_l.level as u16,
            0x1BA => self.main_r.level as u16,
            0x200..=0x25F => {
                let v = &self.voices[((off - 0x200) >> 2) as usize];
                if off & 2 == 0 { v.vol_l.level as u16 } else { v.vol_r.level as u16 }
            }
            _ => self.regs.get((off / 2) as usize).copied().unwrap_or(0),
        }
    }

    fn status(&self) -> u16 {
        let mut s = self.cnt & 0x3F;
        s |= (self.irq_flag as u16) << 6;
        s |= ((self.cnt >> 5) & 1) << 7;
        let mode = (self.cnt >> 4) & 3;
        s |= ((mode == 2) as u16) << 8;
        s |= ((mode == 3) as u16) << 9;
        s |= (((self.capture_idx >> 8) & 1) as u16) << 11;
        s
    }

    pub fn write16(&mut self, off: u32, v: u16, irq: &mut Irq) {
        let off = off & 0x3FE;
        if ((off / 2) as usize) < self.regs.len() {
            self.regs[(off / 2) as usize] = v;
        }
        match off {
            0x000..=0x17F => {
                let voice = &mut self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => voice.vol_l.set(v),
                    0x2 => voice.vol_r.set(v),
                    0x4 => voice.pitch = v,
                    0x6 => voice.start = v,
                    0x8 => voice.adsr1 = v,
                    0xA => voice.adsr2 = v,
                    0xC => voice.env = (v as i16 as i32).max(0),
                    _ => {
                        voice.repeat = v;
                        voice.ignore_loop = true;
                    }
                }
            }
            0x180 => self.main_l.set(v),
            0x182 => self.main_r.set(v),
            0x184 => self.reverb_out_l = v as i16,
            0x186 => self.reverb_out_r = v as i16,
            0x188 => self.key_on(v as u32),
            0x18A => self.key_on((v as u32) << 16),
            0x18C => self.key_off(v as u32),
            0x18E => self.key_off((v as u32) << 16),
            0x190 => self.pmon = (self.pmon & 0xFFFF_0000) | (v as u32 & !1),
            0x192 => self.pmon = (self.pmon & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x194 => self.non = (self.non & 0xFFFF_0000) | v as u32,
            0x196 => self.non = (self.non & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x198 => self.eon = (self.eon & 0xFFFF_0000) | v as u32,
            0x19A => self.eon = (self.eon & 0xFFFF) | ((v as u32 & 0xFF) << 16),
            0x19C | 0x19E => {} // ENDX read-only
            0x1A2 => {
                self.reverb_base = (v as u32) * 8;
                self.reverb_cur = self.reverb_base;
            }
            0x1A4 => self.irq_addr = (v as u32) * 8,
            0x1A6 => self.transfer_addr = (v as u32) * 8,
            0x1A8 => self.write_ram16(v, irq),
            0x1AA => {
                self.cnt = v;
                if v & 0x40 == 0 {
                    self.irq_flag = false;
                }
            }
            0x1AC => self.transfer_ctrl = v,
            0x1B0 => self.cd_vol.0 = v as i16,
            0x1B2 => self.cd_vol.1 = v as i16,
            0x1B4 => self.ext_vol.0 = v as i16,
            0x1B6 => self.ext_vol.1 = v as i16,
            0x1C0..=0x1FF => self.reverb[((off - 0x1C0) / 2) as usize] = v,
            _ => {}
        }
    }

    fn key_on(&mut self, bits: u32) {
        for i in 0..24 {
            if bits & (1 << i) != 0 {
                self.voices[i].key_on();
                self.endx &= !(1 << i);
            }
        }
    }

    fn key_off(&mut self, bits: u32) {
        for i in 0..24 {
            if bits & (1 << i) != 0 {
                self.voices[i].key_off();
            }
        }
    }

    // ------------------------------------------------------------- RAM transfers
    fn check_irq(&mut self, addr: u32, len: u32, irq: &mut Irq) {
        if self.cnt & 0x40 != 0 && !self.irq_flag && self.irq_addr >= addr && self.irq_addr < addr + len {
            self.irq_flag = true;
            irq.request(Interrupt::Spu);
        }
    }

    fn write_ram16(&mut self, v: u16, irq: &mut Irq) {
        let a = (self.transfer_addr as usize) & (SPU_RAM_SIZE - 1);
        self.ram[a] = v as u8;
        self.ram[a + 1] = (v >> 8) as u8;
        self.check_irq(a as u32, 2, irq);
        self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
    }

    fn read_ram16(&mut self, irq: &mut Irq) -> u16 {
        let a = (self.transfer_addr as usize) & (SPU_RAM_SIZE - 1);
        let v = self.ram[a] as u16 | ((self.ram[a + 1] as u16) << 8);
        self.check_irq(a as u32, 2, irq);
        self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_SIZE as u32 - 1);
        v
    }

    pub fn dma_write(&mut self, w: u32, irq: &mut Irq) {
        self.write_ram16(w as u16, irq);
        self.write_ram16((w >> 16) as u16, irq);
    }

    pub fn dma_read(&mut self, irq: &mut Irq) -> u32 {
        let lo = self.read_ram16(irq) as u32;
        let hi = self.read_ram16(irq) as u32;
        lo | (hi << 16)
    }

    // ------------------------------------------------------------- synthesis
    fn decode_block(&mut self, vi: usize, irq: &mut Irq) {
        let addr = self.voices[vi].addr & (SPU_RAM_SIZE as u32 - 1) & !7;
        self.check_irq(addr, 16, irq);
        let a = addr as usize;
        let header = self.ram[a];
        let flags = self.ram[a + 1];
        let mut shift = (header & 0xF) as i32;
        if shift > 12 {
            shift = 9;
        }
        let filter = (((header >> 4) & 7) as usize).min(4);
        let v = &mut self.voices[vi];
        if flags & 0x04 != 0 && !v.ignore_loop {
            v.repeat = (addr / 8) as u16;
        }
        v.flags = flags;
        let tail = [v.buf[28], v.buf[29], v.buf[30]];
        v.buf[0..3].copy_from_slice(&tail);
        let (mut old, mut older) = v.hist;
        for k in 0..28 {
            let byte = self.ram[a + 2 + k / 2];
            let nib = (byte >> ((k & 1) * 4)) & 0xF;
            let mut s = (((nib as i32) << 28) >> 16) >> shift;
            s += (old * ADPCM_POS[filter] + older * ADPCM_NEG[filter] + 32) >> 6;
            let s = sat16(s);
            older = old;
            old = s;
            v.buf[3 + k] = s as i16;
        }
        v.hist = (old, older);
        v.needs_decode = false;
    }

    fn end_of_block(&mut self, vi: usize) {
        let v = &mut self.voices[vi];
        if v.flags & 0x01 != 0 {
            self.endx |= 1 << vi;
            v.addr = (v.repeat as u32) * 8;
            if v.flags & 0x02 == 0 {
                v.phase = Phase::Release;
                v.env = 0;
            }
        } else {
            v.addr = (v.addr + 16) & (SPU_RAM_SIZE as u32 - 1);
        }
    }

    fn tick_noise(&mut self) {
        let shift = ((self.cnt >> 10) & 0xF) as i32;
        let step = ((self.cnt >> 8) & 3) as i32 + 4;
        self.noise_timer -= step;
        let l = self.noise_level;
        let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
        if self.noise_timer < 0 {
            self.noise_level = ((l << 1) | parity) & 0xFFFF;
            self.noise_timer += 0x20000 >> shift;
            if self.noise_timer < 0 {
                self.noise_timer += 0x20000 >> shift;
            }
        }
    }

    fn sample_voice(&mut self, vi: usize, prev_out: i32, irq: &mut Irq) -> i32 {
        if self.voices[vi].needs_decode {
            self.decode_block(vi, irq);
        }
        let noise = self.non & (1 << vi) != 0;
        let v = &mut self.voices[vi];
        let idx = (v.counter >> 12) as usize;
        let frac = (v.counter & 0xFFF) as i32;
        let raw = if noise {
            self.noise_level as i16 as i32
        } else {
            let a = v.buf[idx + 2] as i32;
            let b = v.buf[idx + 3] as i32;
            a + (((b - a) * frac) >> 12)
        };
        v.tick_envelope();
        let out = (raw * v.env) >> 15;
        v.last_out = out;

        let mut step = v.pitch as u32;
        if self.pmon & (1 << vi) != 0 && vi > 0 {
            let factor = (prev_out + 0x8000) as u32;
            step = ((step as i32 as i64 * factor as i64) >> 15) as u32;
        }
        let step = step.min(0x3FFF);
        v.counter += step;
        if v.counter >= 28 << 12 {
            v.counter -= 28 << 12;
            self.end_of_block(vi);
            self.decode_block(vi, irq);
        }
        out
    }

    /// Byte address of reverb buffer slot `units*8 + delta` relative to the
    /// current position, wrapped into [base, 0x80000).
    fn rv_addr(&self, units: i64, delta: i64) -> usize {
        let base = self.reverb_base as i64;
        let size = SPU_RAM_SIZE as i64 - base;
        if size <= 0 {
            return 0;
        }
        let off = (self.reverb_cur as i64 - base + units * 8 + delta).rem_euclid(size);
        ((base + off) as usize) & (SPU_RAM_SIZE - 2)
    }

    fn rv_read_at(&self, units: i64, delta: i64) -> i32 {
        let a = self.rv_addr(units, delta);
        i16::from_le_bytes([self.ram[a], self.ram[a + 1]]) as i32
    }

    fn rv_read(&self, units: u32) -> i32 {
        self.rv_read_at(units as i64, 0)
    }

    fn rv_write(&mut self, units: u32, v: i32) {
        if self.cnt & 0x80 == 0 {
            return;
        }
        let a = self.rv_addr(units as i64, 0);
        let b = (sat16(v) as i16).to_le_bytes();
        self.ram[a] = b[0];
        self.ram[a + 1] = b[1];
    }

    /// One 22.05 kHz reverb step; returns (L, R) output.
    fn reverb_step(&mut self, in_l: i32, in_r: i32) -> (i32, i32) {
        let r = |i: usize| self.reverb[i] as u32;
        let s = |i: usize| self.reverb[i] as i16 as i32;
        let (d_apf1, d_apf2) = (r(0), r(1));
        let (v_iir, v_comb1, v_comb2, v_comb3, v_comb4, v_wall, v_apf1, v_apf2) =
            (s(2), s(3), s(4), s(5), s(6), s(7), s(8), s(9));
        let (m_lsame, m_rsame, m_lcomb1, m_rcomb1, m_lcomb2, m_rcomb2) = (r(10), r(11), r(12), r(13), r(14), r(15));
        let (d_lsame, d_rsame, m_ldiff, m_rdiff, m_lcomb3, m_rcomb3) = (r(16), r(17), r(18), r(19), r(20), r(21));
        let (m_lcomb4, m_rcomb4, d_ldiff, d_rdiff) = (r(22), r(23), r(24), r(25));
        let (m_lapf1, m_rapf1, m_lapf2, m_rapf2) = (r(26), r(27), r(28), r(29));
        let (v_lin, v_rin) = (s(30), s(31));

        let lin = mul(v_lin, in_l);
        let rin = mul(v_rin, in_r);

        let same = |spu: &Self, inp: i32, d: u32, m: u32| -> i32 {
            let prev = spu.rv_read_at(m as i64, -2); // [m - 2 bytes]
            sat16(mul(sat16(inp + mul(spu.rv_read(d), v_wall) - prev), v_iir) + prev)
        };
        let lsame = same(self, lin, d_lsame, m_lsame);
        let rsame = same(self, rin, d_rsame, m_rsame);
        let ldiff = same(self, lin, d_rdiff, m_ldiff);
        let rdiff = same(self, rin, d_ldiff, m_rdiff);
        self.rv_write(m_lsame, lsame);
        self.rv_write(m_rsame, rsame);
        self.rv_write(m_ldiff, ldiff);
        self.rv_write(m_rdiff, rdiff);

        let mut lout = sat16(
            mul(v_comb1, self.rv_read(m_lcomb1))
                + mul(v_comb2, self.rv_read(m_lcomb2))
                + mul(v_comb3, self.rv_read(m_lcomb3))
                + mul(v_comb4, self.rv_read(m_lcomb4)),
        );
        let mut rout = sat16(
            mul(v_comb1, self.rv_read(m_rcomb1))
                + mul(v_comb2, self.rv_read(m_rcomb2))
                + mul(v_comb3, self.rv_read(m_rcomb3))
                + mul(v_comb4, self.rv_read(m_rcomb4)),
        );

        let apf = |spu: &mut Self, x: i32, m: u32, d: u32, v: i32| -> i32 {
            let tap = spu.rv_read_at(m as i64 - d as i64, 0); // [m - d]
            let y = sat16(x - mul(v, tap));
            spu.rv_write(m, y);
            sat16(mul(y, v) + tap)
        };
        lout = apf(self, lout, m_lapf1, d_apf1, v_apf1);
        rout = apf(self, rout, m_rapf1, d_apf1, v_apf1);
        lout = apf(self, lout, m_lapf2, d_apf2, v_apf2);
        rout = apf(self, rout, m_rapf2, d_apf2, v_apf2);

        let size = SPU_RAM_SIZE as u32 - self.reverb_base;
        if size > 0 {
            let off = (self.reverb_cur - self.reverb_base + 2) % size;
            self.reverb_cur = self.reverb_base + off;
        }
        (mul(lout, self.reverb_out_l as i32), mul(rout, self.reverb_out_r as i32))
    }

    fn write_capture(&mut self, region: u32, v: i32) {
        let a = (region + self.capture_idx * 2) as usize;
        let b = (sat16(v) as i16).to_le_bytes();
        self.ram[a] = b[0];
        self.ram[a + 1] = b[1];
    }

    /// Produce one 44.1 kHz stereo frame.
    fn generate(&mut self, cd: &mut CdRom, irq: &mut Irq) {
        self.tick_noise();
        let (mut l, mut r) = (0i32, 0i32);
        let (mut rl, mut rr) = (0i32, 0i32);
        let mut prev_out = 0;
        for vi in 0..24 {
            let out = self.sample_voice(vi, prev_out, irq);
            prev_out = out;
            let v = &mut self.voices[vi];
            v.vol_l.tick();
            v.vol_r.tick();
            let vl = mul(out, v.vol_l.level);
            let vr = mul(out, v.vol_r.level);
            l += vl;
            r += vr;
            if self.eon & (1 << vi) != 0 {
                rl += vl;
                rr += vr;
            }
        }
        let v1 = self.voices[1].last_out;
        let v3 = self.voices[3].last_out;

        let (cdl, cdr) = cd.pop_audio();
        let (cdl, cdr) = (cdl as i32, cdr as i32);
        if self.cnt & 0x01 != 0 {
            let cl = mul(cdl, self.cd_vol.0 as i32);
            let cr = mul(cdr, self.cd_vol.1 as i32);
            l += cl;
            r += cr;
            if self.cnt & 0x04 != 0 {
                rl += cl;
                rr += cr;
            }
        }

        self.write_capture(0x000, cdl);
        self.write_capture(0x400, cdr);
        self.write_capture(0x800, v1);
        self.write_capture(0xC00, v3);
        self.capture_idx = (self.capture_idx + 1) & 0x1FF;

        // Reverb runs at half rate; hold the result between steps.
        self.reverb_phase = !self.reverb_phase;
        if self.reverb_phase {
            self.reverb_last = self.reverb_step(sat16(rl), sat16(rr));
        }

        self.main_l.tick();
        self.main_r.tick();
        let mut out_l = mul(sat16(l), self.main_l.level) + self.reverb_last.0;
        let mut out_r = mul(sat16(r), self.main_r.level) + self.reverb_last.1;
        if self.cnt & 0x4000 == 0 || self.cnt & 0x8000 == 0 {
            out_l = 0;
            out_r = 0;
        }
        self.output.push(sat16(out_l) as i16);
        self.output.push(sat16(out_r) as i16);
        if self.output.len() > 44100 * 2 {
            self.output.drain(..self.output.len() - 44100);
        }
    }

    pub fn tick(&mut self, cycles: u32, cd: &mut CdRom, irq: &mut Irq) {
        self.cycle_acc += cycles;
        while self.cycle_acc >= CYCLES_PER_SAMPLE {
            self.cycle_acc -= CYCLES_PER_SAMPLE;
            self.generate(cd, irq);
        }
    }
}


fn snap_sweep(s: &mut crate::state::Snap, w: &mut Sweep) {
    s.u16(&mut w.raw);
    s.i32(&mut w.level);
    s.u32(&mut w.counter);
}

impl Spu {
    /// The pending `output` audio buffer is not saved (it's transient).
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("SPU ");
        s.buf(&mut self.ram);
        for v in self.voices.iter_mut() {
            snap_sweep(s, &mut v.vol_l);
            snap_sweep(s, &mut v.vol_r);
            for x in [&mut v.pitch, &mut v.start, &mut v.adsr1, &mut v.adsr2, &mut v.repeat] {
                s.u16(x);
            }
            s.i32(&mut v.env);
            let mut ph: u8 = match v.phase {
                Phase::Attack => 0,
                Phase::Decay => 1,
                Phase::Sustain => 2,
                Phase::Release => 3,
                Phase::Off => 4,
            };
            s.u8(&mut ph);
            v.phase = match ph {
                0 => Phase::Attack,
                1 => Phase::Decay,
                2 => Phase::Sustain,
                3 => Phase::Release,
                _ => Phase::Off,
            };
            s.u32(&mut v.env_counter);
            s.u32(&mut v.addr);
            s.u32(&mut v.counter);
            s.arr_i16(&mut v.buf);
            s.i32(&mut v.hist.0);
            s.i32(&mut v.hist.1);
            s.u8(&mut v.flags);
            s.bool(&mut v.ignore_loop);
            s.bool(&mut v.needs_decode);
            s.i32(&mut v.last_out);
        }
        s.vec_u16(&mut self.regs);
        snap_sweep(s, &mut self.main_l);
        snap_sweep(s, &mut self.main_r);
        s.i16(&mut self.reverb_out_l);
        s.i16(&mut self.reverb_out_r);
        for x in [&mut self.pmon, &mut self.non, &mut self.eon, &mut self.endx, &mut self.reverb_base, &mut self.reverb_cur, &mut self.irq_addr, &mut self.transfer_addr, &mut self.capture_idx, &mut self.cycle_acc] {
            s.u32(x);
        }
        s.arr_u16(&mut self.reverb);
        s.u16(&mut self.cnt);
        s.u16(&mut self.transfer_ctrl);
        s.bool(&mut self.irq_flag);
        s.i16(&mut self.cd_vol.0);
        s.i16(&mut self.cd_vol.1);
        s.i16(&mut self.ext_vol.0);
        s.i16(&mut self.ext_vol.1);
        s.i32(&mut self.noise_level);
        s.i32(&mut self.noise_timer);
        s.bool(&mut self.reverb_phase);
        s.i32(&mut self.reverb_last.0);
        s.i32(&mut self.reverb_last.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spu() -> (Spu, Irq, CdRom) {
        let mut s = Spu::new();
        let mut irq = Irq::default();
        s.write16(0x1AA, 0xC000, &mut irq); // enable + unmute
        s.write16(0x180, 0x3FFF, &mut irq);
        s.write16(0x182, 0x3FFF, &mut irq);
        (s, irq, CdRom::new())
    }

    fn upload(s: &mut Spu, irq: &mut Irq, addr: u32, bytes: &[u8]) {
        s.write16(0x1A6, (addr / 8) as u16, irq);
        for pair in bytes.chunks(2) {
            s.write16(0x1A8, pair[0] as u16 | ((pair[1] as u16) << 8), irq);
        }
    }

    /// One ADPCM block with all samples = nibble `n`, shift 0, filter 0.
    fn block(n: u8, flags: u8) -> Vec<u8> {
        let mut b = vec![0u8, flags];
        b.extend(std::iter::repeat_n((n << 4) | n, 14));
        b
    }

    #[test]
    fn manual_transfer_writes_ram_and_status_mirror() {
        let (mut s, mut irq, _) = spu();
        upload(&mut s, &mut irq, 0x1000, &[1, 2, 3, 4]);
        assert_eq!(&s.ram[0x1000..0x1004], &[1, 2, 3, 4]);
        s.write16(0x1AA, 0xC000 | 0x30 | 0x05, &mut irq); // transfer mode 3 = DMA read
        assert_eq!(s.read16(0x1AE) & 0x3F, 0x35);
        assert_eq!(s.read16(0x1AE) & (1 << 9), 1 << 9, "DMA read request");
    }

    #[test]
    fn dma_read_write_roundtrip() {
        let (mut s, mut irq, _) = spu();
        s.write16(0x1A6, 0x200, &mut irq);
        s.dma_write(0xDEAD_BEEF, &mut irq);
        s.write16(0x1A6, 0x200, &mut irq);
        assert_eq!(s.dma_read(&mut irq), 0xDEAD_BEEF);
    }

    #[test]
    fn adpcm_decode_shift_and_filter() {
        let (mut s, mut irq, _) = spu();
        // shift 4 -> nibble 1 = 0x1000 >> 4 = 0x100 ; filter 1 accumulates
        let mut b = vec![0x14u8, 0];
        b.extend(std::iter::repeat_n(0x11, 14));
        upload(&mut s, &mut irq, 0x2000, &b);
        s.voices[0].addr = 0x2000;
        s.decode_block(0, &mut irq);
        assert_eq!(s.voices[0].buf[3], 0x100);
        // filter 1: s = 0x100 + old*60/64
        let expect = 0x100 + ((0x100 * 60 + 32) >> 6);
        assert_eq!(s.voices[0].buf[4] as i32, expect);
    }

    #[test]
    fn key_on_plays_and_loops_set_endx() {
        let (mut s, mut irq, mut cd) = spu();
        let mut data = block(0x7, 0x04); // loop start
        data.extend(block(0x7, 0x03)); // loop end + repeat
        upload(&mut s, &mut irq, 0x3000, &data);
        s.write16(0x000, 0x3FFF, &mut irq);
        s.write16(0x002, 0x3FFF, &mut irq);
        s.write16(0x004, 0x1000, &mut irq); // 1 sample per output sample
        s.write16(0x006, (0x3000 / 8) as u16, &mut irq);
        s.write16(0x008, 0x007F, &mut irq); // fastest attack (rate 0), sustain level max
        s.write16(0x00A, 0x0000, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 60, &mut cd, &mut irq);
        assert_ne!(s.endx & 1, 0, "loop end reached");
        assert_eq!(s.voices[0].repeat, (0x3000 / 8) as u16, "loop start captured");
        assert!(s.voices[0].env > 0x7000, "attack finished: {:x}", s.voices[0].env);
        assert_ne!(s.voices[0].phase, Phase::Off, "repeat keeps it alive");
        let loud = s.output.iter().any(|&x| x.abs() > 0x1000);
        assert!(loud, "voice is audible");
        // key on clears ENDX
        s.write16(0x188, 1, &mut irq);
        assert_eq!(s.endx & 1, 0);
    }

    #[test]
    fn loop_end_without_repeat_silences_voice() {
        let (mut s, mut irq, mut cd) = spu();
        upload(&mut s, &mut irq, 0x4000, &block(0x7, 0x01));
        s.write16(0x004, 0x1000, &mut irq);
        s.write16(0x006, (0x4000 / 8) as u16, &mut irq);
        s.write16(0x008, 0x007F, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 40, &mut cd, &mut irq);
        assert_eq!(s.voices[0].env, 0);
        assert_ne!(s.endx & 1, 0);
    }

    #[test]
    fn manual_repeat_address_overrides_loop_flag() {
        let (mut s, mut irq, mut cd) = spu();
        upload(&mut s, &mut irq, 0x5000, &block(1, 0x04));
        s.write16(0x006, (0x5000 / 8) as u16, &mut irq);
        s.write16(0x004, 0x1000, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.write16(0x00E, 0x1234, &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 2, &mut cd, &mut irq);
        assert_eq!(s.voices[0].repeat, 0x1234);
    }

    #[test]
    fn envelope_phases() {
        let mut v = Voice {
            adsr1: (0 << 8) | (0xF << 4) | 0x7, // attack rate 0, decay rate max, sustain lvl 8*0x800
            adsr2: 0x1F,                        // sustain rate 0 (increase), release slowest
            ..Voice::default()
        };
        v.key_on();
        for _ in 0..10 {
            v.tick_envelope();
        }
        assert_eq!(v.phase, Phase::Decay);
        for _ in 0..200_000 {
            v.tick_envelope();
            if v.phase == Phase::Sustain {
                break;
            }
        }
        assert_eq!(v.phase, Phase::Sustain);
        assert!(v.env <= 0x4000);
        v.key_off();
        assert_eq!(v.phase, Phase::Release);
        v.adsr2 = 0; // release rate 0 = fastest
        for _ in 0..100 {
            v.tick_envelope();
        }
        assert_eq!(v.phase, Phase::Off);
        assert_eq!(v.env, 0);
    }

    #[test]
    fn envelope_step_rules() {
        let mut c = 0;
        // rate 0 increase: +7 << 11 every sample
        assert_eq!(envelope_step(0, &mut c, 0, false, false), 7 << 11);
        // rate 0x7F never changes
        assert_eq!(envelope_step(100, &mut c, 0x7F, false, false), 100);
        // shift 13 (rate 52): one step every 4 samples of +7
        let mut c = 0;
        let mut l = 0;
        for _ in 0..4 {
            l = envelope_step(l, &mut c, 52, false, false);
        }
        assert_eq!(l, 7);
        // exponential decrease scales by level
        let mut c = 0;
        assert_eq!(envelope_step(0x4000, &mut c, 0, true, true), 0x4000 + (((-8 << 11) * 0x4000) >> 15));
    }

    #[test]
    fn sweep_volume() {
        let mut sw = Sweep::default();
        sw.set(0x2000);
        assert_eq!(sw.level, 0x4000);
        sw.set(0x8000 | 0x0000); // linear increase, rate 0
        sw.tick();
        assert_eq!(sw.level, 0x4000 + (7 << 11));
    }

    #[test]
    fn spu_irq_on_address_hit() {
        let (mut s, mut irq, mut cd) = spu();
        s.write16(0x1A4, (0x6010 / 8) as u16, &mut irq);
        s.write16(0x1AA, 0xC040, &mut irq);
        upload(&mut s, &mut irq, 0x6000, &[0; 32]);
        assert_ne!(irq.stat & (1 << 9), 0, "transfer hit");
        assert_ne!(s.read16(0x1AE) & 0x40, 0);
        s.write16(0x1AA, 0xC000, &mut irq);
        assert_eq!(s.read16(0x1AE) & 0x40, 0, "acknowledged");
        // voice reading the block also triggers
        irq.stat = 0;
        s.write16(0x1AA, 0xC040, &mut irq);
        s.write16(0x006, (0x6010 / 8) as u16, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.tick(CYCLES_PER_SAMPLE, &mut cd, &mut irq);
        assert_ne!(irq.stat & (1 << 9), 0);
    }

    #[test]
    fn noise_voice_and_pitch_modulation_paths() {
        let (mut s, mut irq, mut cd) = spu();
        s.write16(0x194, 1, &mut irq);
        s.write16(0x1AA, 0xC000 | (0xF << 10) | (3 << 8), &mut irq);
        s.write16(0x008, 0x007F, &mut irq);
        s.write16(0x000, 0x3FFF, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 100, &mut cd, &mut irq);
        let distinct: std::collections::HashSet<i16> = s.output.iter().step_by(2).copied().collect();
        assert!(distinct.len() > 10, "noise varies");
        // PMON bit 0 is not settable
        s.write16(0x190, 0xFFFF, &mut irq);
        assert_eq!(s.pmon & 1, 0);
    }

    #[test]
    fn output_rate_and_mute() {
        let (mut s, mut irq, mut cd) = spu();
        s.tick(CYCLES_PER_SAMPLE * 100 + 5, &mut cd, &mut irq);
        assert_eq!(s.output.len(), 200);
        s.write16(0x1AA, 0x8000, &mut irq); // muted
        s.output.clear();
        s.tick(CYCLES_PER_SAMPLE, &mut cd, &mut irq);
        assert_eq!(s.output, vec![0, 0]);
    }

    #[test]
    fn register_readback() {
        let (mut s, mut irq, _) = spu();
        s.write16(0x004, 0x1234, &mut irq);
        assert_eq!(s.read16(0x004), 0x1234);
        s.write16(0x1C0, 0xABCD, &mut irq);
        assert_eq!(s.read16(0x1C0), 0xABCD);
        s.write16(0x00C, 0x2000, &mut irq);
        assert_eq!(s.read16(0x00C), 0x2000);
    }

    #[test]
    fn reverb_writes_only_when_enabled() {
        let (mut s, mut irq, mut cd) = spu();
        s.write16(0x1A2, 0xE000, &mut irq); // base = 0x70000
        for k in 0..32 {
            s.write16(0x1C0 + 2 * k, 0x0100 + k as u16, &mut irq);
        }
        s.write16(0x1C0 + 2 * 30, 0x7FFF, &mut irq); // vLIN
        s.write16(0x1C0 + 2 * 31, 0x7FFF, &mut irq);
        s.write16(0x1C0 + 2 * 2, 0x7FFF, &mut irq); // vIIR
        // fast noise voice feeding reverb (reverb master still disabled)
        s.write16(0x1AA, 0xC000 | (0xF << 10), &mut irq);
        s.write16(0x194, 1, &mut irq);
        s.write16(0x198, 1, &mut irq);
        s.write16(0x000, 0x3FFF, &mut irq);
        s.write16(0x008, 0x007F, &mut irq);
        s.write16(0x188, 1, &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 200, &mut cd, &mut irq);
        assert!(s.ram[0x70000..].iter().all(|&b| b == 0), "reverb disabled: no writes");
        s.write16(0x1AA, 0xC080 | (0xF << 10), &mut irq);
        s.tick(CYCLES_PER_SAMPLE * 200, &mut cd, &mut irq);
        assert!(s.ram[0x70000..].iter().any(|&b| b != 0), "reverb enabled: buffer written");
    }
}
