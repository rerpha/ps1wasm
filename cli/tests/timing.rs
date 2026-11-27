//! Timing accuracy vs real-hardware measurements (ps1-tests logs), with
//! numeric tolerances. Skipped when test-assets is absent.

use ps1wasm_cli::testkit::host;

/// Run a ps1-tests program under OpenBIOS; returns (our tty, hardware log).
fn run(test: &str, frames: u32) -> Option<(String, String)> {
    let dir = host::test_assets("ps1-tests")?.join(test);
    let exe = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.extension().map(|x| x == "exe").unwrap_or(false))?;
    let p = host::run_exe(&host::openbios(), &std::fs::read(exe).unwrap(), frames, None);
    let log = std::fs::read_to_string(dir.join("psx.log")).unwrap().replace("% ", "").replace('\r', "");
    Some((p.tty.replace('\r', ""), log))
}

/// First integer after `key` on each line containing `filter`.
fn numbers_after(text: &str, filter: &str, key: &str) -> Vec<f64> {
    text.lines()
        .filter(|l| l.contains(filter))
        .filter_map(|l| {
            let rest = &l[l.find(key)? + key.len()..];
            let n: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
            n.parse().ok()
        })
        .collect()
}

/// All integers after `key` on each line containing `filter` (one Vec per line).
fn all_numbers_after(text: &str, filter: &str, key: &str) -> Vec<Vec<f64>> {
    text.lines()
        .filter(|l| l.contains(filter))
        .filter_map(|l| {
            let rest = &l[l.find(key)? + key.len()..];
            Some(rest.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        })
        .collect()
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1.0)
}

#[test]
fn dma_chopping_durations() {
    let Some((ours, hw)) = run("dma/chopping", 3000) else { return eprintln!("skipped") };
    let (o, h) = (numbers_after(&ours, "took", "took"), numbers_after(&hw, "took", "took"));
    assert_eq!(o.len(), h.len(), "all {} rows produced", h.len());
    let errs: Vec<f64> = o.iter().zip(&h).map(|(a, b)| rel(*a, *b)).collect();
    let mean = errs.iter().sum::<f64>() / errs.len() as f64;
    let worst = errs.iter().cloned().fold(0.0, f64::max);
    let within10 = errs.iter().filter(|e| **e < 0.10).count();
    eprintln!("chopping: mean {:.1}%, worst {:.1}%, {}/{} within 10%", mean * 100.0, worst * 100.0, within10, errs.len());
    assert!(mean < 0.03, "mean error {:.1}%", mean * 100.0);
    assert!(within10 * 100 >= errs.len() * 95, "{} of {} within 10%", within10, errs.len());
    assert!(worst < 0.15, "worst error {:.1}%", worst * 100.0);
}

#[test]
fn gpu_bandwidth_fill_rates() {
    let Some((ours, hw)) = run("gpu/bandwidth", 3000) else { return eprintln!("skipped") };
    let (o, h) = (numbers_after(&ours, "speed:", "speed:"), numbers_after(&hw, "speed:", "speed:"));
    assert_eq!(o.len(), h.len());
    for (k, (a, b)) in o.iter().zip(&h).enumerate() {
        assert!(rel(*a, *b) < 0.08, "row {}: ours {} MB/s, hardware {} MB/s", k, a, b);
    }
}

#[test]
fn timer_clock_rates_and_delay_loops() {
    let Some((ours, hw)) = run("timers", 1500) else { return eprintln!("skipped") };
    for filter in ["1000 cycles delay", "5000 cycles delay", "frame delay"] {
        let o = all_numbers_after(&ours, filter, "delay:");
        let h = all_numbers_after(&hw, filter, "delay:");
        assert_eq!(o.len(), h.len(), "{}", filter);
        for (ro, rh) in o.iter().zip(&h) {
            // hardware's first frame-delay sample starts mid-frame: skip it
            let skip = if filter == "frame delay" { 1 } else { 0 };
            // hardware rows contain the odd interrupt-disturbed sample (e.g.
            // 6737 among 1011s): ignore samples far from the row median
            let mut sorted = rh.clone();
            sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
            let median = sorted[sorted.len() / 2];
            for (a, b) in ro.iter().zip(rh).skip(skip) {
                if (b - median).abs() > median * 0.5 + 2.0 {
                    continue;
                }
                let tol = if *b < 300.0 { 1.0 } else { b * 0.012 };
                assert!((a - b).abs() <= tol, "{}: ours {} hardware {}", filter, a, b);
            }
        }
    }
}

#[test]
fn cpu_access_times() {
    let Some((ours, hw)) = run("cpu/access-time", 600) else { return eprintln!("skipped") };
    // OpenBIOS's printf can't print the fractional part; compare integer
    // parts of every region's 8/16/32-bit timings (unaligned SPU word read
    // measures the exception handler, so it's excluded).
    let rows = |t: &str| -> Vec<(String, Vec<f64>)> {
        t.lines()
            .filter(|l| l.contains("(0x") && !l.starts_with("SEGMENT"))
            .map(|l| {
                let name = l.split_whitespace().next().unwrap().to_string();
                let after = &l[l.find(')').unwrap() + 1..];
                let nums = after.split_whitespace().filter_map(|w| w.split('.').next()?.parse().ok()).collect();
                (name, nums)
            })
            .collect()
    };
    let (o, h) = (rows(&ours), rows(&hw));
    assert_eq!(o.len(), h.len());
    for ((name, a), (_, b)) in o.iter().zip(&h) {
        for k in 0..3 {
            if name == "SPUCNT" && k == 2 {
                continue;
            }
            assert!((a[k] - b[k]).abs() <= 1.0, "{} access {}: ours {} hardware {}", name, k, a[k], b[k]);
        }
    }
}

#[test]
fn dma_chain_looping_cpu_work_and_flags() {
    let Some((ours, hw)) = run("dma/chain-looping", 600) else { return eprintln!("skipped") };
    let (o, h) = (numbers_after(&ours, "Work took", "took"), numbers_after(&hw, "Work took", "took"));
    assert_eq!(o.len(), h.len());
    for (a, b) in o.iter().zip(&h) {
        assert!(rel(*a, *b) < 0.03, "work ticks: ours {} hardware {}", a, b);
    }
    let flags = |t: &str| -> Vec<String> { t.lines().filter_map(|l| l.find("finished").map(|i| l[i..].to_string())).collect() };
    assert_eq!(flags(&ours), flags(&hw), "finished/irq flags");
}

#[test]
fn spu_dma_timing_assertions_pass() {
    let Some((ours, _)) = run("spu/memory-transfer", 900) else { return eprintln!("skipped") };
    assert!(ours.contains("pass - testDMAWriteTiming") && ours.contains("pass - testDMAReadTiming"), "{}", ours);
    assert!(!ours.lines().any(|l| l.starts_with("fail")), "{}", ours);
}
