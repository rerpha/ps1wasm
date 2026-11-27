//! Browser-independent frontend logic (unit-tested natively).

use ps1wasm_core::disc::{CueSheet, Disc, MemReader};
use ps1wasm_core::pad::buttons::*;

// ------------------------------------------------------------------ base64
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for q in bytes.chunks(4) {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for &c in &q[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------------ input
/// PS buttons in display order with their names.
pub const BUTTONS: [(u16, &str); 16] = [
    (UP, "Up"),
    (DOWN, "Down"),
    (LEFT, "Left"),
    (RIGHT, "Right"),
    (CROSS, "Cross \u{2715}"),
    (CIRCLE, "Circle \u{25EF}"),
    (SQUARE, "Square \u{25A1}"),
    (TRIANGLE, "Triangle \u{25B3}"),
    (START, "Start"),
    (SELECT, "Select"),
    (L1, "L1"),
    (R1, "R1"),
    (L2, "L2"),
    (R2, "R2"),
    (L3, "L3"),
    (R3, "R3"),
];

/// Keyboard bindings by `KeyboardEvent.code` (layout independent).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMap {
    /// (button bit, key code)
    pub binds: Vec<(u16, String)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        let d = [
            (UP, "ArrowUp"),
            (DOWN, "ArrowDown"),
            (LEFT, "ArrowLeft"),
            (RIGHT, "ArrowRight"),
            (CROSS, "KeyX"),
            (CIRCLE, "KeyC"),
            (SQUARE, "KeyZ"),
            (TRIANGLE, "KeyS"),
            (START, "Enter"),
            (SELECT, "ShiftRight"),
            (L1, "KeyQ"),
            (R1, "KeyE"),
            (L2, "Digit1"),
            (R2, "Digit3"),
            (L3, "KeyA"),
            (R3, "KeyD"),
        ];
        KeyMap { binds: d.iter().map(|(b, k)| (*b, k.to_string())).collect() }
    }
}

impl KeyMap {
    pub fn button_for(&self, code: &str) -> Option<u16> {
        self.binds.iter().find(|(_, k)| k == code).map(|(b, _)| *b)
    }

    pub fn key_for(&self, button: u16) -> &str {
        self.binds.iter().find(|(b, _)| *b == button).map(|(_, k)| k.as_str()).unwrap_or("")
    }

    /// Bind `code` to `button`; a key drives one button only, so any other
    /// button that used this key loses it.
    pub fn bind(&mut self, button: u16, code: &str) {
        for (b, k) in self.binds.iter_mut() {
            if k == code && *b != button {
                k.clear();
            }
        }
        match self.binds.iter_mut().find(|(b, _)| *b == button) {
            Some((_, k)) => *k = code.to_string(),
            None => self.binds.push((button, code.to_string())),
        }
    }

    /// "bit=code;bit=code" (stored in localStorage)
    pub fn serialize(&self) -> String {
        self.binds.iter().map(|(b, k)| format!("{}={}", b, k)).collect::<Vec<_>>().join(";")
    }

    pub fn parse(s: &str) -> KeyMap {
        let mut m = KeyMap::default();
        for part in s.split(';') {
            if let Some((b, k)) = part.split_once('=') {
                if let Ok(b) = b.parse::<u16>() {
                    if BUTTONS.iter().any(|(x, _)| *x == b) {
                        m.bind(b, k);
                    }
                }
            }
        }
        m
    }
}

/// Human-friendly key label for the settings table.
pub fn key_label(code: &str) -> String {
    if code.is_empty() {
        return "\u{2014}".into();
    }
    let s = code.strip_prefix("Key").or_else(|| code.strip_prefix("Digit")).unwrap_or(code);
    match s {
        "ArrowUp" => "\u{2191}".into(),
        "ArrowDown" => "\u{2193}".into(),
        "ArrowLeft" => "\u{2190}".into(),
        "ArrowRight" => "\u{2192}".into(),
        "ShiftRight" => "Right Shift".into(),
        "ShiftLeft" => "Left Shift".into(),
        other => other.to_string(),
    }
}

/// W3C "standard" gamepad mapping: button index -> PS button.
pub fn gamepad_button(index: usize) -> Option<u16> {
    Some(match index {
        0 => CROSS,
        1 => CIRCLE,
        2 => SQUARE,
        3 => TRIANGLE,
        4 => L1,
        5 => R1,
        6 => L2,
        7 => R2,
        8 => SELECT,
        9 => START,
        10 => L3,
        11 => R3,
        12 => UP,
        13 => DOWN,
        14 => LEFT,
        15 => RIGHT,
        _ => return None,
    })
}

/// Left stick to D-pad with a dead zone.
pub fn stick_to_dpad(x: f64, y: f64) -> u16 {
    const DZ: f64 = 0.5;
    let mut b = 0;
    if x < -DZ {
        b |= LEFT;
    }
    if x > DZ {
        b |= RIGHT;
    }
    if y < -DZ {
        b |= UP;
    }
    if y > DZ {
        b |= DOWN;
    }
    b
}

// ------------------------------------------------------------------ memory cards
pub const MEMCARD_SIZE: usize = 128 * 1024;

/// Accepts raw (.mcr/.mcd/.mc/.srm), DexDrive (.gme) and PSP/PS3 (.vmp)
/// memory card images; returns the raw 128 KiB card.
pub fn import_memcard(data: &[u8]) -> Result<Vec<u8>, String> {
    let raw = if data.len() == MEMCARD_SIZE {
        data
    } else if data.len() == MEMCARD_SIZE + 3904 && data.starts_with(b"123-456-STD") {
        &data[3904..]
    } else if data.len() == MEMCARD_SIZE + 0x80 && data.starts_with(b"\0PMV") {
        &data[0x80..]
    } else {
        return Err(format!("not a memory card image ({} bytes; expected 128 KiB)", data.len()));
    };
    if &raw[0..2] != b"MC" {
        return Err("memory card image has no 'MC' header (unformatted or corrupt)".into());
    }
    Ok(raw.to_vec())
}

// ------------------------------------------------------------------ dropped files
pub const BIOS_SIZE: usize = 512 * 1024;

#[derive(Debug, PartialEq)]
pub enum Kind {
    Bios,
    MemCard,
    State,
    Game,
}

/// Decide what a dropped/selected file is.
pub fn classify(name: &str, size: usize) -> Kind {
    match ext(name).as_str() {
        "mcr" | "mcd" | "gme" | "vmp" | "mc" | "srm" | "mcs" => Kind::MemCard,
        "psxstate" => Kind::State,
        "cue" | "iso" | "img" | "exe" | "psx" | "psexe" | "ps-exe" => Kind::Game,
        // A 512 KiB file can't be a disc image (not a multiple of 2352/2048
        // sectors that make sense) - it's a BIOS dump.
        _ if size == BIOS_SIZE => Kind::Bios,
        _ => Kind::Game,
    }
}

/// Best-effort BIOS description from the ROM's version string.
pub fn bios_description(data: &[u8]) -> String {
    if data.len() != BIOS_SIZE {
        return "invalid".into();
    }
    let text = String::from_utf8_lossy(&data[..0x1000]);
    if text.contains("OpenBios") || String::from_utf8_lossy(data).contains("OpenBios") {
        return "OpenBIOS".into();
    }
    if let Some(i) = text.find("System ROM Version") {
        let v: String = text[i + 19..].chars().take_while(|c| !c.is_control()).collect();
        return format!("Sony BIOS {}", v.trim());
    }
    "BIOS".into()
}

// ------------------------------------------------------------------ loading
pub enum Loaded {
    Disc(Disc, String),
    Exe(Vec<u8>, String),
}

fn ext(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase()
}

/// Turn a set of user-selected files into something bootable.
pub fn load_files(mut files: Vec<(String, Vec<u8>)>) -> Result<Loaded, String> {
    if files.is_empty() {
        return Err("no files selected".into());
    }
    // Executables are recognised by their header, whatever the extension
    // (homebrew uses .exe, .psx, .psexe, .ps-exe, ...).
    if let Some(pos) = files.iter().position(|(n, d)| d.starts_with(b"PS-X EXE") || matches!(ext(n).as_str(), "exe" | "psx" | "psexe" | "ps-exe")) {
        let (name, data) = files.swap_remove(pos);
        ps1wasm_core::parse_exe(&data)?;
        return Ok(Loaded::Exe(data, name));
    }
    if let Some(pos) = files.iter().position(|(n, _)| ext(n) == "cue") {
        let (cue_name, cue) = files.swap_remove(pos);
        let text = String::from_utf8_lossy(&cue).to_string();
        let sheet = CueSheet::parse(&text)?;
        let mut datas = vec![];
        for wanted in &sheet.files {
            let w = basename(wanted);
            let idx = files
                .iter()
                .position(|(n, _)| basename(n) == w)
                .ok_or_else(|| format!("cue references '{}' but it wasn't selected", wanted))?;
            datas.push(files.swap_remove(idx).1);
        }
        let sizes: Vec<u64> = datas.iter().map(|d| d.len() as u64).collect();
        let disc = Disc::from_cue(&sheet, &sizes, Box::new(MemReader(datas)))?;
        return Ok(Loaded::Disc(disc, cue_name));
    }
    // Single image: pick the largest .bin/.img/.iso
    let pos = files
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| matches!(ext(n).as_str(), "bin" | "img" | "iso"))
        .max_by_key(|(_, (_, d))| d.len())
        .map(|(i, _)| i)
        .ok_or("select a .cue (with its .bin files), a .bin/.iso image, or a PS-X .exe")?;
    let (name, data) = files.swap_remove(pos);
    const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];
    let raw = data.len() % 2352 == 0 && data.len() >= 12 && data[..12] == SYNC;
    if !raw && data.len() % 2048 != 0 {
        return Err(format!("{}: size {} is neither 2352- nor 2048-byte sectors", name, data.len()));
    }
    let n = data.len() as u64;
    let disc = Disc::from_single(n, raw, Box::new(MemReader(vec![data])))?;
    Ok(Loaded::Disc(disc, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_and_padding() {
        for n in 0..10 {
            let d: Vec<u8> = (0..n).map(|k| (k * 37 + 5) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&d)).unwrap(), d, "len {}", n);
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_encode(b"M"), "TQ==");
        assert_eq!(b64_decode("TWE=").unwrap(), b"Ma");
        assert!(b64_decode("abc").is_none());
        assert!(b64_decode("ab!=").is_none());
        let big: Vec<u8> = (0..128 * 1024).map(|k| (k ^ (k >> 8)) as u8).collect();
        assert_eq!(b64_decode(&b64_encode(&big)).unwrap(), big);
    }

    #[test]
    fn keymap_defaults_rebind_and_persist() {
        let mut m = KeyMap::default();
        assert_eq!(m.button_for("KeyX"), Some(CROSS));
        assert_eq!(m.button_for("Enter"), Some(START));
        assert_eq!(m.button_for("KeyP"), None);
        m.bind(CROSS, "KeyK");
        assert_eq!(m.button_for("KeyK"), Some(CROSS));
        assert_eq!(m.button_for("KeyX"), None);
        // stealing a key unbinds it from the previous button
        m.bind(CIRCLE, "KeyK");
        assert_eq!(m.button_for("KeyK"), Some(CIRCLE));
        assert_eq!(m.key_for(CROSS), "");
        let back = KeyMap::parse(&m.serialize());
        assert_eq!(back, m);
        assert_eq!(KeyMap::parse("garbage;99999=KeyQ;x=y"), KeyMap::default());
        assert_eq!(key_label("KeyX"), "X");
        assert_eq!(key_label("Digit1"), "1");
        assert_eq!(key_label("ArrowUp"), "\u{2191}");
    }

    #[test]
    fn memcard_import_formats() {
        let mut raw = vec![0u8; MEMCARD_SIZE];
        raw[0..2].copy_from_slice(b"MC");
        assert_eq!(import_memcard(&raw).unwrap(), raw);
        let mut gme = b"123-456-STD".to_vec();
        gme.resize(3904, 0);
        gme.extend_from_slice(&raw);
        assert_eq!(import_memcard(&gme).unwrap(), raw);
        let mut vmp = b"\0PMV".to_vec();
        vmp.resize(0x80, 0);
        vmp.extend_from_slice(&raw);
        assert_eq!(import_memcard(&vmp).unwrap(), raw);
        assert!(import_memcard(&[0u8; 1000]).is_err());
        assert!(import_memcard(&vec![0u8; MEMCARD_SIZE]).is_err(), "no MC header");
    }

    #[test]
    fn classify_dropped_files() {
        assert_eq!(classify("SCPH1001.BIN", BIOS_SIZE), Kind::Bios);
        assert_eq!(classify("bios", BIOS_SIZE), Kind::Bios);
        assert_eq!(classify("Game (Track 1).bin", 300_000_000), Kind::Game);
        assert_eq!(classify("game.cue", 200), Kind::Game);
        assert_eq!(classify("card.mcr", MEMCARD_SIZE), Kind::MemCard);
        assert_eq!(classify("Card.GME", MEMCARD_SIZE + 3904), Kind::MemCard);
        assert_eq!(classify("slot1.psxstate", 4_000_000), Kind::State);
        assert_eq!(classify("demo.exe", BIOS_SIZE), Kind::Game, "extension wins over size");
        let mut bios = vec![0u8; BIOS_SIZE];
        bios[0x100..0x100 + 27].copy_from_slice(b"System ROM Version 4.1 12/1");
        assert!(bios_description(&bios).starts_with("Sony BIOS 4.1"));
    }

    #[test]
    fn input_mappings() {
        assert_eq!(gamepad_button(0), Some(CROSS));
        assert_eq!(gamepad_button(9), Some(START));
        assert_eq!(gamepad_button(15), Some(RIGHT));
        assert_eq!(gamepad_button(16), None);
        assert_eq!(stick_to_dpad(0.0, 0.0), 0);
        assert_eq!(stick_to_dpad(-0.9, 0.9), LEFT | DOWN);
        assert_eq!(stick_to_dpad(0.3, -0.3), 0, "dead zone");
    }

    fn raw_image(sectors: usize) -> Vec<u8> {
        let mut v = vec![0u8; 2352 * sectors];
        for s in 0..sectors {
            v[s * 2352 + 1..s * 2352 + 11].fill(0xFF);
        }
        v
    }

    #[test]
    fn load_cue_with_case_insensitive_names() {
        let cue = b"FILE \"Game (Track 1).BIN\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\nFILE \"Game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n".to_vec();
        let files = vec![
            ("game (track 2).bin".to_string(), vec![0u8; 2352 * 3]),
            ("Game.cue".to_string(), cue),
            ("game (track 1).bin".to_string(), raw_image(5)),
        ];
        match load_files(files).unwrap() {
            Loaded::Disc(d, name) => {
                assert_eq!(name, "Game.cue");
                assert_eq!(d.tracks.len(), 2);
                assert_eq!(d.tracks[1].start, 150 + 5);
            }
            _ => panic!("expected disc"),
        }
    }

    #[test]
    fn load_cue_missing_bin_is_an_error() {
        let cue = b"FILE \"a.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n".to_vec();
        let err = load_files(vec![("x.cue".into(), cue)]).err().unwrap();
        assert!(err.contains("a.bin"), "{}", err);
    }

    #[test]
    fn load_single_images_and_exe() {
        match load_files(vec![("g.bin".into(), raw_image(4))]).unwrap() {
            Loaded::Disc(d, _) => assert_eq!(d.lead_out, 154),
            _ => panic!(),
        }
        match load_files(vec![("g.iso".into(), vec![0u8; 2048 * 3])]).unwrap() {
            Loaded::Disc(d, _) => assert_eq!(d.lead_out, 153),
            _ => panic!(),
        }
        assert!(load_files(vec![("g.bin".into(), vec![0u8; 1000])]).is_err());
        let mut exe = vec![0u8; 0x800 + 16];
        exe[..8].copy_from_slice(b"PS-X EXE");
        exe[0x1C] = 16;
        match load_files(vec![("demo.EXE".into(), exe)]).unwrap() {
            Loaded::Exe(_, n) => assert_eq!(n, "demo.EXE"),
            _ => panic!(),
        }
        assert!(load_files(vec![("bad.exe".into(), vec![0u8; 0x900])]).is_err());
        // homebrew naming: .ps-exe, or any name at all - the header decides
        let mut good = vec![0u8; 0x800 + 16];
        good[..8].copy_from_slice(b"PS-X EXE");
        good[0x1C] = 16;
        for name in ["bowarrow.ps-exe", "GAME", "thing.dat"] {
            match load_files(vec![(name.into(), good.clone())]).unwrap() {
                Loaded::Exe(_, n) => assert_eq!(n, name),
                _ => panic!("{} not loaded as EXE", name),
            }
        }
        assert_eq!(classify("bowarrow.ps-exe", 70_000), Kind::Game);
        assert!(load_files(vec![("readme.txt".into(), vec![1, 2, 3])]).is_err());
    }
}
