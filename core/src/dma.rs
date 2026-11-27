//! DMA controller registers (0x1F801080-0x1F8010FF).
//! The transfer engine itself lives in `Bus::run_dma` since it needs access
//! to RAM and every DMA-capable device.

use crate::irq::{Interrupt, Irq};

pub const MDEC_IN: usize = 0;
pub const MDEC_OUT: usize = 1;
pub const GPU: usize = 2;
pub const CDROM: usize = 3;
pub const SPU: usize = 4;
pub const PIO: usize = 5;
pub const OTC: usize = 6;

#[derive(Clone, Copy, Default, Debug)]
pub struct Channel {
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
}

impl Channel {
    pub fn from_ram(&self) -> bool {
        self.chcr & 1 != 0
    }
    pub fn step_back(&self) -> bool {
        self.chcr & 2 != 0
    }
    pub fn sync_mode(&self) -> u32 {
        (self.chcr >> 9) & 3
    }
    /// `drq`: the device is requesting a transfer. In sync mode 0 a
    /// transfer starts on the manual trigger (bit 28) *or* the device's
    /// request (ps1-tests dma/chopping starts GPU DMA with the trigger clear).
    pub fn active(&self, drq: bool) -> bool {
        let enable = self.chcr & (1 << 24) != 0;
        let trigger = self.chcr & (1 << 28) != 0;
        if self.sync_mode() == 0 {
            enable && (trigger || drq)
        } else {
            enable
        }
    }
    /// Number of words for sync modes 0/1 (None for linked list).
    pub fn transfer_words(&self) -> Option<u32> {
        let bs = self.bcr & 0xFFFF;
        let ba = self.bcr >> 16;
        match self.sync_mode() {
            0 => Some(if bs == 0 { 0x10000 } else { bs }),
            1 => Some(bs.max(1) * ba.max(1)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dma {
    pub ch: [Channel; 7],
    pub dpcr: u32,
    pub dicr: u32,
}

impl Default for Dma {
    fn default() -> Self {
        Dma { ch: [Channel::default(); 7], dpcr: 0x0765_4321, dicr: 0 }
    }
}

impl Dma {
    pub fn read(&self, offset: u32) -> u32 {
        let n = ((offset >> 4) & 7) as usize;
        if n == 7 {
            return match offset & 0xF {
                0x0 => self.dpcr,
                0x4 => self.dicr,
                0x8 => 0x7FFA_C68B, // unknown regs, fixed values on hardware
                _ => 0x00FF_FFF7,
            };
        }
        let c = &self.ch[n];
        match offset & 0xF {
            0x0 => c.madr,
            0x4 => c.bcr,
            0x8 => c.chcr,
            _ => 0,
        }
    }

    /// Returns Some(channel) when the write makes a channel ready to run.
    pub fn write(&mut self, offset: u32, val: u32, irq: &mut Irq) -> Option<usize> {
        let n = ((offset >> 4) & 7) as usize;
        if n == 7 {
            match offset & 0xF {
                0x0 => self.dpcr = val,
                0x4 => {
                    // bits 24-30 acknowledge by writing 1
                    let ack = val & 0x7F00_0000;
                    let flags = (self.dicr & 0x7F00_0000) & !ack;
                    self.dicr = (val & 0x00FF_803F) | flags;
                    self.update_master(irq, false);
                }
                _ => {}
            }
            return self.first_runnable();
        }
        let c = &mut self.ch[n];
        match offset & 0xF {
            0x0 => c.madr = val & 0x00FF_FFFF,
            0x4 => c.bcr = val,
            0x8 => {
                c.chcr = if n == OTC {
                    (val & 0x5100_0000) | 0x2 // fixed: to RAM, step backwards
                } else {
                    val & 0x7177_0703
                };
            }
            _ => {}
        }
        // Only a CHCR write can start a channel (MADR/BCR set-up for the
        // next transfer must not re-trigger one still in flight).
        if offset & 0xF == 0x8 && self.runnable(n) {
            Some(n)
        } else {
            None
        }
    }

    pub fn runnable(&self, n: usize) -> bool {
        // MDEC, GPU, CD-ROM and SPU raise DMA requests; PIO and OTC have no
        // request line and need the manual trigger.
        let drq = n <= SPU;
        self.ch[n].active(drq) && self.dpcr & (8 << (n * 4)) != 0
    }

    fn first_runnable(&self) -> Option<usize> {
        (0..7).find(|&n| self.runnable(n))
    }

    /// Called by the engine when channel `n` finishes.
    pub fn complete(&mut self, n: usize, irq: &mut Irq) {
        self.ch[n].chcr &= !((1 << 24) | (1 << 28));
        if self.dicr & (1 << (16 + n)) != 0 {
            self.dicr |= 1 << (24 + n);
        }
        self.update_master(irq, true);
    }

    fn update_master(&mut self, irq: &mut Irq, may_raise: bool) {
        let old = self.dicr & (1 << 31) != 0;
        let force = self.dicr & (1 << 15) != 0;
        let master = self.dicr & (1 << 23) != 0;
        let en = (self.dicr >> 16) & 0x7F;
        let fl = (self.dicr >> 24) & 0x7F;
        let new = force || (master && en & fl != 0);
        if new {
            self.dicr |= 1 << 31;
        } else {
            self.dicr &= !(1 << 31);
        }
        if new && !old && may_raise {
            irq.request(Interrupt::Dma);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_register_masks() {
        let mut d = Dma::default();
        let mut irq = Irq::default();
        assert_eq!(d.read(0x70), 0x0765_4321);
        d.write(0x20, 0xFFFF_FFFF, &mut irq);
        assert_eq!(d.read(0x20), 0x00FF_FFFF);
        d.write(0x68, 0xFFFF_FFFF, &mut irq);
        assert_eq!(d.read(0x68), 0x5100_0002, "OTC chcr fixed bits");
    }

    #[test]
    fn activation_rules() {
        let mut d = Dma::default();
        let mut irq = Irq::default();
        d.write(0x70, 0x0800_0000 | 0x0000_0800, &mut irq); // enable ch6 & ch2
        // manual mode on OTC (no request line) needs the trigger
        assert_eq!(d.write(0x68, 1 << 24, &mut irq), None);
        assert_eq!(d.write(0x68, (1 << 24) | (1 << 28), &mut irq), Some(6));
        // request mode starts on enable alone
        assert_eq!(d.write(0x28, (1 << 24) | (1 << 9) | 1, &mut irq), Some(2));
        // disabled in DPCR
        assert_eq!(d.write(0x38, (1 << 24) | (1 << 9), &mut irq), None);
    }

    #[test]
    fn sync0_starts_on_device_request_without_trigger() {
        let mut d = Dma::default();
        let mut irq = Irq::default();
        d.write(0x70, 0x0000_0800, &mut irq); // enable GPU channel
        assert_eq!(d.write(0x28, (1 << 24) | 1, &mut irq), Some(GPU), "GPU requests -> starts");
    }

    #[test]
    fn transfer_word_counts() {
        let c = Channel { bcr: 0, chcr: 0, madr: 0 };
        assert_eq!(c.transfer_words(), Some(0x10000));
        let c = Channel { bcr: 0x0004_0010, chcr: 1 << 9, madr: 0 };
        assert_eq!(c.transfer_words(), Some(64));
        let c = Channel { bcr: 0, chcr: 2 << 9, madr: 0 };
        assert_eq!(c.transfer_words(), None);
    }

    #[test]
    fn completion_irq_edge_and_ack() {
        let mut d = Dma::default();
        let mut irq = Irq::default();
        d.write(0x74, (1 << 23) | (1 << (16 + 2)), &mut irq);
        d.ch[2].chcr = 1 << 24;
        d.complete(2, &mut irq);
        assert_eq!(d.ch[2].chcr & (1 << 24), 0);
        assert_ne!(d.read(0x74) & (1 << 26), 0);
        assert_ne!(d.read(0x74) & (1 << 31), 0);
        assert_ne!(irq.stat & (1 << 3), 0);
        irq.stat = 0;
        // second completion while master still high: no new edge
        d.complete(2, &mut irq);
        assert_eq!(irq.stat, 0);
        // acknowledge flag
        d.write(0x74, (1 << 23) | (1 << 18) | (1 << 26), &mut irq);
        assert_eq!(d.read(0x74) & (1 << 26), 0);
        assert_eq!(d.read(0x74) & (1 << 31), 0);
    }

    #[test]
    fn no_flag_when_channel_irq_disabled() {
        let mut d = Dma::default();
        let mut irq = Irq::default();
        d.write(0x74, 1 << 23, &mut irq);
        d.complete(4, &mut irq);
        assert_eq!(d.read(0x74) & 0x7F00_0000, 0);
        assert_eq!(irq.stat, 0);
    }
}

impl Dma {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("DMA ");
        for c in self.ch.iter_mut() {
            s.u32(&mut c.madr);
            s.u32(&mut c.bcr);
            s.u32(&mut c.chcr);
        }
        s.u32(&mut self.dpcr);
        s.u32(&mut self.dicr);
    }
}
