//! Full boot-path tests using OpenBIOS (MIT, from PCSX-Redux). Skipped when
//! test-assets/openbios.bin is absent (fetch with tools/fetch-test-assets.sh).

use ps1wasm_cli::testkit::{bootable_disc, demo_exe, DEMO_PIXELS};
use ps1wasm_core::disc::{Disc, MemReader};
use ps1wasm_core::Psx;

fn openbios() -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets/openbios.bin");
    std::fs::read(p).ok()
}

fn check_demo_screen(p: &Psx) {
    let mut buf = vec![];
    let (w, h) = p.render(&mut buf);
    assert_eq!((w, h), (320, 240));
    for ((x, y), rgb) in DEMO_PIXELS {
        let i = (y * w as usize + x) * 4;
        let got = ((buf[i] as u32) << 16) | ((buf[i + 1] as u32) << 8) | buf[i + 2] as u32;
        assert_eq!(got, rgb, "pixel ({},{})", x, y);
    }
}

#[test]
fn openbios_kernel_boots() {
    let Some(bios) = openbios() else { eprintln!("skipped: no openbios.bin"); return };
    let mut p = Psx::new();
    p.load_bios(&bios).unwrap();
    for _ in 0..120 {
        p.run_frame();
    }
    assert!(p.tty.contains("OpenBios"), "tty: {}", p.tty);
    assert!(p.tty.contains("KERNEL SETUP!"));
    assert_eq!(p.bus.unhandled_io, 0);
}

#[test]
fn exe_sideloaded_after_bios_runs() {
    let Some(bios) = openbios() else { eprintln!("skipped: no openbios.bin"); return };
    let mut p = Psx::new();
    p.load_bios(&bios).unwrap();
    p.load_exe_after_bios(&demo_exe()).unwrap();
    for _ in 0..300 {
        p.run_frame();
    }
    check_demo_screen(&p);
}

#[test]
fn boots_from_cd_through_iso9660_and_system_cnf() {
    let Some(bios) = openbios() else { eprintln!("skipped: no openbios.bin"); return };
    let raw = bootable_disc(&demo_exe());
    let n = raw.len() as u64;
    let mut p = Psx::new();
    p.load_bios(&bios).unwrap();
    p.insert_disc(Disc::from_single(n, true, Box::new(MemReader(vec![raw]))).unwrap());
    for _ in 0..800 {
        p.run_frame();
    }
    assert!(p.tty.contains("BOOT =\tcdrom:\\PSX.EXE;1"), "SYSTEM.CNF parsed: {}", p.tty);
    assert!(p.tty.contains("Execute !"));
    check_demo_screen(&p);
}
