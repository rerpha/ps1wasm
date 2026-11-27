//! PeterLemon/PSX test programs vs their expected screenshots. The CPU/GTE
//! tests draw a green PASS or red FAIL badge per case; GPU tests draw
//! reference scenes. Skipped when test-assets/peterlemon is absent.

use ps1wasm_core::Psx;
use std::path::{Path, PathBuf};

fn root() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets");
    (p.join("peterlemon").exists() && p.join("openbios.bin").exists()).then_some(p)
}

fn load_png(path: &Path) -> Option<(usize, usize, Vec<[u8; 3]>)> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    let ch = info.color_type.samples();
    let px = buf[..info.buffer_size()].chunks(ch).map(|c| [c[0], c[1 % ch], c[2 % ch]]).collect();
    Some((info.width as usize, info.height as usize, px))
}

pub struct Cmp {
    /// exact matches at 5-bit precision
    exact: f64,
    /// fraction of pixels consistent with the best colour mapping ours->ref
    /// (1.0 = structurally identical, colours possibly converted differently)
    structural: f64,
    /// red-glyph pixels (FAIL badges) and green-glyph pixels (PASS badges)
    fail_px: usize,
    pass_px: usize,
    w: u32,
    h: u32,
}

/// Run an EXE under OpenBIOS and compare with its expected screenshot.
pub fn compare(root: &Path, exe: &Path, png: &Path, frames: u32) -> Option<Cmp> {
    let (rw, rh, refpx) = load_png(png)?;
    let mut p = Psx::new();
    p.load_bios(&std::fs::read(root.join("openbios.bin")).ok()?).ok()?;
    p.load_exe_after_bios(&std::fs::read(exe).ok()?).ok()?;
    for _ in 0..frames {
        p.run_frame();
    }
    let mut buf = vec![];
    let (w, h) = p.render(&mut buf);
    // Screenshots may be cropped differently: take the best alignment over
    // small offsets of the reference within our frame.
    let mut best = 0usize;
    let mut best_off = (0usize, 0usize);
    for oy in 0..=24usize {
        for ox in 0..=8usize {
            let mut same = 0;
            for y in 0..rh {
                for x in 0..rw {
                    let (sx, sy) = (x + ox, y + oy);
                    if sx >= w as usize || sy >= h as usize {
                        continue;
                    }
                    let o = &buf[(sy * w as usize + sx) * 4..][..3];
                    let r = refpx[y * rw + x];
                    if (0..3).all(|c| o[c] >> 3 == r[c] >> 3) {
                        same += 1;
                    }
                }
            }
            if same > best {
                best = same;
                best_off = (ox, oy);
            }
        }
    }
    let same = best;
    // Structural score at the best offset
    let mut map: std::collections::HashMap<[u8; 3], std::collections::HashMap<[u8; 3], usize>> = Default::default();
    let mut total = 0usize;
    for y in 0..rh {
        for x in 0..rw {
            let (sx, sy) = (x + best_off.0, y + best_off.1);
            if sx >= w as usize || sy >= h as usize {
                continue;
            }
            let o = &buf[(sy * w as usize + sx) * 4..][..3];
            *map.entry([o[0], o[1], o[2]]).or_default().entry(refpx[y * rw + x]).or_default() += 1;
            total += 1;
        }
    }
    let consistent: usize = map.values().map(|m| m.values().copied().max().unwrap_or(0)).sum();
    // FAIL/PASS badge pixels in the result column (x >= 272)
    let (mut fail_px, mut pass_px) = (0, 0);
    for y in 0..h as usize {
        for x in 272..w as usize {
            let o = &buf[(y * w as usize + x) * 4..][..3];
            if o[0] > 100 && o[1] < 40 && o[2] < 40 {
                fail_px += 1;
            }
            if o[1] > 100 && o[0] < 40 && o[2] < 40 {
                pass_px += 1;
            }
        }
    }
    // pixels outside our image count as mismatches
    Some(Cmp {
        exact: same as f64 / (rw * rh) as f64,
        structural: consistent as f64 / (rw * rh).max(total) as f64,
        fail_px,
        pass_px,
        w,
        h,
    })
}

fn exes(root: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let list = std::fs::read_to_string(root.join("peterlemon/list.txt")).unwrap_or_default();
    let mut out = vec![];
    for line in list.lines().filter(|l| l.ends_with(".exe")) {
        let exe = root.join("peterlemon").join(line);
        let png = exe.with_extension("png");
        if png.exists() {
            out.push((line.trim_end_matches(".exe").to_string(), exe, png));
        }
    }
    out
}

/// Exact (5-bit) matches with the expected screenshots.
const EXACT: &[&str] = &[
    "GPU/16BPP/MemoryTransfer/MemoryTransfer16BPP",
    "GPU/24BPP/MemoryTransfer/MemoryTransfer24BPP",
    "HelloWorld/16BPP/HelloWorld16BPP",
    "HelloWorld/24BPP/HelloWorld24BPP",
    "SPU/PlayADPCMSample/PlayADPCMSample",
    "SPU/PlaySong/PlaySong",
    "Demo/rootexmp/ROOTEXMP",
];

/// Structurally identical (same image up to the screenshot tool's colour
/// conversion). Includes every CPU and GTE instruction test, whose screens
/// show the program's own PASS/FAIL verdict per case.
fn structural_list(r: &Path) -> Vec<String> {
    let mut v: Vec<String> = exes(r).into_iter().map(|(n, _, _)| n).filter(|n| n.starts_with("CPUTest/")).collect();
    for n in [
        "GPU/16BPP/RenderRectangle/RenderRectangle16BPP",
        "GPU/16BPP/RenderTextureRectangle/15BPP/RenderTextureRectangle15BPP",
        "GPU/16BPP/RenderTextureRectangle/CLUT4BPP/RenderTextureRectangleCLUT4BPP",
        "GPU/16BPP/RenderTextureRectangle/CLUT8BPP/RenderTextureRectangleCLUT8BPP",
        "GPU/16BPP/RenderTextureRectangle/MASK15BPP/RenderTextureRectangleMASK15BPP",
        "GPU/16BPP/RenderTextureWindowRectangle/15BPP/RenderTextureWindowRectangle15BPP",
        "GPU/16BPP/RenderTextureWindowRectangle/CLUT4BPP/RenderTextureWindowRectangleCLUT4BPP",
        "GPU/16BPP/RenderTextureWindowRectangle/CLUT8BPP/RenderTextureWindowRectangleCLUT8BPP",
        "GPU/16BPP/RenderTextureWindowRectangle/MASK15BPP/RenderTextureWindowRectangleMASK15BPP",
    ] {
        v.push(n.to_string());
    }
    v
}

#[test]
fn exact_screens() {
    let Some(r) = root() else { return eprintln!("skipped") };
    for (name, exe, png) in exes(&r).into_iter().filter(|(n, _, _)| EXACT.contains(&n.as_str())) {
        let c = compare(&r, &exe, &png, 150).unwrap();
        assert!(c.exact >= 0.9999, "{}: exact {:.3}%", name, c.exact * 100.0);
    }
}

#[test]
fn cpu_gte_and_rectangle_tests_structurally_identical() {
    let Some(r) = root() else { return eprintln!("skipped") };
    let want = structural_list(&r);
    assert!(want.len() >= 40, "{} tests", want.len());
    let mut bad = vec![];
    for (name, exe, png) in exes(&r).into_iter().filter(|(n, _, _)| want.contains(n)) {
        let c = compare(&r, &exe, &png, 150).unwrap();
        if c.structural < 0.9999 {
            bad.push(format!("{}: structural {:.3}%", name, c.structural * 100.0));
        }
    }
    assert!(bad.is_empty(), "{:#?}", bad);
}

/// Full table (cargo test --release -p ps1wasm-cli --test peterlemon survey -- --ignored --nocapture)
#[test]
#[ignore]
fn survey() {
    let Some(r) = root() else { return eprintln!("skipped") };
    let mut results = vec![];
    for (name, exe, png) in exes(&r) {
        let res = compare(&r, &exe, &png, 150);
        results.push((name, res));
    }
    for (name, res) in &results {
        match res {
            Some(c) => println!(
                "exact {:6.2}%  structural {:6.2}%  fail_px {:5} pass_px {:5}  {}x{}  {}",
                c.exact * 100.0, c.structural * 100.0, c.fail_px, c.pass_px, c.w, c.h, name
            ),
            None => println!("  n/a           {}", name),
        }
    }
}

/// Debug: PL_DEBUG=<name substring> prints mismatch bounding box + samples.
#[test]
fn debug_one() {
    let (Some(r), Ok(want)) = (root(), std::env::var("PL_DEBUG")) else { return };
    for (name, exe, png) in exes(&r) {
        if !name.contains(&want) {
            continue;
        }
        let (rw, rh, refpx) = load_png(&png).unwrap();
        let mut p = Psx::new();
        p.load_bios(&std::fs::read(r.join("openbios.bin")).unwrap()).unwrap();
        p.load_exe_after_bios(&std::fs::read(&exe).unwrap()).unwrap();
        for _ in 0..150 {
            p.run_frame();
        }
        let mut buf = vec![];
        let (w, h) = p.render(&mut buf);
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (9999, 9999, 0, 0, 0);
        let mut samples = vec![];
        for y in 0..rh.min(h as usize) {
            for x in 0..rw.min(w as usize) {
                let o = &buf[(y * w as usize + x) * 4..][..3];
                let rr = refpx[y * rw + x];
                if (0..3).any(|c| o[c] >> 3 != rr[c] >> 3) {
                    n += 1;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                    if samples.len() < 6 {
                        samples.push(format!("({},{}) ours {:?} ref {:?}", x, y, &o[..3], rr));
                    }
                }
            }
        }
        println!("{}: ref {}x{}, ours {}x{}: {} differ in x {}..{} y {}..{}", name, rw, rh, w, h, n, x0, x1, y0, y1);
        for s in samples {
            println!("  {}", s);
        }
    }
}

/// The 640x480 24-bit tests (decompressors + image loader) set the 480-line
/// bit without interlace, which per psx-spx displays 240 lines; compare the
/// decoded 24-bit picture straight from VRAM instead of the display.
#[test]
fn decompressed_24bit_images_match_exactly() {
    let Some(r) = root() else { return eprintln!("skipped") };
    for name in [
        "ImageLoad/ImageLoad",
        "Compress/LZ77/LZ77Decode/LZ77Decode",
        "Compress/HUFFMAN/HUFFMANDecode/HUFFMANDecode",
        "Compress/HUFFMAN/HUFFMANVRAM/HUFFMANVRAM",
    ] {
        let base = r.join("peterlemon").join(name);
        let (rw, rh, refpx) = load_png(&base.with_extension("png")).unwrap();
        let mut p = Psx::new();
        p.load_bios(&std::fs::read(r.join("openbios.bin")).unwrap()).unwrap();
        p.load_exe_after_bios(&std::fs::read(base.with_extension("exe")).unwrap()).unwrap();
        for _ in 0..600 {
            p.run_frame();
        }
        let vram = &p.bus.gpu.vram;
        let byte_at = |x: usize, y: usize, b: usize| -> u8 {
            let hw = vram[y * 1024 + (b / 2)];
            let _ = x;
            (hw >> ((b & 1) * 8)) as u8
        };
        let mut diff = 0;
        for y in 0..rh {
            for x in 0..rw {
                let ours = [byte_at(x, y, 3 * x), byte_at(x, y, 3 * x + 1), byte_at(x, y, 3 * x + 2)];
                if ours != refpx[y * rw + x] {
                    diff += 1;
                }
            }
        }
        println!("{}: {} of {} pixels differ", name, diff, rw * rh);
        assert_eq!(diff, 0, "{}", name);
    }
}
