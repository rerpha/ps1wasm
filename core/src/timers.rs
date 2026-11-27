//! Root counters (timers 0-2) at 0x1F801100 + 0x10*n.
//!
//! Mode register bits:
//!   0     sync enable            1-2  sync mode
//!   3     reset at target (else at 0xFFFF)
//!   4     IRQ at target          5    IRQ at 0xFFFF
//!   6     IRQ repeat (else one-shot)
//!   7     IRQ toggle (else pulse)
//!   8-9   clock source           10   IRQ line (0 = asserted)
//!   11    reached target (clears on read)
//!   12    reached 0xFFFF (clears on read)

use crate::irq::{Interrupt, Irq};

#[derive(Debug, Clone, Default)]
pub struct Timer {
    pub counter: u32,
    pub mode: u32,
    pub target: u32,
    paused: bool,
    irq_fired: bool,
    /// Sync mode 3 on timers 0/1: wait for the first blank, then free-run.
    waiting_for_blank: bool,
    div8_frac: u32,
}

/// Per-sync timing events coming from the GPU.
#[derive(Clone, Copy, Default, Debug)]
pub struct VideoEvents {
    pub dots: u32,
    /// hblank starts (also the timer 1 "hblank" clock)
    pub hblanks: u32,
    pub hblank_ends: u32,
    pub in_hblank: bool,
    pub vblank_start: bool,
    pub vblank_end: bool,
    pub in_vblank: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Timers {
    pub t: [Timer; 3],
}

const IRQS: [Interrupt; 3] = [Interrupt::Timer0, Interrupt::Timer1, Interrupt::Timer2];

impl Timer {
    fn sync_enabled(&self) -> bool {
        self.mode & 1 != 0
    }
    fn sync_mode(&self) -> u32 {
        (self.mode >> 1) & 3
    }

    fn write_mode(&mut self, idx: usize, val: u32, in_blank: bool) {
        self.mode = (val & 0x3FF) | (1 << 10);
        self.counter = 0;
        self.irq_fired = false;
        self.waiting_for_blank = false;
        self.paused = false;
        if self.sync_enabled() {
            match (idx, self.sync_mode()) {
                (2, 0) | (2, 3) => self.paused = true,
                (0, 3) | (1, 3) => {
                    self.paused = true;
                    self.waiting_for_blank = true;
                }
                (0, 0) | (1, 0) => self.paused = in_blank,
                (0, 2) | (1, 2) => self.paused = !in_blank,
                _ => {}
            }
        }
    }

    fn read_mode(&mut self) -> u32 {
        let v = self.mode;
        self.mode &= !(3 << 11);
        v
    }

    /// Advance by `n` counter ticks.
    fn advance(&mut self, idx: usize, n: u32, irq: &mut Irq) {
        if self.paused || n == 0 {
            return;
        }
        // The counter visibly *reaches* the target (or 0xFFFF) and wraps on
        // the following tick: periods are target+1 and 0x10000 (ps1-tests
        // timer-dump, a per-clock register dump from real hardware).
        let old = self.counter;
        let new = old + n;
        let mut request = false;
        let target = self.target;
        if self.mode & (1 << 3) != 0 {
            let period = target + 1;
            if new >= target && (old < target || new >= old + period) {
                self.mode |= 1 << 11;
                request |= self.mode & (1 << 4) != 0;
            }
            self.counter = if new > target { (new - period) % period } else { new };
        } else {
            let crossed_target = (old < target && new >= target) || (new > 0xFFFF && new - 0x10000 >= target);
            if crossed_target {
                self.mode |= 1 << 11;
                request |= self.mode & (1 << 4) != 0;
            }
            if new >= 0xFFFF && old < 0xFFFF || new > 0xFFFF {
                self.mode |= 1 << 12;
                request |= self.mode & (1 << 5) != 0;
            }
            self.counter = new & 0xFFFF;
        }
        if request {
            self.raise(idx, irq);
        }
    }

    fn raise(&mut self, idx: usize, irq: &mut Irq) {
        let repeat = self.mode & (1 << 6) != 0;
        if !repeat && self.irq_fired {
            return;
        }
        if self.mode & (1 << 7) != 0 {
            // Toggle mode: bit 10 flips; IRQ fires on the 1 -> 0 edge.
            self.mode ^= 1 << 10;
            if self.mode & (1 << 10) == 0 {
                irq.request(IRQS[idx]);
                self.irq_fired = true;
            }
        } else {
            // Pulse mode: line briefly low, then back high.
            irq.request(IRQS[idx]);
            self.irq_fired = true;
            self.mode |= 1 << 10;
        }
    }

    fn on_blank_start(&mut self, idx: usize) {
        if !self.sync_enabled() || idx == 2 {
            return;
        }
        match self.sync_mode() {
            0 => self.paused = true,
            1 => self.counter = 0,
            2 => {
                self.counter = 0;
                self.paused = false;
            }
            _ => {
                if self.waiting_for_blank {
                    self.waiting_for_blank = false;
                    self.paused = false;
                }
            }
        }
    }

    fn on_blank_end(&mut self, idx: usize) {
        if !self.sync_enabled() || idx == 2 {
            return;
        }
        match self.sync_mode() {
            0 => self.paused = false,
            2 => self.paused = true,
            _ => {}
        }
    }
}

impl Timers {
    pub fn read(&mut self, offset: u32) -> u32 {
        let n = ((offset >> 4) & 3) as usize;
        if n == 3 {
            return 0;
        }
        match offset & 0xF {
            0x0 => self.t[n].counter & 0xFFFF,
            0x4 => self.t[n].read_mode(),
            0x8 => self.t[n].target,
            _ => 0,
        }
    }

    /// `in_hblank`/`in_vblank`: current blanking state (sync modes).
    pub fn write(&mut self, offset: u32, val: u32, in_hblank: bool, in_vblank: bool) {
        let n = ((offset >> 4) & 3) as usize;
        if n == 3 {
            return;
        }
        match offset & 0xF {
            0x0 => self.t[n].counter = val & 0xFFFF,
            0x4 => self.t[n].write_mode(n, val, if n == 0 { in_hblank } else { in_vblank }),
            0x8 => self.t[n].target = val & 0xFFFF,
            _ => {}
        }
    }

    /// Advance all timers by `cycles` CPU cycles plus the given video events.
    pub fn tick(&mut self, cycles: u32, ev: &VideoEvents, irq: &mut Irq) {
        // Timer 0: system clock or dot clock; syncs to hblank.
        let src0 = (self.t[0].mode >> 8) & 3;
        let n0 = if src0 & 1 == 0 { cycles } else { ev.dots };
        // order within a sync batch: an hblank that started and ended
        // within it leaves the timer running
        if ev.hblanks > 0 && ev.hblank_ends > 0 && !ev.in_hblank {
            self.t[0].on_blank_start(0);
            self.t[0].on_blank_end(0);
        } else {
            if ev.hblank_ends > 0 {
                self.t[0].on_blank_end(0);
            }
            if ev.hblanks > 0 {
                self.t[0].on_blank_start(0);
            }
        }
        self.t[0].advance(0, n0, irq);

        // Timer 1: system clock or hblank; syncs to vblank.
        if ev.vblank_start {
            self.t[1].on_blank_start(1);
        }
        if ev.vblank_end {
            self.t[1].on_blank_end(1);
        }
        let src1 = (self.t[1].mode >> 8) & 3;
        let n1 = if src1 & 1 == 0 { cycles } else { ev.hblanks };
        self.t[1].advance(1, n1, irq);

        // Timer 2: system clock or system clock / 8.
        let src2 = (self.t[2].mode >> 8) & 3;
        let n2 = if src2 < 2 {
            cycles
        } else {
            let total = self.t[2].div8_frac + cycles;
            self.t[2].div8_frac = total & 7;
            total >> 3
        };
        self.t[2].advance(2, n2, irq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> VideoEvents {
        VideoEvents::default()
    }

    #[test]
    fn counts_system_clock() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.tick(100, &ev(), &mut irq);
        assert_eq!(t.read(0x00), 100);
        assert_eq!(t.read(0x10), 100);
        assert_eq!(t.read(0x20), 100);
    }

    #[test]
    fn mode_write_resets_counter_and_sets_irq_line_high() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.tick(50, &ev(), &mut irq);
        t.write(0x04, 0, false, false);
        assert_eq!(t.read(0x00), 0);
        assert_ne!(t.read(0x04) & (1 << 10), 0);
    }

    #[test]
    fn target_reset_and_irq_repeat() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x28, 100, false, false);
        t.write(0x24, (1 << 3) | (1 << 4) | (1 << 6), false, false); // reset@target, irq@target, repeat
        t.tick(99, &ev(), &mut irq);
        assert_eq!(irq.stat, 0);
        t.tick(1, &ev(), &mut irq);
        assert_ne!(irq.stat & (1 << 6), 0, "timer2 irq");
        assert_eq!(t.read(0x20), 100, "target value is visible for one tick");
        t.tick(1, &ev(), &mut irq);
        assert_eq!(t.read(0x20), 0, "then wraps: period = target + 1");
        let m = t.read(0x24);
        assert_ne!(m & (1 << 11), 0, "reached target flag");
        assert_eq!(t.read(0x24) & (1 << 11), 0, "flag cleared by read");
        irq.stat = 0;
        t.tick(100, &ev(), &mut irq);
        assert_ne!(irq.stat & (1 << 6), 0, "repeats every 101 ticks");
    }

    #[test]
    fn one_shot_fires_once() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x08, 10, false, false);
        t.write(0x04, (1 << 3) | (1 << 4), false, false);
        t.tick(10, &ev(), &mut irq);
        assert_ne!(irq.stat & (1 << 4), 0);
        irq.stat = 0;
        t.tick(11, &ev(), &mut irq);
        assert_eq!(irq.stat, 0);
    }

    #[test]
    fn overflow_wraps_and_flags() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x04, 1 << 5 | 1 << 6, false, false);
        t.write(0x00, 0xFFF0, false, false);
        t.tick(0x20, &ev(), &mut irq);
        assert_eq!(t.read(0x00), 0x10, "period 0x10000");
        assert_ne!(irq.stat & (1 << 4), 0);
        assert_ne!(t.read(0x04) & (1 << 12), 0);
    }

    #[test]
    fn toggle_mode_fires_every_other_hit() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x08, 10, false, false);
        t.write(0x04, (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7), false, false);
        t.tick(10, &ev(), &mut irq);
        assert_ne!(irq.stat & 0x10, 0, "1->0 edge fires");
        irq.stat = 0;
        t.tick(11, &ev(), &mut irq);
        assert_eq!(irq.stat & 0x10, 0, "0->1 edge silent");
        t.tick(11, &ev(), &mut irq);
        assert_ne!(irq.stat & 0x10, 0);
    }

    #[test]
    fn timer2_div8_and_stop_modes() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x24, 2 << 8, false, false);
        t.tick(20, &ev(), &mut irq);
        t.tick(4, &ev(), &mut irq);
        assert_eq!(t.read(0x20), 3, "24 cycles / 8");
        t.write(0x24, 1, false, false); // sync enable, mode 0 -> stopped
        t.tick(100, &ev(), &mut irq);
        assert_eq!(t.read(0x20), 0);
    }

    #[test]
    fn timer1_hblank_source_and_vblank_sync3() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x14, (1 << 8) | 1 | (3 << 1), false, false); // hblank clock, sync mode 3
        let e = VideoEvents { hblanks: 1, ..Default::default() };
        t.tick(2000, &e, &mut irq);
        assert_eq!(t.read(0x10), 0, "paused until vblank");
        let e = VideoEvents { hblanks: 1, vblank_start: true, ..Default::default() };
        t.tick(2000, &e, &mut irq);
        t.tick(2000, &VideoEvents { hblanks: 1, ..Default::default() }, &mut irq);
        assert_eq!(t.read(0x10), 2);
    }

    fn hb(start: bool, end: bool, inside: bool) -> VideoEvents {
        VideoEvents { hblanks: start as u32, hblank_ends: end as u32, in_hblank: inside, ..Default::default() }
    }

    #[test]
    fn counting_sequence_matches_hardware_dump() {
        // reset-at-target, target 5: 0,1,2,3,4,5,0,1,... with "reached" at 5
        let mut irq = Irq::default();
        let mut t = Timers::default();
        t.write(0x28, 5, false, false);
        t.write(0x24, 1 << 3, false, false);
        let mut seq = vec![];
        for _ in 0..13 {
            seq.push(t.read(0x20));
            t.tick(1, &ev(), &mut irq);
        }
        assert_eq!(seq, vec![0, 1, 2, 3, 4, 5, 0, 1, 2, 3, 4, 5, 0]);
    }

    #[test]
    fn timer0_hblank_sync_modes() {
        let mut irq = Irq::default();
        // mode 0: pause during hblank
        let mut t = Timers::default();
        t.write(0x04, 1, false, false);
        t.tick(100, &ev(), &mut irq);
        t.tick(10, &hb(true, false, true), &mut irq);
        t.tick(100, &hb(false, false, true), &mut irq);
        assert_eq!(t.read(0x00), 100, "paused inside hblank");
        t.tick(50, &hb(false, true, false), &mut irq);
        assert_eq!(t.read(0x00), 150, "runs again after hblank");
        // mode 1: reset at hblank start
        let mut t = Timers::default();
        t.write(0x04, 1 | (1 << 1), false, false);
        t.tick(500, &ev(), &mut irq);
        t.tick(20, &hb(true, false, true), &mut irq);
        assert_eq!(t.read(0x00), 20);
        // mode 2: reset at hblank, counting only inside it
        let mut t = Timers::default();
        t.write(0x04, 1 | (2 << 1), false, false);
        t.tick(300, &ev(), &mut irq);
        assert_eq!(t.read(0x00), 0, "paused outside hblank");
        t.tick(40, &hb(true, false, true), &mut irq);
        assert_eq!(t.read(0x00), 40, "counts inside hblank");
        t.tick(40, &hb(false, true, false), &mut irq);
        t.tick(300, &ev(), &mut irq);
        assert!(t.read(0x00) <= 80, "paused again outside");
        // mode 3: wait for first hblank, then free-run forever
        let mut t = Timers::default();
        t.write(0x04, 1 | (3 << 1), false, false);
        t.tick(300, &ev(), &mut irq);
        assert_eq!(t.read(0x00), 0);
        t.tick(10, &hb(true, false, true), &mut irq);
        t.tick(100, &hb(false, true, false), &mut irq);
        t.tick(100, &hb(true, true, false), &mut irq);
        assert_eq!(t.read(0x00), 210);
        // mode written while inside hblank: mode 0 starts paused
        let mut t = Timers::default();
        t.write(0x04, 1, true, false);
        t.tick(100, &hb(false, false, true), &mut irq);
        assert_eq!(t.read(0x00), 0);
    }

    #[test]
    fn timer0_dotclock() {
        let mut t = Timers::default();
        let mut irq = Irq::default();
        t.write(0x04, 1 << 8, false, false);
        t.tick(100, &VideoEvents { dots: 37, ..Default::default() }, &mut irq);
        assert_eq!(t.read(0x00), 37);
    }
}

impl Timers {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("TMRS");
        for t in self.t.iter_mut() {
            s.u32(&mut t.counter);
            s.u32(&mut t.mode);
            s.u32(&mut t.target);
            s.bool(&mut t.paused);
            s.bool(&mut t.irq_fired);
            s.bool(&mut t.waiting_for_blank);
            s.u32(&mut t.div8_frac);
        }
    }
}
