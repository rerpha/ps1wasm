//! Disc images: CUE sheet parsing, track layout and sector access.
//!
//! Addresses are absolute CD frames ("abs"): 00:00:00 = 0, and the first
//! data track normally starts at 00:02:00 = 150. Sector bytes come from a
//! `SectorReader` so the browser can stream from File objects on demand.

pub const SECTOR_SIZE: usize = 2352;
pub const LEAD_IN: u32 = 150;

pub trait SectorReader {
    /// Read `buf.len()` bytes of file `file` starting at byte `offset`.
    fn read(&mut self, file: usize, offset: u64, buf: &mut [u8]) -> bool;
}

/// Simple in-memory reader (tests, small images).
pub struct MemReader(pub Vec<Vec<u8>>);

impl SectorReader for MemReader {
    fn read(&mut self, file: usize, offset: u64, buf: &mut [u8]) -> bool {
        let Some(data) = self.0.get(file) else { return false };
        let o = offset as usize;
        if o + buf.len() > data.len() {
            return false;
        }
        buf.copy_from_slice(&data[o..o + buf.len()]);
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Japan,
    America,
    Europe,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u8,
    pub audio: bool,
    /// Absolute frame of INDEX 01.
    pub start: u32,
    /// Absolute frame where the track's pregap (INDEX 00 / PREGAP) begins.
    pub pregap_start: u32,
}

#[derive(Clone, Debug)]
struct Extent {
    abs: u32,
    len: u32,
    file: usize,
    file_sector: u64,
    sector_size: u32,
}

pub struct Disc {
    pub tracks: Vec<Track>,
    extents: Vec<Extent>,
    pub lead_out: u32,
    reader: Box<dyn SectorReader>,
}

#[inline]
pub fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}
#[inline]
pub fn from_bcd(v: u8) -> u32 {
    ((v >> 4) as u32) * 10 + (v & 0xF) as u32
}
pub fn msf_to_abs(m: u32, s: u32, f: u32) -> u32 {
    (m * 60 + s) * 75 + f
}
pub fn abs_to_msf(a: u32) -> (u32, u32, u32) {
    (a / 4500, (a / 75) % 60, a % 75)
}

fn parse_msf(s: &str) -> Result<u32, String> {
    let p: Vec<&str> = s.split(':').collect();
    if p.len() != 3 {
        return Err(format!("bad MSF '{}'", s));
    }
    let n = |x: &str| x.trim().parse::<u32>().map_err(|_| format!("bad MSF '{}'", s));
    Ok(msf_to_abs(n(p[0])?, n(p[1])?, n(p[2])?))
}

/// Tokenize a cue line, honouring double quotes.
fn tokens(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[derive(Debug, Default, Clone)]
struct CueTrack {
    number: u8,
    audio: bool,
    sector_size: u32,
    index0: Option<u32>,
    index1: Option<u32>,
    pregap: u32,
}

/// Result of parsing a CUE sheet: file names in order and their tracks.
#[derive(Debug, Default, Clone)]
pub struct CueSheet {
    pub files: Vec<String>,
    tracks: Vec<(usize, CueTrack)>,
}

impl CueSheet {
    pub fn parse(text: &str) -> Result<CueSheet, String> {
        let mut sheet = CueSheet::default();
        let mut cur: Option<CueTrack> = None;
        for (ln, raw) in text.lines().enumerate() {
            let t = tokens(raw);
            if t.is_empty() {
                continue;
            }
            let err = |m: &str| format!("cue line {}: {}", ln + 1, m);
            match t[0].to_ascii_uppercase().as_str() {
                "FILE" => {
                    if t.len() < 2 {
                        return Err(err("FILE needs a name"));
                    }
                    if let Some(tr) = cur.take() {
                        sheet.tracks.push((sheet.files.len() - 1, tr));
                    }
                    sheet.files.push(t[1].clone());
                }
                "TRACK" => {
                    if sheet.files.is_empty() {
                        return Err(err("TRACK before FILE"));
                    }
                    if let Some(tr) = cur.take() {
                        sheet.tracks.push((sheet.files.len() - 1, tr));
                    }
                    let number = t.get(1).and_then(|n| n.parse().ok()).ok_or_else(|| err("bad track number"))?;
                    let mode = t.get(2).map(|m| m.to_ascii_uppercase()).unwrap_or_default();
                    let (audio, sector_size) = match mode.as_str() {
                        "AUDIO" => (true, 2352),
                        "MODE1/2352" | "MODE2/2352" => (false, 2352),
                        "MODE1/2048" | "MODE2/2048" => (false, 2048),
                        other => return Err(err(&format!("unsupported track mode '{}'", other))),
                    };
                    cur = Some(CueTrack { number, audio, sector_size, ..Default::default() });
                }
                "INDEX" => {
                    let tr = cur.as_mut().ok_or_else(|| err("INDEX outside TRACK"))?;
                    let idx: u32 = t.get(1).and_then(|n| n.parse().ok()).ok_or_else(|| err("bad index"))?;
                    let pos = parse_msf(t.get(2).ok_or_else(|| err("INDEX needs a time"))?)?;
                    match idx {
                        0 => tr.index0 = Some(pos),
                        1 => tr.index1 = Some(pos),
                        _ => {}
                    }
                }
                "PREGAP" => {
                    let tr = cur.as_mut().ok_or_else(|| err("PREGAP outside TRACK"))?;
                    tr.pregap = parse_msf(t.get(1).ok_or_else(|| err("PREGAP needs a time"))?)?;
                }
                _ => {} // REM, CATALOG, POSTGAP, FLAGS, ...
            }
        }
        if let Some(tr) = cur.take() {
            sheet.tracks.push((sheet.files.len() - 1, tr));
        }
        if sheet.tracks.is_empty() {
            return Err("cue sheet has no tracks".into());
        }
        for (_, t) in &sheet.tracks {
            if t.index1.is_none() {
                return Err(format!("track {} has no INDEX 01", t.number));
            }
        }
        Ok(sheet)
    }
}

impl std::fmt::Debug for Disc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disc").field("tracks", &self.tracks).field("lead_out", &self.lead_out).finish()
    }
}

impl Disc {
    /// Build a disc from a parsed cue sheet. `file_sizes` are byte lengths
    /// of the files listed in `sheet.files`, in the same order.
    pub fn from_cue(sheet: &CueSheet, file_sizes: &[u64], reader: Box<dyn SectorReader>) -> Result<Disc, String> {
        if file_sizes.len() != sheet.files.len() {
            return Err(format!("cue lists {} files, got {}", sheet.files.len(), file_sizes.len()));
        }
        let mut tracks = vec![];
        let mut extents = vec![];
        let mut abs = LEAD_IN;
        for (fi, &size) in file_sizes.iter().enumerate() {
            let ft: Vec<&CueTrack> = sheet.tracks.iter().filter(|(f, _)| *f == fi).map(|(_, t)| t).collect();
            if ft.is_empty() {
                continue;
            }
            let ss = ft[0].sector_size;
            let file_sectors = (size / ss as u64) as u32;
            let base = abs;
            let mut gap = 0u32;
            for (k, t) in ft.iter().enumerate() {
                let i1 = t.index1.unwrap();
                let first = t.index0.unwrap_or(i1);
                let next_first = ft.get(k + 1).map(|n| n.index0.unwrap_or(n.index1.unwrap())).unwrap_or(file_sectors);
                if next_first < first || first > file_sectors {
                    return Err(format!("track {} indices out of order / beyond file end", t.number));
                }
                let pregap_start = base + first + gap;
                gap += t.pregap;
                tracks.push(Track {
                    number: t.number,
                    audio: t.audio,
                    start: base + i1 + gap,
                    pregap_start: if t.pregap > 0 { pregap_start } else { base + first + gap },
                });
                extents.push(Extent {
                    abs: base + first + gap,
                    len: next_first - first,
                    file: fi,
                    file_sector: first as u64,
                    sector_size: t.sector_size,
                });
            }
            abs = base + file_sectors + gap;
        }
        Ok(Disc { tracks, extents, lead_out: abs, reader })
    }

    /// A raw single-file image without a cue sheet (.bin 2352 or .iso 2048).
    pub fn from_single(size: u64, raw_2352: bool, reader: Box<dyn SectorReader>) -> Result<Disc, String> {
        let mode = if raw_2352 { "MODE2/2352" } else { "MODE1/2048" };
        let sheet = CueSheet::parse(&format!("FILE \"image\" BINARY\n TRACK 01 {}\n  INDEX 01 00:00:00\n", mode))?;
        Disc::from_cue(&sheet, &[size], reader)
    }

    /// Read the full 2352-byte sector at absolute frame `abs`.
    /// 2048-byte (ISO) sectors are wrapped as Mode 2 Form 1.
    pub fn read_sector(&mut self, abs: u32, out: &mut [u8; SECTOR_SIZE]) -> bool {
        let Some(e) = self.extents.iter().find(|e| abs >= e.abs && abs < e.abs + e.len).cloned() else {
            out.fill(0);
            return false;
        };
        let sector = e.file_sector + (abs - e.abs) as u64;
        let off = sector * e.sector_size as u64;
        if e.sector_size == 2352 {
            self.reader.read(e.file, off, out)
        } else {
            out.fill(0);
            out[1..11].fill(0xFF);
            let (m, s, f) = abs_to_msf(abs);
            out[12] = bcd(m);
            out[13] = bcd(s);
            out[14] = bcd(f);
            out[15] = 2;
            out[18] = 8; // submode: data, form 1
            out[22] = 8;
            self.reader.read(e.file, off, &mut out[24..24 + 2048])
        }
    }

    pub fn first_track(&self) -> u8 {
        self.tracks.first().map(|t| t.number).unwrap_or(1)
    }
    pub fn last_track(&self) -> u8 {
        self.tracks.last().map(|t| t.number).unwrap_or(1)
    }

    /// Track containing `abs` (pregaps belong to the following track).
    pub fn track_at(&self, abs: u32) -> Option<&Track> {
        self.tracks.iter().rev().find(|t| abs >= t.pregap_start).or(self.tracks.first())
    }

    pub fn track(&self, number: u8) -> Option<&Track> {
        self.tracks.iter().find(|t| t.number == number)
    }

    /// Region from the licence string in sector 4 of the data track.
    pub fn region(&mut self) -> Region {
        let mut buf = [0u8; SECTOR_SIZE];
        if !self.read_sector(LEAD_IN + 4, &mut buf) {
            return Region::America;
        }
        let text = String::from_utf8_lossy(&buf[24..24 + 0x800]).to_string();
        if text.contains("Europe") {
            Region::Europe
        } else if text.contains("Inc.") {
            Region::Japan
        } else {
            Region::America
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_sector(abs: u32, fill: u8) -> Vec<u8> {
        let mut s = vec![fill; SECTOR_SIZE];
        let (m, sec, f) = abs_to_msf(abs);
        s[12] = bcd(m);
        s[13] = bcd(sec);
        s[14] = bcd(f);
        s
    }

    #[test]
    fn bcd_and_msf() {
        assert_eq!(bcd(59), 0x59);
        assert_eq!(from_bcd(0x74), 74);
        assert_eq!(msf_to_abs(0, 2, 0), 150);
        assert_eq!(abs_to_msf(4500 + 150 + 3), (1, 2, 3));
    }

    #[test]
    fn single_file_multi_track_cue() {
        let cue = r#"
REM test
FILE "Game (USA).bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:00:10
    INDEX 01 00:00:12
"#;
        let sheet = CueSheet::parse(cue).unwrap();
        assert_eq!(sheet.files, vec!["Game (USA).bin"]);
        let mut img = vec![];
        for k in 0..20 {
            img.extend(raw_sector(150 + k, k as u8));
        }
        let size = img.len() as u64;
        let mut d = Disc::from_cue(&sheet, &[size], Box::new(MemReader(vec![img]))).unwrap();
        assert_eq!(d.tracks.len(), 2);
        assert_eq!(d.tracks[0].start, 150);
        assert_eq!(d.tracks[1].start, 162);
        assert_eq!(d.tracks[1].pregap_start, 160);
        assert!(d.tracks[1].audio);
        assert_eq!(d.lead_out, 170);
        let mut buf = [0u8; SECTOR_SIZE];
        assert!(d.read_sector(155, &mut buf));
        assert_eq!(buf[100], 5);
        assert_eq!(d.track_at(161).unwrap().number, 2);
        assert_eq!(d.track_at(159).unwrap().number, 1);
        assert!(!d.read_sector(170, &mut buf), "past lead-out");
    }

    #[test]
    fn multi_file_cue_with_pregap_command() {
        let cue = "FILE \"a.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\nFILE \"b.bin\" BINARY\n TRACK 02 AUDIO\n  PREGAP 00:02:00\n  INDEX 01 00:00:00\n";
        let sheet = CueSheet::parse(cue).unwrap();
        let a: Vec<u8> = (0..10).flat_map(|k| raw_sector(150 + k, 0xA0 + k as u8)).collect();
        let b: Vec<u8> = (0..5).flat_map(|k| vec![0xB0 + k as u8; SECTOR_SIZE]).collect();
        let sizes = [a.len() as u64, b.len() as u64];
        let mut d = Disc::from_cue(&sheet, &sizes, Box::new(MemReader(vec![a, b]))).unwrap();
        assert_eq!(d.tracks[1].start, 160 + 150, "file b starts after 2s gap");
        assert_eq!(d.tracks[1].pregap_start, 160);
        assert_eq!(d.lead_out, 160 + 150 + 5);
        let mut buf = [0u8; SECTOR_SIZE];
        assert!(!d.read_sector(200, &mut buf), "gap is silent");
        assert!(buf.iter().all(|&x| x == 0));
        assert!(d.read_sector(312, &mut buf));
        assert_eq!(buf[0], 0xB2);
    }

    #[test]
    fn iso_sectors_are_wrapped() {
        let mut iso = vec![0u8; 2048 * 3];
        iso[2048 + 7] = 0x42;
        let mut d = Disc::from_single(iso.len() as u64, false, Box::new(MemReader(vec![iso]))).unwrap();
        let mut buf = [0u8; SECTOR_SIZE];
        assert!(d.read_sector(151, &mut buf));
        assert_eq!(&buf[0..12], &[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        assert_eq!(&buf[12..16], &[0x00, 0x02, 0x01, 0x02]);
        assert_eq!(buf[24 + 7], 0x42);
    }

    #[test]
    fn cue_errors() {
        assert!(CueSheet::parse("TRACK 01 AUDIO").is_err());
        assert!(CueSheet::parse("FILE \"x\" BINARY\n TRACK 01 MODE2/2336\n").is_err());
        assert!(CueSheet::parse("FILE \"x\" BINARY\n TRACK 01 AUDIO\n").is_err(), "missing INDEX 01");
        assert!(CueSheet::parse("").is_err());
    }

    #[test]
    fn region_detection() {
        let mut img = vec![0u8; SECTOR_SIZE * 8];
        let text = b"Licensed by Sony Computer Entertainment Europe";
        img[4 * SECTOR_SIZE + 24..4 * SECTOR_SIZE + 24 + text.len()].copy_from_slice(text);
        let mut d = Disc::from_single(img.len() as u64, true, Box::new(MemReader(vec![img]))).unwrap();
        assert_eq!(d.region(), Region::Europe);
    }
}
