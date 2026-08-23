# Benchmark log

A manually-updated history of `cargo bench` runs, so performance-affecting
changes can be checked against a prior snapshot instead of relying on
memory. See `benches/` (and CLAUDE.md's "Benchmarking" section) for what
each of the three targets measures and why.

**Numbers are only meaningful compared against another entry run on the
same machine.** CPU, thermal state, and background load all shift
absolute timings by more than most real regressions would - there's no CI
runner behind this log (see CLAUDE.md's benchmarking section for why that
tradeoff was made deliberately), so every entry records its machine and
should only be compared to other entries from that same machine. Treat a
change of a few percent as noise; look for changes that hold up across a
re-run.

## Adding a new entry

1. `cargo bench --features parquet,sqlite,xlsx` (runs all three targets;
   drop the feature flags to skip `format_comparison`, which requires
   them - see its `required-features` in `Cargo.toml`).
2. Note the git commit (`git rev-parse --short HEAD`) and machine
   (`uname -sm` plus a CPU name is enough).
3. Append a new entry below, newest first, using the same three-table
   shape as the existing entries - the `end_to_end` and
   `suggest_ideal_type` tables report the Criterion point estimate (the
   middle of its `[low high]` confidence interval); `format_comparison`
   likewise.
4. If Criterion prints "Performance has regressed"/"improved" against
   *this same machine's* previous run, that's the useful signal - note it
   in the entry. Against a different machine's prior run (e.g. the very
   first entry after switching laptops), ignore those labels entirely.

---

## 2026-08-23 — `29d99ef` (Darwin arm64, Apple M4)

First recorded snapshot - establishes the baseline this machine's future
entries compare against. `Cargo.toml`/`Cargo.lock` at this commit: no
`[[bench]]` targets existed before this commit added them, so there's no
prior run to compare against here either.

**Heuristic engine** (`suggest_ideal_type`, in-process, values → time):

| Shape | 10 | 1,000 | 100,000 |
|---|---|---|---|
| UUID | 897 ns | 21.8 µs | 2.13 ms |
| Integer | 1.46 µs | 43.2 µs | 5.01 ms |
| Email | 689 ns | 24.4 µs | 2.55 ms |
| Free-text (worst case) | 2.52 µs | 87.4 µs | 11.5 ms |

UUID and email match early in `suggest_ideal_type`'s check order (both are
near the top of the precise-grammar tier), so they're consistently the
fastest shapes. Free-text is the real worst case - it has to fail every
precise-grammar check before falling back to `String` - and is ~5-9x
slower than UUID/email at 100,000 values.

**End-to-end** (full binary via `Command`, CSV/JSON, rows → time):

| Format | 100 | 10,000 | 200,000 |
|---|---|---|---|
| CSV | 2.15 ms | 13.2 ms | 239 ms |
| JSON | 2.24 ms | 20.5 ms | 512 ms |

CSV is consistently faster than JSON at every scale, ending up roughly 2x
faster by 200,000 rows - likely `profile_json_path`'s recursive per-record
flattening carrying more overhead than the flat CSV reader's straight-line
column loop.

**Format comparison** (same 10,000-row dataset, byte-identical across
formats - see `benches/fixtures/generate.py`):

| Format | Time |
|---|---|
| SQLite | 14.4 ms |
| Parquet | 15.3 ms |
| CSV | 16.0 ms |
| JSON | 22.7 ms |
| Excel | 29.3 ms |

Excel is the clear outlier - zip decompression plus XML parsing on top of
everything else. SQLite, Parquet, and CSV all land within ~10% of each
other.
