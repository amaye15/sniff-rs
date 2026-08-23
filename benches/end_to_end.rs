//! End-to-end benchmarks of the compiled binary via `Command` - the same
//! black-box, subprocess-based approach `tests/integration.rs` already
//! uses (see its own doc comment for why: no assert_cmd/predicates
//! dependency, keeping test/bench tooling as lean as the tool itself).
//! This measures what a user of the CLI actually experiences (process
//! startup + file I/O + parsing + profiling together), as opposed to
//! `heuristic_engine.rs`'s in-process, I/O-free micro-benchmarks.
//!
//! Only CSV and JSON are covered here - the two formats that need no
//! optional feature, so this bench always runs regardless of which
//! features `cargo bench` was invoked with. See `format_comparison.rs`
//! for the feature-gated formats compared against these two at a fixed size.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const ROW_COUNTS: &[usize] = &[100, 10_000, 200_000];

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sniff-rs"))
}

/// A representative row shape: an integer id, a free-text name, a date, a
/// float amount, a bool flag, and a UUID - one column of each broad kind
/// this tool's heuristics distinguish, not just plain numbers.
fn write_csv(path: &Path, rows: usize) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "id,name,signup_date,amount,active,user_uuid").unwrap();
    for i in 0..rows {
        writeln!(
            f,
            "{i},user_{i},2024-01-{:02},{}.{:02},{},{i:08x}-e29b-41d4-a716-{i:012x}",
            (i % 28) + 1,
            i % 10_000,
            i % 100,
            i % 2 == 0,
        )
        .unwrap();
    }
}

fn write_json(path: &Path, rows: usize) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "[").unwrap();
    for i in 0..rows {
        let comma = if i + 1 == rows { "" } else { "," };
        writeln!(
            f,
            r#"{{"id":{i},"name":"user_{i}","signup_date":"2024-01-{:02}","amount":{}.{:02},"active":{},"user_uuid":"{i:08x}-e29b-41d4-a716-{i:012x}"}}{comma}"#,
            (i % 28) + 1,
            i % 10_000,
            i % 100,
            i % 2 == 0,
        )
        .unwrap();
    }
    writeln!(f, "]").unwrap();
}

fn run_binary(path: &Path) {
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "benchmark run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn benchmarks(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("failed to create bench scratch dir");

    let mut csv_group = c.benchmark_group("end_to_end/csv");
    for &rows in ROW_COUNTS {
        let path = dir.path().join(format!("bench_{rows}.csv"));
        write_csv(&path, rows);
        // The largest tier takes meaningfully longer per iteration (process
        // spawn + parsing 200k rows) - a smaller sample size keeps the
        // whole suite's runtime reasonable without losing statistical
        // validity, the same tradeoff Criterion's own docs recommend for
        // slower benchmarks.
        if rows >= 200_000 {
            csv_group.sample_size(20);
        }
        csv_group.bench_with_input(BenchmarkId::from_parameter(rows), &path, |b, path| {
            b.iter(|| run_binary(path));
        });
    }
    csv_group.finish();

    let mut json_group = c.benchmark_group("end_to_end/json");
    for &rows in ROW_COUNTS {
        let path = dir.path().join(format!("bench_{rows}.json"));
        write_json(&path, rows);
        if rows >= 200_000 {
            json_group.sample_size(20);
        }
        json_group.bench_with_input(BenchmarkId::from_parameter(rows), &path, |b, path| {
            b.iter(|| run_binary(path));
        });
    }
    json_group.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
