//! SIO0: controller and memory card serial port (0x1F801040-0x1F80104F).
//!
//! Supports a digital pad (SCPH-1080 protocol, ID 0x5A41) and a 128 KiB
//! memory card (read 'R', write 'W', get-ID 'S' commands) on each port, with
//! byte transfer timing derived from JOY_BAUD and the /ACK interrupt (IRQ7).

use crate::irq::{Interrupt, Irq};

pub const MEMCARD_SIZE: usize = 128 * 1024;

/// Button bits (set = pressed) as exposed to the frontend.
pub mod buttons {
    pub const SELECT: u16 = 1 << 0;
    pub const L3: u16 = 1 << 1;
    pub const R3: u16 = 1 << 2;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    None,
    Pad,
    Card,
    Ignored,
}

#[derive(Debug, Clone)]
pub struct MemCard {
    pub data: Vec<u8>,
    pub dirty: bool,
    flag: u8,
    cmd: u8,
    addr: u16,
    checksum: u8,
    last: u8,
    write_ok: bool,
}

impl MemCard {
    /// A freshly formatted (empty) card.
    pub fn formatted() -> MemCard {
        let mut d = vec![0u8; MEMCARD_SIZE];
        let fix = |d: &mut [u8], frame: usize| {
            let f = &mut d[frame * 128..frame * 128 + 128];
            f[127] = f[..127].iter().fold(0, |a, &b| a ^ b);
        };
        d[0] = b'M';
        d[1] = b'C';
        fix(&mut d, 0);
        for k in 1..16 {
            let f = k * 128;
            d[f] = 0xA0;
            d[f + 8] = 0xFF;
            d[f + 9] = 0xFF;
            fix(&mut d, k);
        }
        for k in 16..36 {
            let f = k * 128;
            d[f..f + 4].fill(0xFF);
            d[f + 8] = 0xFF;
            d[f + 9] = 0xFF;
            fix(&mut d, k);
        }
        // Frame 63 (write test frame) mirrors frame 0.
        let (a, b) = d.split_at_mut(63 * 128);
        b[..128].copy_from_slice(&a[..128]);
        MemCard::from_data(d)
    }

    pub fn from_data(data: Vec<u8>) -> MemCard {
        let mut data = data;
        data.resize(MEMCARD_SIZE, 0);
        MemCard { data, dirty: false, flag: 0x08, cmd: 0, addr: 0, checksum: 0, last: 0, write_ok: true }
    }

    /// Byte exchange; `step` counts from 1 (the byte after 0x81).
    /// Returns (reply, ack).
    fn exchange(&mut self, step: usize, v: u8) -> (u8, bool) {
        if step == 1 {
            self.cmd = v;
            return (self.flag, true);
        }
        let prev = self.last;
        self.last = v;
        match self.cmd {
            b'R' => match step {
                2 => (0x5A, true),
                3 => (0x5D, true),
                4 => {
                    self.addr = (v as u16) << 8;
                    (0x00, true)
                }
                5 => {
                    self.addr = (self.addr | v as u16) & 0x3FF;
                    (prev, true)
                }
                6 => (0x5C, true),
                7 => (0x5D, true),
                8 => {
                    self.checksum = (self.addr >> 8) as u8;
                    ((self.addr >> 8) as u8, true)
                }
                9 => {
                    self.checksum ^= self.addr as u8;
                    (self.addr as u8, true)
                }
                10..=137 => {
                    let b = self.data[self.addr as usize * 128 + (step - 10)];
                    self.checksum ^= b;
                    (b, true)
                }
                138 => (self.checksum, true),
                139 => (0x47, false),
                _ => (0xFF, false),
            },
            b'W' => match step {
                2 => (0x5A, true),
                3 => (0x5D, true),
                4 => {
                    self.addr = (v as u16) << 8;
                    self.checksum = v;
                    (0x00, true)
                }
                5 => {
                    self.write_ok = self.addr >> 8 <= 3;
                    self.addr = (self.addr | v as u16) & 0x3FF;
                    self.checksum ^= v;
                    (prev, true)
                }
                6..=133 => {
                    self.data[self.addr as usize * 128 + (step - 6)] = v;
                    self.checksum ^= v;
                    (prev, true)
                }
                134 => {
                    self.write_ok &= self.checksum == v;
                    (prev, true)
                }
                135 => (0x5C, true),
                136 => (0x5D, true),
                137 => {
                    self.dirty = true;
                    self.flag &= !0x08;
                    (if self.write_ok { 0x47 } else { 0x4E }, false)
                }
                _ => (0xFF, false),
            },
            b'S' => {
                const ID: [u8; 9] = [0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80, 0xFF];
                let i = step - 2;
                (ID.get(i).copied().unwrap_or(0xFF), i < 7)
            }
            _ => (0xFF, false),
        }
    }
}

/// Controller state: digital pad or DualShock (SCPH-1200) with analog,
/// config mode and the config command set (psx-spx "Controllers").
#[derive(Debug, Clone)]
pub struct Pad {
    /// DualShock (analog-capable) rather than a plain digital pad.
    pub dualshock: bool,
    /// Analog mode active (ID 0x73) - switched by games or the Analog button.
    pub analog: bool,
    /// Analog mode locked by the game (config cmd 0x44 with 0x03).
    pub locked: bool,
    config: bool,
    /// Right X, right Y, left X, left Y (0x00..0xFF, centre 0x80)
    pub sticks: [u8; 4],
    rumble_map: [u8; 6],
    /// Last motor values sent by the game (small, large)
    pub motors: [u8; 2],
    cmd: u8,
    resp: Vec<u8>,
}

impl Default for Pad {
    fn default() -> Self {
        Pad {
            dualshock: true,
            analog: false,
            locked: false,
            config: false,
            sticks: [0x80; 4],
            rumble_map: [0xFF; 6],
            motors: [0; 2],
            cmd: 0,
            resp: vec![],
        }
    }
}

impl Pad {
    fn id(&self) -> u8 {
        if self.config {
            0xF3
        } else if self.analog && self.dualshock {
            0x73
        } else {
            0x41
        }
    }

    /// The controller's Analog button. Ignored while the game holds the lock.
    pub fn toggle_analog(&mut self) -> bool {
        if self.dualshock && !self.locked {
            self.analog = !self.analog;
        }
        self.analog
    }

    fn poll_response(&self, buttons: u16) -> Vec<u8> {
        let b = !buttons;
        let mut r = vec![self.id(), 0x5A, b as u8, (b >> 8) as u8];
        if self.config || (self.analog && self.dualshock) {
            r.extend_from_slice(&self.sticks);
        }
        r
    }

    /// Start of a command (byte after 0x01). Returns the response bytes or
    /// None if unsupported.
    fn begin(&mut self, cmd: u8, buttons: u16) -> Option<&[u8]> {
        self.cmd = cmd;
        let cfg = self.config;
        self.resp = match cmd {
            0x42 => self.poll_response(buttons),
            0x43 if self.dualshock => {
                if cfg {
                    vec![0xF3, 0x5A, 0, 0, 0, 0, 0, 0]
                } else {
                    self.poll_response(buttons)
                }
            }
            0x44 if cfg => vec![0xF3, 0x5A, 0, 0, 0, 0, 0, 0],
            0x45 if cfg => vec![0xF3, 0x5A, 0x01, 0x02, self.analog as u8, 0x02, 0x01, 0x00],
            0x46 if cfg => vec![0xF3, 0x5A, 0, 0, 0, 0, 0, 0],
            0x47 if cfg => vec![0xF3, 0x5A, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00],
            0x4C if cfg => vec![0xF3, 0x5A, 0, 0, 0, 0, 0, 0],
            0x4D if cfg => {
                let mut r = vec![0xF3, 0x5A];
                r.extend_from_slice(&self.rumble_map);
                r
            }
            _ => return None,
        };
        Some(&self.resp)
    }

    /// Byte `idx` of the response (idx 0 = ID); `tx` is what the host sent
    /// at that position. Params start at idx 2.
    fn byte(&mut self, idx: usize, tx: u8) -> (u8, bool) {
        let param = idx.wrapping_sub(2);
        match self.cmd {
            0x42 if idx >= 2 && param < 2 && self.analog => self.motors[param] = tx,
            0x43 if idx == 2 => self.config = tx == 1,
            // The game may always set the mode; the lock only blocks the
            // player's Analog button (see toggle_analog).
            0x44 if idx == 2 => self.analog = tx == 1,
            0x44 if idx == 3 => self.locked = tx == 3,
            0x46 if idx == 2 => {
                let t: [u8; 4] = if tx == 0 { [0x01, 0x02, 0x00, 0x0A] } else { [0x01, 0x01, 0x01, 0x14] };
                self.resp[4..8].copy_from_slice(&t);
            }
            0x4C if idx == 2 => self.resp[5] = if tx == 0 { 0x04 } else { 0x07 },
            0x4D if idx >= 2 && param < 6 => self.rumble_map[param] = tx,
            _ => {}
        }
        match self.resp.get(idx) {
            Some(&b) => (b, idx + 1 < self.resp.len()),
            None => (0xFF, false),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Port {
    pub buttons: u16,
    pub pad_connected: bool,
    pub pad: Pad,
    pub card: Option<MemCard>,
}

impl Default for Port {
    fn default() -> Self {
        Port { buttons: 0, pad_connected: true, pad: Pad::default(), card: Some(MemCard::formatted()) }
    }
}

#[derive(Debug, Clone)]
pub struct Sio0 {
    pub ports: [Port; 2],
    mode: u16,
    ctrl: u16,
    baud: u16,
    rx: Option<u8>,
    irq_flag: bool,
    ack_level: bool,
    target: Target,
    step: usize,
    /// Countdown until the in-flight byte completes.
    transfer_timer: i32,
    pending_tx: Option<u8>,
    /// Countdown until the /ACK pulse raises IRQ7.
    ack_timer: i32,
}

impl Default for Sio0 {
    fn default() -> Self {
        Self::new()
    }
}

const ACK_DELAY: i32 = 340;

impl Sio0 {
    pub fn new() -> Self {
        Sio0 {
            ports: [Port::default(), Port { card: Some(MemCard::formatted()), ..Port::default() }],
            mode: 0,
            ctrl: 0,
            baud: 0x88,
            rx: None,
            irq_flag: false,
            ack_level: false,
            target: Target::None,
            step: 0,
            transfer_timer: 0,
            pending_tx: None,
            ack_timer: 0,
        }
    }

    fn port(&self) -> usize {
        ((self.ctrl >> 13) & 1) as usize
    }

    fn selected(&self) -> bool {
        self.ctrl & 0x2 != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.rx.take().map(|b| b as u32).unwrap_or(0xFF),
            0x4 => {
                let mut s = 0u32;
                s |= (self.pending_tx.is_none() as u32) << 0;
                s |= (self.rx.is_some() as u32) << 1;
                s |= (self.pending_tx.is_none() as u32) << 2;
                s |= (self.ack_level as u32) << 7;
                s |= (self.irq_flag as u32) << 9;
                s
            }
            0x8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0 => {
                self.pending_tx = Some(v as u8);
                self.transfer_timer = (self.baud.max(1) as i32) * 8;
            }
            0x8 => self.mode = v as u16,
            0xA => {
                let v = v as u16;
                if v & 0x40 != 0 {
                    // reset
                    *self = Sio0 { ports: self.ports.clone(), ..Sio0::new() };
                    return;
                }
                if v & 0x10 != 0 {
                    self.irq_flag = false;
                }
                let was_selected = self.selected();
                let old_port = self.port();
                self.ctrl = v & !0x50;
                if !self.selected() || (was_selected && old_port != self.port()) {
                    self.target = Target::None;
                    self.step = 0;
                    self.ack_level = false;
                }
            }
            0xE => self.baud = v as u16,
            _ => {}
        }
    }

    pub fn tick(&mut self, cycles: u32, irq: &mut Irq) {
        let c = cycles as i32;
        if let Some(tx) = self.pending_tx {
            self.transfer_timer -= c;
            if self.transfer_timer <= 0 {
                self.pending_tx = None;
                let (reply, ack) = self.exchange(tx);
                self.rx = Some(reply);
                // JOY_CTRL bit 10: TX interrupt, bit 11: RX interrupt (psx-spx)
                if self.ctrl & (3 << 10) != 0 {
                    self.raise_irq(irq);
                }
                if ack {
                    self.ack_timer = ACK_DELAY;
                    self.ack_level = true;
                } else {
                    self.ack_level = false;
                }
            }
        }
        if self.ack_timer > 0 {
            self.ack_timer -= c;
            if self.ack_timer <= 0 {
                self.ack_level = false;
                // JOY_CTRL bit 12: /ACK (DSR) interrupt enable
                if self.ctrl & (1 << 12) != 0 {
                    self.raise_irq(irq);
                }
            }
        }
    }

    fn raise_irq(&mut self, irq: &mut Irq) {
        if !self.irq_flag {
            self.irq_flag = true;
            irq.request(Interrupt::PadMemCard);
        }
    }

    fn exchange(&mut self, tx: u8) -> (u8, bool) {
        if !self.selected() {
            return (0xFF, false);
        }
        let p = self.port();
        if self.target == Target::None {
            self.step = 0;
            self.target = match tx {
                0x01 if self.ports[p].pad_connected => Target::Pad,
                0x81 if self.ports[p].card.is_some() => Target::Card,
                _ => Target::Ignored,
            };
            return match self.target {
                Target::Pad | Target::Card => (0xFF, true),
                _ => (0xFF, false),
            };
        }
        self.step += 1;
        let step = self.step;
        let r = match self.target {
            Target::Pad => {
                let port = &mut self.ports[p];
                if step == 1 {
                    let buttons = port.buttons;
                    match port.pad.begin(tx, buttons) {
                        Some(r) => (r[0], r.len() > 1),
                        None => (0xFF, false),
                    }
                } else {
                    port.pad.byte(step - 1, tx)
                }
            }
            Target::Card => self.ports[p].card.as_mut().unwrap().exchange(step, tx),
            _ => (0xFF, false),
        };
        if !r.1 {
            self.target = Target::Ignored;
        }
        r
    }
}


impl Sio0 {
    /// Memory card *contents* are not part of a state (they're persisted
    /// separately); only the card protocol state is.
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("SIO0");
        for port in self.ports.iter_mut() {
            s.u16(&mut port.buttons);
            s.bool(&mut port.pad_connected);
            let p = &mut port.pad;
            s.bool(&mut p.dualshock);
            s.bool(&mut p.analog);
            s.bool(&mut p.locked);
            s.bool(&mut p.config);
            s.buf(&mut p.sticks);
            s.buf(&mut p.rumble_map);
            s.buf(&mut p.motors);
            s.u8(&mut p.cmd);
            s.vec_u8(&mut p.resp);
            let mut has = port.card.is_some();
            s.bool(&mut has);
            let mut tmp = MemCard::from_data(vec![]);
            let c = port.card.as_mut().unwrap_or(&mut tmp);
            s.u8(&mut c.flag);
            s.u8(&mut c.cmd);
            s.u16(&mut c.addr);
            s.u8(&mut c.checksum);
            s.u8(&mut c.last);
            s.bool(&mut c.write_ok);
        }
        s.u16(&mut self.mode);
        s.u16(&mut self.ctrl);
        s.u16(&mut self.baud);
        let mut rx = self.rx.map(|v| v as u16 | 0x100).unwrap_or(0);
        s.u16(&mut rx);
        self.rx = if rx & 0x100 != 0 { Some(rx as u8) } else { None };
        s.bool(&mut self.irq_flag);
        s.bool(&mut self.ack_level);
        let mut t: u8 = match self.target {
            Target::None => 0,
            Target::Pad => 1,
            Target::Card => 2,
            Target::Ignored => 3,
        };
        s.u8(&mut t);
        self.target = match t {
            1 => Target::Pad,
            2 => Target::Card,
            3 => Target::Ignored,
            _ => Target::None,
        };
        s.usize(&mut self.step);
        s.i32(&mut self.transfer_timer);
        let mut tx = self.pending_tx.map(|v| v as u16 | 0x100).unwrap_or(0);
        s.u16(&mut tx);
        self.pending_tx = if tx & 0x100 != 0 { Some(tx as u8) } else { None };
        s.i32(&mut self.ack_timer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        s: Sio0,
        irq: Irq,
    }

    impl Rig {
        fn new() -> Rig {
            Rig { s: Sio0::new(), irq: Irq::default() }
        }
        fn select(&mut self, port: u16) {
            self.s.write(0xA, 0x1003 | ((port as u32) << 13)); // bit 12 = ACK IRQ enable
        }
        fn deselect(&mut self) {
            self.s.write(0xA, 0);
        }
        /// Send one byte, wait for completion; returns (reply, acked)
        fn xfer(&mut self, b: u8) -> (u8, bool) {
            self.irq.stat = 0;
            let ctrl = self.s.read(0xA);
            self.s.write(0xA, ctrl | 0x10); // ack previous IRQ
            self.s.write(0x0, b as u32);
            assert_eq!(self.s.read(0x4) & 1, 0, "busy while transferring");
            for _ in 0..40 {
                self.s.tick(64, &mut self.irq);
            }
            assert_ne!(self.s.read(0x4) & 2, 0, "rx ready");
            let r = self.s.read(0x0) as u8;
            (r, self.irq.stat & (1 << 7) != 0)
        }
    }

    #[test]
    fn digital_pad_read() {
        let mut r = Rig::new();
        r.s.ports[0].buttons = buttons::CROSS | buttons::START | buttons::LEFT;
        r.select(0);
        assert_eq!(r.xfer(0x01), (0xFF, true));
        assert_eq!(r.xfer(0x42), (0x41, true));
        assert_eq!(r.xfer(0x00), (0x5A, true));
        let lo = !(buttons::START | buttons::LEFT) as u8;
        assert_eq!(r.xfer(0x00), (lo, true));
        assert_eq!(r.xfer(0x00), (!(buttons::CROSS >> 8) as u8, false), "last byte: no ack");
        r.deselect();
    }

    /// Full transaction: returns all reply bytes (stops when ACK drops).
    fn transaction(r: &mut Rig, bytes: &[u8]) -> Vec<u8> {
        r.select(0);
        let mut out = vec![];
        let (_, mut ack) = r.xfer(0x01);
        for &b in bytes {
            if !ack {
                break;
            }
            let (v, a) = r.xfer(b);
            out.push(v);
            ack = a;
        }
        r.deselect();
        out
    }

    #[test]
    fn dualshock_powers_up_digital_and_switches_to_analog() {
        let mut r = Rig::new();
        r.s.ports[0].pad.sticks = [0x10, 0x20, 0x30, 0x40];
        assert_eq!(transaction(&mut r, &[0x42, 0, 0, 0]), vec![0x41, 0x5A, 0xFF, 0xFF]);
        // enter config, set analog + lock, exit config
        assert_eq!(transaction(&mut r, &[0x43, 0, 1, 0])[0], 0x41);
        assert_eq!(transaction(&mut r, &[0x44, 0, 1, 3, 0, 0, 0, 0])[0], 0xF3);
        assert_eq!(transaction(&mut r, &[0x45, 0, 0, 0, 0, 0, 0, 0]), vec![0xF3, 0x5A, 0x01, 0x02, 0x01, 0x02, 0x01, 0x00]);
        assert_eq!(transaction(&mut r, &[0x43, 0, 0, 0, 0, 0, 0, 0])[0], 0xF3);
        let poll = transaction(&mut r, &[0x42, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(poll, vec![0x73, 0x5A, 0xFF, 0xFF, 0x10, 0x20, 0x30, 0x40]);
        // locked: the player's Analog button does nothing
        assert!(r.s.ports[0].pad.toggle_analog());
    }

    #[test]
    fn dualshock_config_constant_tables_and_rumble_map() {
        let mut r = Rig::new();
        transaction(&mut r, &[0x43, 0, 1, 0]);
        assert_eq!(transaction(&mut r, &[0x46, 0, 0, 0, 0, 0, 0, 0]), vec![0xF3, 0x5A, 0, 0, 0x01, 0x02, 0x00, 0x0A]);
        assert_eq!(transaction(&mut r, &[0x46, 0, 1, 0, 0, 0, 0, 0]), vec![0xF3, 0x5A, 0, 0, 0x01, 0x01, 0x01, 0x14]);
        assert_eq!(transaction(&mut r, &[0x47, 0, 0, 0, 0, 0, 0, 0]), vec![0xF3, 0x5A, 0, 0, 0x02, 0, 0x01, 0]);
        assert_eq!(transaction(&mut r, &[0x4C, 0, 1, 0, 0, 0, 0, 0])[5], 0x07);
        assert_eq!(transaction(&mut r, &[0x4D, 0, 0, 1, 0xFF, 0xFF, 0xFF, 0xFF])[2..], [0xFF; 6]);
        assert_eq!(transaction(&mut r, &[0x4D, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])[2..4], [0x00, 0x01]);
    }

    #[test]
    fn analog_button_toggles_when_unlocked_and_plain_pad_ignores_config() {
        let mut r = Rig::new();
        assert!(r.s.ports[0].pad.toggle_analog());
        assert_eq!(transaction(&mut r, &[0x42, 0, 0, 0, 0, 0, 0, 0]).len(), 8);
        r.s.ports[0].pad = Pad { dualshock: false, ..Pad::default() };
        assert_eq!(transaction(&mut r, &[0x43, 0, 1, 0]), vec![0xFF], "digital pad: no config mode");
        assert!(!r.s.ports[0].pad.toggle_analog());
    }

    #[test]
    fn no_device_on_port_or_wrong_address() {
        let mut r = Rig::new();
        r.s.ports[1].pad_connected = false;
        r.select(1);
        assert_eq!(r.xfer(0x01), (0xFF, false));
        r.deselect();
        r.select(0);
        assert_eq!(r.xfer(0x77), (0xFF, false));
        assert_eq!(r.xfer(0x42), (0xFF, false), "stays ignored until deselect");
        r.deselect();
        r.select(0);
        assert_eq!(r.xfer(0x01), (0xFF, true));
    }

    #[test]
    fn port_two_is_independent() {
        let mut r = Rig::new();
        r.s.ports[1].buttons = buttons::TRIANGLE;
        r.select(1);
        r.xfer(0x01);
        r.xfer(0x42);
        r.xfer(0x00);
        r.xfer(0x00);
        assert_eq!(r.xfer(0x00).0, !(buttons::TRIANGLE >> 8) as u8);
    }

    fn card_read(r: &mut Rig, sector: u16) -> (Vec<u8>, u8, u8) {
        r.select(0);
        assert_eq!(r.xfer(0x81), (0xFF, true));
        let flag = r.xfer(b'R').0;
        assert_eq!(r.xfer(0).0, 0x5A);
        assert_eq!(r.xfer(0).0, 0x5D);
        r.xfer((sector >> 8) as u8);
        assert_eq!(r.xfer(sector as u8).0, (sector >> 8) as u8, "pre = previous byte");
        assert_eq!(r.xfer(0).0, 0x5C);
        assert_eq!(r.xfer(0).0, 0x5D);
        assert_eq!(r.xfer(0).0, (sector >> 8) as u8);
        assert_eq!(r.xfer(0).0, sector as u8);
        let data: Vec<u8> = (0..128).map(|_| r.xfer(0).0).collect();
        let chk = r.xfer(0).0;
        let end = r.xfer(0);
        assert_eq!(end, (0x47, false));
        r.deselect();
        (data, chk, flag)
    }

    #[test]
    fn memcard_read_formatted_header() {
        let mut r = Rig::new();
        let (data, chk, flag) = card_read(&mut r, 0);
        assert_eq!(flag, 0x08, "fresh card flag");
        assert_eq!(&data[0..2], b"MC");
        let expect = data.iter().fold(0u8, |a, &b| a ^ b);
        assert_eq!(chk, expect);
        let (dir, _, _) = card_read(&mut r, 1);
        assert_eq!(dir[0], 0xA0, "free directory entry");
        assert_eq!(dir[127], dir[..127].iter().fold(0, |a, &b| a ^ b));
    }

    #[test]
    fn memcard_write_then_read_back() {
        let mut r = Rig::new();
        let sector = 0x123u16;
        let payload: Vec<u8> = (0..128).map(|k| (k * 3) as u8).collect();
        r.select(0);
        r.xfer(0x81);
        r.xfer(b'W');
        r.xfer(0);
        r.xfer(0);
        r.xfer((sector >> 8) as u8);
        r.xfer(sector as u8);
        let mut chk = (sector >> 8) as u8 ^ sector as u8;
        for &b in &payload {
            r.xfer(b);
            chk ^= b;
        }
        r.xfer(chk);
        assert_eq!(r.xfer(0).0, 0x5C);
        assert_eq!(r.xfer(0).0, 0x5D);
        assert_eq!(r.xfer(0), (0x47, false));
        r.deselect();
        assert!(r.s.ports[0].card.as_ref().unwrap().dirty);
        let (data, _, flag) = card_read(&mut r, sector);
        assert_eq!(data, payload);
        assert_eq!(flag, 0x00, "flag cleared after write");
    }

    #[test]
    fn memcard_bad_checksum_reported() {
        let mut r = Rig::new();
        r.select(0);
        r.xfer(0x81);
        r.xfer(b'W');
        r.xfer(0);
        r.xfer(0);
        r.xfer(0);
        r.xfer(5);
        for _ in 0..128 {
            r.xfer(0x11);
        }
        r.xfer(0x00); // wrong checksum
        r.xfer(0);
        r.xfer(0);
        assert_eq!(r.xfer(0).0, 0x4E);
    }

    #[test]
    fn memcard_get_id() {
        let mut r = Rig::new();
        r.select(0);
        r.xfer(0x81);
        r.xfer(b'S');
        let rest: Vec<u8> = (0..8).map(|_| r.xfer(0).0).collect();
        assert_eq!(rest, vec![0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn ack_interrupt_is_bit_12_not_bit_10() {
        // Real drivers (BIOS pad routines, Buzzy Bee) set JOY_CTRL = 0x1003:
        // bit 12 enables the /ACK interrupt. Bit 10 alone is the TX interrupt.
        let mut r = Rig::new();
        r.s.write(0xA, 0x1003);
        r.s.write(0x0, 0x01);
        for _ in 0..40 {
            r.s.tick(64, &mut r.irq);
        }
        assert_ne!(r.s.read(0x4) & (1 << 9), 0, "ACK raises JOY_STAT.9 with bit 12");
        assert_ne!(r.irq.stat & (1 << 7), 0);
    }

    #[test]
    fn irq_requires_enable_and_ack_bit_clears() {
        let mut r = Rig::new();
        r.s.write(0xA, 0x0003); // no ACK IRQ enable
        r.s.write(0x0, 0x01);
        for _ in 0..40 {
            r.s.tick(64, &mut r.irq);
        }
        assert_eq!(r.irq.stat, 0);
        r.s.write(0xA, 0x1003);
        r.s.read(0);
        r.s.write(0x0, 0x42);
        for _ in 0..40 {
            r.s.tick(64, &mut r.irq);
        }
        assert_ne!(r.s.read(0x4) & (1 << 9), 0);
        r.s.write(0xA, 0x1003 | 0x10);
        assert_eq!(r.s.read(0x4) & (1 << 9), 0);
    }
}
