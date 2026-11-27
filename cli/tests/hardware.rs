//! Regression tests against real-hardware results from JaCzekanski/ps1-tests
//! (VRAM dumps + TTY logs captured on a PlayStation), run under OpenBIOS.
//! Skipped when test-assets is absent (tools/fetch-test-assets.sh).
//! To diagnose a failing VRAM test (error histogram, diff image):
//!   ps1wasm-run --exe test-assets/ps1-tests/T/T.exe --compare-vram test-assets/ps1-tests/T/vram.png --diff /tmp/diff.png

use ps1wasm_core::gpu::{VRAM_H, VRAM_W};
use ps1wasm_core::Psx;
use ps1wasm_cli::testkit::host;
use std::path::Path;

/// Run a ps1-tests program (`dir/exe`) under OpenBIOS.
fn run(exe: &Path, frames: u32, fill: Option<u16>) -> Psx {
    host::run_exe(&host::openbios(), &std::fs::read(exe).unwrap(), frames, fill)
}

/// Number of program-written pixels (in `region`) differing from the
/// hardware dump at 5-bit precision.
fn vram_diff(test: &str, exe: &str, reference: &str, region: Option<(usize, usize, usize, usize)>) -> Option<usize> {
    vram_diff_err(test, exe, reference, region).map(|(d, _)| d)
}

/// (differing pixels, largest per-channel error in 5-bit units)
fn vram_diff_err(test: &str, exe: &str, reference: &str, region: Option<(usize, usize, usize, usize)>) -> Option<(usize, u8)> {
    let dir = host::test_assets("ps1-tests")?.join(test);
    let p = run(&dir.join(exe), 300, Some(0x0000));
    let q = run(&dir.join(exe), 300, Some(0x7FFF));
    let refpx = host::load_png(&dir.join(reference)).unwrap().2.concat();
    let mut diff = 0;
    let mut max_err = 0u8;
    for k in 0..VRAM_W * VRAM_H {
        let (x, y) = (k % VRAM_W, k / VRAM_W);
        if p.bus.gpu.vram[k] != q.bus.gpu.vram[k] {
            continue; // not written by the program
        }
        if let Some((rx, ry, rw, rh)) = region {
            if x < rx || x >= rx + rw || y < ry || y >= ry + rh {
                continue;
            }
        }
        let v = p.bus.gpu.vram[k];
        let ours = [(v & 31) as u8, ((v >> 5) & 31) as u8, ((v >> 10) & 31) as u8];
        let theirs = [refpx[3 * k] >> 3, refpx[3 * k + 1] >> 3, refpx[3 * k + 2] >> 3];
        if ours != theirs {
            diff += 1;
            for c in 0..3 {
                max_err = max_err.max(ours[c].abs_diff(theirs[c]));
            }
        }
    }
    Some((diff, max_err))
}

macro_rules! pixel_perfect {
    ($name:ident, $test:expr, $exe:expr) => {
        pixel_perfect!($name, $test, $exe, None);
    };
    ($name:ident, $test:expr, $exe:expr, $region:expr) => {
        #[test]
        fn $name() {
            match vram_diff($test, $exe, "vram.png", $region) {
                None => eprintln!("skipped: no test assets"),
                Some(d) => assert_eq!(d, 0, "{} pixels differ from the hardware dump", d),
            }
        }
    };
}

pixel_perfect!(gpu_clipping, "gpu/clipping", "clipping.exe");
pixel_perfect!(gpu_clut_cache, "gpu/clut-cache", "clut-cache.exe");
pixel_perfect!(gpu_quad, "gpu/quad", "quad.exe");
pixel_perfect!(gpu_rectangles, "gpu/rectangles", "rectangles.exe");
pixel_perfect!(gpu_texture_flip, "gpu/texture-flip", "texture-flip.exe");
pixel_perfect!(gpu_texture_overflow, "gpu/texture-overflow", "texture-overflow.exe");
pixel_perfect!(gpu_triangle, "gpu/triangle", "triangle.exe");
pixel_perfect!(gpu_uv_interpolation, "gpu/uv-interpolation", "uv-interpolation.exe");
pixel_perfect!(gpu_vram_to_vram_overlap, "gpu/vram-to-vram-overlap", "vram-to-vram-overlap.exe");
// The reference dump comes from a test build that didn't clear VRAM; compare
// the drawn area only.
pixel_perfect!(gpu_transparency, "gpu/transparency", "transparency.exe", Some((0, 0, 320, 240)));

#[test]
fn gpu_lines_nearly_exact() {
    // 64 pixels at shaded-polyline joins still differ (documented).
    if let Some(d) = vram_diff("gpu/lines", "lines.exe", "vram.png", None) {
        assert!(d <= 64, "{} pixels differ (was 64)", d);
    }
}

#[test]
fn mdec_frame_dma_decodes_full_frame() {
    // DMA output path; only 1-LSB rounding differences remain (IDCT model).
    if let Some((d, e)) = vram_diff_err("mdec/frame", "frame-15bit-dma.exe", "vram-15bit.png", Some((0, 0, 320, 240))) {
        assert!(e <= 1, "{} pixels differ, max error {} (expected only 1-LSB rounding)", d, e);
    }
}

/// TTY-log tests: every line of the hardware log must appear, no "fail".
fn log_test(test: &str, exe: &str, frames: u32) {
    let Some(tests) = host::test_assets("ps1-tests") else { return eprintln!("skipped: no test assets") };
    let dir = tests.join(test);
    let p = run(&dir.join(exe), frames, None);
    let reference = std::fs::read_to_string(dir.join("psx.log")).unwrap();
    let out: Vec<&str> = p.tty.lines().map(|l| l.trim()).collect();
    for line in reference.lines() {
        let line = line.trim_start_matches('%').trim().trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        assert!(out.contains(&line), "missing hardware line {:?}\n--- ours:\n{}", line, p.tty);
    }
    assert!(!p.tty.lines().any(|l| l.starts_with("fail")), "failures:\n{}", p.tty);
}

#[test]
fn log_cpu_cop() {
    log_test("cpu/cop", "cop.exe", 300);
}
#[test]
fn log_dma_dpcr() {
    log_test("dma/dpcr", "dpcr.exe", 300);
}
#[test]
fn log_dma_otc() {
    log_test("dma/otc-test", "otc-test.exe", 300);
}
#[test]
fn log_gpu_gp0_e1() {
    log_test("gpu/gp0-e1", "gp0-e1.exe", 300);
}
#[test]
fn log_gpu_mask_bit() {
    log_test("gpu/mask-bit", "mask-bit.exe", 300);
}
#[test]
fn log_gte_test_all() {
    log_test("gte/test-all", "test-all.exe", 900);
}
