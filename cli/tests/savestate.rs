//! Save-state completeness: save at frame N, run M more frames -> state A;
//! load the save, run the same M frames -> state B. A must equal B
//! byte-for-byte (any field missing from the state makes them diverge).

use ps1wasm_cli::testkit::host::{openbios, test_assets};
use ps1wasm_cli::testkit::{bootable_disc, demo_exe, fuzz_cpu_exe, DEMO_PIXELS};
use ps1wasm_core::disc::{Disc, MemReader};
use ps1wasm_core::Psx;

fn frames(p: &mut Psx, n: u32) {
    for _ in 0..n {
        p.run_frame();
    }
}

/// Core determinism check. `make` builds an identical machine (needed to
/// re-attach the disc); `at` = frame to save at; `after` = frames to run.
fn check(name: &str, make: &dyn Fn() -> Psx, at: u32, after: u32) {
    let mut p = make();
    frames(&mut p, at);
    let saved = p.save_state();
    frames(&mut p, after);
    let a = p.save_state();

    // Same machine, continuing from the save
    let mut q = make();
    q.load_state(&saved).expect("load");
    frames(&mut q, after);
    let b = q.save_state();
    if a != b {
        let first = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        panic!("{}: states diverge at byte {} of {} (lengths {} / {})", name, first, a.len(), a.len(), b.len());
    }

    // Loading into the *same* running machine (rewind) works too
    p.load_state(&saved).expect("rewind");
    frames(&mut p, after);
    assert!(p.save_state() == a, "{}: rewind in place diverges", name);
}

#[test]
fn deterministic_without_bios() {
    // demo EXE, no BIOS: CPU + GPU + timers + SPU ticking
    let make = || {
        let mut p = Psx::new();
        p.load_exe_now(&demo_exe()).unwrap();
        p
    };
    check("demo exe", &make, 30, 60);
}

#[test]
fn deterministic_cd_boot_saved_mid_loading() {
    let bios = openbios();
    let raw = bootable_disc(&demo_exe());
    let make = || {
        let mut p = Psx::new();
        p.load_bios(&bios).unwrap();
        let n = raw.len() as u64;
        p.insert_disc(Disc::from_single(n, true, Box::new(MemReader(vec![raw.clone()]))).unwrap());
        p
    };
    // Save at moments with CD activity in flight: first frame where the
    // drive is reading sectors, and first frame with a command pending.
    let mut p = make();
    let (mut reading_at, mut pending_at) = (None, None);
    for f in 1..900 {
        p.run_frame();
        if reading_at.is_none() && p.bus.cdrom.is_reading() {
            reading_at = Some(f);
        }
        if pending_at.is_none() && p.bus.cdrom.command_pending() {
            pending_at = Some(f);
        }
    }
    let reading_at = reading_at.expect("drive never read");
    let pending_at = pending_at.expect("no command ever pending");
    for at in [reading_at, pending_at, 100, 480] {
        check(&format!("cd boot @{}", at), &make, at, 200);
    }
    // and the restored machine still reaches the right screen
    let mut p = make();
    frames(&mut p, 400);
    let s = p.save_state();
    let mut q = make();
    q.load_state(&s).unwrap();
    frames(&mut q, 400);
    let mut buf = vec![];
    let (w, _) = q.render(&mut buf);
    for ((x, y), rgb) in DEMO_PIXELS {
        let i = (y * w as usize + x) * 4;
        assert_eq!(((buf[i] as u32) << 16) | ((buf[i + 1] as u32) << 8) | buf[i + 2] as u32, rgb);
    }
}

#[test]
fn deterministic_through_hardware_test_programs() {
    let bios = openbios();
    for (exe, at) in [
        ("mdec/frame/frame-15bit-dma.exe", 160), // mid MDEC decode
        ("gte/test-all/test-all.exe", 150),       // GTE heavy
        ("spu/test/test.exe", 120),               // SPU playback
        ("gpu/lines/lines.exe", 100),
    ] {
        let Some(path) = test_assets("ps1-tests").map(|t| t.join(exe)) else { continue };
        let Ok(data) = std::fs::read(&path) else { continue };
        let make = || {
            let mut p = Psx::new();
            p.load_bios(&bios).unwrap();
            p.load_exe_after_bios(&data).unwrap();
            p
        };
        check(exe, &make, at, 120);
    }
}

#[test]
fn deterministic_random_cpu_programs() {
    let bios = openbios();
    for seed in 1..4 {
        let exe = fuzz_cpu_exe(seed, 400);
        let make = || {
            let mut p = Psx::new();
            p.load_bios(&bios).unwrap();
            p.load_exe_after_bios(&exe).unwrap();
            p
        };
        check(&format!("fuzz {}", seed), &make, 90, 60);
    }
}

#[test]
fn bad_states_are_rejected_and_leave_machine_untouched() {
    let mut p = Psx::new();
    p.load_exe_now(&demo_exe()).unwrap();
    frames(&mut p, 10);
    let good = p.save_state();
    frames(&mut p, 5);
    let before = p.save_state();
    assert!(p.load_state(&good[..good.len() / 2]).is_err(), "truncated");
    assert!(p.load_state(b"garbage").is_err(), "garbage");
    let mut wrong = good.clone();
    wrong[8] ^= 0xFF; // version
    assert!(p.load_state(&wrong).is_err(), "version mismatch");
    let mut extra = good.clone();
    extra.push(0);
    assert!(p.load_state(&extra).is_err(), "trailing data");
    assert!(p.save_state() == before, "machine unchanged after failed loads");
    assert!(p.load_state(&good).is_ok());
}

#[test]
fn state_size_is_reasonable() {
    let mut p = Psx::new();
    let s = p.save_state();
    // RAM 2M + VRAM 1M + SPU 512K + small stuff
    assert!(s.len() > 3_500_000 && s.len() < 4_500_000, "{} bytes", s.len());
}

/// Complete internal state via Debug (every field of every struct), with
/// deliberately-unsaved transient fields normalized.
fn dump(p: &mut Psx) -> String {
    p.tty.clear();
    p.bus.spu.output.clear();
    p.bus.unhandled_io = 0;
    format!("{:?}", p)
}

/// Structural completeness: after load_state into a *fresh* machine, every
/// field must equal the original - even fields whose values aren't live.
fn check_structural(name: &str, make: &dyn Fn() -> Psx, at: u32) {
    let mut p = make();
    frames(&mut p, at);
    let s = p.save_state();
    let mut q = make();
    q.load_state(&s).unwrap();
    let (a, b) = (dump(&mut p), dump(&mut q));
    if a != b {
        let i = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        let lo = i.saturating_sub(120);
        panic!(
            "{}: field not restored near:\n  orig: ...{}...\n  load: ...{}...",
            name,
            &a[lo..(i + 60).min(a.len())],
            &b[lo..(i + 60).min(b.len())]
        );
    }
}

#[test]
fn structural_every_field_restored() {
    let make = || {
        let mut p = Psx::new();
        p.load_exe_now(&demo_exe()).unwrap();
        p
    };
    check_structural("demo exe", &make, 20);
    let bios = openbios();
    let raw = bootable_disc(&demo_exe());
    let make_cd = || {
        let mut p = Psx::new();
        p.load_bios(&bios).unwrap();
        let n = raw.len() as u64;
        p.insert_disc(Disc::from_single(n, true, Box::new(MemReader(vec![raw.clone()]))).unwrap());
        p
    };
    for at in [6, 50, 300, 450] {
        check_structural(&format!("cd boot @{}", at), &make_cd, at);
    }
    for (exe, at) in [("mdec/frame/frame-15bit-dma.exe", 160), ("spu/test/test.exe", 120), ("gte/test-all/test-all.exe", 150)] {
        let Some(path) = test_assets("ps1-tests").map(|t| t.join(exe)) else { continue };
        let Ok(data) = std::fs::read(path) else { continue };
        let make = || {
            let mut p = Psx::new();
            p.load_bios(&bios).unwrap();
            p.load_exe_after_bios(&data).unwrap();
            p
        };
        check_structural(exe, &make, at);
    }
}

/// Save at exact mid-operation moments that frame boundaries never hit.
#[test]
fn structural_mid_operation_moments() {
    use ps1wasm_core::bus::Size;
    let roundtrip = |p: &mut Psx, name: &str| {
        let s = p.save_state();
        let mut q = Psx::new();
        q.load_exe_now(&demo_exe()).unwrap(); // different machine on purpose
        q.bus.bios = p.bus.bios.clone(); // BIOS ROM isn't part of a state (like the disc)
        q.load_state(&s).unwrap();
        if let Some(d) = p.bus.cdrom.disc.take() {
            // disc isn't part of a state: compare without it
            let a = dump(p);
            p.bus.cdrom.disc = Some(d);
            let b = dump(&mut q);
            if a != b {
                let i = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(0);
                let lo = i.saturating_sub(150);
                panic!("{}: field not restored near:\n orig ...{}...\n load ...{}...", name, &a[lo..(i + 80).min(a.len())], &b[lo..(i + 80).min(b.len())]);
            }
        } else {
            assert!(dump(p) == dump(&mut q), "{}: field not restored", name);
        }
    };

    // 1. Controller byte in flight (SIO0 transfer timer running)
    let mut p = Psx::new();
    p.load_exe_now(&demo_exe()).unwrap();
    p.bus.write(0x1F80_104A, 0x1003, Size::Half);
    p.bus.write(0x1F80_1040, 0x01, Size::Byte);
    p.bus.add_cycles(100);
    roundtrip(&mut p, "pad mid-transfer");

    // 2. MDEC command half-received (remaining > 0)
    let mut p = Psx::new();
    p.load_exe_now(&demo_exe()).unwrap();
    p.bus.write(0x1F80_1820, 0x3000_0010, Size::Word);
    for _ in 0..5 {
        p.bus.write(0x1F80_1820, 0xFE00_FE00, Size::Word);
    }
    roundtrip(&mut p, "mdec mid-command");

    // 3. CD sector delivered but not yet read out
    let bios = openbios();
    let raw = bootable_disc(&demo_exe());
    let mut p = Psx::new();
    p.load_bios(&bios).unwrap();
    let n = raw.len() as u64;
    p.insert_disc(Disc::from_single(n, true, Box::new(MemReader(vec![raw]))).unwrap());
    let limit = 33_868_800u64 * 20;
    while !p.bus.cdrom.has_unread_sector() && p.bus.cycles < limit {
        p.step();
    }
    assert!(p.bus.cdrom.has_unread_sector(), "never caught an unread sector");
    roundtrip(&mut p, "cd sector pending");
}
