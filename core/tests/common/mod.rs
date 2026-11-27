//! Shared mini MIPS assembler for integration tests.
#![allow(dead_code)]

pub fn r(funct: u32, rs: u32, rt: u32, rd: u32, sh: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sh << 6) | funct
}
pub fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm as u32 & 0xFFFF)
}
pub const NOP: u32 = 0;
pub const ZERO: u32 = 0;
pub const T0: u32 = 8;
pub const T1: u32 = 9;
pub const T2: u32 = 10;
pub const K0: u32 = 26;
pub const K1: u32 = 27;
pub fn jr(rs: u32) -> u32 { r(0x08, rs, 0, 0, 0) }
pub fn addu(rd: u32, rs: u32, rt: u32) -> u32 { r(0x21, rs, rt, rd, 0) }
pub fn j(target: u32) -> u32 { (0x02 << 26) | ((target >> 2) & 0x03FF_FFFF) }
pub fn bne(rs: u32, rt: u32, off: i32) -> u32 { i(0x05, rs, rt, off) }
pub fn addiu(rt: u32, rs: u32, imm: i32) -> u32 { i(0x09, rs, rt, imm) }
pub fn ori(rt: u32, rs: u32, imm: i32) -> u32 { i(0x0D, rs, rt, imm) }
pub fn lui(rt: u32, imm: u32) -> u32 { i(0x0F, 0, rt, imm as i32) }
pub fn mfc0(rt: u32, rd: u32) -> u32 { (0x10 << 26) | (rt << 16) | (rd << 11) }
pub fn mtc0(rt: u32, rd: u32) -> u32 { (0x10 << 26) | (4 << 21) | (rt << 16) | (rd << 11) }
pub fn rfe() -> u32 { 0x4200_0010 }
pub fn lw(rt: u32, off: i32, rs: u32) -> u32 { i(0x23, rs, rt, off) }
pub fn lbu(rt: u32, off: i32, rs: u32) -> u32 { i(0x24, rs, rt, off) }
pub fn sw(rt: u32, off: i32, rs: u32) -> u32 { i(0x2B, rs, rt, off) }
pub fn sb(rt: u32, off: i32, rs: u32) -> u32 { i(0x28, rs, rt, off) }
pub fn li(rt: u32, v: u32) -> [u32; 2] { [lui(rt, v >> 16), ori(rt, rt, (v & 0xFFFF) as i32)] }
pub fn cat(parts: &[&[u32]]) -> Vec<u32> { parts.iter().flat_map(|p| p.iter().copied()).collect() }
