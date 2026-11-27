//! MIPS R3000A interpreter (PlayStation CPU) with System Control Coprocessor
//! (COP0). Implements:
//!   * every R3000A integer instruction, incl. LWL/LWR/SWL/SWR
//!   * branch delay slots and load delay slots (with the LWL/LWR bypass)
//!   * precise exceptions: AdEL/AdES, Syscall, Break, RI, CpU, Ov, Int
//!   * COP0 SR/CAUSE/EPC/BadVaddr/PRId, RFE, cache isolation
//!
//! COP2 instructions are forwarded to the GTE (gte.rs).

use crate::bus::{Bus, Size};
use crate::gte::Gte;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Exception {
    Interrupt = 0x0,
    LoadAddressError = 0x4,
    StoreAddressError = 0x5,
    Syscall = 0x8,
    Break = 0x9,
    ReservedInstruction = 0xA,
    CoprocessorUnusable = 0xB,
    InstructionBusError = 0x6,
    Overflow = 0xC,
}

pub const SR_IEC: u32 = 1 << 0;
pub const SR_ISC: u32 = 1 << 16;
pub const SR_BEV: u32 = 1 << 22;
pub const SR_CU2: u32 = 1 << 30;
pub const PRID: u32 = 0x0000_0002;

#[derive(Debug, Clone, Default)]
pub struct Cop0 {
    pub sr: u32,
    pub cause: u32,
    pub epc: u32,
    pub bad_vaddr: u32,
    // Debug registers: stored, no breakpoint logic.
    pub bpc: u32,
    pub bda: u32,
    pub jumpdest: u32,
    pub dcic: u32,
    pub bdam: u32,
    pub bpcm: u32,
}

#[derive(Debug)]
pub struct Cpu {
    /// Architectural register values visible to the current instruction.
    pub regs: [u32; 32],
    /// Register values after the current instruction (load delay emulation).
    out_regs: [u32; 32],
    /// Pending delayed load (register, value).
    load: (usize, u32),
    pub hi: u32,
    pub lo: u32,
    /// Address of the instruction being executed.
    pub current_pc: u32,
    /// Address of the next instruction to fetch.
    pub pc: u32,
    pub next_pc: u32,
    /// Set by a branch/jump executed in this step (so the next is a delay slot).
    branch: bool,
    /// The instruction being executed sits in a branch delay slot.
    delay_slot: bool,
    pub cop0: Cop0,
    pub gte: Gte,
    /// Retired instruction count.
    pub instructions: u64,
    /// Instruction cache tags (physical line address) and per-word valid
    /// bits. Timing only: instructions are always read from memory.
    icache_tag: Vec<u32>,
    icache_valid: Vec<u8>,
    /// Cycles consumed by the instruction being executed.
    cycles: u32,
    /// Bus time at which the multiplier/divider result is ready.
    muldiv_done: u64,
    /// Bus time at which the current GTE command finishes.
    gte_done: u64,
    /// Bus time at the start of this step (for latency bookkeeping).
    step_start: u64,
    /// Cycles of this step already handed to the bus (so devices see the
    /// right time when an access samples them).
    committed: u32,
    /// Bus time at which the write buffer has drained (stores to RAM/IO are
    /// buffered; loads and uncached fetches must wait for the bus).
    wbuf_free: u64,
    /// Extra cycles per CPU bus access while a DMA hogs the bus (per step).
    hog: u32,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

/// Bus cycles a buffered RAM write occupies (fitted to ps1-tests
/// dma/chain-looping's load/add/store loop measured on hardware).
const RAM_WRITE_CYCLES: u32 = 8;

/// Bus contention per CPU memory access while an endless DMA list hogs the
/// bus (fitted to ps1-tests dma/chain-looping).
const DMA_HOG_CYCLES: u32 = 5;

/// Multiplier latency by operand magnitude (psx-spx "CPU Timings").
#[inline(always)]
fn mult_cycles(v: u32) -> u32 {
    if v < 0x800 {
        6
    } else if v < 0x10_0000 {
        9
    } else {
        13
    }
}

#[inline(always)] fn op_rs(i: u32) -> usize { ((i >> 21) & 0x1F) as usize }
#[inline(always)] fn op_rt(i: u32) -> usize { ((i >> 16) & 0x1F) as usize }
#[inline(always)] fn op_rd(i: u32) -> usize { ((i >> 11) & 0x1F) as usize }
#[inline(always)] fn op_shamt(i: u32) -> u32 { (i >> 6) & 0x1F }
#[inline(always)] fn op_imm(i: u32) -> u32 { i & 0xFFFF }
#[inline(always)] fn op_imm_se(i: u32) -> u32 { (i & 0xFFFF) as i16 as u32 }

impl Cpu {
    pub fn new() -> Self {
        let pc = 0xBFC0_0000;
        Cpu {
            regs: [0; 32],
            out_regs: [0; 32],
            load: (0, 0),
            hi: 0,
            lo: 0,
            current_pc: pc,
            pc,
            next_pc: pc.wrapping_add(4),
            branch: false,
            delay_slot: false,
            cop0: Cop0 { sr: SR_BEV, ..Default::default() },
            gte: Gte::new(),
            instructions: 0,
            icache_tag: vec![u32::MAX; 256],
            icache_valid: vec![0; 256],
            cycles: 0,
            muldiv_done: 0,
            gte_done: 0,
            step_start: 0,
            committed: 0,
            wbuf_free: 0,
            hog: 0,
        }
    }

    /// Jump the CPU to `pc` (used for EXE side-loading and tests).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
    }

    #[inline(always)]
    pub fn reg(&self, r: usize) -> u32 {
        self.regs[r]
    }

    /// Set a register immediately (outside of instruction execution).
    pub fn set_reg_now(&mut self, r: usize, v: u32) {
        self.regs[r] = v;
        self.out_regs[r] = v;
        self.regs[0] = 0;
        self.out_regs[0] = 0;
    }

    #[inline(always)]
    fn set_reg(&mut self, r: usize, v: u32) {
        self.out_regs[r] = v;
        self.out_regs[0] = 0;
    }

    /// Current time inside this step.
    #[inline(always)]
    fn now(&self) -> u64 {
        self.step_start + self.cycles as u64
    }
    // note: step_start is taken before any commit, so now() stays correct

    /// Stall until bus time `t`.
    #[inline(always)]
    fn stall_until(&mut self, t: u64) {
        let n = self.now();
        if t > n {
            self.cycles += (t - n) as u32;
        }
    }

    /// Instruction fetch cost: I-cache hit = 1 cycle; a miss fills the line
    /// from the fetched word to its end; uncached fetches pay the full
    /// memory access time.
    #[inline(always)]
    fn fetch_cycles(&mut self, bus: &Bus, pc: u32) -> u32 {
        let phys = crate::bus::mask_region(pc);
        let cached = pc >> 29 <= 4 && bus.icache_enabled();
        let mem = crate::bus::access_cycles(phys, Size::Word);
        if !cached {
            return mem;
        }
        let line = ((pc >> 4) & 0xFF) as usize;
        let word = (pc >> 2) & 3;
        let tag = phys & !0xF;
        if self.icache_tag[line] == tag && self.icache_valid[line] & (1 << word) != 0 {
            return 1;
        }
        if self.icache_tag[line] != tag {
            self.icache_tag[line] = tag;
            self.icache_valid[line] = 0;
        }
        self.icache_valid[line] |= (0xF << word) & 0xF;
        // first word at memory speed, the rest of the burst 1 cycle each
        mem + (3 - word)
    }

    /// Execute one instruction; returns the CPU cycles it took.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        self.cycles = 0;
        self.committed = 0;
        self.step_start = bus.now();
        self.hog = if bus.dma_hog { DMA_HOG_CYCLES } else { 0 };
        self.step_inner(bus);
        // remaining (uncommitted) cycles; at least 1 per step overall
        let total = self.cycles.max(1);
        let rest = total - self.committed.min(total);
        // per-instruction scratch: nothing survives between steps
        self.cycles = 0;
        self.committed = 0;
        self.step_start = 0;
        self.hog = 0;
        rest
    }

    /// Hand the cycles elapsed so far in this instruction to the bus.
    #[inline(always)]
    fn commit(&mut self, bus: &mut Bus) {
        let n = self.cycles - self.committed;
        if n > 0 {
            bus.add_cycles(n);
            self.committed = self.cycles;
        }
    }

    fn step_inner(&mut self, bus: &mut Bus) {
        self.current_pc = self.pc;
        self.delay_slot = self.branch;
        self.branch = false;

        // External interrupt line -> CAUSE.IP2
        if bus.irq.pending() {
            self.cop0.cause |= 1 << 10;
        } else {
            self.cop0.cause &= !(1 << 10);
        }

        if self.current_pc & 3 != 0 {
            self.cop0.bad_vaddr = self.current_pc;
            self.exception(Exception::LoadAddressError);
            self.retire();
            return;
        }

        // The scratchpad is data-only: instruction fetches from it fault.
        let phys = crate::bus::mask_region(self.current_pc);
        if (0x1F80_0000..0x1F80_0400).contains(&phys) && self.current_pc >> 29 != 5 {
            self.exception(Exception::InstructionBusError);
            self.retire();
            return;
        }

        let fetch = self.fetch_cycles(bus, self.current_pc);
        if fetch > 1 {
            // memory fetch (cache miss / uncached) needs the bus
            self.stall_until(self.wbuf_free);
        }
        self.cycles += fetch;
        let instr = bus.read(self.current_pc, Size::Word);

        // Interrupts are taken before the instruction executes; EPC points
        // at it so it re-executes after RFE. GTE instructions are special:
        // hardware executes them even when interrupted, so we let them run
        // first (matches the well-known BIOS/game workaround behaviour).
        let ip = (self.cop0.cause & self.cop0.sr) & 0xFF00;
        if self.cop0.sr & SR_IEC != 0 && ip != 0 {
            if (instr >> 25) == 0x25 {
                self.gte.command(instr & 0x1FF_FFFF);
            }
            self.exception(Exception::Interrupt);
            self.retire();
            return;
        }

        self.pc = self.next_pc;
        self.next_pc = self.next_pc.wrapping_add(4);

        // Apply the pending load delayed from the previous instruction.
        let (r, v) = self.load;
        let before = self.out_regs[r];
        self.set_reg(r, v);
        self.load = (0, 0);

        self.execute(instr, bus);

        // A load issued in the delay slot of a load to the same register
        // cancels the earlier one: its value never becomes visible.
        // (Matches PCSX-Redux and DuckStation; found by differential fuzzing.)
        // LWL/LWR already merged with the in-flight value above.
        if r != 0 && self.load.0 == r {
            self.out_regs[r] = before;
        }
        self.retire();
        self.instructions += 1;
    }

    #[inline(always)]
    fn retire(&mut self) {
        self.regs = self.out_regs;
    }

    pub fn exception(&mut self, cause: Exception) {
        let handler = if self.cop0.sr & SR_BEV != 0 { 0xBFC0_0180 } else { 0x8000_0080 };
        // Push the KU/IE mode stack.
        let mode = self.cop0.sr & 0x3F;
        self.cop0.sr = (self.cop0.sr & !0x3F) | ((mode << 2) & 0x3F);

        self.cop0.cause = (self.cop0.cause & !0x8000_007C) | ((cause as u32) << 2);
        self.cop0.epc = self.current_pc;
        if self.delay_slot {
            self.cop0.epc = self.cop0.epc.wrapping_sub(4);
            self.cop0.cause |= 1 << 31;
        }
        // A pending load still lands (it was issued before the fault).
        let (r, v) = self.load;
        self.set_reg(r, v);
        self.load = (0, 0);

        self.pc = handler;
        self.next_pc = handler.wrapping_add(4);
        self.branch = false;
    }

    #[inline(always)]
    fn branch_to(&mut self, offset: u32) {
        // self.pc already points at the delay slot.
        self.next_pc = self.pc.wrapping_add(offset << 2);
        self.branch = true;
    }

    /// Extra cycles a data read adds beyond the instruction's own cycle.
    #[inline(always)]
    fn load_cycles(&mut self, addr: u32, size: Size) {
        let phys = crate::bus::mask_region(addr);
        if !(0x1F80_0000..0x1F80_0400).contains(&phys) {
            self.stall_until(self.wbuf_free); // reads wait for buffered writes
            self.cycles += self.hog;
        }
        self.cycles += crate::bus::access_cycles(phys, size) - 1;
    }

    /// Queue a store in the 4-entry write buffer.
    #[inline(always)]
    fn store_cycles(&mut self, addr: u32, size: Size) {
        let phys = crate::bus::mask_region(addr);
        if (0x1F80_0000..0x1F80_0400).contains(&phys) {
            return; // scratchpad: no bus involved
        }
        let cost = match phys {
            0x0000_0000..=0x007F_FFFF => RAM_WRITE_CYCLES,
            _ => crate::bus::access_cycles(phys, size),
        } as u64
            + self.hog as u64;
        let now = self.now();
        // buffer full (4 entries in flight): stall until one drains
        if self.wbuf_free > now + 4 * cost {
            self.stall_until(self.wbuf_free - 4 * cost);
        }
        self.wbuf_free = self.wbuf_free.max(self.now()) + cost;
    }

    fn load_mem(&mut self, bus: &mut Bus, addr: u32, size: Size) -> Option<u32> {
        if addr & (size as u32 - 1) != 0 {
            self.cop0.bad_vaddr = addr;
            self.exception(Exception::LoadAddressError);
            return None;
        }
        self.load_cycles(addr, size);
        self.commit(bus);
        Some(bus.read(addr, size))
    }

    fn store_mem(&mut self, bus: &mut Bus, addr: u32, val: u32, size: Size) {
        if addr & (size as u32 - 1) != 0 {
            self.cop0.bad_vaddr = addr;
            self.exception(Exception::StoreAddressError);
            return;
        }
        if self.cop0.sr & SR_ISC != 0 {
            // Cache isolated: writes go to the I-cache (the BIOS uses this
            // to flush it), not to memory.
            let line = ((addr >> 4) & 0xFF) as usize;
            self.icache_valid[line] = 0;
            return;
        }
        self.store_cycles(addr, size);
        self.commit(bus);
        bus.write(addr, val, size);
    }

    fn execute(&mut self, i: u32, bus: &mut Bus) {
        let rs = op_rs(i);
        let rt = op_rt(i);
        let s = self.regs[rs];
        let t = self.regs[rt];

        match i >> 26 {
            0x00 => self.execute_special(i, bus),
            0x01 => {
                // BLTZ / BGEZ / BLTZAL / BGEZAL (decoding quirks as on hw:
                // only bit 0 picks GE/LT; link iff rt[4:1] == 0b1000).
                let ge = (i >> 16) & 1 != 0;
                let link = (i >> 17) & 0xF == 0x8;
                let taken = ((s as i32) < 0) ^ ge;
                if link {
                    let ra = self.next_pc;
                    self.set_reg(31, ra);
                }
                if taken {
                    self.branch_to(op_imm_se(i));
                }
            }
            0x02 => {
                self.next_pc = (self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2);
                self.branch = true;
            }
            0x03 => {
                let ra = self.next_pc;
                self.set_reg(31, ra);
                self.next_pc = (self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2);
                self.branch = true;
            }
            0x04 => if s == t { self.branch_to(op_imm_se(i)) },
            0x05 => if s != t { self.branch_to(op_imm_se(i)) },
            0x06 => if (s as i32) <= 0 { self.branch_to(op_imm_se(i)) },
            0x07 => if (s as i32) > 0 { self.branch_to(op_imm_se(i)) },
            0x08 => match (s as i32).checked_add(op_imm_se(i) as i32) {
                Some(v) => self.set_reg(rt, v as u32),
                None => self.exception(Exception::Overflow),
            },
            0x09 => self.set_reg(rt, s.wrapping_add(op_imm_se(i))),
            0x0A => self.set_reg(rt, ((s as i32) < (op_imm_se(i) as i32)) as u32),
            0x0B => self.set_reg(rt, (s < op_imm_se(i)) as u32),
            0x0C => self.set_reg(rt, s & op_imm(i)),
            0x0D => self.set_reg(rt, s | op_imm(i)),
            0x0E => self.set_reg(rt, s ^ op_imm(i)),
            0x0F => self.set_reg(rt, op_imm(i) << 16),
            0x10 => self.execute_cop0(i),
            0x11 | 0x13 => self.cop_if_disabled(((i >> 26) & 3) as u32),
            0x12 => self.execute_cop2(i),
            0x20 => {
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Byte) {
                    self.load = (rt, v as i8 as u32);
                }
            }
            0x21 => {
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Half) {
                    self.load = (rt, v as i16 as u32);
                }
            }
            0x22 => {
                // LWL: merge with the in-flight value of rt (load-delay bypass)
                let a = s.wrapping_add(op_imm_se(i));
                let cur = self.out_regs[rt];
                self.load_cycles(a, Size::Word);
                let w = bus.read(a & !3, Size::Word);
                let v = match a & 3 {
                    0 => (cur & 0x00FF_FFFF) | (w << 24),
                    1 => (cur & 0x0000_FFFF) | (w << 16),
                    2 => (cur & 0x0000_00FF) | (w << 8),
                    _ => w,
                };
                self.load = (rt, v);
            }
            0x23 => {
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Word) {
                    self.load = (rt, v);
                }
            }
            0x24 => {
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Byte) {
                    self.load = (rt, v);
                }
            }
            0x25 => {
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Half) {
                    self.load = (rt, v);
                }
            }
            0x26 => {
                let a = s.wrapping_add(op_imm_se(i));
                let cur = self.out_regs[rt];
                self.load_cycles(a, Size::Word);
                let w = bus.read(a & !3, Size::Word);
                let v = match a & 3 {
                    0 => w,
                    1 => (cur & 0xFF00_0000) | (w >> 8),
                    2 => (cur & 0xFFFF_0000) | (w >> 16),
                    _ => (cur & 0xFFFF_FF00) | (w >> 24),
                };
                self.load = (rt, v);
            }
            0x28 => {
                let a = s.wrapping_add(op_imm_se(i));
                self.store_mem(bus, a, t, Size::Byte);
            }
            0x29 => {
                let a = s.wrapping_add(op_imm_se(i));
                self.store_mem(bus, a, t, Size::Half);
            }
            0x2A => {
                let a = s.wrapping_add(op_imm_se(i));
                if self.cop0.sr & SR_ISC == 0 {
                    self.store_cycles(a, Size::Word);
                    self.commit(bus);
                    let m = bus.read(a & !3, Size::Word);
                    let v = match a & 3 {
                        0 => (m & 0xFFFF_FF00) | (t >> 24),
                        1 => (m & 0xFFFF_0000) | (t >> 16),
                        2 => (m & 0xFF00_0000) | (t >> 8),
                        _ => t,
                    };
                    bus.write(a & !3, v, Size::Word);
                }
            }
            0x2B => {
                let a = s.wrapping_add(op_imm_se(i));
                self.store_mem(bus, a, t, Size::Word);
            }
            0x2E => {
                let a = s.wrapping_add(op_imm_se(i));
                if self.cop0.sr & SR_ISC == 0 {
                    self.store_cycles(a, Size::Word);
                    self.commit(bus);
                    let m = bus.read(a & !3, Size::Word);
                    let v = match a & 3 {
                        0 => t,
                        1 => (m & 0x0000_00FF) | (t << 8),
                        2 => (m & 0x0000_FFFF) | (t << 16),
                        _ => (m & 0x00FF_FFFF) | (t << 24),
                    };
                    bus.write(a & !3, v, Size::Word);
                }
            }
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => {
                // LWC0/1/3, SWC0/1/3: no such coprocessor hardware; only
                // fault when the CU bit is clear, otherwise a no-op.
                self.cop_if_disabled(((i >> 26) & 3) as u32)
            }
            0x32 => {
                // LWC2
                if self.cop0.sr & SR_CU2 == 0 {
                    return self.cop_unusable(2);
                }
                let a = s.wrapping_add(op_imm_se(i));
                if let Some(v) = self.load_mem(bus, a, Size::Word) {
                    self.stall_until(self.gte_done);
                    self.gte.write_data(rt as u32, v);
                }
            }
            0x3A => {
                // SWC2
                if self.cop0.sr & SR_CU2 == 0 {
                    return self.cop_unusable(2);
                }
                let a = s.wrapping_add(op_imm_se(i));
                self.stall_until(self.gte_done);
                let v = self.gte.read_data(rt as u32);
                self.store_mem(bus, a, v, Size::Word);
            }
            _ => self.exception(Exception::ReservedInstruction),
        }
    }

    fn execute_special(&mut self, i: u32, _bus: &mut Bus) {
        let rs = op_rs(i);
        let rt = op_rt(i);
        let rd = op_rd(i);
        let s = self.regs[rs];
        let t = self.regs[rt];

        match i & 0x3F {
            0x00 => self.set_reg(rd, t << op_shamt(i)),
            0x02 => self.set_reg(rd, t >> op_shamt(i)),
            0x03 => self.set_reg(rd, ((t as i32) >> op_shamt(i)) as u32),
            0x04 => self.set_reg(rd, t << (s & 0x1F)),
            0x06 => self.set_reg(rd, t >> (s & 0x1F)),
            0x07 => self.set_reg(rd, ((t as i32) >> (s & 0x1F)) as u32),
            0x08 => {
                self.next_pc = s;
                self.branch = true;
            }
            0x09 => {
                let ra = self.next_pc;
                self.set_reg(rd, ra);
                self.next_pc = s;
                self.branch = true;
            }
            0x0C => self.exception(Exception::Syscall),
            0x0D => self.exception(Exception::Break),
            0x10 => {
                self.stall_until(self.muldiv_done);
                self.set_reg(rd, self.hi)
            }
            0x11 => self.hi = s,
            0x12 => {
                self.stall_until(self.muldiv_done);
                self.set_reg(rd, self.lo)
            }
            0x13 => self.lo = s,
            0x18 => {
                self.muldiv_done = self.now() + mult_cycles(if (s as i32) < 0 { !s } else { s }) as u64;
                let r = (s as i32 as i64) * (t as i32 as i64);
                self.hi = (r >> 32) as u32;
                self.lo = r as u32;
            }
            0x19 => {
                self.muldiv_done = self.now() + mult_cycles(s) as u64;
                let r = (s as u64) * (t as u64);
                self.hi = (r >> 32) as u32;
                self.lo = r as u32;
            }
            0x1A => {
                self.muldiv_done = self.now() + 36;
                let n = s as i32;
                let d = t as i32;
                if d == 0 {
                    self.hi = n as u32;
                    self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
                } else if n as u32 == 0x8000_0000 && d == -1 {
                    self.hi = 0;
                    self.lo = 0x8000_0000;
                } else {
                    self.hi = (n % d) as u32;
                    self.lo = (n / d) as u32;
                }
            }
            0x1B => {
                self.muldiv_done = self.now() + 36;
                if t == 0 {
                    self.hi = s;
                    self.lo = 0xFFFF_FFFF;
                } else {
                    self.hi = s % t;
                    self.lo = s / t;
                }
            }
            0x20 => match (s as i32).checked_add(t as i32) {
                Some(v) => self.set_reg(rd, v as u32),
                None => self.exception(Exception::Overflow),
            },
            0x21 => self.set_reg(rd, s.wrapping_add(t)),
            0x22 => match (s as i32).checked_sub(t as i32) {
                Some(v) => self.set_reg(rd, v as u32),
                None => self.exception(Exception::Overflow),
            },
            0x23 => self.set_reg(rd, s.wrapping_sub(t)),
            0x24 => self.set_reg(rd, s & t),
            0x25 => self.set_reg(rd, s | t),
            0x26 => self.set_reg(rd, s ^ t),
            0x27 => self.set_reg(rd, !(s | t)),
            0x2A => self.set_reg(rd, ((s as i32) < (t as i32)) as u32),
            0x2B => self.set_reg(rd, (s < t) as u32),
            _ => self.exception(Exception::ReservedInstruction),
        }
    }

    fn cop_unusable(&mut self, n: u32) {
        self.exception(Exception::CoprocessorUnusable);
        self.cop0.cause = (self.cop0.cause & !(3 << 28)) | (n << 28);
    }

    fn execute_cop0(&mut self, i: u32) {
        let rt = op_rt(i);
        let rd = op_rd(i);
        match op_rs(i) {
            0x00 => {
                // MFC0 (has a load delay)
                let v = match rd {
                    3 => self.cop0.bpc,
                    5 => self.cop0.bda,
                    6 => self.cop0.jumpdest,
                    7 => self.cop0.dcic,
                    8 => self.cop0.bad_vaddr,
                    9 => self.cop0.bdam,
                    11 => self.cop0.bpcm,
                    12 => self.cop0.sr,
                    13 => self.cop0.cause,
                    14 => self.cop0.epc,
                    15 => PRID,
                    _ => 0, // unimplemented registers: no exception on hardware
                };
                self.load = (rt, v);
            }
            0x04 => {
                // MTC0
                let v = self.regs[rt];
                match rd {
                    3 => self.cop0.bpc = v,
                    5 => self.cop0.bda = v,
                    6 => {} // JUMPDEST is read-only
                    7 => self.cop0.dcic = v,
                    9 => self.cop0.bdam = v,
                    11 => self.cop0.bpcm = v,
                    12 => self.cop0.sr = v,
                    13 => self.cop0.cause = (self.cop0.cause & !0x300) | (v & 0x300),
                    _ => {}
                }
            }
            0x10 if i & 0x3F == 0x10 => {
                // RFE: pop the mode stack
                let mode = self.cop0.sr & 0x3F;
                self.cop0.sr = (self.cop0.sr & !0xF) | (mode >> 2);
            }
            _ => {} // invalid COP0 operations are ignored by hardware
        }
    }

    /// Coprocessors 1 and 3 don't exist: instructions targeting them raise
    /// Coprocessor Unusable only when SR.CUn is clear, else do nothing.
    fn cop_if_disabled(&mut self, n: u32) {
        if self.cop0.sr & (1 << (28 + n)) == 0 {
            self.cop_unusable(n);
        }
    }

    fn execute_cop2(&mut self, i: u32) {
        if self.cop0.sr & SR_CU2 == 0 {
            return self.cop_unusable(2);
        }
        let rt = op_rt(i);
        let rd = op_rd(i) as u32;
        // Any GTE access waits for the command in flight.
        self.stall_until(self.gte_done);
        if i & (1 << 25) != 0 {
            self.gte.command(i & 0x1FF_FFFF);
            self.gte_done = self.now() + Gte::command_cycles(i) as u64;
            return;
        }
        match op_rs(i) {
            0x00 => {
                let v = self.gte.read_data(rd);
                self.load = (rt, v);
            }
            0x02 => {
                let v = self.gte.read_ctrl(rd);
                self.load = (rt, v);
            }
            0x04 => {
                let v = self.regs[rt];
                self.gte.write_data(rd, v);
            }
            0x06 => {
                let v = self.regs[rt];
                self.gte.write_ctrl(rd, v);
            }
            _ => self.exception(Exception::ReservedInstruction),
        }
    }
}

impl Cpu {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("CPU ");
        s.arr_u32(&mut self.regs);
        s.arr_u32(&mut self.out_regs);
        s.usize(&mut self.load.0);
        if self.load.0 >= 32 {
            self.load.0 = 0;
            s.error.get_or_insert("bad load register".into());
        }
        s.u32(&mut self.load.1);
        s.u32(&mut self.hi);
        s.u32(&mut self.lo);
        s.u32(&mut self.current_pc);
        s.u32(&mut self.pc);
        s.u32(&mut self.next_pc);
        s.bool(&mut self.branch);
        s.bool(&mut self.delay_slot);
        let c = &mut self.cop0;
        for r in [&mut c.sr, &mut c.cause, &mut c.epc, &mut c.bad_vaddr, &mut c.bpc, &mut c.bda, &mut c.jumpdest, &mut c.dcic, &mut c.bdam, &mut c.bpcm] {
            s.u32(r);
        }
        s.u64(&mut self.instructions);
        s.vec_u32(&mut self.icache_tag);
        s.vec_u8(&mut self.icache_valid);
        if self.icache_tag.len() != 256 || self.icache_valid.len() != 256 {
            self.icache_tag.resize(256, u32::MAX);
            self.icache_valid.resize(256, 0);
            s.error.get_or_insert("bad icache size".into());
        }
        s.u64(&mut self.muldiv_done);
        s.u64(&mut self.wbuf_free);
        s.u64(&mut self.gte_done);
        self.gte.snap(s);
    }
}
