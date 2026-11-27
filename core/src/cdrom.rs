//! CD-ROM controller (0x1F801800-0x1F801803).
//!
//! Implements the command/response protocol (INT1/2/3/4/5), parameter and
//! response FIFOs, sector reading at 1x/2x with the 0x800/0x924 data FIFO,
//! XA-ADPCM decoding (4/8-bit, mono/stereo, 37.8/18.9 kHz with file/channel
//! filter), CD-DA playback, and the CD audio volume matrix. Audio is
//! resampled to 44.1 kHz and pulled by the SPU via `pop_audio`.

use std::collections::VecDeque;

use crate::disc::{abs_to_msf, bcd, from_bcd, msf_to_abs, Disc, Region, LEAD_IN, SECTOR_SIZE};
use crate::irq::{Interrupt, Irq};

pub const CPU_HZ: u32 = 33_868_800;
const SECTOR_1X: i64 = (CPU_HZ / 75) as i64;
const SECTOR_2X: i64 = (CPU_HZ / 150) as i64;
const ACK_DELAY: i64 = 0xC4E1;
const ACK_DELAY_INIT: i64 = 0x13CCE;
const IRQ_RETRY_DELAY: i64 = 1000;

const STAT_ERROR: u8 = 0x01;
const STAT_MOTOR: u8 = 0x02;
const STAT_SHELL_OPEN: u8 = 0x10;
const STAT_READING: u8 = 0x20;
const STAT_SEEKING: u8 = 0x40;
const STAT_PLAYING: u8 = 0x80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drive {
    Idle,
    Reading,
    Playing,
}

#[derive(Clone, Debug)]
struct Pending {
    int: u8,
    data: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy)]
struct XaChannel {
    old: i32,
    older: i32,
}

#[derive(Debug)]
struct Resampler {
    prev: (i32, i32),
    cur: (i32, i32),
    phase: u32, // 16.16, position between prev and cur
}

impl Resampler {
    fn new() -> Self {
        Resampler { prev: (0, 0), cur: (0, 0), phase: 0 }
    }
    /// Push one input frame at `rate` Hz, emitting 44.1 kHz frames.
    fn push(&mut self, s: (i32, i32), rate: u32, out: &mut Vec<(i32, i32)>) {
        self.prev = self.cur;
        self.cur = s;
        let step = ((rate as u64) << 16) / 44100;
        while self.phase < 0x10000 {
            let f = self.phase as i32;
            let l = self.prev.0 + (((self.cur.0 - self.prev.0) * f) >> 16);
            let r = self.prev.1 + (((self.cur.1 - self.prev.1) * f) >> 16);
            out.push((l, r));
            self.phase += step as u32;
        }
        self.phase -= 0x10000;
    }
}

#[derive(Debug)]
pub struct CdRom {
    index: u8,
    params: VecDeque<u8>,
    response: VecDeque<u8>,
    ie: u8,
    iflag: u8,
    request: u8,

    stat: u8,
    mode: u8,
    filter_file: u8,
    filter_chan: u8,
    setloc: u32,
    setloc_pending: bool,
    pos: u32,
    drive: Drive,
    play_track_end: u32,

    /// Command currently waiting for its acknowledge.
    cmd: Option<u8>,
    cmd_params: Vec<u8>,
    cmd_timer: i64,
    /// Second response scheduled after the ack: (delay, int, data).
    second: Option<(i64, Pending)>,
    /// Interrupts waiting for the CPU to acknowledge the current one.
    queue: VecDeque<Pending>,
    retry_timer: i64,
    sector_timer: i64,
    report_counter: u32,

    sector: Box<[u8; SECTOR_SIZE]>,
    sector_valid: bool,
    data: Vec<u8>,
    data_pos: usize,
    last_header: [u8; 8],

    pub disc: Option<Disc>,
    region: Region,
    /// Debug: log commands, responses and sector reads to stderr.
    pub trace: bool,

    // audio
    atv_pending: [u8; 4],
    atv: [u8; 4],
    muted: bool,
    adpcm_muted: bool,
    xa: [XaChannel; 2],
    resampler: Resampler,
    audio: VecDeque<(i16, i16)>,
}

impl Default for CdRom {
    fn default() -> Self {
        Self::new()
    }
}

const XA_POS: [i32; 4] = [0, 60, 115, 98];
const XA_NEG: [i32; 4] = [0, 0, -52, -55];

impl CdRom {
    pub fn new() -> Self {
        CdRom {
            index: 0,
            params: VecDeque::with_capacity(16),
            response: VecDeque::with_capacity(16),
            ie: 0,
            iflag: 0,
            request: 0,
            stat: STAT_SHELL_OPEN,
            mode: 0,
            filter_file: 0,
            filter_chan: 0,
            setloc: LEAD_IN,
            setloc_pending: false,
            pos: LEAD_IN,
            drive: Drive::Idle,
            play_track_end: 0,
            cmd: None,
            cmd_params: vec![],
            cmd_timer: 0,
            second: None,
            queue: VecDeque::new(),
            retry_timer: 0,
            sector_timer: 0,
            report_counter: 0,
            sector: Box::new([0; SECTOR_SIZE]),
            sector_valid: false,
            data: Vec::with_capacity(0x924),
            data_pos: 0,
            last_header: [0; 8],
            disc: None,
            region: Region::America,
            trace: false,
            atv_pending: [0x80, 0, 0x80, 0],
            atv: [0x80, 0, 0x80, 0],
            muted: false,
            adpcm_muted: false,
            xa: [XaChannel::default(); 2],
            resampler: Resampler::new(),
            audio: VecDeque::with_capacity(8192),
        }
    }

    pub fn insert_disc(&mut self, mut disc: Disc) {
        self.region = disc.region();
        self.disc = Some(disc);
        self.stat = STAT_MOTOR;
    }

    pub fn eject(&mut self) {
        self.disc = None;
        self.drive = Drive::Idle;
        self.stat = STAT_SHELL_OPEN;
    }

    /// Drive is reading or playing sectors.
    pub fn is_reading(&self) -> bool {
        self.drive != Drive::Idle
    }

    /// A sector has been delivered (INT1) but not yet moved to the data FIFO.
    pub fn has_unread_sector(&self) -> bool {
        self.sector_valid
    }

    /// A command is waiting for its first or second response.
    pub fn command_pending(&self) -> bool {
        self.cmd.is_some() || self.second.is_some()
    }

    pub fn region(&self) -> Region {
        self.region
    }

    fn status_byte(&self) -> u8 {
        let mut s = self.stat & !(STAT_READING | STAT_PLAYING | STAT_SEEKING);
        match self.drive {
            Drive::Reading => s |= STAT_READING,
            Drive::Playing => s |= STAT_PLAYING,
            Drive::Idle => {}
        }
        s
    }

    // ------------------------------------------------------------- registers
    pub fn read(&mut self, reg: u32) -> u8 {
        match reg & 3 {
            0 => {
                let mut s = self.index & 3;
                s |= (self.params.is_empty() as u8) << 3;
                s |= ((self.params.len() < 16) as u8) << 4;
                s |= (!self.response.is_empty() as u8) << 5;
                s |= ((self.data_pos < self.data.len()) as u8) << 6;
                s |= (self.cmd.is_some() as u8) << 7;
                s
            }
            1 => self.response.pop_front().unwrap_or(0),
            2 => self.read_data_byte(),
            _ => match self.index & 1 {
                0 => self.ie | 0xE0,
                _ => self.iflag | 0xE0,
            },
        }
    }

    pub fn write(&mut self, reg: u32, v: u8, irq: &mut Irq) {
        match (reg & 3, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => self.start_command(v),
            (1, 1) | (1, 2) => {} // sound map (unsupported)
            (1, 3) => self.atv_pending[2] = v,
            (2, 0) => {
                if self.params.len() < 16 {
                    self.params.push_back(v);
                }
            }
            (2, 1) => {
                self.ie = v & 0x1F;
                if self.iflag & self.ie != 0 {
                    irq.request(Interrupt::CdRom);
                }
            }
            (2, 2) => self.atv_pending[0] = v,
            (2, 3) => self.atv_pending[3] = v,
            (3, 0) => {
                if self.trace {
                    eprintln!("[cd] request reg <- {:02x} (sector_valid={}, fifo {}/{})", v, self.sector_valid, self.data_pos, self.data.len());
                }
                self.request = v;
                if v & 0x80 != 0 {
                    if self.data_pos >= self.data.len() {
                        self.load_data_fifo();
                    }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            (3, 1) => {
                self.iflag &= !(v & 0x1F);
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                if self.iflag & 7 == 0 {
                    self.response.clear();
                    if !self.queue.is_empty() {
                        self.retry_timer = IRQ_RETRY_DELAY;
                    }
                }
            }
            (3, 2) => self.atv_pending[1] = v,
            (3, 3) => {
                self.adpcm_muted = v & 1 != 0;
                if v & 0x20 != 0 {
                    self.atv = self.atv_pending;
                }
            }
            _ => {}
        }
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.data_pos < self.data.len() {
            let b = self.data[self.data_pos];
            self.data_pos += 1;
            b
        } else {
            0
        }
    }

    /// 32-bit read for DMA channel 3.
    pub fn dma_read_word(&mut self) -> u32 {
        let b0 = self.read_data_byte() as u32;
        let b1 = self.read_data_byte() as u32;
        let b2 = self.read_data_byte() as u32;
        let b3 = self.read_data_byte() as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn load_data_fifo(&mut self) {
        self.data.clear();
        self.data_pos = 0;
        if !self.sector_valid {
            return;
        }
        if self.mode & 0x20 != 0 {
            self.data.extend_from_slice(&self.sector[12..12 + 0x924]);
        } else {
            self.data.extend_from_slice(&self.sector[24..24 + 0x800]);
        }
        self.sector_valid = false;
    }

    // ------------------------------------------------------------- interrupts
    fn deliver(&mut self, p: Pending, irq: &mut Irq) {
        if self.trace {
            eprintln!("[cd] INT{} {:02x?}{}", p.int, p.data, if self.iflag & 7 != 0 { " (queued)" } else { "" });
        }
        if self.iflag & 7 != 0 {
            self.queue.push_back(p);
            return;
        }
        self.response.clear();
        self.response.extend(p.data.iter().copied());
        self.iflag = (self.iflag & !7) | p.int;
        if self.iflag & self.ie != 0 {
            irq.request(Interrupt::CdRom);
        }
    }

    fn ok(&self, int: u8) -> Pending {
        Pending { int, data: vec![self.status_byte()] }
    }

    fn error(&self, code: u8) -> Pending {
        Pending { int: 5, data: vec![self.status_byte() | STAT_ERROR, code] }
    }

    // ------------------------------------------------------------- commands
    fn start_command(&mut self, cmd: u8) {
        self.cmd = Some(cmd);
        self.cmd_params = self.params.drain(..).collect();
        self.cmd_timer = if cmd == 0x0A { ACK_DELAY_INIT } else { ACK_DELAY };
    }

    fn sector_period(&self) -> i64 {
        if self.mode & 0x80 != 0 { SECTOR_2X } else { SECTOR_1X }
    }

    fn seek_time(&self, target: u32) -> i64 {
        let dist = (target as i64 - self.pos as i64).abs();
        20_000 + (dist * 6).min(1_000_000)
    }

    fn execute(&mut self, cmd: u8, irq: &mut Irq) {
        if self.trace {
            eprintln!("[cd] cmd {:02x} params {:02x?}", cmd, self.cmd_params);
        }
        let p = std::mem::take(&mut self.cmd_params);
        let need = match cmd {
            0x02 => 3,
            0x0D => 2,
            0x0E | 0x14 | 0x19 | 0x12 => 1,
            _ => 0,
        };
        if p.len() < need {
            let e = self.error(0x20);
            self.deliver(e, irq);
            return;
        }
        let have_disc = self.disc.is_some();
        match cmd {
            0x01 => {
                let r = self.ok(3);
                self.stat &= !STAT_SHELL_OPEN;
                self.deliver(r, irq);
            }
            0x02 => {
                self.setloc = msf_to_abs(from_bcd(p[0]), from_bcd(p[1]), from_bcd(p[2]));
                self.setloc_pending = true;
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x03 => {
                // Play (optional track number)
                if !have_disc {
                    let e = self.error(0x80);
                    return self.deliver(e, irq);
                }
                let track = p.first().map(|&t| from_bcd(t) as u8).filter(|&t| t > 0);
                let disc = self.disc.as_ref().unwrap();
                if let Some(t) = track.and_then(|t| disc.track(t)) {
                    self.pos = t.start;
                    self.setloc_pending = false;
                } else if self.setloc_pending {
                    self.pos = self.setloc;
                    self.setloc_pending = false;
                }
                self.play_track_end = self.track_end(self.pos);
                self.drive = Drive::Playing;
                self.stat |= STAT_MOTOR;
                self.sector_timer = SECTOR_1X;
                self.report_counter = 0;
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x04 | 0x05 => {
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x06 | 0x1B => {
                if !have_disc {
                    let e = self.error(0x80);
                    return self.deliver(e, irq);
                }
                let mut delay = self.sector_period();
                if self.setloc_pending {
                    delay += self.seek_time(self.setloc);
                    self.pos = self.setloc;
                    self.setloc_pending = false;
                }
                self.stat |= STAT_MOTOR;
                let r = self.ok(3);
                self.drive = Drive::Reading;
                self.sector_timer = delay;
                self.deliver(r, irq);
            }
            0x07 => {
                let r = self.ok(3);
                self.stat |= STAT_MOTOR;
                self.deliver(r, irq);
                self.second = Some((0x1DF2, self.ok(2)));
            }
            0x08 => {
                let was_spinning = self.stat & STAT_MOTOR != 0;
                self.drive = Drive::Idle;
                let r = self.ok(3);
                self.deliver(r, irq);
                self.stat &= !STAT_MOTOR;
                let d = if was_spinning { 0x00D3_8ACA } else { 0x1DF2 };
                self.second = Some((d, self.ok(2)));
            }
            0x09 => {
                let r = self.ok(3);
                let busy = self.drive != Drive::Idle;
                self.deliver(r, irq);
                self.drive = Drive::Idle;
                let d = if busy {
                    if self.mode & 0x80 != 0 { 0x0010_BD93 } else { 0x0021_181C }
                } else {
                    0x1DF2
                };
                self.second = Some((d, self.ok(2)));
            }
            0x0A => {
                self.drive = Drive::Idle;
                self.mode = 0x20;
                self.stat |= STAT_MOTOR;
                let r = self.ok(3);
                self.deliver(r, irq);
                self.second = Some((ACK_DELAY_INIT, self.ok(2)));
            }
            0x0B => {
                self.muted = true;
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x0C => {
                self.muted = false;
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x0D => {
                self.filter_file = p[0];
                self.filter_chan = p[1];
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x0E => {
                self.mode = p[0];
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x0F => {
                let r = Pending { int: 3, data: vec![self.status_byte(), self.mode, 0, self.filter_file, self.filter_chan] };
                self.deliver(r, irq);
            }
            0x10 => {
                let r = Pending { int: 3, data: self.last_header.to_vec() };
                self.deliver(r, irq);
            }
            0x11 => {
                let r = self.getloc_p();
                self.deliver(r, irq);
            }
            0x12 => {
                let r = self.ok(3);
                self.deliver(r, irq);
                let second = if p[0] == 1 { self.ok(2) } else { self.error(0x10) };
                self.second = Some((0x4A00, second));
            }
            0x13 => {
                let r = match &self.disc {
                    Some(d) => Pending {
                        int: 3,
                        data: vec![self.status_byte(), bcd(d.first_track() as u32), bcd(d.last_track() as u32)],
                    },
                    None => self.error(0x80),
                };
                self.deliver(r, irq);
            }
            0x14 => {
                let t = from_bcd(p[0]) as u8;
                let r = match &self.disc {
                    Some(d) => {
                        let a = if t == 0 { Some(d.lead_out) } else { d.track(t).map(|tr| tr.start) };
                        match a {
                            Some(a) => {
                                let (m, s, _) = abs_to_msf(a);
                                Pending { int: 3, data: vec![self.status_byte(), bcd(m), bcd(s)] }
                            }
                            None => self.error(0x10),
                        }
                    }
                    None => self.error(0x80),
                };
                self.deliver(r, irq);
            }
            0x15 | 0x16 => {
                if !have_disc {
                    let e = self.error(0x80);
                    return self.deliver(e, irq);
                }
                self.drive = Drive::Idle;
                let t = self.seek_time(self.setloc);
                self.pos = self.setloc;
                self.setloc_pending = false;
                self.stat |= STAT_MOTOR;
                let r = self.ok(3);
                self.deliver(r, irq);
                self.second = Some((t, self.ok(2)));
            }
            0x19 => {
                let r = match p[0] {
                    0x20 => Pending { int: 3, data: vec![0x94, 0x09, 0x19, 0xC0] },
                    0x04 | 0x05 => self.ok(3),
                    _ => self.error(0x10),
                };
                self.deliver(r, irq);
            }
            0x1A => {
                let r = self.ok(3);
                self.deliver(r, irq);
                let second = match &self.disc {
                    None => Pending { int: 5, data: vec![0x08, 0x40, 0, 0, 0, 0, 0, 0] },
                    Some(d) if d.tracks.first().map(|t| t.audio).unwrap_or(false) => {
                        Pending { int: 5, data: vec![self.status_byte() | STAT_ERROR, 0x90, 0, 0, 0, 0, 0, 0] }
                    }
                    Some(_) => {
                        let letter = match self.region {
                            Region::America => b'A',
                            Region::Europe => b'E',
                            Region::Japan => b'I',
                        };
                        Pending { int: 2, data: vec![self.status_byte(), 0x00, 0x20, 0x00, b'S', b'C', b'E', letter] }
                    }
                };
                self.second = Some((0x4A00, second));
            }
            0x1C => {
                self.drive = Drive::Idle;
                let r = self.ok(3);
                self.deliver(r, irq);
            }
            0x1E => {
                let r = self.ok(3);
                self.deliver(r, irq);
                let second = if have_disc { self.ok(2) } else { self.error(0x80) };
                self.second = Some((CPU_HZ as i64 / 2, second));
            }
            _ => {
                let e = self.error(0x40);
                self.deliver(e, irq);
            }
        }
    }

    fn track_end(&self, abs: u32) -> u32 {
        let Some(d) = &self.disc else { return 0 };
        let cur = d.track_at(abs).map(|t| t.number).unwrap_or(1);
        d.tracks.iter().find(|t| t.number > cur).map(|t| t.pregap_start).unwrap_or(d.lead_out)
    }

    fn getloc_p(&self) -> Pending {
        let Some(d) = &self.disc else { return self.error(0x80) };
        let a = self.pos;
        let (tn, rel, idx) = match d.track_at(a) {
            Some(t) if a >= t.start => (t.number, a - t.start, 1),
            Some(t) => (t.number, t.start - a, 0),
            None => (1, 0, 1),
        };
        let (rm, rs, rf) = abs_to_msf(rel);
        let (am, as_, af) = abs_to_msf(a);
        Pending {
            int: 3,
            data: vec![bcd(tn as u32), idx, bcd(rm), bcd(rs), bcd(rf), bcd(am), bcd(as_), bcd(af)],
        }
    }

    // ------------------------------------------------------------- timing
    pub fn tick(&mut self, cycles: u32, irq: &mut Irq) {
        let c = cycles as i64;
        if let Some(cmd) = self.cmd {
            self.cmd_timer -= c;
            if self.cmd_timer <= 0 {
                self.cmd = None;
                self.execute(cmd, irq);
            }
        }
        if let Some((ref mut t, _)) = self.second {
            *t -= c;
            if *t <= 0 {
                let (_, p) = self.second.take().unwrap();
                self.deliver(p, irq);
            }
        }
        if !self.queue.is_empty() && self.iflag & 7 == 0 {
            self.retry_timer -= c;
            if self.retry_timer <= 0 {
                let p = self.queue.pop_front().unwrap();
                self.deliver(p, irq);
            }
        }
        match self.drive {
            Drive::Idle => {}
            Drive::Reading | Drive::Playing => {
                self.sector_timer -= c;
                while self.sector_timer <= 0 && self.drive != Drive::Idle {
                    if self.drive == Drive::Reading {
                        self.sector_timer += self.sector_period();
                        self.read_sector(irq);
                    } else {
                        self.sector_timer += SECTOR_1X;
                        self.play_sector(irq);
                    }
                }
            }
        }
    }

    fn read_sector(&mut self, irq: &mut Irq) {
        if self.trace {
            eprintln!("[cd] read sector abs {} (lba {})", self.pos, self.pos as i64 - 150);
        }
        let mut buf = [0u8; SECTOR_SIZE];
        let ok = match self.disc.as_mut() {
            Some(d) => d.read_sector(self.pos, &mut buf),
            None => false,
        };
        self.pos += 1;
        if !ok {
            self.drive = Drive::Idle;
            let e = self.error(0x04);
            self.deliver(e, irq);
            return;
        }
        let submode = buf[18];
        let is_audio = buf[15] == 2 && submode & 0x04 != 0;
        if is_audio && self.mode & 0x40 != 0 {
            let matches = self.mode & 0x08 == 0 || (buf[16] == self.filter_file && buf[17] == self.filter_chan);
            if matches {
                self.decode_xa(&buf);
            }
            return;
        }
        if self.mode & 0x08 != 0 && is_audio {
            return; // filtered out
        }
        self.sector.copy_from_slice(&buf);
        self.sector_valid = true;
        self.last_header.copy_from_slice(&buf[12..20]);
        if self.iflag & 7 == 0 {
            let r = self.ok(1);
            self.deliver(r, irq);
        } else {
            // Replace any previously queued data-ready with the newest one.
            self.queue.retain(|p| p.int != 1);
            let r = self.ok(1);
            self.queue.push_back(r);
        }
    }

    fn play_sector(&mut self, irq: &mut Irq) {
        if self.pos >= self.play_track_end {
            if self.mode & 0x02 != 0 {
                self.drive = Drive::Idle;
                let r = self.ok(4);
                self.deliver(r, irq);
                return;
            }
            self.play_track_end = self.track_end(self.pos);
        }
        let mut buf = [0u8; SECTOR_SIZE];
        if let Some(d) = self.disc.as_mut() {
            d.read_sector(self.pos, &mut buf);
        }
        self.pos += 1;
        // Play outputs CD-DA regardless of Setmode bit 0 (that bit only
        // governs reading audio sectors as data).
        for k in 0..SECTOR_SIZE / 4 {
            let l = i16::from_le_bytes([buf[4 * k], buf[4 * k + 1]]) as i32;
            let r = i16::from_le_bytes([buf[4 * k + 2], buf[4 * k + 3]]) as i32;
            self.push_audio(l, r);
        }
        if self.mode & 0x04 != 0 {
            self.report_counter += 1;
            if self.report_counter.is_multiple_of(10) {
                let data = self.getloc_p().data;
                if data.len() == 8 {
                    let mut rep = vec![self.status_byte()];
                    rep.extend_from_slice(&data[0..2]);
                    rep.extend_from_slice(&data[5..8]);
                    rep.extend_from_slice(&[0, 0]);
                    if self.iflag & 7 == 0 {
                        self.deliver(Pending { int: 1, data: rep }, irq);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- audio
    fn push_audio(&mut self, l: i32, r: i32) {
        if self.muted {
            self.audio.push_back((0, 0));
        } else {
            let a = &self.atv;
            let ol = ((l * a[0] as i32) >> 7) + ((r * a[3] as i32) >> 7);
            let or = ((r * a[2] as i32) >> 7) + ((l * a[1] as i32) >> 7);
            self.audio.push_back((ol.clamp(-32768, 32767) as i16, or.clamp(-32768, 32767) as i16));
        }
        while self.audio.len() > 44100 / 2 {
            self.audio.pop_front();
        }
    }

    /// One 44.1 kHz stereo frame of CD audio for the SPU.
    pub fn pop_audio(&mut self) -> (i16, i16) {
        self.audio.pop_front().unwrap_or((0, 0))
    }

    pub fn audio_queued(&self) -> usize {
        self.audio.len()
    }

    /// Decode one XA-ADPCM sector (Form 2 audio) into the audio queue.
    pub fn decode_xa(&mut self, sector: &[u8; SECTOR_SIZE]) {
        let coding = sector[19];
        let stereo = coding & 0x01 != 0;
        let rate = if coding & 0x04 != 0 { 18900 } else { 37800 };
        let eight_bit = coding & 0x10 != 0;
        let mut samples: Vec<i32> = Vec::with_capacity(4032);
        let mut ch_samples: [Vec<i32>; 2] = [Vec::with_capacity(2016), Vec::with_capacity(2016)];

        for g in 0..18 {
            let grp = &sector[24 + g * 128..24 + (g + 1) * 128];
            let units = if eight_bit { 4 } else { 8 };
            for u in 0..units {
                let param = grp[4 + u];
                let filter = ((param >> 4) & 3) as usize;
                let range = (param & 0x0F) as i32;
                let ch = if stereo { u & 1 } else { 0 };
                let st = &mut self.xa[ch];
                for j in 0..28 {
                    let t = if eight_bit {
                        (grp[16 + u + j * 4] as i8 as i32) << 8
                    } else {
                        let byte = grp[16 + (u / 2) + j * 4];
                        let nib = (byte >> ((u & 1) * 4)) & 0xF;
                        ((nib as i32) << 28) >> 16 // sign-extend to 16-bit scale
                    };
                    let shift = if range > 12 { 9 } else { range };
                    let mut s = t >> shift;
                    s += (st.old * XA_POS[filter] + st.older * XA_NEG[filter] + 32) >> 6;
                    let s = s.clamp(-0x8000, 0x7FFF);
                    st.older = st.old;
                    st.old = s;
                    if stereo {
                        ch_samples[ch].push(s);
                    } else {
                        samples.push(s);
                    }
                }
            }
        }

        if self.adpcm_muted {
            return;
        }
        let mut out = Vec::with_capacity(4800);
        if stereo {
            let n = ch_samples[0].len().min(ch_samples[1].len());
            for k in 0..n {
                self.resampler.push((ch_samples[0][k], ch_samples[1][k]), rate, &mut out);
            }
        } else {
            for &s in &samples {
                self.resampler.push((s, s), rate, &mut out);
            }
        }
        for &(l, r) in &out {
            self.push_audio(l, r);
        }
    }
}


fn snap_pending(s: &mut crate::state::Snap, p: &mut Pending) {
    s.u8(&mut p.int);
    s.vec_u8(&mut p.data);
}

fn snap_deque_u8(s: &mut crate::state::Snap, q: &mut VecDeque<u8>) {
    let mut v: Vec<u8> = q.iter().copied().collect();
    s.vec_u8(&mut v);
    *q = v.into_iter().collect();
}

impl CdRom {
    /// The disc itself is not saved: it is re-attached by the frontend.
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("CDRM");
        for b in [&mut self.index, &mut self.ie, &mut self.iflag, &mut self.request, &mut self.stat, &mut self.mode, &mut self.filter_file, &mut self.filter_chan] {
            s.u8(b);
        }
        snap_deque_u8(s, &mut self.params);
        snap_deque_u8(s, &mut self.response);
        s.u32(&mut self.setloc);
        s.bool(&mut self.setloc_pending);
        s.u32(&mut self.pos);
        let mut d: u8 = match self.drive {
            Drive::Idle => 0,
            Drive::Reading => 1,
            Drive::Playing => 2,
        };
        s.u8(&mut d);
        self.drive = match d {
            1 => Drive::Reading,
            2 => Drive::Playing,
            _ => Drive::Idle,
        };
        s.u32(&mut self.play_track_end);
        let mut cmd = self.cmd.map(|c| c as u16 | 0x100).unwrap_or(0);
        s.u16(&mut cmd);
        self.cmd = if cmd & 0x100 != 0 { Some(cmd as u8) } else { None };
        s.vec_u8(&mut self.cmd_params);
        s.i64(&mut self.cmd_timer);
        let mut has2 = self.second.is_some();
        s.bool(&mut has2);
        let (mut t2, mut p2) = self.second.take().unwrap_or((0, Pending { int: 0, data: vec![] }));
        s.i64(&mut t2);
        snap_pending(s, &mut p2);
        self.second = if has2 { Some((t2, p2)) } else { None };
        let mut n = self.queue.len() as u32;
        s.u32(&mut n);
        if s.is_loading() {
            self.queue = (0..n.min(256)).map(|_| Pending { int: 0, data: vec![] }).collect();
        }
        for p in self.queue.iter_mut() {
            snap_pending(s, p);
        }
        s.i64(&mut self.retry_timer);
        s.i64(&mut self.sector_timer);
        s.u32(&mut self.report_counter);
        s.buf(&mut self.sector[..]);
        s.bool(&mut self.sector_valid);
        s.vec_u8(&mut self.data);
        s.usize(&mut self.data_pos);
        s.buf(&mut self.last_header);
        let mut r: u8 = match self.region {
            Region::Japan => 0,
            Region::America => 1,
            Region::Europe => 2,
        };
        s.u8(&mut r);
        self.region = match r {
            0 => Region::Japan,
            2 => Region::Europe,
            _ => Region::America,
        };
        s.buf(&mut self.atv_pending);
        s.buf(&mut self.atv);
        s.bool(&mut self.muted);
        s.bool(&mut self.adpcm_muted);
        for x in self.xa.iter_mut() {
            s.i32(&mut x.old);
            s.i32(&mut x.older);
        }
        let rs = &mut self.resampler;
        for v in [&mut rs.prev.0, &mut rs.prev.1, &mut rs.cur.0, &mut rs.cur.1] {
            s.i32(v);
        }
        s.u32(&mut rs.phase);
        let mut audio: Vec<u32> = self.audio.iter().map(|&(l, r)| (l as u16 as u32) | ((r as u16 as u32) << 16)).collect();
        s.vec_u32(&mut audio);
        self.audio = audio.into_iter().map(|w| (w as i16, (w >> 16) as i16)).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::{Disc, MemReader};

    /// Build a small Mode 2 disc where sector k's user data is filled with k.
    fn disc(sectors: u32) -> Disc {
        let mut img = vec![];
        for k in 0..sectors {
            let mut s = vec![0u8; SECTOR_SIZE];
            s[1..11].fill(0xFF);
            let (m, sec, f) = abs_to_msf(LEAD_IN + k);
            s[12] = bcd(m);
            s[13] = bcd(sec);
            s[14] = bcd(f);
            s[15] = 2;
            s[18] = 0x08;
            s[22] = 0x08;
            for b in &mut s[24..24 + 0x800] {
                *b = k as u8;
            }
            img.extend(s);
        }
        let n = img.len() as u64;
        Disc::from_single(n, true, Box::new(MemReader(vec![img]))).unwrap()
    }

    struct Rig {
        cd: CdRom,
        irq: Irq,
    }

    impl Rig {
        fn new(with_disc: bool) -> Rig {
            let mut cd = CdRom::new();
            if with_disc {
                cd.insert_disc(disc(32));
            }
            let mut irq = Irq::default();
            cd.write(0, 1, &mut irq);
            cd.write(2, 0x1F, &mut irq); // enable all INTs
            cd.write(0, 0, &mut irq);
            Rig { cd, irq }
        }
        fn cmd(&mut self, c: u8, params: &[u8]) {
            self.cd.write(0, 0, &mut self.irq);
            for &p in params {
                self.cd.write(2, p, &mut self.irq);
            }
            self.cd.write(1, c, &mut self.irq);
        }
        /// Run until an interrupt is raised; returns (int, response bytes).
        fn wait_int(&mut self, max_cycles: u64) -> (u8, Vec<u8>) {
            let mut t = 0;
            while t < max_cycles {
                self.cd.tick(500, &mut self.irq);
                t += 500;
                self.cd.write(0, 1, &mut self.irq);
                let f = self.cd.read(3) & 7;
                if f != 0 {
                    let mut resp = vec![];
                    while self.cd.read(0) & 0x20 != 0 {
                        resp.push(self.cd.read(1));
                    }
                    self.cd.write(3, 0x1F, &mut self.irq); // ack
                    self.cd.write(0, 0, &mut self.irq);
                    return (f, resp);
                }
                self.cd.write(0, 0, &mut self.irq);
            }
            panic!("no interrupt within {} cycles", max_cycles);
        }
    }

    #[test]
    fn getstat_and_busy_flag() {
        let mut r = Rig::new(true);
        r.cmd(0x01, &[]);
        assert_ne!(r.cd.read(0) & 0x80, 0, "busy until ack");
        let (i, resp) = r.wait_int(200_000);
        assert_eq!(i, 3);
        assert_eq!(resp, vec![STAT_MOTOR]);
        assert_eq!(r.cd.read(0) & 0x80, 0);
        assert_ne!(r.irq.stat & (1 << 2), 0, "CDROM IRQ to I_STAT");
    }

    #[test]
    fn test_version_and_param_fifo_status() {
        let mut r = Rig::new(true);
        assert_ne!(r.cd.read(0) & 0x08, 0, "param fifo empty");
        r.cd.write(2, 0x20, &mut r.irq);
        assert_eq!(r.cd.read(0) & 0x08, 0);
        r.cd.write(1, 0x19, &mut r.irq);
        let (i, resp) = r.wait_int(200_000);
        assert_eq!((i, resp), (3, vec![0x94, 0x09, 0x19, 0xC0]));
    }

    #[test]
    fn getid_licensed_disc_and_no_disc() {
        let mut r = Rig::new(true);
        r.cmd(0x1A, &[]);
        assert_eq!(r.wait_int(200_000).0, 3);
        let (i, resp) = r.wait_int(200_000);
        assert_eq!(i, 2);
        assert_eq!(&resp[4..], b"SCEA");
        assert_eq!(resp[2], 0x20);

        let mut r = Rig::new(false);
        r.cmd(0x1A, &[]);
        assert_eq!(r.wait_int(200_000).0, 3);
        let (i, resp) = r.wait_int(200_000);
        assert_eq!(i, 5);
        assert_eq!(&resp[..2], &[0x08, 0x40]);
    }

    #[test]
    fn setloc_readn_delivers_sectors_in_order() {
        let mut r = Rig::new(true);
        r.cmd(0x0E, &[0x80]); // double speed, 0x800 bytes
        r.wait_int(200_000);
        r.cmd(0x02, &[0x00, 0x02, 0x05]); // LBA 5
        r.wait_int(200_000);
        r.cmd(0x06, &[]);
        assert_eq!(r.wait_int(200_000).0, 3);
        for expect in 5..9u8 {
            let (i, resp) = r.wait_int(2_000_000);
            assert_eq!(i, 1);
            assert_ne!(resp[0] & STAT_READING, 0);
            r.cd.write(3, 0x80, &mut r.irq); // BFRD
            assert_ne!(r.cd.read(0) & 0x40, 0, "data fifo not empty");
            let first = r.cd.read(2);
            let mut n = 1;
            while r.cd.read(0) & 0x40 != 0 {
                assert_eq!(r.cd.read(2), expect);
                n += 1;
            }
            assert_eq!(first, expect);
            assert_eq!(n, 0x800);
            r.cd.write(3, 0x00, &mut r.irq);
        }
        // GetlocL returns the header of the last sector (LBA 8 -> 00:02:08)
        r.cmd(0x10, &[]);
        let (_, resp) = r.wait_int(200_000);
        assert_eq!(&resp[0..4], &[0x00, 0x02, 0x08, 0x02]);
        // Pause: INT3 then INT2, reading stops
        r.cmd(0x09, &[]);
        assert_eq!(r.wait_int(400_000).0, 3);
        assert_eq!(r.wait_int(4_000_000).0, 2);
    }

    #[test]
    fn raw_sector_mode_gives_0x924_bytes() {
        let mut r = Rig::new(true);
        r.cmd(0x0E, &[0xA0]);
        r.wait_int(200_000);
        r.cmd(0x06, &[]);
        r.wait_int(200_000);
        r.wait_int(2_000_000);
        r.cd.write(3, 0x80, &mut r.irq);
        let hdr: Vec<u8> = (0..4).map(|_| r.cd.read(2)).collect();
        assert_eq!(hdr, vec![0x00, 0x02, 0x00, 0x02]);
        let mut n = 4;
        while r.cd.read(0) & 0x40 != 0 {
            r.cd.read(2);
            n += 1;
        }
        assert_eq!(n, 0x924);
    }

    #[test]
    fn gettn_gettd_getlocp() {
        let mut r = Rig::new(true);
        r.cmd(0x13, &[]);
        assert_eq!(r.wait_int(200_000).1, vec![STAT_MOTOR, 0x01, 0x01]);
        r.cmd(0x14, &[0x00]);
        // lead-out = 150 + 32 = 182 frames = 00:02:32
        assert_eq!(r.wait_int(200_000).1, vec![STAT_MOTOR, 0x00, 0x02]);
        r.cmd(0x14, &[0x01]);
        assert_eq!(r.wait_int(200_000).1, vec![STAT_MOTOR, 0x00, 0x02]);
        r.cmd(0x02, &[0x00, 0x02, 0x10]);
        r.wait_int(200_000);
        r.cmd(0x15, &[]);
        r.wait_int(200_000);
        assert_eq!(r.wait_int(2_000_000).0, 2);
        r.cmd(0x11, &[]);
        let (_, resp) = r.wait_int(200_000);
        assert_eq!(resp, vec![0x01, 0x01, 0x00, 0x00, 0x10, 0x00, 0x02, 0x10]);
    }

    #[test]
    fn errors_for_bad_params_and_unknown_commands() {
        let mut r = Rig::new(true);
        r.cmd(0x02, &[0x00]); // Setloc needs 3 params
        let (i, resp) = r.wait_int(200_000);
        assert_eq!(i, 5);
        assert_eq!(resp[1], 0x20);
        r.cmd(0x55, &[]);
        let (i, resp) = r.wait_int(200_000);
        assert_eq!(i, 5);
        assert_eq!(resp[1], 0x40);
    }

    #[test]
    fn second_response_waits_for_ack() {
        let mut r = Rig::new(true);
        r.cmd(0x0A, &[]); // Init: INT3 then INT2
        // don't ack the first interrupt for a long time
        for _ in 0..2000 {
            r.cd.tick(500, &mut r.irq);
        }
        r.cd.write(0, 1, &mut r.irq);
        assert_eq!(r.cd.read(3) & 7, 3, "still showing INT3");
        r.cd.write(3, 0x1F, &mut r.irq);
        r.cd.write(0, 0, &mut r.irq);
        assert_eq!(r.wait_int(100_000).0, 2, "INT2 delivered after ack");
    }

    #[test]
    fn interrupt_enable_masks_irq_line() {
        let mut cd = CdRom::new();
        cd.insert_disc(disc(4));
        let mut irq = Irq::default();
        cd.write(1, 0x01, &mut irq);
        for _ in 0..200 {
            cd.tick(500, &mut irq);
        }
        assert_eq!(irq.stat, 0, "IE=0: no IRQ");
        cd.write(0, 1, &mut irq);
        assert_eq!(cd.read(3) & 7, 3);
        cd.write(2, 0x1F, &mut irq); // enabling with pending flag raises it
        assert_ne!(irq.stat & 4, 0);
    }

    #[test]
    fn xa_adpcm_decoding_produces_audio() {
        let mut cd = CdRom::new();
        let mut s = [0u8; SECTOR_SIZE];
        s[15] = 2;
        s[18] = 0x64; // audio | form2 | realtime
        s[19] = 0x00; // mono, 37.8kHz, 4-bit
        // every group: filter 0, range 0; data nibbles = 0x7 (max positive)
        for g in 0..18 {
            let base = 24 + g * 128;
            for b in &mut s[base + 16..base + 128] {
                *b = 0x77;
            }
        }
        cd.decode_xa(&s);
        // 18 groups * 8 units * 28 samples = 4032 mono samples at 37.8kHz
        let expect = 4032.0 * 44100.0 / 37800.0;
        let got = cd.audio_queued() as f64;
        assert!((got - expect).abs() < 3.0, "{} vs {}", got, expect);
        // nibble 7 << 12 = 0x7000 with range 0
        let (l, r) = (0..10).map(|_| cd.pop_audio()).last().unwrap();
        assert_eq!(l, 0x7000);
        assert_eq!(l, r);
    }

    #[test]
    fn volume_matrix_applies_on_adpctl() {
        let mut cd = CdRom::new();
        let mut irq = Irq::default();
        cd.write(0, 2, &mut irq);
        cd.write(2, 0x40, &mut irq); // ATV0 L->L = half
        cd.write(0, 3, &mut irq);
        cd.write(1, 0x00, &mut irq); // ATV2 R->R = 0
        cd.push_audio(1000, 1000);
        assert_eq!(cd.pop_audio(), (1000, 1000), "not applied yet");
        cd.write(3, 0x20, &mut irq);
        cd.push_audio(1000, 1000);
        assert_eq!(cd.pop_audio(), (500, 0));
    }
}
