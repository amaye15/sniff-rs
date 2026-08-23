//! Micro-benchmarks of `suggest_ideal_type`, the core type-detection
//! heuristic, called directly in-process (no subprocess/file I/O noise -
//! see `end_to_end.rs`/`format_comparison.rs` for that). Exercises a
//! handful of realistic column "shapes" at a few sizes each, since the
//! function's cost isn't shape-independent: a column of UUIDs matches
//! early (the first check in the precise-grammar tier), while a column of
//! high-cardinality free text is the actual worst case - it has to fail
//! every single check before falling back to `String`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sniff_rs::suggest_ideal_type;

const SIZES: &[usize] = &[10, 1_000, 100_000];

fn uuids(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("{i:08x}-e29b-41d4-a716-{i:012x}"))
        .collect()
}

fn integers(n: usize) -> Vec<String> {
    (0..n).map(|i| i.to_string()).collect()
}

fn emails(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("user{i}@example.com")).collect()
}

/// High-cardinality free text - the worst case for this function, since
/// every precise-grammar check (UUID, email, IPv4/IPv6, checksummed IDs,
/// date/time, ...) has to run and fail before falling back to String.
fn free_text(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("The quick brown fox jumps over lazy dog number {i}"))
        .collect()
}

fn bench_shape(c: &mut Criterion, name: &str, make_values: fn(usize) -> Vec<String>) {
    let mut group = c.benchmark_group(name);
    for &size in SIZES {
        let values = make_values(size);
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        group.bench_with_input(BenchmarkId::from_parameter(size), &refs, |b, refs| {
            b.iter(|| suggest_ideal_type(std::hint::black_box(refs), "String"));
        });
    }
    group.finish();
}

fn benchmarks(c: &mut Criterion) {
    bench_shape(c, "suggest_ideal_type/uuid", uuids);
    bench_shape(c, "suggest_ideal_type/integer", integers);
    bench_shape(c, "suggest_ideal_type/email", emails);
    bench_shape(c, "suggest_ideal_type/free_text_worst_case", free_text);
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
