//! ps1wasm-run: headless emulator runner for automated testing.
//!
//!   ps1wasm-run --bios BIOS [--exe FILE | --cue FILE | --bin FILE] [--frames N]
//!           [--vram OUT.png] [--screen OUT.png] [--compare-vram REF.png]
//!
//! Prints BIOS TTY output to stdout. Exit code 1 if --compare-vram differs.

use ps1wasm_core::disc::{CueSheet, Disc, MemReader};
use ps1wasm_core::gpu::{VRAM_H, VRAM_W};
use ps1wasm_core::Psx;
use std::path::Path;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn save_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let f = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

/// VRAM as RGBA (15-bit -> 8-bit per channel, same expansion as ps1-tests' dumps).
fn vram_rgba(p: &Psx) -> Vec<u8> {
    let mut out = Vec::with_capacity(VRAM_W * VRAM_H * 4);
    for &px in &p.bus.gpu.vram {
        let c = |v: u16| ((v & 0x1F) << 3) as u8;
        out.extend_from_slice(&[c(px), c(px >> 5), c(px >> 10), 0xFF]);
    }
    out
}

fn load_png(path: &str) -> (u32, u32, Vec<u8>) {
    let mut dec = png::Decoder::new(std::fs::File::open(path).expect("open ref png"));
    // Expand palette / low-bit-depth images to plain RGB(A).
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let buf = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf.chunks(3).flat_map(|c| [c[0], c[1], c[2], 0xFF]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 0xFF]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|c| [c[0], c[0], c[0], 0xFF]).collect(),
        other => panic!("unsupported png color type {:?}", other),
    };
    (info.width, info.height, rgba)
}

/// Build a machine from the command line.
fn build(args: &[String]) -> Psx {
    let bios = arg(args, "--bios").expect("--bios required");
    let mut p = Psx::new();
    p.load_bios(&std::fs::read(&bios).expect("read bios")).expect("bad bios");
    if let Some(exe) = arg(args, "--exe") {
        p.load_exe_after_bios(&std::fs::read(&exe).expect("read exe")).expect("bad exe");
    }
    if let Some(cue) = arg(args, "--cue") {
        let text = std::fs::read_to_string(&cue).expect("read cue");
        let sheet = CueSheet::parse(&text).expect("parse cue");
        let dir = Path::new(&cue).parent().unwrap().to_path_buf();
        let datas: Vec<Vec<u8>> = sheet.files.iter().map(|f| std::fs::read(dir.join(f)).expect("read bin")).collect();
        let sizes: Vec<u64> = datas.iter().map(|d| d.len() as u64).collect();
        p.insert_disc(Disc::from_cue(&sheet, &sizes, Box::new(MemReader(datas))).expect("disc"));
    }
    if let Some(bin) = arg(args, "--bin") {
        let data = std::fs::read(&bin).expect("read bin");
        let n = data.len() as u64;
        p.insert_disc(Disc::from_single(n, n.is_multiple_of(2352), Box::new(MemReader(vec![data]))).expect("disc"));
    }
    p
}

/// Run `frames` frames. If `fill` is given and an EXE was supplied, VRAM is
/// filled with that value at the moment the EXE starts (after BIOS init), so
/// the pixels the program writes can be told apart from leftover memory.
fn run(args: &[String], p: &mut Psx, frames: u32, fill: Option<u16>, echo: bool) {
    let entry = arg(args, "--exe").map(|e| ps1wasm_core::parse_exe(&std::fs::read(e).unwrap()).unwrap().pc);
    let start = p.bus.gpu.frame_count;
    let mut printed = 0;
    if let (Some(fill), Some(entry)) = (fill, entry) {
        let limit = p.bus.cycles + 33_868_800 * 60;
        while p.cpu.current_pc != entry && p.bus.cycles < limit {
            p.step();
        }
        if std::env::var("PSX_DEBUG").is_ok() {
            eprintln!("[run] entry {:08x} reached={} at cycle {}", entry, p.cpu.current_pc == entry, p.bus.cycles);
        }
        p.bus.gpu.vram.fill(fill);
    }
    while p.bus.gpu.frame_count - start < frames as u64 {
        p.run_frame();
        if echo && p.tty.len() > printed {
            print!("{}", &p.tty[printed..]);
            printed = p.tty.len();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let frames: u32 = arg(&args, "--frames").map(|f| f.parse().unwrap()).unwrap_or(300);
    let compare = arg(&args, "--compare-vram").is_some();
    let mut p = build(&args);

    if args.iter().any(|a| a == "--trace-cd") {
        p.bus.cdrom.trace = true;
    }
    run(&args, &mut p, frames, if compare { Some(0x0000) } else { None }, true);
    eprintln!(
        "[ps1wasm-run] {} frames, {} instructions, pc={:08x}, unmapped accesses={}",
        frames, p.cpu.instructions, p.cpu.pc, p.bus.unhandled_io
    );

    if let Some(spec) = arg(&args, "--peek") {
        // --peek ADDR:WORDS (hex address)
        let (a, n) = spec.split_once(':').expect("--peek ADDR:WORDS");
        let a = u32::from_str_radix(a.trim_start_matches("0x"), 16).expect("hex address");
        let n: u32 = n.parse().expect("word count");
        for k in 0..n {
            if k % 8 == 0 {
                print!("\n{:08x}:", a + 4 * k);
            }
            print!(" {:08x}", p.bus.read(a + 4 * k, ps1wasm_core::bus::Size::Word));
        }
        println!();
    }
    // --pixel X,Y[;X,Y...] prints display pixels as RGB
    if let Some(spec) = arg(&args, "--pixel") {
        let mut buf = vec![];
        let (w, _h) = p.render(&mut buf);
        for pt in spec.split(';') {
            let (x, y) = pt.split_once(',').expect("--pixel X,Y");
            let (x, y): (usize, usize) = (x.parse().unwrap(), y.parse().unwrap());
            let i = (y * w as usize + x) * 4;
            println!("pixel {},{} = {:02x}{:02x}{:02x}", x, y, buf[i], buf[i + 1], buf[i + 2]);
        }
    }
    if let Some(out) = arg(&args, "--screen") {
        let mut buf = vec![];
        let (w, h) = p.render(&mut buf);
        save_png(&out, w, h, &buf);
    }
    let vram = vram_rgba(&p);
    if let Some(out) = arg(&args, "--vram") {
        save_png(&out, VRAM_W as u32, VRAM_H as u32, &vram);
    }
    if let Some(r) = arg(&args, "--compare-vram") {
        let (w, h, refpx) = load_png(&r);
        assert_eq!((w, h), (VRAM_W as u32, VRAM_H as u32), "reference must be a full 1024x512 VRAM dump");
        // Second run with VRAM pre-filled white: pixels identical in both runs
        // were written by the program; everything else is leftover memory and
        // is excluded (hardware dumps contain arbitrary prior VRAM contents).
        let mut q = build(&args);
        run(&args, &mut q, frames, Some(0x7FFF), false);
        let mut written: Vec<bool> = p.bus.gpu.vram.iter().zip(&q.bus.gpu.vram).map(|(a, b)| a == b).collect();
        // --region X,Y,W,H restricts the comparison to a rectangle
        if let Some(r) = arg(&args, "--region") {
            let v: Vec<usize> = r.split(',').map(|n| n.parse().expect("--region X,Y,W,H")).collect();
            for (k, w) in written.iter_mut().enumerate() {
                let (x, y) = (k % VRAM_W, k / VRAM_W);
                *w &= x >= v[0] && x < v[0] + v[2] && y >= v[1] && y < v[1] + v[3];
            }
        }
        let n_written = written.iter().filter(|&&w| w).count();
        let mut diff = 0usize;
        let mut max_err = 0i32;
        let mut hist = [0usize; 4]; // max channel error 1, 2, 3-7, 8+
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        let mut samples = vec![];
        let mut diff_img = vram.clone();
        for k in 0..VRAM_W * VRAM_H {
            let a = &vram[4 * k..4 * k + 3];
            let b = &refpx[4 * k..4 * k + 3];
            // compare written pixels at 5-bit precision
            if written[k] && a.iter().zip(b).any(|(x, y)| (x >> 3) != (y >> 3)) {
                diff += 1;
                let mut e = 0;
                for c in 0..3 {
                    e = e.max(((a[c] >> 3) as i32 - (b[c] >> 3) as i32).abs());
                }
                max_err = max_err.max(e);
                hist[match e { 1 => 0, 2 => 1, 3..=7 => 2, _ => 3 }] += 1;
                let (x, y) = (k % VRAM_W, k / VRAM_W);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                if samples.len() < 8 {
                    samples.push(format!("({},{}) ours {:?} ref {:?}", x, y, [a[0] >> 3, a[1] >> 3, a[2] >> 3], [b[0] >> 3, b[1] >> 3, b[2] >> 3]));
                }
                diff_img[4 * k..4 * k + 4].copy_from_slice(&[255, 0, 255, 255]);
            }
        }
        if diff > 0 {
            println!("[compare] error histogram: 1:{} 2:{} 3-7:{} 8+:{}", hist[0], hist[1], hist[2], hist[3]);
            println!("[compare] differences within x {}..={} y {}..={}", x0, x1, y0, y1);
            for s in &samples {
                println!("[compare]   {}", s);
            }
            if let Some(out) = arg(&args, "--diff") {
                save_png(&out, VRAM_W as u32, VRAM_H as u32, &diff_img);
            }
        }
        println!(
            "[compare] {} of {} program-written pixels differ ({:.3}%), max channel error {} (5-bit units)",
            diff, n_written, diff as f64 * 100.0 / n_written.max(1) as f64, max_err
        );
        if diff > 0 {
            std::process::exit(1);
        }
    }
}
