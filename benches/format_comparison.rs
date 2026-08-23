//! Same underlying 10,000-row dataset (7 columns: int id, free-text name,
//! date, float amount, bool, UUID, int score), written to five different
//! formats from one pandas DataFrame (`benches/fixtures/generate.py`) so
//! the comparison is genuinely apples-to-apples - not just "same row
//! count," but byte-identical values across formats. Answers "which
//! format does this tool read fastest," as opposed to `end_to_end.rs`'s
//! "how does one format scale with size."
//!
//! Requires --features parquet,sqlite,xlsx (`required-features` in
//! Cargo.toml enforces this - `cargo bench` skips this target otherwise
//! rather than failing to build).

use criterion::{Criterion, criterion_group, criterion_main};
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sniff-rs"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches/fixtures")
        .join(name)
}

fn run_binary(path: &PathBuf) {
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "benchmark run failed for {path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_comparison/10k_rows");
    for ext in ["csv", "json", "parquet", "sqlite", "xlsx"] {
        let path = fixture(&format!("comparison_data.{ext}"));
        assert!(
            path.exists(),
            "missing bench fixture {path:?} - run benches/fixtures/generate.py first"
        );
        group.bench_with_input(ext, &path, |b, path| {
            b.iter(|| run_binary(path));
        });
    }
    group.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
