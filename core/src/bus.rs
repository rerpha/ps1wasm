//! System bus: memory map, device I/O dispatch, DMA engine and the shared
//! cycle clock that keeps devices in step with the CPU.
//!
//! Physical map (after segment masking):
//!   0x0000_0000  2 MiB main RAM (mirrored 4x up to 8 MiB)
//!   0x1F00_0000  Expansion 1 (unconnected: reads 0xFF)
//!   0x1F80_0000  1 KiB scratchpad (only via KUSEG/KSEG0)
//!   0x1F80_1000  I/O ports
//!   0x1F80_2000  Expansion 2
//!   0x1FC0_0000  512 KiB BIOS ROM
//!   0xFFFE_0130  Cache control (KSEG2)
//!
//! Devices are advanced lazily: CPU cycles accumulate in `pending` and are
//! flushed to every device before any I/O access, or every SYNC_INTERVAL.

use crate::cdrom::CdRom;
use crate::dma::{self, Dma};
use crate::gpu::Gpu;
use crate::irq::Irq;
use crate::mdec::Mdec;
use crate::pad::Sio0;
use crate::spu::Spu;
use crate::timers::Timers;

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const BIOS_SIZE: usize = 512 * 1024;
pub const SCRATCH_SIZE: usize = 1024;
pub const SYNC_INTERVAL: u32 = 128;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Byte = 1,
    Half = 2,
    Word = 4,
}

const REGION_MASK: [u32; 8] = [
    0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, // KUSEG 2 GiB
    0x7FFF_FFFF, // KSEG0
    0x1FFF_FFFF, // KSEG1
    0xFFFF_FFFF, 0xFFFF_FFFF, // KSEG2
];

#[inline(always)]
pub fn mask_region(addr: u32) -> u32 {
    addr & REGION_MASK[(addr >> 29) as usize]
}

/// CPU cycles for one read of `size` at physical address `phys` (the whole
/// load instruction), from ps1-tests cpu/access-time measured on hardware
/// with the BIOS's default memory-control settings.
pub fn access_cycles(phys: u32, size: Size) -> u32 {
    let (b, h, w) = match phys {
        0x0000_0000..=0x007F_FFFF => (5, 5, 5),          // RAM
        0x1F00_0000..=0x1F7F_FFFF => (7, 14, 26),        // Expansion 1
        0x1F80_0000..=0x1F80_03FF => (1, 1, 1),          // scratchpad
        0x1F80_1800..=0x1F80_1803 => (8, 14, 26),        // CD-ROM (8-bit bus)
        0x1F80_1C00..=0x1F80_1FFF => (18, 18, 39),       // SPU (16-bit bus)
        0x1F80_1000..=0x1F80_1FFF => (3, 3, 3),          // other I/O
        0x1F80_2000..=0x1F80_2FFF => (11, 26, 56),       // Expansion 2
        0x1FA0_0000..=0x1FBF_FFFF => (7, 6, 10),         // Expansion 3
        0x1FC0_0000..=0x1FC7_FFFF => (8, 13, 25),        // BIOS ROM (8-bit bus)
        0xFFFE_0000..=0xFFFE_FFFF => (1, 2, 2),          // cache control
        _ => (5, 5, 5),
    };
    match size {
        Size::Byte => b,
        Size::Half => h,
        Size::Word => w,
    }
}

#[derive(Debug)]
pub struct Bus {
    pub ram: Vec<u8>,
    pub bios: Vec<u8>,
    pub scratch: Vec<u8>,
    pub irq: Irq,
    pub cache_ctrl: u32,
    pub gpu: Gpu,
    pub dma: Dma,
    pub timers: Timers,
    pub cdrom: CdRom,
    pub spu: Spu,
    pub sio: Sio0,
    pub mdec: Mdec,
    /// Fallback storage for registers with no device behaviour (MEM_CTRL,
    /// RAM_SIZE, SIO1, expansion 2) so read-back works.
    pub io_regs: Vec<u8>,
    /// Count of accesses to unmapped addresses, for diagnostics.
    pub unhandled_io: u64,
    pending: u32,
    /// Total CPU cycles elapsed.
    pub cycles: u64,
    /// Bus time at which each DMA channel's transfer finishes (u64::MAX =
    /// none in flight). Data moves at the start; the channel stays busy and
    /// signals completion when this time is reached.
    dma_done: [u64; 7],
    /// An endless DMA list is hogging the bus: CPU memory accesses contend.
    pub dma_hog: bool,
}

#[inline(always)]
fn rd(buf: &[u8], off: usize, size: Size) -> u32 {
    match size {
        Size::Byte => buf[off] as u32,
        Size::Half => u16::from_le_bytes([buf[off], buf[off + 1]]) as u32,
        Size::Word => u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]),
    }
}

#[inline(always)]
fn wr(buf: &mut [u8], off: usize, val: u32, size: Size) {
    match size {
        Size::Byte => buf[off] = val as u8,
        Size::Half => buf[off..off + 2].copy_from_slice(&(val as u16).to_le_bytes()),
        Size::Word => buf[off..off + 4].copy_from_slice(&val.to_le_bytes()),
    }
}

#[inline(always)]
fn width_mask(size: Size) -> u32 {
    match size {
        Size::Byte => 0xFF,
        Size::Half => 0xFFFF,
        Size::Word => 0xFFFF_FFFF,
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        Bus {
            ram: vec![0; RAM_SIZE],
            bios: vec![0; BIOS_SIZE],
            scratch: vec![0; SCRATCH_SIZE],
            irq: Irq::default(),
            cache_ctrl: 0,
            gpu: Gpu::new(),
            dma: Dma::default(),
            timers: Timers::default(),
            cdrom: CdRom::new(),
            spu: Spu::new(),
            sio: Sio0::new(),
            mdec: Mdec::new(),
            io_regs: vec![0; 0x2000],
            unhandled_io: 0,
            pending: 0,
            cycles: 0,
            dma_done: [u64::MAX; 7],
            dma_hog: false,
        }
    }

    pub fn load_bios(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() != BIOS_SIZE {
            return Err(format!("BIOS must be {} bytes, got {}", BIOS_SIZE, data.len()));
        }
        self.bios.copy_from_slice(data);
        Ok(())
    }

    // ------------------------------------------------------------- clock
    /// Current time in CPU cycles, including cycles not yet synced.
    #[inline(always)]
    pub fn now(&self) -> u64 {
        self.cycles + self.pending as u64
    }

    /// Instruction cache enabled (cache control register bit 11).
    #[inline(always)]
    pub fn icache_enabled(&self) -> bool {
        self.cache_ctrl & 0x800 != 0
    }

    #[inline(always)]
    pub fn add_cycles(&mut self, c: u32) {
        self.pending += c;
        if self.pending >= SYNC_INTERVAL {
            self.sync();
        }
    }

    /// Bring every device up to date with the CPU.
    pub fn sync(&mut self) {
        let c = self.pending;
        if c == 0 {
            return;
        }
        self.pending = 0;
        self.cycles += c as u64;
        let ev = self.gpu.tick(c, &mut self.irq);
        self.timers.tick(c, &ev, &mut self.irq);
        self.cdrom.tick(c, &mut self.irq);
        self.spu.tick(c, &mut self.cdrom, &mut self.irq);
        self.sio.tick(c, &mut self.irq);
        self.gpu.now = self.cycles;
        for ch in 0..7 {
            if self.dma_done[ch] <= self.cycles {
                self.dma_done[ch] = u64::MAX;
                self.dma.complete(ch, &mut self.irq);
            }
        }
    }

    // ------------------------------------------------------------- memory
    pub fn read(&mut self, vaddr: u32, size: Size) -> u32 {
        let seg = vaddr >> 29;
        let a = mask_region(vaddr);
        match a {
            0x0000_0000..=0x007F_FFFF => rd(&self.ram, (a as usize) & (RAM_SIZE - 1), size),
            0x1F00_0000..=0x1F7F_FFFF => width_mask(size),
            0x1F80_0000..=0x1F80_03FF if seg != 5 => rd(&self.scratch, (a & 0x3FF) as usize, size),
            0x1F80_1000..=0x1F80_2FFF => self.io_read(a, size),
            0x1FC0_0000..=0x1FC7_FFFF => rd(&self.bios, (a & 0x7FFFF) as usize, size),
            0xFFFE_0130 => self.cache_ctrl,
            _ => {
                self.unhandled_io += 1;
                0
            }
        }
    }

    pub fn write(&mut self, vaddr: u32, val: u32, size: Size) {
        let seg = vaddr >> 29;
        let a = mask_region(vaddr);
        match a {
            0x0000_0000..=0x007F_FFFF => wr(&mut self.ram, (a as usize) & (RAM_SIZE - 1), val, size),
            0x1F80_0000..=0x1F80_03FF if seg != 5 => wr(&mut self.scratch, (a & 0x3FF) as usize, val, size),
            0x1F80_1000..=0x1F80_2FFF => self.io_write(a, val, size),
            0x1FC0_0000..=0x1FC7_FFFF => {} // ROM
            0xFFFE_0130 => self.cache_ctrl = val,
            _ => self.unhandled_io += 1,
        }
    }

    // ------------------------------------------------------------- I/O
    fn io_read(&mut self, a: u32, size: Size) -> u32 {
        self.sync();
        let lane = (a & 3) * 8;
        let m = width_mask(size);
        match a {
            0x1F80_1040..=0x1F80_104F => {
                let off = a - 0x1F80_1040;
                let base = if off < 4 { 0 } else { off & !1 };
                (self.sio.read(base) >> ((off - base) * 8)) & m
            }
            0x1F80_1070..=0x1F80_1077 => {
                let v = if a & !3 == 0x1F80_1070 { self.irq.stat } else { self.irq.mask };
                (v >> lane) & m
            }
            0x1F80_1080..=0x1F80_10FF => (self.dma.read(a & 0x7C) >> lane) & m,
            0x1F80_1100..=0x1F80_112F => (self.timers.read((a - 0x1F80_1100) & !3) >> lane) & m,
            0x1F80_1800..=0x1F80_1803 => {
                let mut v = 0u32;
                for k in 0..size as u32 {
                    v |= (self.cdrom.read((a + k) & 3) as u32) << (8 * k);
                }
                v
            }
            0x1F80_1810..=0x1F80_1813 => (self.gpu.read_data() >> lane) & m,
            0x1F80_1814..=0x1F80_1817 => (self.gpu.status() >> lane) & m,
            0x1F80_1820..=0x1F80_1823 => (self.mdec.read_data() >> lane) & m,
            0x1F80_1824..=0x1F80_1827 => (self.mdec.status() >> lane) & m,
            0x1F80_1C00..=0x1F80_1FFF => {
                let off = (a - 0x1F80_1C00) & !1;
                match size {
                    Size::Word => self.spu.read16(off) as u32 | ((self.spu.read16(off + 2) as u32) << 16),
                    Size::Half => self.spu.read16(off) as u32,
                    Size::Byte => (self.spu.read16(off) >> ((a & 1) * 8)) as u32 & 0xFF,
                }
            }
            // Expansion 2: open bus
            0x1F80_2000..=0x1F80_2FFF => width_mask(size),
            _ => rd(&self.io_regs, (a - 0x1F80_1000) as usize, size),
        }
    }

    fn io_write(&mut self, a: u32, val: u32, size: Size) {
        self.sync();
        let lane = (a & 3) * 8;
        let m = width_mask(size);
        match a {
            0x1F80_1040..=0x1F80_104F => {
                let off = a - 0x1F80_1040;
                let base = if off < 4 { 0 } else { off & !1 };
                self.sio.write(base, val);
            }
            0x1F80_1070..=0x1F80_1077 => {
                let lm = m << lane;
                let v = val << lane;
                if a & !3 == 0x1F80_1070 {
                    self.irq.ack(v | !lm);
                } else {
                    self.irq.set_mask((self.irq.mask & !lm) | (v & lm));
                }
            }
            0x1F80_1080..=0x1F80_10FF => {
                let reg = a & 0x7C;
                let mut old = self.dma.read(reg);
                if reg == 0x74 {
                    // DICR flags are write-1-to-acknowledge: never echo them back.
                    old &= !0x7F00_0000;
                }
                let lm = m << lane;
                let v = (old & !lm) | ((val << lane) & lm);
                let chcr2 = self.dma.ch[dma::GPU].chcr;
                if self.dma_hog && chcr2 & (1 << 24) == 0 {
                    self.dma_hog = false;
                }
                if let Some(ch) = self.dma.write(reg, v, &mut self.irq) {
                    // DPCR/DICR writes may start a waiting channel, but never
                    // one whose transfer is still in flight
                    if reg < 0x70 || self.dma_done[ch] == u64::MAX {
                        self.run_dma(ch);
                    }
                }
                if self.dma_hog && self.dma.ch[dma::GPU].chcr & (1 << 24) == 0 {
                    self.dma_hog = false;
                }
            }
            0x1F80_1100..=0x1F80_112F => {
                let (hb, vb) = (self.gpu.in_hblank, self.gpu.in_vblank);
                self.timers.write((a - 0x1F80_1100) & !3, val << lane, hb, vb);
            }
            0x1F80_1800..=0x1F80_1803 => {
                for k in 0..size as u32 {
                    self.cdrom.write((a + k) & 3, (val >> (8 * k)) as u8, &mut self.irq);
                }
            }
            0x1F80_1810..=0x1F80_1813 => self.gpu.gp0(val, &mut self.irq),
            0x1F80_1814..=0x1F80_1817 => self.gpu.gp1(val, &mut self.irq),
            0x1F80_1820..=0x1F80_1823 => self.mdec.write_command(val),
            0x1F80_1824..=0x1F80_1827 => self.mdec.write_control(val),
            0x1F80_1C00..=0x1F80_1FFF => {
                let off = (a - 0x1F80_1C00) & !1;
                match size {
                    Size::Word => {
                        self.spu.write16(off, val as u16, &mut self.irq);
                        self.spu.write16(off + 2, (val >> 16) as u16, &mut self.irq);
                    }
                    _ => self.spu.write16(off, val as u16, &mut self.irq),
                }
            }
            _ => wr(&mut self.io_regs, (a - 0x1F80_1000) as usize, val, size),
        }
    }

    // ------------------------------------------------------------- DMA engine
    fn ram_word(&self, addr: u32) -> u32 {
        rd(&self.ram, (addr & 0x1F_FFFC) as usize, Size::Word)
    }

    fn set_ram_word(&mut self, addr: u32, v: u32) {
        wr(&mut self.ram, (addr & 0x1F_FFFC) as usize, v, Size::Word);
    }

    /// Run channel `ch`: data moves now; the CPU is stalled while the DMA
    /// owns the bus, and the channel completes after the transfer duration.
    /// Costs calibrated against ps1-tests dma/chopping (real hardware).
    pub fn run_dma(&mut self, ch: usize) {
        // A transfer still finishing on this channel completes first, so its
        // completion (and IRQ) is never lost.
        if self.dma_done[ch] != u64::MAX {
            self.dma_done[ch] = u64::MAX;
            self.dma.complete(ch, &mut self.irq);
            self.dma.ch[ch].chcr |= 1 << 24;
        }
        let c = self.dma.ch[ch];
        let mut nodes = 0u64;
        let mut moved = 0u64;
        let mut looping = false;
        if c.sync_mode() == 2 {
            if ch == dma::GPU && c.from_ram() {
                let mut addr = c.madr & 0x1F_FFFC;
                let mut seen = std::collections::HashSet::new();
                // A list that revisits a node never ends on hardware: the
                // channel stays busy (no completion, no IRQ) until stopped
                // (ps1-tests dma/chain-looping).
                for _ in 0..0x10_0000 {
                    if !seen.insert(addr) {
                        looping = true;
                        break;
                    }
                    let header = self.ram_word(addr);
                    let count = header >> 24;
                    nodes += 1;
                    moved += count as u64;
                    for k in 1..=count {
                        let w = self.ram_word(addr.wrapping_add(4 * k));
                        self.gpu.gp0(w, &mut self.irq);
                    }
                    if header & 0x80_0000 != 0 {
                        break;
                    }
                    addr = header & 0x1F_FFFC;
                }
            }
            self.dma.ch[ch].madr = 0x00FF_FFFF;
        } else {
            let words = c.transfer_words().unwrap_or(0);
            let step: u32 = if c.step_back() { 4u32.wrapping_neg() } else { 4 };
            let mut addr = c.madr & 0x1F_FFFC;
            for k in 0..words {
                if c.from_ram() {
                    let v = self.ram_word(addr);
                    match ch {
                        dma::MDEC_IN => self.mdec.write_command(v),
                        dma::GPU => self.gpu.gp0(v, &mut self.irq),
                        dma::SPU => self.spu.dma_write(v, &mut self.irq),
                        _ => {}
                    }
                } else {
                    let v = match ch {
                        dma::MDEC_OUT => self.mdec.read_dma(),
                        dma::GPU => self.gpu.read_data(),
                        dma::CDROM => self.cdrom.dma_read_word(),
                        dma::SPU => self.spu.dma_read(&mut self.irq),
                        dma::OTC => {
                            if k == words - 1 {
                                0x00FF_FFFF
                            } else {
                                addr.wrapping_sub(4) & 0x1F_FFFF
                            }
                        }
                        _ => 0,
                    };
                    self.set_ram_word(addr, v);
                }
                addr = addr.wrapping_add(step) & 0x1F_FFFC;
            }
            if c.sync_mode() == 1 {
                self.dma.ch[ch].madr = addr;
                self.dma.ch[ch].bcr &= 0xFFFF;
            }
            moved = words as u64;
        }

        // SPU RAM sits behind the SPU's slow 16-bit interface: ~16 cycles per
        // byte (ps1-tests spu/memory-transfer timing bounds, measured on HW).
        // Per-word cost x1024. VRAM->RAM reads are slower than writes:
        // ~1.63 cycles/word (ps1-tests gpu/bandwidth vramToCpu, hardware).
        let per_word_1024 = match ch {
            dma::SPU => 64 * 1024,
            dma::GPU if !c.from_ram() => 1670,
            _ => 1024,
        };
        let busy = moved * per_word_1024 / 1024;
        let (duration, stall) = match c.sync_mode() {
            0 if c.chcr & (1 << 8) != 0 => {
                // chopping: dma window words, then cpu window clocks for the CPU
                let dma_win = 1u64 << ((c.chcr >> 16) & 7);
                let cpu_win = 1u64 << ((c.chcr >> 20) & 7);
                let chunks = moved.div_ceil(dma_win).max(1);
                (busy + chunks * (6 + cpu_win), busy + chunks * 6)
            }
            // block mode: the CPU is stalled for the whole transfer, so even
            // the start-up latency is spent stalled (ps1-tests otc-test reads
            // CHCR right after starting and sees it finished)
            0 => (busy + 10 + 200, busy + 10 + 200),
            1 => {
                let bs = (c.bcr & 0xFFFF).max(1) as u64;
                let blocks = moved.div_ceil(bs).max(1);
                // CPU runs between blocks; it only loses the bus cycles
                (busy + blocks * 10, moved)
            }
            _ => (moved + nodes * 10, moved + nodes),
        };
        // Fixed start-up latency (block mode includes it above): one
        // parameter fitted to the 130 rows of ps1-tests dma/chopping
        // (mean error 5.3% -> 2.0%).
        let duration = if c.sync_mode() == 0 && c.chcr & (1 << 8) == 0 { duration } else { duration + 200 };
        if looping {
            self.dma.ch[ch].madr = c.madr; // still walking the list
            self.dma_hog = true;
            return;
        }
        if stall > 0 {
            self.add_cycles(stall as u32);
        }
        let mut duration = duration;
        if ch == dma::GPU {
            // the channel can't finish before the GPU has consumed the data
            let now = self.now() + stall;
            duration = duration.max(stall + self.gpu.busy_until.saturating_sub(now));
        }
        if duration > stall {
            self.dma_done[ch] = self.now() + (duration - stall);
            if ch == dma::GPU {
                // GPU keeps reporting busy until the data has "arrived"
                self.gpu.dma_busy_until = self.dma_done[ch];
            }
        } else {
            self.dma.complete(ch, &mut self.irq);
        }
    }
}

impl Bus {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("BUS ");
        s.buf(&mut self.ram);
        s.buf(&mut self.scratch);
        s.u32(&mut self.cache_ctrl);
        s.buf(&mut self.io_regs);
        s.u32(&mut self.pending);
        s.u64(&mut self.cycles);
        for d in self.dma_done.iter_mut() {
            s.u64(d);
        }
        s.bool(&mut self.dma_hog);
        self.irq.snap(s);
        self.gpu.snap(s);
        self.dma.snap(s);
        self.timers.snap(s);
        self.cdrom.snap(s);
        self.spu.snap(s);
        self.sio.snap(s);
        self.mdec.snap(s);
    }
}
