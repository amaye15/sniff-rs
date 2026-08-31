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

## 2026-08-31 (follow-up) — `f2cba62`+perf pass 2 (Darwin arm64, Apple M4)

A second optimization pass in the same session, prompted by a follow-up
"continue to optimize" request rather than a new reported problem - see
CLAUDE.md's own "Performance" section for the full writeup of the three
fixes this found (`profile_json_path`'s sample-collection cloning an
entire column just to keep a handful of examples, `normalize_numeric_str`
always allocating even for an already-clean value, `is_iban` making three
allocations where one suffices).

**End-to-end** (full binary via `Command`, CSV/JSON, rows → time) -
against the *original*, pre-any-of-this-session's-work baseline
(2026-08-23 entry below), not just the first perf-pass entry above it:

| Format | Rows | Original | Now | Change |
|---|---|---|---|---|
| CSV | 10,000 | 25.7 ms | 18.3 ms | **-28.8%** |
| CSV | 200,000 | 545 ms | 375 ms | **-31.3%** |
| JSON | 10,000 | 19.5 ms | 14.7 ms | **-24.3%** |
| JSON | 200,000 | 563 ms | 416 ms | -26.2%† |

(†This row's own confidence interval was wide (~371-468ms) even after
several re-runs - some background load on the machine during this
specific benchmark, not something this pass's own code changes caused;
treat the point estimate as approximate, not precise, the same "a few
percent is noise" caution this file's own header already gives.) The
JSON row's improvement is larger than the first pass's own entry showed
specifically because the sample-collection fix applies to *every*
column in this benchmark's fixture (all six are scalar columns, each
paying for the old code's full-column clone before this pass), not just
to nested-object columns the way the first pass's `bucket_object_fields`
fix was.

Two more findings only show up on shapes `benches/end_to_end.rs`'s own
fixture doesn't exercise (a flat, non-object, moderately-sized row
shape), so they're recorded here as one-off measurements on
uncommitted synthetic fixtures rather than as permanent benchmark
targets - the same treatment the first pass's own 300-field fixture got:

- A 200,000-row JSONL file with a small nested object column (3 fields:
  a nested user object, a tag array, a meta object) went from 1.20s to
  0.64s (user time) - the sample-collection fix's own worst case, since
  cloning a `Map` recursively clones everything inside it, not just a
  flat byte copy.
- A 500,000-row, purely-numeric 3-column CSV (no formatting noise at
  all - plain integers and decimals) went from 0.37s to 0.31s (user
  time) via the `normalize_numeric_str` `Cow` fix alone.

Both compared with `diff` against the pre-fix output and confirmed
byte-identical.

---

## 2026-08-31 — `25d2cbc`+perf pass (Darwin arm64, Apple M4)

A dedicated optimization pass (see CLAUDE.md's own "Performance" section
for the full writeup of what was found and how each fix was verified),
not a response to a reported slowdown. Four fixes, all on the CSV reader
and the recursive JSON-shaped flattener every non-native nested format
bridges through: `parse_csv` no longer collects the whole file into a
`Vec<char>` before parsing it; `is_missing_sentinel`/`is_bool_word` no
longer heap-allocate a lowercased `String` per value; `columns_from_csv`
no longer clones every cell twice on its way into a `ColumnInput`; and
`profile_json_path`/`profile_json_records` bucket a set of objects'
fields in one pass instead of rescanning every object once per distinct
key. Numbers below are same-session, same-machine before/after pairs
(the "before" runs were captured fresh, immediately before any code
changed, specifically so this entry doesn't rely on comparing against
the older 2026-08-23 snapshot below across ~a week of otherwise-unrelated
changes) - `format_comparison` wasn't re-run for this entry: none of
today's fixes touch the Parquet/SQLite/Excel readers it also exercises,
and this session's one attempt at re-running it landed on a machine
under incidental background load (a concurrent, unrelated `cargo bench`
invocation from this environment's own tooling), so its numbers weren't
trustworthy enough to record - the 2026-08-23 entry below remains the
most recent clean snapshot for that target.

**Heuristic engine** (`suggest_ideal_type`, in-process, values → time):

| Shape | Size | Before | After | Change |
|---|---|---|---|---|
| UUID | 1,000 | 20.6 µs | 17.7 µs | -14.0% |
| UUID | 100,000 | 2.76 ms | 1.72 ms | -37.6%* |
| Free-text (worst case) | 10 | 2.28 µs | 2.22 µs | -2.7% |
| Free-text (worst case) | 1,000 | 102 µs | 90.6 µs | -11.5% |
| Free-text (worst case) | 100,000 | 13.6 ms | 11.4 ms | -15.7% |

Free-text is the one shape with a clear, mechanistic explanation for the
win (it's the only one of these four that reaches the `is_bool_word`
check on every value, since it fails everything else first) - the UUID
row's own improvement is real in this specific before/after pair but
larger than any single fix here obviously accounts for, and is marked
with `*` as more likely to include some session-level noise (thermal
state, background load - see this file's own header) than the free-text
numbers are; re-run before trusting the UUID figure specifically.
Integer and email shapes moved by a few percent either way, within this
file's own "treat a few percent as noise" guidance.

**End-to-end** (full binary via `Command`, CSV/JSON, rows → time):

| Format | Rows | Before | After | Change |
|---|---|---|---|---|
| CSV | 100 | 1.57 ms | 1.54 ms | -2.0%† |
| CSV | 10,000 | 25.7 ms | 19.0 ms | **-26.0%** |
| CSV | 200,000 | 545 ms | 397 ms | **-27.2%** |
| JSON | 100 | 1.53 ms | 1.48 ms | -3.3%† |
| JSON | 10,000 | 19.5 ms | 17.8 ms | -8.7% |
| JSON | 200,000 | 563 ms | 515 ms | -8.6% |

(†At 100 rows, subprocess spawn overhead dominates the actual parsing
work being measured - CLAUDE.md's own Benchmarking section already notes
this - so these two rows are closer to a process-startup measurement
than a reader-performance one; the 10,000/200,000 rows are where the
fixes actually show up.) This benchmark's own JSON fixture is a flat,
six-field record shape, so it doesn't exercise the
`profile_json_path`/`profile_json_records` bucketing fix's own worst
case at all - a separate, uncommitted synthetic fixture (8,000 records,
300 fields each, generated for this pass only) went from 2.42s to 1.25s,
roughly 2x, with byte-identical output confirmed via `diff` before and
after. Not repeated here as a permanent fixture since `benches/
end_to_end.rs`'s existing shape already covers the "ordinary JSON" case
this file's numbers above are about, and a dedicated wide-object
regression test lives in `src/lib.rs`'s own test suite instead of here.

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
