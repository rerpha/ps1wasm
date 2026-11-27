//! GPU: GP0/GP1 command processing, software rasterizer and video timing.
//!
//! VRAM is 1024x512 16-bit pixels (BGR555 + mask bit). Rendering follows the
//! hardware rules documented in psx-spx: top-left fill convention, quads as
//! two triangles (0-1-2, 1-2-3), 4/8/15bpp textures with CLUT, texture
//! window, 4 semi-transparency modes, 4x4 ordered dither, mask set/check.

use crate::irq::{Interrupt, Irq};
use crate::timers::VideoEvents;

pub const VRAM_W: usize = 1024;
pub const VRAM_H: usize = 512;

const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Vertex {
    pub x: i32,
    pub y: i32,
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub u: i32,
    pub v: i32,
}

#[derive(Clone, Copy, Debug)]
struct TexInfo {
    page_x: i32,
    page_y: i32,
    depth: u32,
    clut_x: i32,
    clut_y: i32,
}

#[derive(Clone, Copy)]
struct DrawFlags {
    gouraud: bool,
    tex: Option<TexInfo>,
    raw: bool,
    semi: Option<u32>,
    dither: bool,
}

#[derive(Clone, Copy, Debug)]
struct Transfer {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    cx: u32,
    cy: u32,
}

impl Transfer {
    fn next(&mut self) -> (usize, usize, bool) {
        let px = ((self.x + self.cx) & 0x3FF) as usize;
        let py = ((self.y + self.cy) & 0x1FF) as usize;
        self.cx += 1;
        if self.cx == self.w {
            self.cx = 0;
            self.cy += 1;
        }
        (px, py, self.cy == self.h)
    }
}

#[derive(Clone, Copy, Debug)]
struct PolyLine {
    shaded: bool,
    semi: bool,
    prev: Vertex,
    color: u32,
    expect_color: bool,
}

#[derive(Clone, Copy, Debug)]
enum Gp0Mode {
    Command,
    CpuToVram(Transfer),
    PolyLine(PolyLine),
}

#[derive(Debug)]
pub struct Gpu {
    pub vram: Vec<u16>,

    // Draw mode (E1)
    pub tp_x: u32,
    pub tp_y: u32,
    pub semi_mode: u32,
    pub tex_depth: u32,
    pub dither: bool,
    pub draw_to_display: bool,
    pub tex_disable: bool,
    /// GP1(09): whether GP0(E1) bit 11 (texture disable) is honoured.
    pub allow_tex_disable: bool,
    pub rect_flip_x: bool,
    pub rect_flip_y: bool,
    // E2
    tw_mask_x: u32,
    tw_mask_y: u32,
    tw_off_x: u32,
    tw_off_y: u32,
    // E3-E5
    pub da_x1: i32,
    pub da_y1: i32,
    pub da_x2: i32,
    pub da_y2: i32,
    pub off_x: i32,
    pub off_y: i32,
    // E6
    pub set_mask: bool,
    pub check_mask: bool,

    // Display (GP1)
    pub display_disabled: bool,
    pub dma_dir: u32,
    pub disp_x: u32,
    pub disp_y: u32,
    pub h_start: u32,
    pub h_end: u32,
    pub v_start: u32,
    pub v_end: u32,
    pub hres1: u32,
    pub hres2: bool,
    pub vres: bool,
    pub pal: bool,
    pub depth24: bool,
    pub interlace: bool,
    pub reverse: bool,
    pub irq1: bool,
    /// CLUT cache: the palette is kept on-chip. It's reloaded only when the
    /// CLUT position changes, when more entries are needed than are cached
    /// (4bpp loads 16, 8bpp 256), or after GP0(01h). Per ps1-tests
    /// gpu/clut-cache (hardware dump + test source).
    clut_cache: Vec<u16>,
    /// (clut_x, clut_y, entries loaded)
    clut_key: Option<(i32, i32, usize)>,

    // GP0 command processing
    mode: Gp0Mode,
    cmd: Vec<u32>,
    cmd_len: usize,
    read: Option<Transfer>,
    gpuread: u32,

    // Timing
    line: u32,
    vfrac: u32,
    line_clk: u32,
    dot_frac: u32,
    pub field: bool,
    pub in_vblank: bool,
    pub in_hblank: bool,
    pub frame_count: u64,
    /// Bus time until which a GPU DMA is still streaming data in, and the
    /// bus time as of the last sync (set by the bus).
    pub dma_busy_until: u64,
    pub now: u64,
    /// Bus time until which the GPU is still rasterizing queued work.
    pub busy_until: u64,
    /// Pixels shaded so far (drives the fill-rate timing model).
    pixels: u64,
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline(always)]
fn sext11(v: u32) -> i32 {
    ((v << 21) as i32) >> 21
}

#[inline(always)]
fn rgb24_to_15(c: u32) -> u16 {
    let r = (c >> 3) & 0x1F;
    let g = (c >> 11) & 0x1F;
    let b = (c >> 19) & 0x1F;
    (r | (g << 5) | (b << 10)) as u16
}

// CPU cycles per pixel x1024 (rates from ps1-tests gpu/bandwidth on hardware:
// fill 779 MB/s, flat 124, semi-transparent 82, textured rect 62,
// textured quad 23, VRAM copy 49).
const PX_FILL: u64 = 89;
const PX_FLAT: u64 = 560;
const PX_SEMI: u64 = 846;
const PX_TEX_RECT: u64 = 1119;
const PX_TEX_POLY: u64 = 3012;
const PX_COPY: u64 = 1422;

pub fn cmd_len(op: u32) -> usize {
    match op {
        0x02 => 3,
        0x20..=0x3F => {
            let n = if op & 0x08 != 0 { 4 } else { 3 };
            let tex = op & 0x04 != 0;
            let gour = op & 0x10 != 0;
            1 + n * (1 + tex as usize) + if gour { n - 1 } else { 0 }
        }
        0x40..=0x5F => {
            if op & 0x10 != 0 { 4 } else { 3 }
        }
        0x60..=0x7F => {
            let tex = op & 0x04 != 0;
            let var = (op >> 3) & 3 == 0;
            2 + tex as usize + var as usize
        }
        0x80..=0x9F => 4,
        0xA0..=0xDF => 3,
        _ => 1,
    }
}

impl Gpu {
    pub fn new() -> Self {
        let mut g = Gpu {
            vram: vec![0; VRAM_W * VRAM_H],
            tp_x: 0,
            tp_y: 0,
            semi_mode: 0,
            tex_depth: 0,
            dither: false,
            draw_to_display: false,
            tex_disable: false,
            allow_tex_disable: false,
            rect_flip_x: false,
            rect_flip_y: false,
            tw_mask_x: 0,
            tw_mask_y: 0,
            tw_off_x: 0,
            tw_off_y: 0,
            da_x1: 0,
            da_y1: 0,
            da_x2: 0,
            da_y2: 0,
            off_x: 0,
            off_y: 0,
            set_mask: false,
            check_mask: false,
            display_disabled: true,
            dma_dir: 0,
            disp_x: 0,
            disp_y: 0,
            h_start: 0x200,
            h_end: 0xC00,
            v_start: 0x10,
            v_end: 0x100,
            hres1: 0,
            hres2: false,
            vres: false,
            pal: false,
            depth24: false,
            interlace: false,
            reverse: false,
            irq1: false,
            clut_cache: vec![0; 256],
            clut_key: None,
            mode: Gp0Mode::Command,
            cmd: Vec::with_capacity(16),
            cmd_len: 0,
            read: None,
            gpuread: 0,
            line: 0,
            vfrac: 0,
            line_clk: 0,
            dot_frac: 0,
            field: false,
            in_vblank: false,
            in_hblank: true, // line position 0 is inside horizontal blanking
            frame_count: 0,
            dma_busy_until: 0,
            now: 0,
            busy_until: 0,
            pixels: 0,
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        self.allow_tex_disable = false;
        self.gp0_e1(0);
        self.gp0_e2(0);
        self.da_x1 = 0;
        self.da_y1 = 0;
        self.da_x2 = 0;
        self.da_y2 = 0;
        self.off_x = 0;
        self.off_y = 0;
        self.set_mask = false;
        self.check_mask = false;
        self.display_disabled = true;
        self.dma_dir = 0;
        self.disp_x = 0;
        self.disp_y = 0;
        self.h_start = 0x200;
        self.h_end = 0xC00;
        self.v_start = 0x10;
        self.v_end = 0x100;
        self.hres1 = 0;
        self.hres2 = false;
        self.vres = false;
        self.pal = false;
        self.depth24 = false;
        self.interlace = false;
        self.reverse = false;
        self.irq1 = false;
        self.reset_cmd_buffer();
    }

    fn reset_cmd_buffer(&mut self) {
        self.mode = Gp0Mode::Command;
        self.cmd.clear();
        self.cmd_len = 0;
        self.read = None;
    }

    // ---------------------------------------------------------------- status
    pub fn status(&self) -> u32 {
        let mut s = self.tp_x
            | (self.tp_y << 4)
            | (self.semi_mode << 5)
            | (self.tex_depth << 7)
            | ((self.dither as u32) << 9)
            | ((self.draw_to_display as u32) << 10)
            | ((self.set_mask as u32) << 11)
            | ((self.check_mask as u32) << 12)
            | ((self.tex_disable as u32) << 15)
            | ((self.hres2 as u32) << 16)
            | (self.hres1 << 17)
            | ((self.vres as u32) << 19)
            | ((self.pal as u32) << 20)
            | ((self.depth24 as u32) << 21)
            | ((self.interlace as u32) << 22)
            | ((self.display_disabled as u32) << 23)
            | ((self.irq1 as u32) << 24)
            | (self.dma_dir << 29);
        if self.interlace && self.field {
            s |= 1 << 13;
        }
        if self.reverse {
            s |= 1 << 14;
        }
        let streaming = self.now < self.dma_busy_until.max(self.busy_until);
        let idle = matches!(self.mode, Gp0Mode::Command) && self.cmd.is_empty() && !streaming;
        let ready_cmd = idle;
        let ready_read = self.read.is_some();
        let ready_dma = !matches!(self.mode, Gp0Mode::PolyLine(_)) && !streaming;
        s |= (ready_cmd as u32) << 26;
        s |= (ready_read as u32) << 27;
        s |= (ready_dma as u32) << 28;
        let req = match self.dma_dir {
            0 => false,
            1 => true,
            2 => ready_dma,
            _ => ready_read,
        };
        s |= (req as u32) << 25;
        let odd = if self.interlace {
            self.field && !self.in_vblank
        } else {
            self.line & 1 != 0
        };
        s |= (odd as u32) << 31;
        s
    }

    pub fn read_data(&mut self) -> u32 {
        if let Some(mut t) = self.read {
            let mut out = 0u32;
            for k in 0..2 {
                let (x, y, done) = t.next();
                out |= (self.vram[y * VRAM_W + x] as u32) << (16 * k);
                if done {
                    self.read = None;
                    self.gpuread = out;
                    return out;
                }
            }
            self.read = Some(t);
            self.gpuread = out;
        }
        self.gpuread
    }

    // ---------------------------------------------------------------- GP1
    pub fn gp1(&mut self, w: u32, irq: &mut Irq) {
        let _ = irq;
        let v = w & 0x00FF_FFFF;
        match (w >> 24) & 0x3F {
            0x00 => self.reset(),
            0x01 => self.reset_cmd_buffer(),
            0x02 => self.irq1 = false,
            0x03 => self.display_disabled = v & 1 != 0,
            0x04 => self.dma_dir = v & 3,
            0x05 => {
                self.disp_x = v & 0x3FE;
                self.disp_y = (v >> 10) & 0x1FF;
            }
            0x06 => {
                self.h_start = v & 0xFFF;
                self.h_end = (v >> 12) & 0xFFF;
            }
            0x07 => {
                self.v_start = v & 0x3FF;
                self.v_end = (v >> 10) & 0x3FF;
            }
            0x08 => {
                self.hres1 = v & 3;
                self.vres = v & 4 != 0;
                self.pal = v & 8 != 0;
                self.depth24 = v & 0x10 != 0;
                self.interlace = v & 0x20 != 0;
                self.hres2 = v & 0x40 != 0;
                self.reverse = v & 0x80 != 0;
            }
            0x09 => self.allow_tex_disable = v & 1 != 0,
            0x10..=0x1F => {
                self.gpuread = match v & 7 {
                    2 => self.tw_mask_x | (self.tw_mask_y << 5) | (self.tw_off_x << 10) | (self.tw_off_y << 15),
                    3 => (self.da_x1 as u32) | ((self.da_y1 as u32) << 10),
                    4 => (self.da_x2 as u32) | ((self.da_y2 as u32) << 10),
                    5 => ((self.off_x as u32) & 0x7FF) | (((self.off_y as u32) & 0x7FF) << 11),
                    7 => 2,
                    _ => self.gpuread,
                };
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- GP0
    pub fn gp0(&mut self, w: u32, irq: &mut Irq) {
        match self.mode {
            Gp0Mode::CpuToVram(mut t) => {
                for k in 0..2 {
                    let (x, y, done) = t.next();
                    self.put_raw((w >> (16 * k)) as u16, x, y);
                    if done {
                        self.mode = Gp0Mode::Command;
                        return;
                    }
                }
                self.mode = Gp0Mode::CpuToVram(t);
            }
            Gp0Mode::PolyLine(pl) => self.polyline_word(pl, w),
            Gp0Mode::Command => {
                if self.cmd.is_empty() {
                    self.cmd_len = cmd_len(w >> 24);
                }
                self.cmd.push(w);
                if self.cmd.len() >= self.cmd_len {
                    let words = std::mem::take(&mut self.cmd);
                    self.execute(&words, irq);
                    self.cmd = words;
                    self.cmd.clear();
                }
            }
        }
    }

    #[inline(always)]
    fn put_raw(&mut self, val: u16, x: usize, y: usize) {
        let i = y * VRAM_W + x;
        if self.check_mask && self.vram[i] & 0x8000 != 0 {
            return;
        }
        self.vram[i] = val | ((self.set_mask as u16) << 15);
    }

    fn execute(&mut self, c: &[u32], irq: &mut Irq) {
        let op = c[0] >> 24;
        match op {
            0x02 => {
                self.fill_rect(c);
                let w = (((c[2] & 0x3FF) + 0xF) & !0xF) as u64;
                let h = ((c[2] >> 16) & 0x1FF) as u64;
                self.charge(w * h, PX_FILL);
            }
            0x1F => {
                self.irq1 = true;
                irq.request(Interrupt::Gpu);
            }
            0x20..=0x3F => {
                let p0 = self.pixels;
                self.polygon(c);
                let cost = if op & 4 != 0 { PX_TEX_POLY } else if op & 2 != 0 { PX_SEMI } else { PX_FLAT };
                self.charge(self.pixels - p0, cost);
            }
            0x40..=0x5F => {
                let p0 = self.pixels;
                self.line_cmd(c);
                self.charge(self.pixels - p0, if op & 2 != 0 { PX_SEMI } else { PX_FLAT });
            }
            0x60..=0x7F => {
                let p0 = self.pixels;
                self.rect(c);
                let cost = if op & 4 != 0 { PX_TEX_RECT } else if op & 2 != 0 { PX_SEMI } else { PX_FLAT };
                self.charge(self.pixels - p0, cost);
            }
            0x80..=0x9F => {
                self.vram_copy(c);
                let w = (((c[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1) as u64;
                let h = (((c[3] >> 16).wrapping_sub(1) & 0x1FF) + 1) as u64;
                self.charge(w * h, PX_COPY);
            }
            0xA0..=0xBF => {
                let t = self.transfer_from(c);
                self.mode = Gp0Mode::CpuToVram(t);
            }
            0xC0..=0xDF => self.read = Some(self.transfer_from(c)),
            0xE1 => self.gp0_e1(c[0]),
            0xE2 => self.gp0_e2(c[0]),
            0xE3 => {
                self.da_x1 = (c[0] & 0x3FF) as i32;
                self.da_y1 = ((c[0] >> 10) & 0x1FF) as i32;
            }
            0xE4 => {
                self.da_x2 = (c[0] & 0x3FF) as i32;
                self.da_y2 = ((c[0] >> 10) & 0x1FF) as i32;
            }
            0xE5 => {
                self.off_x = sext11(c[0]);
                self.off_y = sext11(c[0] >> 11);
            }
            0xE6 => {
                self.set_mask = c[0] & 1 != 0;
                self.check_mask = c[0] & 2 != 0;
            }
            0x01 => self.clut_key = None, // clear cache
            _ => {}                       // NOPs
        }
    }

    /// Keep the GPU busy for `pixels` at `per_px` CPU cycles/1024 per pixel.
    /// Rates from ps1-tests gpu/bandwidth measured on hardware.
    fn charge(&mut self, pixels: u64, per_px: u64) {
        let start = self.busy_until.max(self.now);
        self.busy_until = start + (pixels * per_px) / 1024;
    }

    fn gp0_e1(&mut self, w: u32) {
        self.tp_x = w & 0xF;
        self.tp_y = (w >> 4) & 1;
        self.semi_mode = (w >> 5) & 3;
        self.tex_depth = (w >> 7) & 3;
        self.dither = w & (1 << 9) != 0;
        self.draw_to_display = w & (1 << 10) != 0;
        self.tex_disable = self.allow_tex_disable && w & (1 << 11) != 0;
        self.rect_flip_x = w & (1 << 12) != 0;
        self.rect_flip_y = w & (1 << 13) != 0;
    }

    fn gp0_e2(&mut self, w: u32) {
        self.tw_mask_x = w & 0x1F;
        self.tw_mask_y = (w >> 5) & 0x1F;
        self.tw_off_x = (w >> 10) & 0x1F;
        self.tw_off_y = (w >> 15) & 0x1F;
    }

    fn transfer_from(&self, c: &[u32]) -> Transfer {
        let x = c[1] & 0x3FF;
        let y = (c[1] >> 16) & 0x1FF;
        let w = ((c[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let h = ((c[2] >> 16).wrapping_sub(1) & 0x1FF) + 1;
        Transfer { x, y, w, h, cx: 0, cy: 0 }
    }

    // ---------------------------------------------------------------- simple ops
    fn fill_rect(&mut self, c: &[u32]) {
        let color = rgb24_to_15(c[0]);
        let x0 = c[1] & 0x3F0;
        let y0 = (c[1] >> 16) & 0x1FF;
        let w = ((c[2] & 0x3FF) + 0xF) & !0xF;
        let h = (c[2] >> 16) & 0x1FF;
        for y in 0..h {
            let py = ((y0 + y) & 0x1FF) as usize;
            for x in 0..w {
                let px = ((x0 + x) & 0x3FF) as usize;
                self.vram[py * VRAM_W + px] = color;
            }
        }
    }

    fn vram_copy(&mut self, c: &[u32]) {
        let sx = c[1] & 0x3FF;
        let sy = (c[1] >> 16) & 0x1FF;
        let dx = c[2] & 0x3FF;
        let dy = (c[2] >> 16) & 0x1FF;
        let w = ((c[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let h = ((c[3] >> 16).wrapping_sub(1) & 0x1FF) + 1;
        let mut row = vec![0u16; w as usize];
        for y in 0..h {
            let ys = ((sy + y) & 0x1FF) as usize;
            let yd = ((dy + y) & 0x1FF) as usize;
            for x in 0..w {
                row[x as usize] = self.vram[ys * VRAM_W + ((sx + x) & 0x3FF) as usize];
            }
            for x in 0..w {
                self.put_raw(row[x as usize], ((dx + x) & 0x3FF) as usize, yd);
            }
        }
    }

    // ---------------------------------------------------------------- pixel pipeline
    #[inline(always)]
    fn tex_fetch(&self, t: &TexInfo, u: i32, v: i32) -> u16 {
        let u = ((u as u32 & 0xFF) & !(self.tw_mask_x * 8)) | ((self.tw_off_x & self.tw_mask_x) * 8);
        let v = ((v as u32 & 0xFF) & !(self.tw_mask_y * 8)) | ((self.tw_off_y & self.tw_mask_y) * 8);
        let row = ((t.page_y as u32 + v) & 0x1FF) as usize * VRAM_W;
        match t.depth {
            0 => {
                let w = self.vram[row + ((t.page_x as u32 + u / 4) & 0x3FF) as usize];
                let idx = (w >> ((u & 3) * 4)) & 0xF;
                self.clut_cache[idx as usize]
            }
            1 => {
                let w = self.vram[row + ((t.page_x as u32 + u / 2) & 0x3FF) as usize];
                let idx = (w >> ((u & 1) * 8)) & 0xFF;
                self.clut_cache[idx as usize]
            }
            _ => self.vram[row + ((t.page_x as u32 + u) & 0x3FF) as usize],
        }
    }

    fn load_clut(&mut self, t: &TexInfo) {
        if t.depth >= 2 {
            return;
        }
        let need = if t.depth == 0 { 16 } else { 256 };
        if let Some((x, y, loaded)) = self.clut_key {
            if x == t.clut_x && y == t.clut_y && loaded >= need {
                return;
            }
        }
        self.clut_key = Some((t.clut_x, t.clut_y, need));
        let row = t.clut_y as usize * VRAM_W;
        for k in 0..need {
            self.clut_cache[k] = self.vram[row + ((t.clut_x as usize + k) & 0x3FF)];
        }
    }

    /// Shade and write one pixel. `r,g,b` are 8-bit vertex colors.
    #[inline(always)]
    fn shade(&mut self, x: i32, y: i32, r: i32, g: i32, b: i32, u: i32, v: i32, f: &DrawFlags) {
        let idx = y as usize * VRAM_W + x as usize;
        self.pixels += 1;
        let bg = self.vram[idx];
        if self.check_mask && bg & 0x8000 != 0 {
            return;
        }
        let (mut r8, mut g8, mut b8, mask_bit, mut semi_on);
        match f.tex {
            Some(ref t) => {
                let texel = self.tex_fetch(t, u, v);
                if texel == 0 {
                    return;
                }
                let tr = (texel & 0x1F) as i32;
                let tg = ((texel >> 5) & 0x1F) as i32;
                let tb = ((texel >> 10) & 0x1F) as i32;
                if f.raw {
                    r8 = tr << 3;
                    g8 = tg << 3;
                    b8 = tb << 3;
                } else {
                    r8 = (tr * r) >> 4;
                    g8 = (tg * g) >> 4;
                    b8 = (tb * b) >> 4;
                }
                mask_bit = texel & 0x8000;
                semi_on = texel & 0x8000 != 0;
            }
            None => {
                r8 = r;
                g8 = g;
                b8 = b;
                mask_bit = 0;
                semi_on = true;
            }
        }
        if f.dither {
            let d = DITHER[(y & 3) as usize][(x & 3) as usize];
            r8 += d;
            g8 += d;
            b8 += d;
        }
        let mut cr = r8.clamp(0, 255) >> 3;
        let mut cg = g8.clamp(0, 255) >> 3;
        let mut cb = b8.clamp(0, 255) >> 3;
        semi_on &= f.semi.is_some();
        if semi_on {
            let br = (bg & 0x1F) as i32;
            let bgg = ((bg >> 5) & 0x1F) as i32;
            let bb = ((bg >> 10) & 0x1F) as i32;
            let m = f.semi.unwrap();
            let mix = |bk: i32, fg: i32| -> i32 {
                let v = match m {
                    0 => (bk + fg) >> 1,
                    1 => bk + fg,
                    2 => bk - fg,
                    _ => bk + (fg >> 2),
                };
                v.clamp(0, 31)
            };
            cr = mix(br, cr);
            cg = mix(bgg, cg);
            cb = mix(bb, cb);
        }
        self.vram[idx] =
            (cr as u16) | ((cg as u16) << 5) | ((cb as u16) << 10) | mask_bit | ((self.set_mask as u16) << 15);
    }

    // ---------------------------------------------------------------- polygons
    fn polygon(&mut self, c: &[u32]) {
        let op = c[0] >> 24;
        let quad = op & 0x08 != 0;
        let textured = op & 0x04 != 0;
        let semi = op & 0x02 != 0;
        let raw = op & 0x01 != 0;
        let gouraud = op & 0x10 != 0;
        let n = if quad { 4 } else { 3 };

        let mut verts = [Vertex::default(); 4];
        let mut k = 0usize;
        let mut clut = 0u32;
        let mut page = 0u32;
        for (i, vtx) in verts.iter_mut().enumerate().take(n) {
            let color = if i == 0 {
                k += 1;
                c[0]
            } else if gouraud {
                let col = c[k];
                k += 1;
                col
            } else {
                c[0]
            };
            let pos = c[k];
            k += 1;
            vtx.x = sext11(pos) + self.off_x;
            vtx.y = sext11(pos >> 16) + self.off_y;
            vtx.r = (color & 0xFF) as i32;
            vtx.g = ((color >> 8) & 0xFF) as i32;
            vtx.b = ((color >> 16) & 0xFF) as i32;
            if textured {
                let uv = c[k];
                k += 1;
                vtx.u = (uv & 0xFF) as i32;
                vtx.v = ((uv >> 8) & 0xFF) as i32;
                if i == 0 {
                    clut = uv >> 16;
                } else if i == 1 {
                    page = uv >> 16;
                }
            }
        }

        let tex = if textured {
            // Polygon texpage also updates the global draw mode.
            self.tp_x = page & 0xF;
            self.tp_y = (page >> 4) & 1;
            self.semi_mode = (page >> 5) & 3;
            self.tex_depth = (page >> 7) & 3;
            self.tex_disable = self.allow_tex_disable && page & (1 << 11) != 0;
            Some(TexInfo {
                page_x: (self.tp_x * 64) as i32,
                page_y: (self.tp_y * 256) as i32,
                depth: self.tex_depth,
                clut_x: ((clut & 0x3F) * 16) as i32,
                clut_y: ((clut >> 6) & 0x1FF) as i32,
            })
        } else {
            None
        };
        let flags = DrawFlags {
            gouraud,
            tex,
            raw: raw && textured,
            semi: if semi { Some(self.semi_mode) } else { None },
            dither: self.dither && (gouraud || (textured && !raw)),
        };
        if let Some(t) = &tex {
            self.load_clut(t);
        }
        self.triangle(verts[0], verts[1], verts[2], &flags);
        if quad {
            self.triangle(verts[1], verts[2], verts[3], &flags);
        }
    }

    fn triangle(&mut self, v0: Vertex, mut v1: Vertex, mut v2: Vertex, f: &DrawFlags) {
        // Reject oversized primitives like the hardware does.
        let xs = [v0.x, v1.x, v2.x];
        let ys = [v0.y, v1.y, v2.y];
        let (minx, maxx) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
        let (miny, maxy) = (*ys.iter().min().unwrap(), *ys.iter().max().unwrap());
        if maxx - minx >= 1024 || maxy - miny >= 512 {
            return;
        }
        // Attribute anchor: the leftmost vertex, with the hardware's tie rules
        // (chosen from the original vertex order).
        let anchor = if v1.x <= v0.x {
            if v2.x <= v1.x { v2 } else { v1 }
        } else if v2.x < v0.x {
            v2
        } else {
            v0
        };
        let edge = |a: &Vertex, b: &Vertex, px: i32, py: i32| -> i64 {
            (b.x - a.x) as i64 * (py - a.y) as i64 - (b.y - a.y) as i64 * (px - a.x) as i64
        };
        let area = edge(&v0, &v1, v2.x, v2.y);
        if area == 0 {
            return;
        }
        if area < 0 {
            // Coverage needs counter-clockwise order; attribute steps are
            // invariant under the swap (det and detA both change sign).
            std::mem::swap(&mut v1, &mut v2);
        }

        let x_lo = minx.max(self.da_x1);
        let x_hi = maxx.min(self.da_x2);
        let y_lo = miny.max(self.da_y1);
        let y_hi = maxy.min(self.da_y2);
        if x_lo > x_hi || y_lo > y_hi {
            return;
        }

        // Attribute interpolation as on hardware (Mednafen-derived model,
        // validated against ps1-tests gpu/triangle + uv-interpolation dumps):
        // 12-bit steps = trunc(detA * 4096 / det) << 12, values anchored at the
        // leftmost vertex with +0.5, accumulated in wrapping 32-bit math and
        // read from bits 24..31.
        let det = ((v1.x - v0.x) as i64) * ((v2.y - v1.y) as i64) - ((v2.x - v1.x) as i64) * ((v1.y - v0.y) as i64);
        let step = |a0: i32, a1: i32, a2: i32| -> (u32, u32) {
            let dx = ((a1 - a0) as i64) * ((v2.y - v1.y) as i64) - ((a2 - a1) as i64) * ((v1.y - v0.y) as i64);
            let dy = ((v1.x - v0.x) as i64) * ((a2 - a1) as i64) - ((v2.x - v1.x) as i64) * ((a1 - a0) as i64);
            (((dx * 4096 / det) as i32 as u32) << 12, ((dy * 4096 / det) as i32 as u32) << 12)
        };
        let base = |a: i32, sx: u32, sy: u32| -> u32 {
            let start = (((a as u32) << 12) + (1 << 11)) << 12;
            start.wrapping_sub(sx.wrapping_mul(anchor.x as u32)).wrapping_sub(sy.wrapping_mul(anchor.y as u32))
        };
        let (drdx, drdy) = step(v0.r, v1.r, v2.r);
        let (dgdx, dgdy) = step(v0.g, v1.g, v2.g);
        let (dbdx, dbdy) = step(v0.b, v1.b, v2.b);
        let (dudx, dudy) = step(v0.u, v1.u, v2.u);
        let (dvdx, dvdy) = step(v0.v, v1.v, v2.v);
        let (r0, g0, b0) = (base(anchor.r, drdx, drdy), base(anchor.g, dgdx, dgdy), base(anchor.b, dbdx, dbdy));
        let (u0, v0b) = (base(anchor.u, dudx, dudy), base(anchor.v, dvdx, dvdy));
        let at = |b: u32, sx: u32, sy: u32, x: i32, y: i32| -> i32 {
            (b.wrapping_add(sx.wrapping_mul(x as u32)).wrapping_add(sy.wrapping_mul(y as u32)) >> 24) as i32
        };

        // Top-left rule bias: an edge a->b owns its pixels when it's a top or left edge.
        let is_tl = |a: &Vertex, b: &Vertex| -> bool {
            let dy = b.y - a.y;
            let dx = b.x - a.x;
            dy < 0 || (dy == 0 && dx > 0)
        };
        let bias0 = if is_tl(&v1, &v2) { 0 } else { -1 };
        let bias1 = if is_tl(&v2, &v0) { 0 } else { -1 };
        let bias2 = if is_tl(&v0, &v1) { 0 } else { -1 };

        // Edge function increments
        let (a12, b12) = ((v1.y - v2.y) as i64, (v2.x - v1.x) as i64); // w0
        let (a20, b20) = ((v2.y - v0.y) as i64, (v0.x - v2.x) as i64); // w1
        let (a01, b01) = ((v0.y - v1.y) as i64, (v1.x - v0.x) as i64); // w2

        let mut w0_row = edge(&v1, &v2, x_lo, y_lo) + bias0;
        let mut w1_row = edge(&v2, &v0, x_lo, y_lo) + bias1;
        let mut w2_row = edge(&v0, &v1, x_lo, y_lo) + bias2;

        for y in y_lo..=y_hi {
            let (mut w0, mut w1, mut w2) = (w0_row, w1_row, w2_row);
            for x in x_lo..=x_hi {
                if (w0 | w1 | w2) >= 0 {
                    let (r, g, b) = if f.gouraud {
                        (at(r0, drdx, drdy, x, y), at(g0, dgdx, dgdy, x, y), at(b0, dbdx, dbdy, x, y))
                    } else {
                        (v0.r, v0.g, v0.b)
                    };
                    let (u, v) = if f.tex.is_some() {
                        (at(u0, dudx, dudy, x, y), at(v0b, dvdx, dvdy, x, y))
                    } else {
                        (0, 0)
                    };
                    self.shade(x, y, r, g, b, u, v, f);
                }
                w0 += a12;
                w1 += a20;
                w2 += a01;
            }
            w0_row += b12;
            w1_row += b20;
            w2_row += b01;
        }
    }

    // ---------------------------------------------------------------- rectangles
    fn rect(&mut self, c: &[u32]) {
        let op = c[0] >> 24;
        let textured = op & 0x04 != 0;
        let semi = op & 0x02 != 0;
        let raw = op & 0x01 != 0;
        let mut k = 1;
        let pos = c[k];
        k += 1;
        let x0 = sext11(pos) + self.off_x;
        let y0 = sext11(pos >> 16) + self.off_y;
        let (mut u0, mut v0, mut clut) = (0i32, 0i32, 0u32);
        if textured {
            let uv = c[k];
            k += 1;
            u0 = (uv & 0xFF) as i32;
            v0 = ((uv >> 8) & 0xFF) as i32;
            clut = uv >> 16;
        }
        let (w, h) = match (op >> 3) & 3 {
            0 => ((c[k] & 0x3FF) as i32, ((c[k] >> 16) & 0x1FF) as i32),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        let r = (c[0] & 0xFF) as i32;
        let g = ((c[0] >> 8) & 0xFF) as i32;
        let b = ((c[0] >> 16) & 0xFF) as i32;
        let tex = if textured {
            Some(TexInfo {
                page_x: (self.tp_x * 64) as i32,
                page_y: (self.tp_y * 256) as i32,
                depth: self.tex_depth,
                clut_x: ((clut & 0x3F) * 16) as i32,
                clut_y: ((clut >> 6) & 0x1FF) as i32,
            })
        } else {
            None
        };
        let f = DrawFlags {
            gouraud: false,
            tex,
            raw: raw && textured,
            semi: if semi { Some(self.semi_mode) } else { None },
            dither: false,
        };
        if let Some(t) = &tex {
            self.load_clut(t);
        }
        let ys = y0.max(self.da_y1);
        let ye = (y0 + h - 1).min(self.da_y2);
        let xs = x0.max(self.da_x1);
        let xe = (x0 + w - 1).min(self.da_x2);
        for y in ys..=ye {
            let dv = y - y0;
            let v = if self.rect_flip_y { v0 - dv } else { v0 + dv };
            for x in xs..=xe {
                let du = x - x0;
                // Hardware quirk (matches ps1-tests gpu/texture-flip dump and
                // Mednafen): X-flipped sprites start from u0 | 1.
                let u = if self.rect_flip_x { (u0 | 1) - du } else { u0 + du };
                self.shade(x, y, r, g, b, u, v, &f);
            }
        }
    }

    // ---------------------------------------------------------------- lines
    fn line_cmd(&mut self, c: &[u32]) {
        let op = c[0] >> 24;
        let shaded = op & 0x10 != 0;
        let semi = op & 0x02 != 0;
        let poly = op & 0x08 != 0;
        let c0 = c[0];
        let v0 = self.line_vertex(c[1], c0);
        let (c1, p1) = if shaded { (c[2], c[3]) } else { (c0, c[2]) };
        let v1 = self.line_vertex(p1, c1);
        self.draw_line(v0, v1, shaded, semi, false);
        if poly {
            self.mode = Gp0Mode::PolyLine(PolyLine { shaded, semi, prev: v1, color: c0, expect_color: shaded });
        }
    }

    fn line_vertex(&self, pos: u32, color: u32) -> Vertex {
        Vertex {
            x: sext11(pos) + self.off_x,
            y: sext11(pos >> 16) + self.off_y,
            r: (color & 0xFF) as i32,
            g: ((color >> 8) & 0xFF) as i32,
            b: ((color >> 16) & 0xFF) as i32,
            u: 0,
            v: 0,
        }
    }

    fn polyline_word(&mut self, mut pl: PolyLine, w: u32) {
        if w & 0xF000_F000 == 0x5000_5000 {
            self.mode = Gp0Mode::Command;
            return;
        }
        if pl.expect_color {
            pl.color = w;
            pl.expect_color = false;
        } else {
            let v = self.line_vertex(w, pl.color);
            self.draw_line(pl.prev, v, pl.shaded, pl.semi, false);
            pl.prev = v;
            pl.expect_color = pl.shaded;
        }
        self.mode = Gp0Mode::PolyLine(pl);
    }

    fn draw_line(&mut self, a: Vertex, b: Vertex, shaded: bool, semi: bool, skip_first: bool) {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        if dx.abs() >= 1024 || dy.abs() >= 512 {
            return;
        }
        let f = DrawFlags {
            gouraud: shaded,
            tex: None,
            raw: false,
            semi: if semi { Some(self.semi_mode) } else { None },
            dither: self.dither,
        };
        let steps = dx.abs().max(dy.abs());
        if steps == 0 {
            if self.in_draw_area(a.x, a.y) {
                self.shade(a.x, a.y, a.r, a.g, a.b, 0, 0, &f);
            }
            return;
        }
        // Fixed-point stepping as in Mednafen (matched to hardware, and to the
        // ps1-tests gpu/lines dump): 32 fraction bits, division rounded away
        // from zero, start at +0.5 with a small negative bias on X (and on Y
        // when Y decreases).
        let s = steps as i64;
        let line_div = |delta: i64| -> i64 {
            let mut d = delta << 32;
            if d < 0 {
                d -= s - 1;
            } else if d > 0 {
                d += s - 1;
            }
            d / s
        };
        let fx = line_div(dx as i64);
        let fy = line_div(dy as i64);
        let fr = (((b.r - a.r) as i64) << 12) / s;
        let fg = (((b.g - a.g) as i64) << 12) / s;
        let fb = (((b.b - a.b) as i64) << 12) / s;
        let half = 1i64 << 31;
        let mut x = ((a.x as i64) << 32) + half - 1024;
        let mut y = ((a.y as i64) << 32) + half;
        if fy < 0 {
            y -= 1024;
        }
        let chalf = 1i64 << 11;
        let (mut r, mut g, mut bb) = (((a.r as i64) << 12) + chalf, ((a.g as i64) << 12) + chalf, ((a.b as i64) << 12) + chalf);
        for i in 0..=steps {
            let px = (x >> 32) as i32;
            let py = (y >> 32) as i32;
            if self.in_draw_area(px, py) && !(skip_first && i == 0) {
                self.shade(px, py, (r >> 12) as i32, (g >> 12) as i32, (bb >> 12) as i32, 0, 0, &f);
            }
            x += fx;
            y += fy;
            if shaded {
                r += fr;
                g += fg;
                bb += fb;
            }
        }
    }

    #[inline(always)]
    fn in_draw_area(&self, x: i32, y: i32) -> bool {
        x >= self.da_x1 && x <= self.da_x2 && y >= self.da_y1 && y <= self.da_y2
    }

    // ---------------------------------------------------------------- timing
    pub fn lines_per_frame(&self) -> u32 {
        if self.pal { 314 } else { 263 }
    }
    fn clocks_per_line(&self) -> u32 {
        if self.pal { 3406 } else { 3413 }
    }
    pub fn dot_divider(&self) -> u32 {
        if self.hres2 {
            7
        } else {
            [10, 8, 5, 4][self.hres1 as usize]
        }
    }
    fn vblank_lines(&self) -> (u32, u32) {
        let total = self.lines_per_frame();
        let (s, e) = (self.v_start, self.v_end);
        if e > s && e < total {
            (s, e)
        } else if self.pal {
            (0x23, 0x23 + 288)
        } else {
            (0x10, 0x10 + 240)
        }
    }

    /// Advance video timing by `cycles` CPU cycles.
    pub fn tick(&mut self, cycles: u32, irq: &mut Irq) -> VideoEvents {
        let mut ev = VideoEvents::default();
        self.vfrac += cycles * 11;
        let vc = self.vfrac / 7;
        self.vfrac %= 7;

        let div = self.dot_divider();
        self.dot_frac += vc;
        ev.dots = self.dot_frac / div;
        self.dot_frac %= div;

        let cpl = self.clocks_per_line();
        let (vis_start, vis_end) = self.vblank_lines();
        // Horizontal blanking is the part of each line outside the display
        // range [h_start, h_end) (GP1(06), in video clocks).
        let hb_start = self.h_end.min(cpl - 1);
        let hb_end = self.h_start.min(hb_start);
        let mut remaining = vc;
        while remaining > 0 {
            // next event position on this line
            let mut next = cpl;
            if self.line_clk < hb_end {
                next = hb_end;
            } else if self.line_clk < hb_start {
                next = hb_start;
            }
            let step = (next - self.line_clk).min(remaining);
            self.line_clk += step;
            remaining -= step;
            if self.line_clk == hb_end && self.in_hblank {
                self.in_hblank = false;
                ev.hblank_ends += 1;
            }
            if self.line_clk == hb_start && !self.in_hblank {
                self.in_hblank = true;
                ev.hblanks += 1;
            }
            if self.line_clk >= cpl {
                self.line_clk = 0;
                self.line += 1;
                if self.line >= self.lines_per_frame() {
                    self.line = 0;
                    if self.interlace {
                        self.field = !self.field;
                    } else {
                        self.field = false;
                    }
                }
                if self.line == vis_end {
                    self.in_vblank = true;
                    ev.vblank_start = true;
                    self.frame_count += 1;
                    irq.request(Interrupt::VBlank);
                } else if self.line == vis_start {
                    self.in_vblank = false;
                    ev.vblank_end = true;
                }
            }
        }
        ev.in_hblank = self.in_hblank;
        ev.in_vblank = self.in_vblank;
        ev
    }

    /// Convert the visible display area to RGBA8 (row-major, 4 bytes/pixel).
    /// Handles 15-bit and 24-bit display modes; disabled display is black.
    pub fn render_display(&self, out: &mut Vec<u8>) -> (u32, u32) {
        let (w, h) = self.display_size();
        out.clear();
        out.resize((w * h * 4) as usize, 0);
        if self.display_disabled {
            for px in out.as_chunks_mut::<4>().0 {
                px[3] = 0xFF;
            }
            return (w, h);
        }
        for y in 0..h as usize {
            let vy = (self.disp_y as usize + y) & 0x1FF;
            let row = &self.vram[vy * VRAM_W..(vy + 1) * VRAM_W];
            let dst = &mut out[y * w as usize * 4..(y + 1) * w as usize * 4];
            if self.depth24 {
                // 3 bytes per pixel packed across consecutive halfwords
                let byte_at = |b: usize| -> u8 {
                    let hw = row[((self.disp_x as usize) + b / 2) & 0x3FF];
                    (hw >> ((b & 1) * 8)) as u8
                };
                for x in 0..w as usize {
                    dst[4 * x] = byte_at(3 * x);
                    dst[4 * x + 1] = byte_at(3 * x + 1);
                    dst[4 * x + 2] = byte_at(3 * x + 2);
                    dst[4 * x + 3] = 0xFF;
                }
            } else {
                for x in 0..w as usize {
                    let p = row[(self.disp_x as usize + x) & 0x3FF];
                    let c5 = |v: u16| -> u8 {
                        let v = (v & 0x1F) as u8;
                        (v << 3) | (v >> 2)
                    };
                    dst[4 * x] = c5(p);
                    dst[4 * x + 1] = c5(p >> 5);
                    dst[4 * x + 2] = c5(p >> 10);
                    dst[4 * x + 3] = 0xFF;
                }
            }
        }
        (w, h)
    }

    /// Visible display size in pixels (width, height).
    pub fn display_size(&self) -> (u32, u32) {
        let w = if self.hres2 { 368 } else { [256, 320, 512, 640][self.hres1 as usize] };
        let (s, e) = self.vblank_lines();
        let mut h = e - s;
        if self.interlace && self.vres {
            h *= 2;
        }
        (w, h.min(512))
    }
}


fn snap_transfer(s: &mut crate::state::Snap, t: &mut Transfer) {
    for v in [&mut t.x, &mut t.y, &mut t.w, &mut t.h, &mut t.cx, &mut t.cy] {
        s.u32(v);
    }
}

fn snap_vertex(s: &mut crate::state::Snap, v: &mut Vertex) {
    for f in [&mut v.x, &mut v.y, &mut v.r, &mut v.g, &mut v.b, &mut v.u, &mut v.v] {
        s.i32(f);
    }
}

impl Gpu {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("GPU ");
        s.vec_u16(&mut self.vram);
        if self.vram.len() != VRAM_W * VRAM_H {
            self.vram.resize(VRAM_W * VRAM_H, 0);
            s.error.get_or_insert("bad VRAM size".into());
        }
        for v in [&mut self.tp_x, &mut self.tp_y, &mut self.semi_mode, &mut self.tex_depth] {
            s.u32(v);
        }
        for b in [
            &mut self.dither, &mut self.draw_to_display, &mut self.tex_disable, &mut self.allow_tex_disable,
            &mut self.rect_flip_x, &mut self.rect_flip_y, &mut self.set_mask, &mut self.check_mask,
            &mut self.display_disabled, &mut self.hres2, &mut self.vres, &mut self.pal, &mut self.depth24,
            &mut self.interlace, &mut self.reverse, &mut self.irq1, &mut self.field, &mut self.in_vblank,
            &mut self.in_hblank,
        ] {
            s.bool(b);
        }
        for v in [&mut self.tw_mask_x, &mut self.tw_mask_y, &mut self.tw_off_x, &mut self.tw_off_y] {
            s.u32(v);
        }
        for v in [&mut self.da_x1, &mut self.da_y1, &mut self.da_x2, &mut self.da_y2, &mut self.off_x, &mut self.off_y] {
            s.i32(v);
        }
        for v in [
            &mut self.dma_dir, &mut self.disp_x, &mut self.disp_y, &mut self.h_start, &mut self.h_end,
            &mut self.v_start, &mut self.v_end, &mut self.hres1, &mut self.gpuread, &mut self.line,
            &mut self.vfrac, &mut self.line_clk, &mut self.dot_frac,
        ] {
            s.u32(v);
        }
        s.u64(&mut self.frame_count);
        s.u64(&mut self.dma_busy_until);
        s.u64(&mut self.now);
        s.u64(&mut self.busy_until);
        s.u64(&mut self.pixels);
        s.vec_u16(&mut self.clut_cache);
        let mut has = self.clut_key.is_some();
        s.bool(&mut has);
        let mut k = self.clut_key.unwrap_or((0, 0, 0));
        s.i32(&mut k.0);
        s.i32(&mut k.1);
        s.usize(&mut k.2);
        self.clut_key = if has { Some(k) } else { None };

        // GP0 state machine
        let mut tag = match self.mode {
            Gp0Mode::Command => 0u8,
            Gp0Mode::CpuToVram(_) => 1,
            Gp0Mode::PolyLine(_) => 2,
        };
        s.u8(&mut tag);
        let mut t = match self.mode {
            Gp0Mode::CpuToVram(t) => t,
            _ => Transfer { x: 0, y: 0, w: 1, h: 1, cx: 0, cy: 0 },
        };
        snap_transfer(s, &mut t);
        let mut pl = match self.mode {
            Gp0Mode::PolyLine(p) => p,
            _ => PolyLine { shaded: false, semi: false, prev: Vertex::default(), color: 0, expect_color: false },
        };
        s.bool(&mut pl.shaded);
        s.bool(&mut pl.semi);
        snap_vertex(s, &mut pl.prev);
        s.u32(&mut pl.color);
        s.bool(&mut pl.expect_color);
        self.mode = match tag {
            1 => Gp0Mode::CpuToVram(t),
            2 => Gp0Mode::PolyLine(pl),
            _ => Gp0Mode::Command,
        };
        s.vec_u32(&mut self.cmd);
        s.usize(&mut self.cmd_len);
        let mut has_read = self.read.is_some();
        s.bool(&mut has_read);
        let mut r = self.read.unwrap_or(Transfer { x: 0, y: 0, w: 1, h: 1, cx: 0, cy: 0 });
        snap_transfer(s, &mut r);
        self.read = if has_read { Some(r) } else { None };
    }
}
