//! ps1wasm-mktest OUT_DIR: writes demo.exe, demo.bin and demo.cue (self-made test content).
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = ps1wasm_cli::testkit::demo_exe();
    std::fs::write(dir.join("demo.exe"), &exe).unwrap();
    std::fs::write(dir.join("demo.bin"), ps1wasm_cli::testkit::bootable_disc(&exe)).unwrap();
    std::fs::write(dir.join("alu-bench.exe"), ps1wasm_cli::testkit::alu_bench_exe()).unwrap();
    std::fs::write(dir.join("demo.cue"), "FILE \"demo.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n").unwrap();
    println!("wrote demo.exe, demo.bin, demo.cue to {}", dir.display());
}
