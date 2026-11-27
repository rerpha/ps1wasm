//! Interrupt controller (I_STAT 0x1F801070 / I_MASK 0x1F801074).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Interrupt {
    VBlank = 0,
    Gpu = 1,
    CdRom = 2,
    Dma = 3,
    Timer0 = 4,
    Timer1 = 5,
    Timer2 = 6,
    PadMemCard = 7,
    Sio = 8,
    Spu = 9,
    Lightpen = 10,
}

#[derive(Debug, Default, Clone)]
pub struct Irq {
    pub stat: u32,
    pub mask: u32,
}

impl Irq {
    pub fn request(&mut self, i: Interrupt) {
        self.stat |= 1 << i as u32;
    }

    /// Writing to I_STAT acknowledges: bits written as 0 are cleared.
    pub fn ack(&mut self, val: u32) {
        self.stat &= val & 0x7FF;
    }

    pub fn set_mask(&mut self, val: u32) {
        self.mask = val & 0x7FF;
    }

    /// Level of the line that feeds COP0 CAUSE bit 10.
    pub fn pending(&self) -> bool {
        self.stat & self.mask != 0
    }
}

impl Irq {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("IRQ ");
        s.u32(&mut self.stat);
        s.u32(&mut self.mask);
    }
}
