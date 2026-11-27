//! ps1wasm-fuzzexe SEED OPS OUT.exe : random CPU differential-test program
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let seed: u64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let ops: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let out = a.get(3).cloned().unwrap_or_else(|| "fuzz.exe".into());
    std::fs::write(&out, ps1wasm_cli::testkit::fuzz_cpu_exe(seed, ops)).unwrap();
}
