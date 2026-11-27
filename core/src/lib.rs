//! PlayStation emulator core.

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod disc;
pub mod dma;
pub mod gpu;
pub mod gte;
pub mod irq;
pub mod mdec;
pub mod pad;
pub mod spu;
pub mod state;
pub mod timers;

use bus::{Bus, Size};
use cpu::Cpu;

#[derive(Debug)]
pub struct Psx {
    pub cpu: Cpu,
    pub bus: Bus,
    /// Characters printed via BIOS putchar (A(3Ch) / B(3Dh)) and std_out_puts.
    pub tty: String,
    /// EXE to side-load once the BIOS has finished its init (shell entry).
    pending_exe: Option<Vec<u8>>,
}

impl Default for Psx {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed header of a PS-X EXE.
#[derive(Debug, Clone, Copy)]
pub struct ExeHeader {
    pub pc: u32,
    pub gp: u32,
    pub dest: u32,
    pub size: u32,
    pub sp: u32,
}

pub fn parse_exe(data: &[u8]) -> Result<ExeHeader, String> {
    if data.len() < 0x800 || &data[0..8] != b"PS-X EXE" {
        return Err("not a PS-X EXE".into());
    }
    let w = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let size = w(0x1C);
    if 0x800 + size as usize > data.len() {
        return Err(format!("EXE truncated: header says {} bytes, file has {}", size, data.len() - 0x800));
    }
    Ok(ExeHeader { pc: w(0x10), gp: w(0x14), dest: w(0x18), size, sp: w(0x30).wrapping_add(w(0x34)) })
}

impl Psx {
    pub fn new() -> Self {
        Psx { cpu: Cpu::new(), bus: Bus::new(), tty: String::new(), pending_exe: None }
    }

    pub fn load_bios(&mut self, data: &[u8]) -> Result<(), String> {
        self.bus.load_bios(data)
    }

    /// Copy an EXE into RAM and point the CPU at it immediately (no BIOS
    /// init). Suitable for self-contained test programs.
    pub fn load_exe_now(&mut self, data: &[u8]) -> Result<(), String> {
        let h = parse_exe(data)?;
        for (k, b) in data[0x800..0x800 + h.size as usize].iter().enumerate() {
            self.bus.write(h.dest.wrapping_add(k as u32), *b as u32, Size::Byte);
        }
        self.cpu.set_reg_now(28, h.gp);
        if h.sp != 0 {
            self.cpu.set_reg_now(29, h.sp);
            self.cpu.set_reg_now(30, h.sp);
        }
        self.cpu.set_pc(h.pc);
        Ok(())
    }

    /// Boot the BIOS and side-load `data` when it reaches the shell entry
    /// point (0x80030000), exactly like a disc EXE would be started.
    pub fn load_exe_after_bios(&mut self, data: &[u8]) -> Result<(), String> {
        parse_exe(data)?;
        self.pending_exe = Some(data.to_vec());
        Ok(())
    }

    #[inline]
    pub fn step(&mut self) {
        let pc = self.cpu.pc & 0x1FFF_FFFF;
        if pc == 0xA0 || pc == 0xB0 {
            self.bios_call_hook(pc);
        } else if pc == 0x0003_0000 {
            if let Some(exe) = self.pending_exe.take() {
                self.load_exe_now(&exe).expect("validated earlier");
            }
        }
        let c = self.cpu.step(&mut self.bus);
        self.bus.add_cycles(c);
    }

    /// Run until the GPU signals the start of the next vertical blank.
    /// Bounded so a stalled display can't hang the caller.
    pub fn run_frame(&mut self) {
        let start = self.bus.gpu.frame_count;
        let limit = self.bus.cycles + 2 * 33_868_800 / 50;
        while self.bus.gpu.frame_count == start && self.bus.cycles < limit {
            self.step();
        }
    }

    fn bios_call_hook(&mut self, vector: u32) {
        let f = self.cpu.reg(9);
        let putchar = (vector == 0xA0 && f == 0x3C) || (vector == 0xB0 && f == 0x3D);
        if putchar {
            self.tty.push(self.cpu.reg(4) as u8 as char);
        }
    }

    fn snap(&mut self, s: &mut state::Snap) {
        let mut magic = *state::MAGIC;
        s.buf(&mut magic);
        let mut version = state::VERSION;
        s.u32(&mut version);
        if s.is_loading() && (magic != *state::MAGIC || version != state::VERSION) {
            s.error.get_or_insert("not a compatible save state".into());
            return;
        }
        self.cpu.snap(s);
        self.bus.snap(s);
        let mut has = self.pending_exe.is_some();
        s.bool(&mut has);
        let mut exe = self.pending_exe.take().unwrap_or_default();
        s.vec_u8(&mut exe);
        self.pending_exe = if has { Some(exe) } else { None };
        s.tag("END ");
    }

    /// Serialize the whole machine (except disc contents and memory cards).
    pub fn save_state(&mut self) -> Vec<u8> {
        self.bus.sync();
        let mut s = state::Snap::saver();
        self.snap(&mut s);
        s.into_bytes()
    }

    /// Restore a state. On any error the machine is left unchanged.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let backup = self.save_state();
        let mut s = state::Snap::loader(data);
        self.snap(&mut s);
        if s.finished_cleanly() {
            return Ok(());
        }
        let err = s.error.clone().unwrap_or_else(|| "save state has trailing data".into());
        let mut b = state::Snap::loader(&backup);
        self.snap(&mut b);
        debug_assert!(b.finished_cleanly());
        Err(err)
    }

    /// Insert a disc into the CD-ROM drive.
    pub fn insert_disc(&mut self, disc: disc::Disc) {
        self.bus.cdrom.insert_disc(disc);
    }

    /// Set pressed buttons (see `pad::buttons`) for port 0 or 1.
    pub fn set_buttons(&mut self, port: usize, mask: u16) {
        if let Some(p) = self.bus.sio.ports.get_mut(port) {
            p.buttons = mask;
        }
    }

    /// Move generated audio (44.1 kHz interleaved stereo) into `out`.
    pub fn take_audio(&mut self, out: &mut Vec<i16>) {
        out.extend_from_slice(&self.bus.spu.output);
        self.bus.spu.output.clear();
    }

    /// Render the current display to RGBA8; returns (width, height).
    pub fn render(&self, out: &mut Vec<u8>) -> (u32, u32) {
        self.bus.gpu.render_display(out)
    }

    /// Hard reset, keeping the BIOS, disc and memory cards.
    pub fn reset(&mut self) {
        let bios = std::mem::take(&mut self.bus.bios);
        let disc = self.bus.cdrom.disc.take();
        let cards: Vec<Option<pad::MemCard>> = self.bus.sio.ports.iter_mut().map(|p| p.card.take()).collect();
        *self = Psx::new();
        self.bus.bios = bios;
        if let Some(d) = disc {
            self.insert_disc(d);
        }
        for (port, card) in self.bus.sio.ports.iter_mut().zip(cards) {
            port.card = card;
        }
    }

    /// Memory card contents for `port`, if a card is inserted.
    pub fn memcard(&self, port: usize) -> Option<&[u8]> {
        self.bus.sio.ports.get(port)?.card.as_ref().map(|c| c.data.as_slice())
    }

    /// True (and clears the flag) if the card was written since last call.
    pub fn memcard_take_dirty(&mut self, port: usize) -> bool {
        match self.bus.sio.ports.get_mut(port).and_then(|p| p.card.as_mut()) {
            Some(c) if c.dirty => {
                c.dirty = false;
                true
            }
            _ => false,
        }
    }

    pub fn set_memcard(&mut self, port: usize, data: Vec<u8>) {
        if let Some(p) = self.bus.sio.ports.get_mut(port) {
            p.card = Some(pad::MemCard::from_data(data));
        }
    }

    pub fn run_instructions(&mut self, n: u64) {
        for _ in 0..n {
            self.step();
        }
    }
}
