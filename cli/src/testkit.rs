//! Builders for self-made test content: a GPU demo PS-X EXE and a minimal
//! bootable ISO9660 disc (raw Mode 2). No copyrighted material involved.

fn lui(rt: u32, imm: u32) -> u32 { (0x0F << 26) | (rt << 16) | (imm & 0xFFFF) }
fn ori(rt: u32, rs: u32, imm: u32) -> u32 { (0x0D << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF) }
fn sw(rt: u32, off: u32, rs: u32) -> u32 { (0x2B << 26) | (rs << 21) | (rt << 16) | (off & 0xFFFF) }
fn j(t: u32) -> u32 { (0x02 << 26) | ((t >> 2) & 0x03FF_FFFF) }

/// Expected display pixels of `demo_exe` as ((x, y), 0xRRGGBB).
pub const DEMO_PIXELS: [((usize, usize), u32); 5] = [
    ((300, 10), 0x000084),  // background
    ((160, 30), 0xF70000),  // triangle: red vertex
    ((50, 214), 0x08F700),  // green vertex
    ((270, 214), 0x0800F7), // blue vertex
    ((12, 12), 0xFFFF00),   // yellow rectangle
];

/// A PS-X EXE (no BIOS calls) that sets up a 320x240 display and draws a
/// background, a gouraud triangle and a rectangle, then loops.
pub fn demo_exe() -> Vec<u8> {
    const T0: u32 = 8;
    const T1: u32 = 9;
    let base = 0x8001_0000u32;
    let mut code = vec![lui(T1, 0x1F80)];
    let gp = |code: &mut Vec<u32>, port: u32, w: u32| {
        code.push(lui(T0, w >> 16));
        code.push(ori(T0, T0, w & 0xFFFF));
        code.push(sw(T0, port, T1));
    };
    for w in [0x0000_0000, 0x0800_0001, 0x0500_0000, 0x0600_0000 | 0x260 | (0xC60 << 12), 0x0700_0000 | 0x10 | (0x100 << 10), 0x0300_0000] {
        gp(&mut code, 0x1814, w);
    }
    for w in [
        0xE300_0000, 0xE400_0000 | 319 | (239 << 10), 0xE500_0000,
        0x0280_0000, 0x0000_0000, 0x00F0_0140,
        0x3000_00FF, 0x0014_00A0, 0x0000_FF00, 0x00DC_0028, 0x00FF_0000, 0x00DC_0118,
        0x6000_FFFF, 0x0008_0008, 0x0010_0010,
    ] {
        gp(&mut code, 0x1810, w);
    }
    let here = base + 4 * code.len() as u32;
    code.push(j(here));
    code.push(0);

    let mut exe = vec![0u8; 0x800];
    exe[0..8].copy_from_slice(b"PS-X EXE");
    let put = |exe: &mut Vec<u8>, off: usize, v: u32| exe[off..off + 4].copy_from_slice(&v.to_le_bytes());
    // Body size must be a multiple of 2048: BIOS loaders read size/2048 sectors.
    let body = ((code.len() * 4 + 2047) / 2048) * 2048;
    put(&mut exe, 0x10, base);
    put(&mut exe, 0x18, base);
    put(&mut exe, 0x1C, body as u32);
    put(&mut exe, 0x30, 0x801F_FF00);
    for w in &code {
        exe.extend_from_slice(&w.to_le_bytes());
    }
    exe.resize(0x800 + body, 0);
    exe
}

const SECTOR: usize = 2048;

fn both16(v: u16) -> [u8; 4] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], b[0], b[1]]
}
fn both32(v: u32) -> [u8; 8] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

fn dir_record(name: &[u8], lba: u32, size: u32, dir: bool) -> Vec<u8> {
    let mut r = vec![0u8; 33];
    r[2..10].copy_from_slice(&both32(lba));
    r[10..18].copy_from_slice(&both32(size));
    r[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    r[25] = if dir { 2 } else { 0 };
    r[28..32].copy_from_slice(&both16(1));
    r[32] = name.len() as u8;
    r.extend_from_slice(name);
    if r.len() % 2 == 1 {
        r.push(0);
    }
    r[0] = r.len() as u8;
    r
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Raw (2352-byte Mode 2 Form 1) disc image with SYSTEM.CNF + PSX.EXE.
pub fn bootable_disc(exe: &[u8]) -> Vec<u8> {
    let cnf = b"BOOT = cdrom:\\PSX.EXE;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFF00\r\n";
    let (cnf_lba, exe_lba) = (21u32, 22u32);
    let total = exe_lba as usize + (exe.len() + SECTOR - 1) / SECTOR;
    let mut img = vec![0u8; total * SECTOR];

    let lic = b"          Licensed  by          Sony Computer Entertainment Amer  ica ";
    img[4 * SECTOR..4 * SECTOR + lic.len()].copy_from_slice(lic);

    let mut root = vec![];
    root.extend(dir_record(&[0], 20, SECTOR as u32, true));
    root.extend(dir_record(&[1], 20, SECTOR as u32, true));
    root.extend(dir_record(b"PSX.EXE;1", exe_lba, exe.len() as u32, false));
    root.extend(dir_record(b"SYSTEM.CNF;1", cnf_lba, cnf.len() as u32, false));
    img[20 * SECTOR..20 * SECTOR + root.len()].copy_from_slice(&root);

    img[18 * SECTOR..18 * SECTOR + 10].copy_from_slice(&[1, 0, 20, 0, 0, 0, 1, 0, 0, 0]);
    img[19 * SECTOR..19 * SECTOR + 10].copy_from_slice(&[1, 0, 0, 0, 0, 20, 0, 1, 0, 0]);

    let pvd = &mut img[16 * SECTOR..17 * SECTOR];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[8..40].fill(b' ');
    pvd[8..19].copy_from_slice(b"PLAYSTATION");
    pvd[40..72].fill(b' ');
    pvd[40..48].copy_from_slice(b"PSX_TEST");
    pvd[80..88].copy_from_slice(&both32(total as u32));
    pvd[120..124].copy_from_slice(&both16(1));
    pvd[124..128].copy_from_slice(&both16(1));
    pvd[128..132].copy_from_slice(&both16(SECTOR as u16));
    pvd[132..140].copy_from_slice(&both32(10));
    pvd[140..144].copy_from_slice(&18u32.to_le_bytes());
    pvd[148..152].copy_from_slice(&19u32.to_be_bytes());
    pvd[156..190].copy_from_slice(&dir_record(&[0], 20, SECTOR as u32, true)[..34]);
    pvd[881] = 1;
    let term = &mut img[17 * SECTOR..18 * SECTOR];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;

    img[cnf_lba as usize * SECTOR..][..cnf.len()].copy_from_slice(cnf);
    img[exe_lba as usize * SECTOR..][..exe.len()].copy_from_slice(exe);

    let mut raw = Vec::with_capacity(total * 2352);
    for lba in 0..total {
        let mut s = vec![0u8; 2352];
        s[1..11].fill(0xFF);
        let a = lba as u32 + 150;
        s[12] = bcd(a / 4500);
        s[13] = bcd((a / 75) % 60);
        s[14] = bcd(a % 75);
        s[15] = 2;
        let sub = if lba == 16 || lba == 17 { 0x09 } else { 0x08 };
        s[16..24].copy_from_slice(&[0, 0, sub, 0, 0, 0, sub, 0]);
        s[24..24 + SECTOR].copy_from_slice(&img[lba * SECTOR..(lba + 1) * SECTOR]);
        raw.extend_from_slice(&s);
    }
    raw
}

// ---------------------------------------------------------------------------
// Random CPU program generator for differential testing against other
// emulators. Programs avoid traps (no ADD/SUB/ADDI, aligned accesses only),
// then dump r1..r25, HI, LO and a 64-word scratch buffer as hex lines via
// BIOS putchar A(3Ch), ending with "END".

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

fn r_type(funct: u32, rs: u32, rt: u32, rd: u32, sh: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sh << 6) | funct
}
fn i_type(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF)
}

const SCRATCH: u32 = 0x8010_0000;
const DUMP: u32 = 0x8010_1000;
const BASE_REG: u32 = 28;

fn random_alu(rng: &mut Rng) -> u32 {
    let rd = 1 + rng.below(25);
    let rs = rng.below(26);
    let rt = rng.below(26);
    let imm = rng.next() & 0xFFFF;
    match rng.below(22) {
        0 => r_type(0x21, rs, rt, rd, 0), // addu
        1 => r_type(0x23, rs, rt, rd, 0), // subu
        2 => r_type(0x24, rs, rt, rd, 0),
        3 => r_type(0x25, rs, rt, rd, 0),
        4 => r_type(0x26, rs, rt, rd, 0),
        5 => r_type(0x27, rs, rt, rd, 0),
        6 => r_type(0x2A, rs, rt, rd, 0), // slt
        7 => r_type(0x2B, rs, rt, rd, 0), // sltu
        8 => r_type(0x00, 0, rt, rd, rng.below(32)), // sll
        9 => r_type(0x02, 0, rt, rd, rng.below(32)),
        10 => r_type(0x03, 0, rt, rd, rng.below(32)),
        11 => r_type(0x04, rs, rt, rd, 0), // sllv
        12 => r_type(0x06, rs, rt, rd, 0),
        13 => r_type(0x07, rs, rt, rd, 0),
        14 => i_type(0x09, rs, rd, imm), // addiu
        15 => i_type(0x0A, rs, rd, imm), // slti
        16 => i_type(0x0B, rs, rd, imm), // sltiu
        17 => i_type(0x0C, rs, rd, imm),
        18 => i_type(0x0D, rs, rd, imm),
        19 => i_type(0x0E, rs, rd, imm),
        20 => i_type(0x0F, 0, rd, imm), // lui
        _ => r_type(0x10 + 2 * rng.below(2), 0, 0, rd, 0), // mfhi / mflo
    }
}

fn random_op(rng: &mut Rng, code: &mut Vec<u32>) {
    let rd = 1 + rng.below(25);
    let rs = rng.below(26);
    let rt = rng.below(26);
    match rng.below(10) {
        0..=4 => code.push(random_alu(rng)),
        5 => {
            // mult/multu/div/divu/mthi/mtlo
            let f = [0x18, 0x19, 0x1A, 0x1B][rng.below(4) as usize];
            if rng.below(6) == 0 {
                code.push(r_type(0x11 + 2 * rng.below(2), rs, 0, 0, 0));
            } else {
                code.push(r_type(f, rs, rt, 0, 0));
            }
        }
        6 => {
            // aligned loads incl. unaligned-safe lwl/lwr
            let (op, align) = [(0x20, 1), (0x24, 1), (0x21, 2), (0x25, 2), (0x23, 4), (0x22, 1), (0x26, 1)][rng.below(7) as usize];
            let off = (rng.below(256 / align) * align) as u32;
            code.push(i_type(op, BASE_REG, rd, off));
        }
        7 => {
            let (op, align) = [(0x28, 1), (0x29, 2), (0x2B, 4), (0x2A, 1), (0x2E, 1)][rng.below(5) as usize];
            let off = (rng.below(256 / align) * align) as u32;
            code.push(i_type(op, BASE_REG, rt, off));
        }
        8 => {
            // forward branch over 1..4 random ALU ops, with an ALU delay slot
            let skip = 1 + rng.below(4);
            let br = match rng.below(6) {
                0 => i_type(0x04, rs, rt, skip),
                1 => i_type(0x05, rs, rt, skip),
                2 => i_type(0x06, rs, 0, skip),
                3 => i_type(0x07, rs, 0, skip),
                4 => i_type(0x01, rs, 0, skip),
                _ => i_type(0x01, rs, 1, skip),
            };
            code.push(br);
            code.push(random_alu(rng)); // delay slot
            for _ in 0..skip {
                code.push(random_alu(rng));
            }
        }
        _ => {
            // load-use hazard: load immediately followed by a use of rd
            let off = rng.below(64) * 4;
            code.push(i_type(0x23, BASE_REG, rd, off));
            code.push(r_type(0x21, rd, rt, 1 + rng.below(25), 0));
        }
    }
}

pub fn fuzz_cpu_exe(seed: u64, ops: usize) -> Vec<u8> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let base = 0x8001_0000u32;
    let mut code: Vec<u32> = vec![];
    let li = |code: &mut Vec<u32>, r: u32, v: u32| {
        code.push(i_type(0x0F, 0, r, v >> 16));
        code.push(i_type(0x0D, r, r, v & 0xFFFF));
    };
    // scratch buffer: 64 random words
    li(&mut code, BASE_REG, SCRATCH);
    for k in 0..64 {
        li(&mut code, 1, rng.next() ^ (rng.next() << 16));
        code.push(i_type(0x2B, BASE_REG, 1, k * 4));
    }
    for r in 1..=25 {
        li(&mut code, r, rng.next() ^ (rng.next() << 16));
    }
    for _ in 0..ops {
        random_op(&mut rng, &mut code);
    }
    code.push(0);
    code.push(0);
    // dump registers r1..r25, hi, lo
    li(&mut code, 27, DUMP);
    for r in 1..=25 {
        code.push(i_type(0x2B, 27, r, (r - 1) * 4));
    }
    code.push(r_type(0x10, 0, 0, 1, 0)); // mfhi r1
    code.push(i_type(0x2B, 27, 1, 25 * 4));
    code.push(r_type(0x12, 0, 0, 1, 0)); // mflo r1
    code.push(i_type(0x2B, 27, 1, 26 * 4));
    // copy scratch after it
    for k in 0..64 {
        code.push(i_type(0x23, BASE_REG, 1, k * 4));
        code.push(0);
        code.push(i_type(0x2B, 27, 1, (27 + k) * 4));
    }
    // print loop: s0 = ptr, s1 = count; putchar(a0) via t1=0x3C, jal 0xA0
    let (s0, s1, s2, s3, a0, t0, t1) = (16, 17, 18, 19, 4, 8, 9);
    li(&mut code, s0, DUMP);
    li(&mut code, s1, 27 + 64);
    let putchar = |code: &mut Vec<u32>| {
        code.push(i_type(0x09, 0, t1, 0x3C));
        code.push((0x03 << 26) | (0xA0 >> 2)); // jal 0xA0
        code.push(0);
    };
    let outer = code.len();
    code.push(i_type(0x23, s0, s2, 0)); // lw s2,0(s0)
    code.push(i_type(0x09, 0, s3, 8));
    code.push(0);
    let inner = code.len();
    code.push(r_type(0x02, 0, s2, a0, 28)); // srl a0,s2,28
    code.push(i_type(0x0B, a0, t0, 10)); // sltiu t0,a0,10
    code.push(i_type(0x09, a0, a0, 0x30)); // addiu a0,a0,'0'
    code.push(i_type(0x05, t0, 0, 2)); // bne t0,zero,+2
    code.push(0);
    code.push(i_type(0x09, a0, a0, 7)); // to 'A'..'F'
    putchar(&mut code);
    code.push(r_type(0x00, 0, s2, s2, 4)); // sll s2,s2,4
    code.push(i_type(0x09, s3, s3, 0xFFFF)); // addiu s3,-1
    let back = (inner as i32 - (code.len() as i32 + 1)) as u32;
    code.push(i_type(0x05, s3, 0, back));
    code.push(0);
    code.push(i_type(0x09, 0, a0, b'\n' as u32));
    putchar(&mut code);
    code.push(i_type(0x09, s0, s0, 4));
    code.push(i_type(0x09, s1, s1, 0xFFFF));
    let back = (outer as i32 - (code.len() as i32 + 1)) as u32;
    code.push(i_type(0x05, s1, 0, back));
    code.push(0);
    for ch in b"END\n" {
        code.push(i_type(0x09, 0, a0, *ch as u32));
        putchar(&mut code);
    }
    let here = base + 4 * code.len() as u32;
    code.push((0x02 << 26) | ((here >> 2) & 0x03FF_FFFF));
    code.push(0);

    let mut exe = vec![0u8; 0x800];
    exe[0..8].copy_from_slice(b"PS-X EXE");
    let body = ((code.len() * 4 + 2047) / 2048) * 2048;
    exe[0x10..0x14].copy_from_slice(&base.to_le_bytes());
    exe[0x18..0x1C].copy_from_slice(&base.to_le_bytes());
    exe[0x1C..0x20].copy_from_slice(&(body as u32).to_le_bytes());
    exe[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
    for w in &code {
        exe.extend_from_slice(&w.to_le_bytes());
    }
    exe.resize(0x800 + body, 0);
    exe
}

/// Worst-case CPU load for performance checks: enables the I-cache and runs
/// a tight ALU loop forever (every cycle is an instruction).
pub fn alu_bench_exe() -> Vec<u8> {
    let base = 0x8001_0000u32;
    let code: Vec<u32> = vec![
        0x3C08_FFFE, // lui t0,0xFFFE
        0x3409_0800, // ori t1,zero,0x800
        0xAD09_0130, // sw t1,0x130(t0)   cache control: I-cache on
        0x2508_0001, // loop: addiu t0,t0,1
        0x2529_0003, // addiu t1,t1,3
        0x0109_5021, // addu t2,t0,t1
        0x0800_0000 | ((base + 12) >> 2) & 0x03FF_FFFF, // j loop
        0,
    ];
    let mut exe = vec![0u8; 0x800];
    exe[0..8].copy_from_slice(b"PS-X EXE");
    exe[0x10..0x14].copy_from_slice(&base.to_le_bytes());
    exe[0x18..0x1C].copy_from_slice(&base.to_le_bytes());
    exe[0x1C..0x20].copy_from_slice(&2048u32.to_le_bytes());
    exe[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
    for w in &code {
        exe.extend_from_slice(&w.to_le_bytes());
    }
    exe.resize(0x800 + 2048, 0);
    exe
}
