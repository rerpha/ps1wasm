//! Throughput benchmarks: CPU+I/O loop, and GPU fill rate.
use ps1wasm_core::bus::Size;
use ps1wasm_core::irq::Irq;
use std::time::Instant;

fn cpu_bench() {
    let mut p = ps1wasm_core::Psx::new();
    let base = 0x8001_0000u32;
    // loop: lw from RAM, addu, sw to RAM, read GPUSTAT every iteration
    let prog = [
        0x3C09_1F80u32, // lui t1,0x1F80
        0x3C0B_8002,    // lui t3,0x8002
        0x8D6C_0000,    // lw t4,0(t3)
        0x0000_0000,
        0x258C_0001,    // addiu t4,t4,1
        0xAD6C_0000,    // sw t4,0(t3)
        0x8D2A_1814,    // lw t2,0x1814(t1)
        0x0800_0000 | ((base + 8) >> 2) & 0x03FF_FFFF,
        0,
    ];
    for (k, w) in prog.iter().enumerate() {
        p.bus.write(base + 4 * k as u32, *w, Size::Word);
    }
    p.cpu.set_pc(base);
    p.bus.write(0xFFFE_0130, 0x800, Size::Word); // I-cache on: 1 cycle/instr worst case
    let frames = 600;
    let t = Instant::now();
    for _ in 0..frames {
        p.run_frame();
    }
    let dt = t.elapsed().as_secs_f64();
    println!("CPU+I/O: {:.0} emulated fps ({:.1} M instr/s, need ~34 with cache)", frames as f64 / dt, p.cpu.instructions as f64 / dt / 1e6);
}

fn gpu_bench() {
    let mut g = ps1wasm_core::gpu::Gpu::new();
    let mut irq = Irq::default();
    g.gp0(0xE400_0000 | 1023 | (511 << 10), &mut irq);
    let n = 20_000;
    let t = Instant::now();
    for k in 0..n {
        let o = (k % 200) as u32;
        // gouraud textured-free triangle, 64x64 half-square = 2048 px
        for w in [0x3000_00FF, o, 0x0000_FF00, o + 64, 0x00FF_0000, o + (64 << 16)] {
            g.gp0(w, &mut irq);
        }
    }
    let dt = t.elapsed().as_secs_f64();
    println!("GPU: {:.0} M gouraud px/s (hardware peak ~33 M)", n as f64 * 2048.0 / dt / 1e6);
}

/// Worst case: tight cached ALU loop (every cycle is an instruction).
fn alu_bench() {
    let mut p = ps1wasm_core::Psx::new();
    let base = 0x8001_0000u32;
    let prog = [0x2508_0001u32, 0x2529_0003, 0x0109_5021, 0x0800_0000 | (base >> 2) & 0x03FF_FFFF, 0];
    for (k, w) in prog.iter().enumerate() {
        p.bus.write(base + 4 * k as u32, *w, Size::Word);
    }
    p.cpu.set_pc(base);
    p.bus.write(0xFFFE_0130, 0x800, Size::Word);
    let frames = 300;
    let t = Instant::now();
    for _ in 0..frames {
        p.run_frame();
    }
    let dt = t.elapsed().as_secs_f64();
    println!("ALU worst case: {:.0} emulated fps ({:.1} M instr/s)", frames as f64 / dt, p.cpu.instructions as f64 / dt / 1e6);
}

fn main() {
    alu_bench();
    cpu_bench();
    gpu_bench();
}
