//! R3000A conformance tests: hand-assembled programs run on the real
//! interpreter, checking architectural results (incl. hazards & exceptions).

use ps1wasm_core::bus::Size;
use ps1wasm_core::cpu::{Exception, SR_BEV, SR_ISC};
use ps1wasm_core::irq::Interrupt;
use ps1wasm_core::Psx;

// ---- tiny assembler -------------------------------------------------------
fn r(funct: u32, rs: u32, rt: u32, rd: u32, sh: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sh << 6) | funct
}
fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm as u32 & 0xFFFF)
}
const NOP: u32 = 0;
fn sll(rd: u32, rt: u32, sh: u32) -> u32 { r(0x00, 0, rt, rd, sh) }
fn srl(rd: u32, rt: u32, sh: u32) -> u32 { r(0x02, 0, rt, rd, sh) }
fn sra(rd: u32, rt: u32, sh: u32) -> u32 { r(0x03, 0, rt, rd, sh) }
fn sllv(rd: u32, rt: u32, rs: u32) -> u32 { r(0x04, rs, rt, rd, 0) }
fn srav(rd: u32, rt: u32, rs: u32) -> u32 { r(0x07, rs, rt, rd, 0) }
fn jr(rs: u32) -> u32 { r(0x08, rs, 0, 0, 0) }
fn jalr(rd: u32, rs: u32) -> u32 { r(0x09, rs, 0, rd, 0) }
fn syscall() -> u32 { 0x0C }
fn brk() -> u32 { 0x0D }
fn mfhi(rd: u32) -> u32 { r(0x10, 0, 0, rd, 0) }
fn mflo(rd: u32) -> u32 { r(0x12, 0, 0, rd, 0) }
fn mthi(rs: u32) -> u32 { r(0x11, rs, 0, 0, 0) }
fn mult(rs: u32, rt: u32) -> u32 { r(0x18, rs, rt, 0, 0) }
fn multu(rs: u32, rt: u32) -> u32 { r(0x19, rs, rt, 0, 0) }
fn div(rs: u32, rt: u32) -> u32 { r(0x1A, rs, rt, 0, 0) }
fn divu(rs: u32, rt: u32) -> u32 { r(0x1B, rs, rt, 0, 0) }
fn add(rd: u32, rs: u32, rt: u32) -> u32 { r(0x20, rs, rt, rd, 0) }
fn addu(rd: u32, rs: u32, rt: u32) -> u32 { r(0x21, rs, rt, rd, 0) }
fn sub(rd: u32, rs: u32, rt: u32) -> u32 { r(0x22, rs, rt, rd, 0) }
fn subu(rd: u32, rs: u32, rt: u32) -> u32 { r(0x23, rs, rt, rd, 0) }
fn and(rd: u32, rs: u32, rt: u32) -> u32 { r(0x24, rs, rt, rd, 0) }
fn or(rd: u32, rs: u32, rt: u32) -> u32 { r(0x25, rs, rt, rd, 0) }
fn xor(rd: u32, rs: u32, rt: u32) -> u32 { r(0x26, rs, rt, rd, 0) }
fn nor(rd: u32, rs: u32, rt: u32) -> u32 { r(0x27, rs, rt, rd, 0) }
fn slt(rd: u32, rs: u32, rt: u32) -> u32 { r(0x2A, rs, rt, rd, 0) }
fn sltu(rd: u32, rs: u32, rt: u32) -> u32 { r(0x2B, rs, rt, rd, 0) }
fn bltz(rs: u32, off: i32) -> u32 { i(0x01, rs, 0x00, off) }
fn bgez(rs: u32, off: i32) -> u32 { i(0x01, rs, 0x01, off) }
fn bltzal(rs: u32, off: i32) -> u32 { i(0x01, rs, 0x10, off) }
fn j(target: u32) -> u32 { (0x02 << 26) | ((target >> 2) & 0x03FF_FFFF) }
fn jal(target: u32) -> u32 { (0x03 << 26) | ((target >> 2) & 0x03FF_FFFF) }
fn beq(rs: u32, rt: u32, off: i32) -> u32 { i(0x04, rs, rt, off) }
fn bne(rs: u32, rt: u32, off: i32) -> u32 { i(0x05, rs, rt, off) }
fn blez(rs: u32, off: i32) -> u32 { i(0x06, rs, 0, off) }
fn bgtz(rs: u32, off: i32) -> u32 { i(0x07, rs, 0, off) }
fn addi(rt: u32, rs: u32, imm: i32) -> u32 { i(0x08, rs, rt, imm) }
fn addiu(rt: u32, rs: u32, imm: i32) -> u32 { i(0x09, rs, rt, imm) }
fn slti(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0A, rs, rt, imm) }
fn sltiu(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0B, rs, rt, imm) }
fn andi(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0C, rs, rt, imm) }
fn ori(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0D, rs, rt, imm) }
fn xori(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0E, rs, rt, imm) }
fn lui(rt: u32, imm: u32) -> u32 { i(0x0F, 0, rt, imm as i32) }
fn mfc0(rt: u32, rd: u32) -> u32 { (0x10 << 26) | (rt << 16) | (rd << 11) }
fn mtc0(rt: u32, rd: u32) -> u32 { (0x10 << 26) | (4 << 21) | (rt << 16) | (rd << 11) }
fn rfe() -> u32 { 0x4200_0010 }
fn lb(rt: u32, off: i32, rs: u32) -> u32 { i(0x20, rs, rt, off) }
fn lh(rt: u32, off: i32, rs: u32) -> u32 { i(0x21, rs, rt, off) }
fn lwl(rt: u32, off: i32, rs: u32) -> u32 { i(0x22, rs, rt, off) }
fn lw(rt: u32, off: i32, rs: u32) -> u32 { i(0x23, rs, rt, off) }
fn lbu(rt: u32, off: i32, rs: u32) -> u32 { i(0x24, rs, rt, off) }
fn lhu(rt: u32, off: i32, rs: u32) -> u32 { i(0x25, rs, rt, off) }
fn lwr(rt: u32, off: i32, rs: u32) -> u32 { i(0x26, rs, rt, off) }
fn sb(rt: u32, off: i32, rs: u32) -> u32 { i(0x28, rs, rt, off) }
fn sh(rt: u32, off: i32, rs: u32) -> u32 { i(0x29, rs, rt, off) }
fn swl(rt: u32, off: i32, rs: u32) -> u32 { i(0x2A, rs, rt, off) }
fn sw(rt: u32, off: i32, rs: u32) -> u32 { i(0x2B, rs, rt, off) }
fn swr(rt: u32, off: i32, rs: u32) -> u32 { i(0x2E, rs, rt, off) }
/// li via lui+ori (always 2 instructions)
fn li(rt: u32, v: u32) -> [u32; 2] { [lui(rt, v >> 16), ori(rt, rt, (v & 0xFFFF) as i32)] }

// ---- harness --------------------------------------------------------------
const BASE: u32 = 0x8001_0000;
const HANDLER: u32 = 0x8000_0080;
const DATA: u32 = 0x8002_0000;

fn machine(prog: &[u32]) -> Psx {
    let mut p = Psx::new();
    for (k, w) in prog.iter().enumerate() {
        p.bus.write(BASE + 4 * k as u32, *w, Size::Word);
    }
    // Exception handler: spin forever (tests inspect COP0 afterwards).
    p.bus.write(HANDLER, j(HANDLER), Size::Word);
    p.bus.write(HANDLER + 4, NOP, Size::Word);
    p.cpu.cop0.sr = 0; // BEV=0 -> RAM vector
    p.cpu.set_pc(BASE);
    p
}

fn run(prog: &[u32], steps: u64) -> Psx {
    let mut p = machine(prog);
    p.run_instructions(steps);
    p
}

fn cat(parts: &[&[u32]]) -> Vec<u32> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

fn exc_code(p: &Psx) -> u32 {
    (p.cpu.cop0.cause >> 2) & 0x1F
}

// ---- tests ----------------------------------------------------------------
#[test]
fn reset_state() {
    let p = Psx::new();
    assert_eq!(p.cpu.pc, 0xBFC0_0000);
    assert_ne!(p.cpu.cop0.sr & SR_BEV, 0);
}

#[test]
fn r0_is_hardwired() {
    let p = run(&[ori(0, 0, 0x1234), addiu(1, 0, 0)], 2);
    assert_eq!(p.cpu.reg(0), 0);
    assert_eq!(p.cpu.reg(1), 0);
}

#[test]
fn alu_basic() {
    let prog = cat(&[
        &li(1, 0xF0F0_1234), &li(2, 0x0FF0_00FF),
        &[
            addu(3, 1, 2), subu(4, 1, 2), and(5, 1, 2), or(6, 1, 2),
            xor(7, 1, 2), nor(8, 1, 2), slt(9, 1, 2), sltu(10, 1, 2),
            andi(11, 1, 0xFFFF), xori(12, 1, 0xFFFF), slti(13, 1, -1), sltiu(14, 2, -1),
            sll(15, 2, 4), srl(16, 1, 4), sra(17, 1, 4),
        ],
    ]);
    let p = run(&prog, prog.len() as u64);
    let (a, b) = (0xF0F0_1234u32, 0x0FF0_00FFu32);
    assert_eq!(p.cpu.reg(3), a.wrapping_add(b));
    assert_eq!(p.cpu.reg(4), a.wrapping_sub(b));
    assert_eq!(p.cpu.reg(5), a & b);
    assert_eq!(p.cpu.reg(6), a | b);
    assert_eq!(p.cpu.reg(7), a ^ b);
    assert_eq!(p.cpu.reg(8), !(a | b));
    assert_eq!(p.cpu.reg(9), 1); // a negative signed
    assert_eq!(p.cpu.reg(10), 0);
    assert_eq!(p.cpu.reg(11), 0x1234); // andi zero-extends
    assert_eq!(p.cpu.reg(12), a ^ 0xFFFF); // xori zero-extends
    assert_eq!(p.cpu.reg(13), 1); // a < -1 signed
    assert_eq!(p.cpu.reg(14), 1); // sltiu compares against 0xFFFFFFFF
    assert_eq!(p.cpu.reg(15), b << 4);
    assert_eq!(p.cpu.reg(16), a >> 4);
    assert_eq!(p.cpu.reg(17), ((a as i32) >> 4) as u32);
}

#[test]
fn variable_shifts_mask_amount() {
    let prog = cat(&[&li(1, 0x8000_0001), &[addiu(2, 0, 33), sllv(3, 1, 2), srav(4, 1, 2)]]);
    let p = run(&prog, 5);
    assert_eq!(p.cpu.reg(3), 0x0000_0002);
    assert_eq!(p.cpu.reg(4), 0xC000_0000);
}

#[test]
fn load_delay_slot() {
    let prog = cat(&[
        &li(1, DATA),
        &[
            addiu(2, 0, 7),       // r2 = 7
            lw(2, 0, 1),          // r2 <- mem (delayed)
            addu(3, 2, 0),        // delay slot: sees old r2 = 7
            addu(4, 2, 0),        // sees loaded value
        ],
    ]);
    let mut p = machine(&prog);
    p.bus.write(DATA, 0xDEAD_BEEF, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(3), 7);
    assert_eq!(p.cpu.reg(4), 0xDEAD_BEEF);
}

#[test]
fn load_delay_overwritten_by_delay_slot_write() {
    let prog = cat(&[&li(1, DATA), &[lw(2, 0, 1), addiu(2, 0, 5), NOP]]);
    let mut p = machine(&prog);
    p.bus.write(DATA, 0x1111_1111, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(2), 5, "write in the delay slot wins over the load");
}

#[test]
fn back_to_back_loads_same_register() {
    // The second load cancels the first: its value is never observed.
    // (Behaviour confirmed by differential fuzzing against PCSX-Redux; also
    // implemented by DuckStation.)
    let prog = cat(&[&li(1, DATA), &[addiu(2, 0, 7), lw(2, 0, 1), lw(2, 4, 1), addu(3, 2, 0), addu(4, 2, 0)]]);
    let mut p = machine(&prog);
    p.bus.write(DATA, 0xAAAA_AAAA, Size::Word);
    p.bus.write(DATA + 4, 0xBBBB_BBBB, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(3), 7, "first load cancelled; old value still visible");
    assert_eq!(p.cpu.reg(4), 0xBBBB_BBBB);
}

#[test]
fn lwr_lwl_pair_still_merges_with_pending_load() {
    // The classic unaligned-load idiom must keep working with cancellation.
    let prog = cat(&[&li(1, DATA), &[lwr(2, 1, 1), lwl(2, 4, 1), NOP, addu(3, 2, 0)]]);
    let mut p = machine(&prog);
    for (k, b) in [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88].iter().enumerate() {
        p.bus.write(DATA + k as u32, *b as u32, Size::Byte);
    }
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(3), 0x5544_3322);
}

#[test]
fn byte_and_half_loads_extend_correctly() {
    let prog = cat(&[&li(1, DATA), &[lb(2, 0, 1), lbu(3, 0, 1), lh(4, 0, 1), lhu(5, 0, 1), NOP]]);
    let mut p = machine(&prog);
    p.bus.write(DATA, 0x0000_8081, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(2), 0xFFFF_FF81);
    assert_eq!(p.cpu.reg(3), 0x81);
    assert_eq!(p.cpu.reg(4), 0xFFFF_8081);
    assert_eq!(p.cpu.reg(5), 0x8081);
}

#[test]
fn stores_all_sizes() {
    let prog = cat(&[&li(1, DATA), &li(2, 0x1234_5678), &[sw(2, 0, 1), sh(2, 4, 1), sb(2, 6, 1)]]);
    let mut p = machine(&prog);
    p.bus.write(DATA + 4, 0xFFFF_FFFF, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.bus.read(DATA, Size::Word), 0x1234_5678);
    assert_eq!(p.bus.read(DATA + 4, Size::Word), 0xFF78_5678);
}

#[test]
fn branch_delay_slot_executes() {
    // beq taken skips one instruction, but the delay slot runs.
    let prog = [beq(0, 0, 2), addiu(1, 0, 1), addiu(2, 0, 1), addiu(3, 0, 1)];
    let p = run(&prog, 3);
    assert_eq!(p.cpu.reg(1), 1, "delay slot");
    assert_eq!(p.cpu.reg(2), 0, "skipped");
    assert_eq!(p.cpu.reg(3), 1, "target");
}

#[test]
fn conditional_branches() {
    let prog = cat(&[
        &li(1, 0xFFFF_FFFF), // -1
        &[
            bltz(1, 2), NOP, addiu(10, 0, 1), // skipped
            bgez(1, 2), NOP, addiu(11, 0, 1), // executed
            blez(0, 2), NOP, addiu(12, 0, 1), // skipped
            bgtz(0, 2), NOP, addiu(13, 0, 1), // executed
            bne(1, 0, 2), NOP, addiu(14, 0, 1), // skipped
        ],
    ]);
    let p = run(&prog, 30);
    assert_eq!(
        [p.cpu.reg(10), p.cpu.reg(11), p.cpu.reg(12), p.cpu.reg(13), p.cpu.reg(14)],
        [0, 1, 0, 1, 0]
    );
}

#[test]
fn jal_links_and_jumps() {
    let target = BASE + 0x40;
    let mut prog = vec![jal(target), addiu(1, 0, 9)];
    prog.resize(0x10, NOP);
    prog.push(addiu(2, 0, 3)); // at BASE+0x40
    let p = run(&prog, 3);
    assert_eq!(p.cpu.reg(31), BASE + 8);
    assert_eq!(p.cpu.reg(1), 9);
    assert_eq!(p.cpu.reg(2), 3);
}

#[test]
fn jr_and_jalr() {
    let prog = cat(&[&li(5, BASE + 0x20), &[jalr(7, 5), NOP, NOP, NOP, NOP, NOP, addiu(1, 0, 1), jr(7), NOP]]);
    // after jr(7) we return to BASE+0x10 (instruction after jalr's delay slot)
    let p = run(&prog, 7);
    assert_eq!(p.cpu.reg(7), BASE + 0x10);
    assert_eq!(p.cpu.pc, BASE + 0x10);
    assert_eq!(p.cpu.reg(1), 1);
}

#[test]
fn bltzal_links_even_when_not_taken() {
    let p = run(&[bltzal(0, 4), NOP], 2);
    assert_eq!(p.cpu.reg(31), BASE + 8);
    assert_eq!(p.cpu.pc, BASE + 8);
}

#[test]
fn mult_div() {
    let prog = cat(&[
        &li(1, 0xFFFF_FFFE), // -2
        &li(2, 7),
        &[mult(1, 2), mfhi(3), mflo(4), multu(1, 2), mfhi(5), mflo(6), div(1, 2), mfhi(7), mflo(8), divu(1, 2), mfhi(9), mflo(10)],
    ]);
    let p = run(&prog, prog.len() as u64);
    assert_eq!((p.cpu.reg(3), p.cpu.reg(4)), (0xFFFF_FFFF, (-14i32) as u32));
    let u = 0xFFFF_FFFEu64 * 7;
    assert_eq!((p.cpu.reg(5), p.cpu.reg(6)), ((u >> 32) as u32, u as u32));
    assert_eq!((p.cpu.reg(7), p.cpu.reg(8)), ((-2i32 % 7) as u32, 0)); // -2/7 = 0 rem -2
    assert_eq!((p.cpu.reg(9), p.cpu.reg(10)), (0xFFFF_FFFE % 7, 0xFFFF_FFFE / 7));
}

#[test]
fn div_edge_cases() {
    let prog = cat(&[
        &li(1, 5), &li(2, 0xFFFF_FFFB), &li(3, 0x8000_0000), &li(4, 0xFFFF_FFFF),
        &[
            div(1, 0), mfhi(10), mflo(11),   // 5/0
            div(2, 0), mfhi(12), mflo(13),   // -5/0
            div(3, 4), mfhi(14), mflo(15),   // INT_MIN / -1
            divu(1, 0), mfhi(16), mflo(17),  // 5/0 unsigned
        ],
    ]);
    let p = run(&prog, prog.len() as u64);
    assert_eq!((p.cpu.reg(10), p.cpu.reg(11)), (5, 0xFFFF_FFFF));
    assert_eq!((p.cpu.reg(12), p.cpu.reg(13)), (0xFFFF_FFFB, 1));
    assert_eq!((p.cpu.reg(14), p.cpu.reg(15)), (0, 0x8000_0000));
    assert_eq!((p.cpu.reg(16), p.cpu.reg(17)), (5, 0xFFFF_FFFF));
}

#[test]
fn mthi_mtlo_roundtrip() {
    let prog = cat(&[&li(1, 0xCAFE_BABE), &[mthi(1), mfhi(2)]]);
    let p = run(&prog, 4);
    assert_eq!(p.cpu.reg(2), 0xCAFE_BABE);
}

#[test]
fn unaligned_lwl_lwr_all_offsets() {
    // Classic idiom: lwr rt,off(base); lwl rt,off+3(base) loads an unaligned word
    let mem: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    for off in 0..4i32 {
        let prog = cat(&[&li(1, DATA), &[lwr(2, off, 1), lwl(2, off + 3, 1), NOP]]);
        let mut p = machine(&prog);
        for (k, b) in mem.iter().enumerate() {
            p.bus.write(DATA + k as u32, *b as u32, Size::Byte);
        }
        p.run_instructions(prog.len() as u64);
        let o = off as usize;
        let expect = u32::from_le_bytes([mem[o], mem[o + 1], mem[o + 2], mem[o + 3]]);
        assert_eq!(p.cpu.reg(2), expect, "offset {}", off);
    }
}

#[test]
fn unaligned_swl_swr_all_offsets() {
    for off in 0..4i32 {
        let prog = cat(&[&li(1, DATA), &li(2, 0xA1B2_C3D4), &[swr(2, off, 1), swl(2, off + 3, 1)]]);
        let mut p = machine(&prog);
        for k in 0..8 {
            p.bus.write(DATA + k, 0xEE, Size::Byte);
        }
        p.run_instructions(prog.len() as u64);
        let mut expect = [0xEEu8; 8];
        expect[off as usize..off as usize + 4].copy_from_slice(&0xA1B2_C3D4u32.to_le_bytes());
        let got: Vec<u8> = (0..8).map(|k| p.bus.read(DATA + k, Size::Byte) as u8).collect();
        assert_eq!(got, expect, "offset {}", off);
    }
}

#[test]
fn add_overflow_traps_without_writing() {
    let prog = cat(&[&li(1, 0x7FFF_FFFF), &[addiu(2, 0, 77), add(2, 1, 1)]]);
    let p = run(&prog, 4);
    assert_eq!(exc_code(&p), Exception::Overflow as u32);
    assert_eq!(p.cpu.cop0.epc, BASE + 12);
    assert_eq!(p.cpu.reg(2), 77);
    assert_eq!(p.cpu.pc, HANDLER);
}

#[test]
fn addi_and_sub_overflow() {
    let prog = cat(&[&li(1, 0x8000_0000), &[addiu(2, 0, 1), sub(3, 1, 2)]]);
    let p = run(&prog, 4);
    assert_eq!(exc_code(&p), Exception::Overflow as u32);
    let prog = cat(&[&li(1, 0x7FFF_FFFF), &[addi(3, 1, 1)]]);
    let p = run(&prog, 3);
    assert_eq!(exc_code(&p), Exception::Overflow as u32);
}

#[test]
fn exception_in_delay_slot_sets_bd() {
    let prog = cat(&[&li(1, 0x7FFF_FFFF), &[beq(0, 0, 4), add(2, 1, 1)]]);
    let p = run(&prog, 4);
    assert_eq!(exc_code(&p), Exception::Overflow as u32);
    assert_eq!(p.cpu.cop0.epc, BASE + 8, "EPC points at the branch");
    assert_ne!(p.cpu.cop0.cause & (1 << 31), 0, "BD set");
}

#[test]
fn syscall_break_and_rfe_mode_stack() {
    let prog = [addiu(1, 0, 0b0101), mtc0(1, 12), syscall()];
    let p = run(&prog, 3);
    assert_eq!(exc_code(&p), Exception::Syscall as u32);
    assert_eq!(p.cpu.cop0.epc, BASE + 8);
    assert_eq!(p.cpu.cop0.sr & 0x3F, 0b010100, "mode stack pushed");
    assert_eq!(p.cpu.cop0.cause & (1 << 31), 0);

    let p = run(&[brk()], 1);
    assert_eq!(exc_code(&p), Exception::Break as u32);

    // RFE pops it back
    let prog = [addiu(1, 0, 0b010100), mtc0(1, 12), rfe(), mfc0(2, 12), NOP];
    let p = run(&prog, 5);
    assert_eq!(p.cpu.reg(2) & 0x3F, 0b010101);
}

#[test]
fn mfc0_has_load_delay_and_prid() {
    let prog = [addiu(2, 0, 1), mfc0(2, 15), addu(3, 2, 0), addu(4, 2, 0)];
    let p = run(&prog, 4);
    assert_eq!(p.cpu.reg(3), 1);
    assert_eq!(p.cpu.reg(4), 2);
}

#[test]
fn misaligned_access_faults() {
    let prog = cat(&[&li(1, DATA + 1), &[lw(2, 0, 1)]]);
    let p = run(&prog, 3);
    assert_eq!(exc_code(&p), Exception::LoadAddressError as u32);
    assert_eq!(p.cpu.cop0.bad_vaddr, DATA + 1);

    let prog = cat(&[&li(1, DATA + 1), &[sh(2, 0, 1)]]);
    let p = run(&prog, 3);
    assert_eq!(exc_code(&p), Exception::StoreAddressError as u32);

    let prog = cat(&[&li(1, BASE + 2), &[jr(1), NOP]]);
    let p = run(&prog, 5);
    assert_eq!(exc_code(&p), Exception::LoadAddressError as u32);
    assert_eq!(p.cpu.cop0.bad_vaddr, BASE + 2);
    assert_eq!(p.cpu.cop0.epc, BASE + 2);
}

#[test]
fn reserved_and_cop_unusable() {
    let p = run(&[0xFC00_0000], 1);
    assert_eq!(exc_code(&p), Exception::ReservedInstruction as u32);
    // COP2 with CU2 clear
    let p = run(&[0x4A00_0001], 1);
    assert_eq!(exc_code(&p), Exception::CoprocessorUnusable as u32);
    assert_eq!((p.cpu.cop0.cause >> 28) & 3, 2);
    // COP1 always unusable
    let p = run(&[0x4400_0000], 1);
    assert_eq!((p.cpu.cop0.cause >> 28) & 3, 1);
}

#[test]
fn interrupt_taken_when_enabled_and_unmasked() {
    // SR: IEc=1, IM bit 10 (IP2) = 1
    let prog = cat(&[&li(1, 0x0000_0401), &[mtc0(1, 12), NOP, NOP, NOP]]);
    let mut p = machine(&prog);
    p.bus.irq.set_mask(1 << Interrupt::VBlank as u32);
    p.run_instructions(4); // li, mtc0, nop
    assert_ne!(p.cpu.pc, HANDLER);
    p.bus.irq.request(Interrupt::VBlank);
    p.step();
    assert_eq!(exc_code(&p), Exception::Interrupt as u32);
    assert_eq!(p.cpu.cop0.epc, BASE + 16, "EPC = interrupted (not executed) instruction");
    assert_eq!(p.cpu.cop0.sr & 0x3F, 0b000100, "interrupts disabled in handler");
    assert_eq!(p.cpu.pc, HANDLER);
}

#[test]
fn masked_interrupt_ignored_and_ack_works() {
    let prog = cat(&[&li(1, 0x0000_0401), &[mtc0(1, 12), NOP, NOP]]);
    let mut p = machine(&prog);
    p.bus.irq.request(Interrupt::Timer0); // not in mask
    p.run_instructions(5);
    assert_ne!(p.cpu.pc, HANDLER);
    // ack via I/O write: writing 0 to bit 4 clears it
    p.bus.write(0x1F80_1070, !(1 << 4), Size::Word);
    assert_eq!(p.bus.irq.stat, 0);
}

#[test]
fn software_interrupt_via_cause() {
    // IM0 (bit 8) + IEc, then set CAUSE bit 8
    let prog = cat(&[&li(1, 0x0000_0101), &[mtc0(1, 12), addiu(2, 0, 0x100), mtc0(2, 13), NOP, NOP]]);
    let p = run(&prog, 6);
    assert_eq!(exc_code(&p), Exception::Interrupt as u32);
}

#[test]
fn cache_isolation_blocks_stores() {
    let prog = cat(&[&li(1, SR_ISC), &li(2, DATA), &[mtc0(1, 12), addiu(3, 0, 0x55), sw(3, 0, 2)]]);
    let mut p = run(&prog, prog.len() as u64);
    assert_eq!(p.bus.read(DATA, Size::Word), 0);
}

#[test]
fn memory_mirrors() {
    let mut p = Psx::new();
    p.bus.write(0x0000_1000, 0x1234_5678, Size::Word);
    assert_eq!(p.bus.read(0x8000_1000, Size::Word), 0x1234_5678);
    assert_eq!(p.bus.read(0xA000_1000, Size::Word), 0x1234_5678);
    assert_eq!(p.bus.read(0x0020_1000, Size::Word), 0x1234_5678, "2MiB RAM mirror");
    p.bus.write(0x1F80_0010, 0xABCD, Size::Half);
    assert_eq!(p.bus.read(0x9F80_0010, Size::Half), 0xABCD);
    assert_eq!(p.bus.read(0x1F00_0000, Size::Byte), 0xFF);
    // BIOS is read-only
    p.bus.write(0xBFC0_0000, 0xFFFF_FFFF, Size::Word);
    assert_eq!(p.bus.read(0xBFC0_0000, Size::Word), 0);
}

#[test]
fn exe_loader_and_tty_hook() {
    // Program: print "Hi" with A(3Ch) putchar using a fake BIOS stub at 0xA0
    // (stub just returns), then spin.
    let code = [
        addiu(4, 0, b'H' as i32), addiu(9, 0, 0x3C), jal(0xA0), NOP,
        addiu(4, 0, b'i' as i32), addiu(9, 0, 0x3C), jal(0xA0), NOP,
        j(0x8001_0020), NOP,
    ];
    let mut exe = vec![0u8; 0x800];
    exe[0..8].copy_from_slice(b"PS-X EXE");
    exe[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    exe[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    exe[0x1C..0x20].copy_from_slice(&((code.len() * 4) as u32).to_le_bytes());
    exe[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
    for w in code {
        exe.extend_from_slice(&w.to_le_bytes());
    }
    let mut p = Psx::new();
    p.bus.write(0xA0, jr(31), Size::Word);
    p.bus.write(0xA4, NOP, Size::Word);
    p.load_exe_now(&exe).unwrap();
    assert_eq!(p.cpu.reg(29), 0x801F_FF00);
    p.run_instructions(40);
    assert_eq!(p.tty, "Hi");
}

#[test]
fn cop2_transfers_and_commands_via_cpu() {
    const SR_CU2: u32 = 1 << 30;
    let mtc2 = |rt: u32, rd: u32| 0x4880_0000 | (rt << 16) | (rd << 11);
    let mfc2 = |rt: u32, rd: u32| 0x4800_0000 | (rt << 16) | (rd << 11);
    let ctc2 = |rt: u32, rd: u32| 0x48C0_0000 | (rt << 16) | (rd << 11);
    let cfc2 = |rt: u32, rd: u32| 0x4840_0000 | (rt << 16) | (rd << 11);
    let cop2 = |c: u32| 0x4A00_0000 | c;
    let lwc2 = |rt: u32, off: i32, rs: u32| i(0x32, rs, rt, off);
    let swc2 = |rt: u32, off: i32, rs: u32| i(0x3A, rs, rt, off);
    let prog = cat(&[
        &li(1, SR_CU2), &[mtc0(1, 12)],
        &li(2, 0x1000), &[mtc2(2, 9), mtc2(2, 10)],      // IR1 = IR2 = 0x1000
        &[cop2(0x0008_0028)],                             // SQR sf=1
        &[mfc2(3, 25), NOP],                              // MAC1 -> r3
        &li(4, 0xFFFF_8000), &[ctc2(4, 26), cfc2(5, 26), NOP], // H quirk
        &li(6, DATA), &[lwc2(9, 0, 6), NOP, swc2(9, 4, 6)],   // IR1 <- mem, mem+4 <- IR1
    ]);
    let mut p = machine(&prog);
    p.bus.write(DATA, 0x0000_ABCD, Size::Word);
    p.run_instructions(prog.len() as u64);
    assert_eq!(p.cpu.reg(3), 0x1000);
    assert_eq!(p.cpu.reg(5), 0xFFFF_8000);
    assert_eq!(p.bus.read(DATA + 4, Size::Word), 0xFFFF_ABCD, "IR1 sign-extends");
}

#[test]
fn absent_coprocessors_follow_cu_bits() {
    // COP3 with CU3 clear: CpU exception naming coprocessor 3 (not 1)
    let p = run(&[0x4C00_0000], 1);
    assert_eq!(exc_code(&p), Exception::CoprocessorUnusable as u32);
    assert_eq!((p.cpu.cop0.cause >> 28) & 3, 3);
    // COP1 / SWC3 with their CU bits set: silently ignored
    let prog = cat(&[&li(1, (1 << 29) | (1 << 31)), &[mtc0(1, 12), 0x4400_0000, 0xEC00_0000, addiu(2, 0, 9)]]);
    let p = run(&prog, prog.len() as u64);
    assert_eq!(p.cpu.reg(2), 9, "no exception taken");
    // Unknown COP0 register / operation: no exception
    let p = run(&[mfc0(2, 0), 0x4200_0001, addiu(3, 0, 1)], 3);
    assert_eq!(p.cpu.reg(3), 1);
    assert_ne!(p.cpu.pc, HANDLER);
}

#[test]
fn instruction_fetch_from_scratchpad_is_a_bus_error() {
    let prog = cat(&[&li(1, 0x1F80_0000), &[jr(1), NOP]]);
    let p = run(&prog, 5);
    assert_eq!(exc_code(&p), 6);
    assert_eq!(p.cpu.cop0.epc, 0x1F80_0000);
}
