//! Replays JaCzekanski/ps1-tests' GTE fuzz log, recorded on a real PS1:
//! each case lists all 64 GTE registers before a command and after it.
//! Skipped when test-assets is absent (tools/fetch-test-assets.sh).

use ps1wasm_core::gte::Gte;

struct Case {
    name: String,
    cmd: u32,
    input: Vec<(u32, u32)>,
    output: Vec<(u32, u32)>,
}

fn parse_reg(line: &str) -> Option<(u32, u32)> {
    // "> r[12] = 0x048dc664"
    let l = line.find('[')?;
    let r = line.find(']')?;
    let idx: u32 = line[l + 1..r].parse().ok()?;
    let hex = line.rsplit("0x").next()?;
    Some((idx, u32::from_str_radix(hex.trim(), 16).ok()?))
}

fn parse_cmd(line: &str) -> Option<u32> {
    // "GTE 0x01 RTPS (sf=0, lm=1, tx=2, vx=1, mx=0)"
    let op = u32::from_str_radix(line.split_whitespace().nth(1)?.trim_start_matches("0x"), 16).ok()?;
    let field = |name: &str| -> u32 {
        line.split(&format!("{}=", name)).nth(1).and_then(|s| s.split(|c| c == ',' || c == ')').next()).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    };
    Some(op | (field("sf") << 19) | (field("mx") << 17) | (field("vx") << 15) | (field("tx") << 13) | (field("lm") << 10))
}

fn load() -> Option<Vec<Case>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets/ps1-tests/gte-fuzz/gte_valid_0xc0ffee_50.log");
    let text = std::fs::read_to_string(p).ok()?;
    let mut cases = vec![];
    let mut cur: Option<Case> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Test ") {
            if let Some(c) = cur.take() {
                cases.push(c);
            }
            cur = Some(Case { name: String::new(), cmd: 0, input: vec![], output: vec![] });
        } else if let Some(c) = cur.as_mut() {
            if line.starts_with('>') {
                c.input.extend(parse_reg(line));
            } else if line.starts_with('<') {
                c.output.extend(parse_reg(line));
            } else if line.starts_with("GTE ") {
                c.name = line.to_string();
                c.cmd = parse_cmd(line).expect("command line");
            }
        }
    }
    cases.extend(cur);
    Some(cases)
}

#[test]
fn gte_matches_hardware_fuzz_log() {
    let Some(cases) = load() else { eprintln!("skipped: no gte fuzz log"); return };
    assert!(cases.len() > 1000, "parsed {} cases", cases.len());
    let mut failures = vec![];
    for (n, c) in cases.iter().enumerate() {
        let mut g = Gte::new();
        for &(r, v) in &c.input {
            if r < 32 { g.write_data(r, v) } else { g.write_ctrl(r - 32, v) }
        }
        g.command(c.cmd & 0x1FF_FFFF);
        let mut diffs = vec![];
        for &(r, want) in &c.output {
            let got = if r < 32 { g.read_data(r) } else { g.read_ctrl(r - 32) };
            if got != want {
                diffs.push(format!("r[{}] got {:08x} want {:08x}", r, got, want));
            }
        }
        if !diffs.is_empty() {
            failures.push(format!("case {} {}: {}", n + 1, c.name, diffs.join(", ")));
        }
    }
    for f in failures.iter().take(10) {
        eprintln!("{}", f);
    }
    assert!(failures.is_empty(), "{} of {} hardware fuzz cases differ", failures.len(), cases.len());
}
