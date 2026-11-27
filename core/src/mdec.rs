//! MDEC: macroblock decoder (0x1F801820 command/data, 0x1F801824 control/status).
//!
//! Implements RLE decoding with quantization, the two-pass IDCT using the
//! game-uploaded scale table, YUV->RGB, and 4/8/15/24-bit output packing.
//! Decoding happens as soon as a command's parameter words are complete,
//! which is compatible with the DMA0-in / DMA1-out usage of games.

use std::collections::VecDeque;

/// Natural-order index of the k-th zigzag coefficient.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

/// One decoded unit in both output orders. Reading via the data register
/// yields 8x8-block order; DMA1 yields 16x16 row-major order (per ps1-tests
/// mdec/frame: its register path swizzles blocks, its DMA path doesn't, and
/// both match the same hardware VRAM dump).
#[derive(Debug)]
struct OutChunk {
    block_order: Vec<u32>,
    row_major: Vec<u32>,
    pos: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Decode,
    Quant { color: bool },
    Scale,
}

#[derive(Debug)]
pub struct Mdec {
    state: State,
    remaining: u32,
    params: Vec<u32>,
    depth: u32,
    signed: bool,
    bit15: bool,
    qt_y: [u8; 64],
    qt_uv: [u8; 64],
    scale: [i16; 64],
    /// Decoded output, one chunk per macroblock (or mono block).
    out: VecDeque<OutChunk>,
    pub dma_in_enabled: bool,
    pub dma_out_enabled: bool,
    current_block: u32,
}

impl Default for Mdec {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn sext10(v: u16) -> i32 {
    (((v & 0x3FF) as i32) << 22) >> 22
}

impl Mdec {
    pub fn new() -> Self {
        Mdec {
            state: State::Idle,
            remaining: 0,
            params: vec![],
            depth: 0,
            signed: false,
            bit15: false,
            qt_y: [0; 64],
            qt_uv: [0; 64],
            scale: [0; 64],
            out: VecDeque::new(),
            dma_in_enabled: false,
            dma_out_enabled: false,
            current_block: 4,
        }
    }

    pub fn status(&self) -> u32 {
        let mut s = 0u32;
        s |= (self.out.is_empty() as u32) << 31;
        s |= ((self.state != State::Idle) as u32) << 29;
        s |= ((self.dma_in_enabled && self.state != State::Idle) as u32) << 28;
        s |= ((self.dma_out_enabled && !self.out.is_empty()) as u32) << 27;
        s |= self.depth << 25;
        s |= (self.signed as u32) << 24;
        s |= (self.bit15 as u32) << 23;
        s |= (self.current_block & 7) << 16;
        s |= self.remaining.wrapping_sub(1) & 0xFFFF;
        s
    }

    pub fn write_control(&mut self, v: u32) {
        if v & (1 << 31) != 0 {
            self.state = State::Idle;
            self.remaining = 0;
            self.params.clear();
            self.out.clear();
            self.current_block = 4;
        }
        self.dma_in_enabled = v & (1 << 30) != 0;
        self.dma_out_enabled = v & (1 << 29) != 0;
    }

    pub fn write_command(&mut self, w: u32) {
        match self.state {
            State::Idle => {
                self.depth = (w >> 27) & 3;
                self.signed = w & (1 << 26) != 0;
                self.bit15 = w & (1 << 25) != 0;
                match w >> 29 {
                    1 => {
                        self.state = State::Decode;
                        self.remaining = w & 0xFFFF;
                    }
                    2 => {
                        let color = w & 1 != 0;
                        self.state = State::Quant { color };
                        self.remaining = if color { 32 } else { 16 };
                    }
                    3 => {
                        self.state = State::Scale;
                        self.remaining = 32;
                    }
                    _ => {}
                }
                self.params.clear();
                if self.remaining == 0 {
                    self.finish();
                }
            }
            _ => {
                self.params.push(w);
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.finish();
                }
            }
        }
    }

    /// Data register read (block order).
    pub fn read_data(&mut self) -> u32 {
        self.read_word(false)
    }

    /// DMA1 read (row-major order for colour output).
    pub fn read_dma(&mut self) -> u32 {
        self.read_word(true)
    }

    fn read_word(&mut self, dma: bool) -> u32 {
        let Some(c) = self.out.front_mut() else { return 0 };
        let w = if dma { c.row_major[c.pos] } else { c.block_order[c.pos] };
        c.pos += 1;
        if c.pos == c.block_order.len() {
            self.out.pop_front();
        }
        w
    }

    pub fn output_len(&self) -> usize {
        self.out.iter().map(|c| c.block_order.len() - c.pos).sum()
    }

    fn push_words(&mut self, block_order: Vec<u32>, row_major: Vec<u32>) {
        self.out.push_back(OutChunk { block_order, row_major, pos: 0 });
    }

    fn finish(&mut self) {
        let params = std::mem::take(&mut self.params);
        let bytes: Vec<u8> = params.iter().flat_map(|w| w.to_le_bytes()).collect();
        match self.state {
            State::Quant { color } => {
                self.qt_y.copy_from_slice(&bytes[0..64]);
                if color {
                    self.qt_uv.copy_from_slice(&bytes[64..128]);
                }
            }
            State::Scale => {
                for k in 0..64 {
                    self.scale[k] = i16::from_le_bytes([bytes[2 * k], bytes[2 * k + 1]]);
                }
            }
            State::Decode => {
                let halfs: Vec<u16> = params.iter().flat_map(|&w| [w as u16, (w >> 16) as u16]).collect();
                self.decode(&halfs);
            }
            State::Idle => {}
        }
        self.state = State::Idle;
        self.params = params;
        self.params.clear();
    }

    /// RLE + dequantize one block. Returns None at end of data.
    fn decode_block(&self, src: &[u16], pos: &mut usize, qt: &[u8; 64]) -> Option<[i32; 64]> {
        while *pos < src.len() && src[*pos] == 0xFE00 {
            *pos += 1;
        }
        if *pos >= src.len() {
            return None;
        }
        let mut blk = [0i32; 64];
        let mut n = src[*pos];
        *pos += 1;
        let q_scale = ((n >> 10) & 0x3F) as i32;
        let mut k = 0usize;
        let mut val = sext10(n) * qt[0] as i32;
        loop {
            if q_scale == 0 {
                val = sext10(n) * 2;
            }
            val = val.clamp(-0x400, 0x3FF);
            if q_scale > 0 {
                blk[ZIGZAG[k]] = val;
            } else {
                blk[k] = val;
            }
            // A block is complete once coefficient 63 is written: the next
            // halfword already belongs to the following block (no EOB needed).
            // Verified with ps1-tests mdec/frame (stream + hardware dump).
            if k == 63 || *pos >= src.len() {
                break;
            }
            n = src[*pos];
            *pos += 1;
            if n == 0xFE00 {
                break;
            }
            k += ((n >> 10) as usize) + 1;
            if k > 63 {
                break;
            }
            val = (sext10(n) * qt[k] as i32 * q_scale + 4) / 8;
        }
        self.idct(&mut blk);
        Some(blk)
    }

    /// IDCT: first pass at full precision, second rounds once ((sum >> 32) +
    /// bit 31), wraps to 9 bits signed and clamps to -128..127.
    /// NOT bit-exact: the hardware's internal rounding points are unknown
    /// (PCSX-Redux documents the same). Of the variants measured against the
    /// ps1-tests mdec/4bit + mdec/8bit hardware dumps this is the closest:
    /// 32 of 128 bytes differ, each by 1-2 LSB (invisible in video).
    fn idct(&self, blk: &mut [i32; 64]) {
        let mut tmp = [0i64; 64];
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for u in 0..8 {
                    sum += blk[u * 8 + x] as i64 * self.scale[u * 8 + y] as i64;
                }
                tmp[x + y * 8] = sum;
            }
        }
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for u in 0..8 {
                    sum += tmp[u + y * 8] * self.scale[u * 8 + x] as i64;
                }
                let v = (sum >> 32) + ((sum >> 31) & 1);
                let v9 = ((v as i32) << 23) >> 23;
                blk[x + y * 8] = v9.clamp(-128, 127);
            }
        }
    }

    fn decode(&mut self, src: &[u16]) {
        let mut pos = 0usize;
        if self.depth < 2 {
            // Monochrome: one Y block per 8x8 output
            loop {
                let qt = self.qt_y;
                let Some(y) = self.decode_block(src, &mut pos, &qt) else { break };
                let pixels: Vec<u8> = y
                    .iter()
                    .map(|&v| {
                        let c = v.clamp(-128, 127);
                        (if self.signed { c } else { c + 128 }) as u8
                    })
                    .collect();
                let mut words = vec![];
                if self.depth == 1 {
                    for chunk in pixels.chunks(4) {
                        words.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }
                } else {
                    for chunk in pixels.chunks(8) {
                        let mut w = 0u32;
                        for (i, &p) in chunk.iter().enumerate() {
                            w |= ((p >> 4) as u32) << (4 * i);
                        }
                        words.push(w);
                    }
                }
                self.push_words(words.clone(), words);
            }
            return;
        }
        loop {
            let (qy, quv) = (self.qt_y, self.qt_uv);
            let Some(cr) = self.decode_block(src, &mut pos, &quv) else { break };
            let Some(cb) = self.decode_block(src, &mut pos, &quv) else { break };
            let mut ys = [[0i32; 64]; 4];
            let mut complete = true;
            for y in ys.iter_mut() {
                match self.decode_block(src, &mut pos, &qy) {
                    Some(b) => *y = b,
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete {
                break;
            }
            let mut rgb = [[0u8; 3]; 256];
            for (bi, yb) in ys.iter().enumerate() {
                let (xx, yy) = ((bi & 1) * 8, (bi >> 1) * 8);
                for y in 0..8 {
                    for x in 0..8 {
                        let ci = ((x + xx) / 2) + ((y + yy) / 2) * 8;
                        let r0 = cr[ci];
                        let b0 = cb[ci];
                        let g = (-3437 * b0 - 7143 * r0) / 10000;
                        let r = (14020 * r0) / 10000;
                        let b = (17720 * b0) / 10000;
                        let lum = yb[x + y * 8];
                        let conv = |c: i32| -> u8 {
                            let c = (lum + c).clamp(-128, 127);
                            (if self.signed { c } else { c + 128 }) as u8
                        };
                        rgb[(x + xx) + (y + yy) * 16] = [conv(r), conv(g), conv(b)];
                    }
                }
            }
            // Block order: Y1 (top-left), Y2 (top-right), Y3, Y4, each 8x8.
            let mut blocked = Vec::with_capacity(256);
            for bi in 0..4 {
                let (xx, yy) = ((bi & 1) * 8, (bi >> 1) * 8);
                for y in 0..8 {
                    for x in 0..8 {
                        blocked.push(rgb[(xx + x) + (yy + y) * 16]);
                    }
                }
            }
            let pack = |px: &[[u8; 3]]| -> Vec<u32> {
                if self.depth == 3 {
                    let m = (self.bit15 as u32) << 15;
                    let p = |c: &[u8; 3]| -> u32 {
                        ((c[0] >> 3) as u32) | (((c[1] >> 3) as u32) << 5) | (((c[2] >> 3) as u32) << 10) | m
                    };
                    px.chunks(2).map(|pair| p(&pair[0]) | (p(&pair[1]) << 16)).collect()
                } else {
                    let flat: Vec<u8> = px.iter().flat_map(|c| c.iter().copied()).collect();
                    flat.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
                }
            };
            let (b, r) = (pack(&blocked), pack(&rgb));
            self.push_words(b, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard scale table uploaded by the PsyQ libraries.
    pub const STD_SCALE: [i16; 64] = [
        0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x7D8A, 0x6A6D, 0x471C, 0x18F8, -0x18F8,
        -0x471C, -0x6A6D, -0x7D8A, 0x7641, 0x30FB, -0x30FB, -0x7641, -0x7641, -0x30FB, 0x30FB, 0x7641, 0x6A6D,
        -0x18F8, -0x7D8A, -0x471C, 0x471C, 0x7D8A, 0x18F8, -0x6A6D, 0x5A82, -0x5A82, -0x5A82, 0x5A82, 0x5A82,
        -0x5A82, -0x5A82, 0x5A82, 0x471C, -0x7D8A, 0x18F8, 0x6A6D, -0x6A6D, -0x18F8, 0x7D8A, -0x471C, 0x30FB,
        -0x7641, 0x7641, -0x30FB, -0x30FB, 0x7641, -0x7641, 0x30FB, 0x18F8, -0x471C, 0x6A6D, -0x7D8A, 0x7D8A,
        -0x6A6D, 0x471C, -0x18F8,
    ];

    fn setup(m: &mut Mdec) {
        m.write_command(0x6000_0000); // scale table
        for k in 0..32 {
            let lo = STD_SCALE[2 * k] as u16 as u32;
            let hi = STD_SCALE[2 * k + 1] as u16 as u32;
            m.write_command(lo | (hi << 16));
        }
        m.write_command(0x4000_0001); // quant table, luma + chroma
        for _ in 0..32 {
            m.write_command(0x0101_0101);
        }
    }

    fn stream(halfs: &[u16]) -> Vec<u32> {
        let mut h = halfs.to_vec();
        if h.len() % 2 == 1 {
            h.push(0xFE00);
        }
        h.chunks(2).map(|p| p[0] as u32 | ((p[1] as u32) << 16)).collect()
    }

    #[test]
    fn table_uploads_consume_correct_word_counts() {
        let mut m = Mdec::new();
        setup(&mut m);
        assert_eq!(m.status() & (1 << 29), 0, "idle after both uploads");
        assert_eq!(m.scale[9], 0x6A6D);
        assert_eq!(m.qt_uv[63], 1);
        // luma-only quant table is 16 words
        m.write_command(0x4000_0000);
        for _ in 0..15 {
            m.write_command(0x0202_0202);
        }
        assert_ne!(m.status() & (1 << 29), 0, "still busy");
        m.write_command(0x0202_0202);
        assert_eq!(m.status() & (1 << 29), 0);
        assert_eq!(m.qt_y[0], 2);
        assert_eq!(m.qt_uv[0], 1, "chroma table untouched");
    }

    #[test]
    fn idct_dc_only_gives_flat_block() {
        let mut m = Mdec::new();
        setup(&mut m);
        // DC = 256 with q_scale 1 -> flat 256/8 = 32
        let data = stream(&[(1 << 10) | 0x100, 0xFE00]);
        m.write_command(0x2800_0000 | (1 << 26) | data.len() as u32); // 8-bit, signed
        for w in &data {
            m.write_command(*w);
        }
        assert_eq!(m.output_len(), 16);
        for _ in 0..16 {
            assert_eq!(m.read_data(), 0x2020_2020);
        }
    }

    #[test]
    fn mono_unsigned_and_4bit() {
        let mut m = Mdec::new();
        setup(&mut m);
        let data = stream(&[(1 << 10) | 0x100, 0xFE00]);
        m.write_command(0x2000_0000 | data.len() as u32); // 4-bit unsigned
        for w in &data {
            m.write_command(*w);
        }
        assert_eq!(m.output_len(), 8);
        // 32 + 128 = 160 -> >>4 = 0xA
        assert_eq!(m.read_data(), 0xAAAA_AAAA);
    }

    #[test]
    fn color_15bit_gray_macroblock() {
        let mut m = Mdec::new();
        setup(&mut m);
        let mut h = vec![];
        h.extend([(1 << 10), 0xFE00]); // Cr = 0
        h.extend([(1 << 10), 0xFE00]); // Cb = 0
        for _ in 0..4 {
            h.extend([(1 << 10) | 0x100, 0xFE00]); // Y DC 256 -> 32
        }
        let data = stream(&h);
        m.write_command(0x3800_0000 | (1 << 25) | data.len() as u32); // depth 3 = 15-bit, unsigned, bit15
        for w in &data {
            m.write_command(*w);
        }
        assert_eq!(m.output_len(), 128);
        let c = (160u32 >> 3) as u32;
        let px = c | (c << 5) | (c << 10) | 0x8000;
        assert_eq!(m.read_data(), px | (px << 16));
        assert_ne!(m.status() & (1 << 31), 1 << 31, "output not empty");
    }

    #[test]
    fn color_24bit_and_red_chroma() {
        let mut m = Mdec::new();
        setup(&mut m);
        let mut h = vec![];
        h.extend([(1 << 10) | 0x200 | 0x080, 0xFE00]); // Cr DC negative? 0x280 -> sext10 = -384
        h.extend([(1 << 10), 0xFE00]);
        for _ in 0..4 {
            h.extend([(1 << 10), 0xFE00]);
        }
        let data = stream(&h);
        m.write_command(0x3000_0000 | data.len() as u32); // depth 2 = 24-bit
        for w in &data {
            m.write_command(*w);
        }
        assert_eq!(m.output_len(), 192, "256 px * 3 bytes / 4");
        let w = m.read_data().to_le_bytes();
        // Cr = -384/8 = -48: R = 128 - 67 = 61, G = 128 + 34 = 162, B = 128
        assert!((w[0] as i32 - 61).abs() <= 1, "R {}", w[0]);
        assert!((w[1] as i32 - 162).abs() <= 1, "G {}", w[1]);
        assert_eq!(w[2], 128);
    }

    #[test]
    fn ac_coefficients_use_zigzag_and_quant() {
        let mut m = Mdec::new();
        setup(&mut m);
        // DC 0, then run 0 -> k=1 (natural index 1), value 64 * qt(1) * qscale(8) /8 = 64
        let data = stream(&[(8 << 10), 64, 0xFE00]);
        m.write_command(0x2800_0000 | (1 << 26) | data.len() as u32);
        for w in &data {
            m.write_command(*w);
        }
        let row0: Vec<i8> = (0..2).flat_map(|_| m.read_data().to_le_bytes()).map(|b| b as i8).collect();
        // horizontal cosine: left side positive, right side negative, antisymmetric
        assert!(row0[0] > 0 && row0[7] < 0, "{:?}", row0);
        assert_eq!(row0[0], -row0[7]);
    }

    #[test]
    fn block_ends_at_coefficient_63_without_eob() {
        // Block A: DC, then a run landing exactly on k=63 (no 0xFE00).
        // The next halfword must start block B (as on hardware).
        let mut m = Mdec::new();
        setup(&mut m);
        let data = stream(&[
            (1 << 10) | 0x100, 62 << 10, // A: DC 256, run 62 -> k=63 value 0
            (1 << 10) | 0x080, 0xFE00,   // B: DC 128
        ]);
        m.write_command(0x2800_0000 | (1 << 26) | data.len() as u32);
        for w in &data {
            m.write_command(*w);
        }
        assert_eq!(m.output_len(), 32, "two 8x8 blocks decoded");
        assert_eq!(m.read_data(), 0x2020_2020, "block A = 256/8");
        for _ in 0..15 {
            m.read_data();
        }
        assert_eq!(m.read_data(), 0x1010_1010, "block B = 128/8");
    }

    #[test]
    fn register_reads_block_order_dma_reads_row_major() {
        let mut m = Mdec::new();
        setup(&mut m);
        // Y1 bright, Y2..Y4 dark: rows of the macroblock differ by half
        let mut h = vec![(1 << 10), 0xFE00, (1 << 10), 0xFE00];
        h.extend([(1 << 10) | 0x1F0, 0xFE00]); // Y1 DC 496 -> 62
        for _ in 0..3 {
            h.extend([(1 << 10) | 0x3C0, 0xFE00]); // DC -64 -> -8
        }
        let data = stream(&h);
        let decode = |m: &mut Mdec| {
            m.write_command(0x3800_0000 | (1 << 26) | data.len() as u32); // 15-bit signed
            for w in &data {
                m.write_command(*w);
            }
        };
        decode(&mut m);
        let reg: Vec<u32> = (0..128).map(|_| m.read_data()).collect();
        decode(&mut m);
        let dma: Vec<u32> = (0..128).map(|_| m.read_dma()).collect();
        // register: first 32 words are all of Y1 (8x8); DMA: words 0..4 are
        // Y1's first row, words 4..8 already Y2's first row
        assert!(reg[..32].iter().all(|&w| w == reg[0]));
        assert_eq!(dma[..4], reg[..4]);
        assert_ne!(dma[4], reg[4], "row-major switches to Y2 after 8 pixels");
        assert_eq!(dma[4], reg[32], "Y2 first row");
    }

    #[test]
    fn reset_and_status() {
        let mut m = Mdec::new();
        m.write_control(0x6000_0000);
        assert!(m.dma_in_enabled && m.dma_out_enabled);
        m.write_command(0x3000_0010);
        assert_ne!(m.status() & (1 << 28), 0, "data-in request while expecting params");
        assert_eq!(m.status() & 0xFFFF, 0xF);
        m.write_control(0x8000_0000);
        assert_eq!(m.status() & (1 << 29), 0);
        assert_eq!(m.status() & 0xFFFF, 0xFFFF);
    }
}

impl Mdec {
    pub fn snap(&mut self, s: &mut crate::state::Snap) {
        s.tag("MDEC");
        let mut st: u8 = match self.state {
            State::Idle => 0,
            State::Decode => 1,
            State::Quant { color: false } => 2,
            State::Quant { color: true } => 3,
            State::Scale => 4,
        };
        s.u8(&mut st);
        self.state = match st {
            1 => State::Decode,
            2 => State::Quant { color: false },
            3 => State::Quant { color: true },
            4 => State::Scale,
            _ => State::Idle,
        };
        s.u32(&mut self.remaining);
        s.vec_u32(&mut self.params);
        s.u32(&mut self.depth);
        s.bool(&mut self.signed);
        s.bool(&mut self.bit15);
        s.buf(&mut self.qt_y);
        s.buf(&mut self.qt_uv);
        s.arr_i16(&mut self.scale);
        let mut n = self.out.len() as u32;
        s.u32(&mut n);
        if s.is_loading() {
            self.out.clear();
            for _ in 0..n.min(1 << 16) {
                self.out.push_back(OutChunk { block_order: vec![], row_major: vec![], pos: 0 });
            }
        }
        for c in self.out.iter_mut() {
            s.vec_u32(&mut c.block_order);
            s.vec_u32(&mut c.row_major);
            s.usize(&mut c.pos);
            if c.pos > c.block_order.len() || c.row_major.len() != c.block_order.len() {
                s.error.get_or_insert("bad MDEC output chunk".into());
                c.pos = c.block_order.len();
            }
        }
        if s.is_loading() {
            self.out.retain(|c| c.pos < c.block_order.len());
        }
        s.bool(&mut self.dma_in_enabled);
        s.bool(&mut self.dma_out_enabled);
        s.u32(&mut self.current_block);
    }
}
