//! The controller demo reacts to every PS1 input: drives the emulated pad
//! and checks the program's on-screen controller diagram.

use ps1wasm_cli::testkit::{controller_demo_exe, controller_demo_layout, DEMO_BG, DEMO_LED};
use ps1wasm_core::gpu::VRAM_W;
use ps1wasm_core::Psx;

/// 24-bit 0xBBGGRR -> VRAM 15-bit as the GPU stores it.
fn c15(c: u32) -> u16 {
    ((c & 0xFF) >> 3) as u16 | ((((c >> 8) & 0xFF) >> 3) << 5) as u16 | ((((c >> 16) & 0xFF) >> 3) << 10) as u16
}

fn px(p: &Psx, x: u32, y: u32) -> u16 {
    p.bus.gpu.vram[y as usize * VRAM_W + x as usize] & 0x7FFF
}

fn boot() -> Psx {
    let mut p = Psx::new();
    p.load_exe_now(&controller_demo_exe()).unwrap();
    for _ in 0..10 {
        p.run_frame();
    }
    p
}

fn frames(p: &mut Psx, n: u32) {
    for _ in 0..n {
        p.run_frame();
    }
}

#[test]
fn draws_the_diagram_with_everything_released() {
    let p = boot();
    assert_eq!(px(&p, 5, 5), c15(DEMO_BG), "background");
    for b in controller_demo_layout() {
        assert_eq!(px(&p, b.x + 3, b.y + 3), c15(b.off), "button {:#06x} drawn released", b.mask);
    }
    assert_eq!(px(&p, DEMO_LED.0 + 2, DEMO_LED.1 + 2), c15(0x30), "LED off in digital mode");
}

#[test]
fn every_button_lights_up_alone() {
    let mut p = boot();
    let layout = controller_demo_layout();
    for b in &layout {
        p.set_buttons(0, b.mask);
        frames(&mut p, 3);
        for other in &layout {
            let want = if other.mask == b.mask { other.on } else { other.off };
            assert_eq!(px(&p, other.x + 3, other.y + 3), c15(want), "holding {:#06x}: button {:#06x}", b.mask, other.mask);
        }
        p.set_buttons(0, 0);
        frames(&mut p, 3);
        assert_eq!(px(&p, b.x + 3, b.y + 3), c15(b.off), "{:#06x} released again", b.mask);
    }
}

#[test]
fn all_buttons_at_once() {
    let mut p = boot();
    p.set_buttons(0, 0xFFFF);
    frames(&mut p, 3);
    for b in controller_demo_layout() {
        assert_eq!(px(&p, b.x + 3, b.y + 3), c15(b.on), "{:#06x}", b.mask);
    }
}

#[test]
fn analog_sticks_and_led_in_dualshock_analog_mode() {
    let mut p = boot();
    assert!(p.bus.sio.ports[0].pad.toggle_analog());
    // left stick hard up-left, right stick hard down-right
    p.bus.sio.ports[0].pad.sticks = [0xFF, 0xFF, 0x00, 0x00]; // rx, ry, lx, ly
    frames(&mut p, 3);
    assert_eq!(px(&p, DEMO_LED.0 + 2, DEMO_LED.1 + 2), c15(0xFF), "LED lit in analog mode");
    let white = c15(0xFFFFFF);
    assert_eq!(px(&p, 60 + 2, 150 + 2), white, "left dot at top-left of its box");
    assert_eq!(px(&p, 190 + 63 + 2, 150 + 63 + 2), white, "right dot at bottom-right of its box");
    // centre both sticks
    p.bus.sio.ports[0].pad.sticks = [0x80; 4];
    frames(&mut p, 3);
    assert_eq!(px(&p, 60 + 32 + 2, 150 + 32 + 2), white, "left dot centred");
    assert_ne!(px(&p, 60 + 2, 150 + 2), white, "old position cleared");
}

#[test]
fn works_with_a_plain_digital_pad() {
    let mut p = boot();
    p.bus.sio.ports[0].pad.dualshock = false;
    p.set_buttons(0, ps1wasm_core::pad::buttons::CIRCLE);
    frames(&mut p, 3);
    let circle = controller_demo_layout().into_iter().find(|b| b.mask == ps1wasm_core::pad::buttons::CIRCLE).unwrap();
    assert_eq!(px(&p, circle.x + 3, circle.y + 3), c15(circle.on));
}

#[test]
fn no_controller_means_nothing_pressed() {
    let mut p = boot();
    p.bus.sio.ports[0].pad_connected = false;
    p.set_buttons(0, 0xFFFF);
    frames(&mut p, 3);
    for b in controller_demo_layout() {
        assert_eq!(px(&p, b.x + 3, b.y + 3), c15(b.off), "{:#06x}", b.mask);
    }
}

#[test]
fn runs_under_openbios_too() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/static/openbios/openbios.bin");
    let Ok(bios) = std::fs::read(path) else { return eprintln!("skipped: no openbios") };
    let mut p = Psx::new();
    p.load_bios(&bios).unwrap();
    p.load_exe_after_bios(&controller_demo_exe()).unwrap();
    frames(&mut p, 300);
    let cross = controller_demo_layout().into_iter().find(|b| b.mask == ps1wasm_core::pad::buttons::CROSS).unwrap();
    assert_eq!(px(&p, cross.x + 3, cross.y + 3), c15(cross.off));
    p.set_buttons(0, cross.mask);
    frames(&mut p, 3);
    assert_eq!(px(&p, cross.x + 3, cross.y + 3), c15(cross.on));
}

