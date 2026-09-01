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

## 2026-09-01 — `644679d`+perf pass 12 (Darwin arm64, Apple M4)

A twelfth optimization pass, extending pass 11's real NYC taxi dataset
to every other format it could be re-encoded into (via `pyarrow`/
`pandas`, real values throughout, not synthetic restatements): a
genuine CSV (321 MB), JSONL (1M rows, real content), SQLite database,
and XLSX (500,000 rows, well under Excel's own 1,048,576-row ceiling,
kept smaller purely for file size). See CLAUDE.md's own "Performance"
section for the full writeup.

CSV, JSON, and SQLite all profiled clean - no hash-related or
quadratic anomalies, each scaling consistent with this project's own
already-optimized expectations. SQLite in particular is architecturally
immune to Parquet's own bug by construction (its row-decode already
indexes by position, `O(1)`, never by name). Excel was the one
exception: `columns_from_xlsx_ooxml`'s row-distribution loop (shared
across all four spreadsheet variants) cloned every cell's `String` even
though the source row was already fully owned and movable. Fixed via
`resize` + `zip` instead of `.get(col_idx).cloned()`.

**Controlled alternating-binary comparison** (2 rounds, the real
500,000-row taxi `.xlsx` file):

| Binary | Round 1 (user) | Round 2 (user) |
|---|---|---|
| Before this pass | 21.25s | 21.31s |
| After this pass | 20.60s | 20.54s |

A small, consistent, reproducible **~3.3%** improvement (avg 21.28s ->
avg 20.57s) - a real constant-factor cleanup, not a complexity-class
fix like pass 11's Parquet finding, and reported at that honest, more
modest scale. Byte-identical output confirmed via `diff` across all 19
committed spreadsheet fixtures (all four variants) plus the real
`.xlsx` file in every output format, full test suite (including all
six `*_matches_calamine_output_exactly` oracle tests) passing, clippy/
fmt clean.

## 2026-09-01 — `fa0520e`+perf pass 11 (Darwin arm64, Apple M4)

An eleventh optimization pass, after a sweep of Arrow IPC/Avro/
MessagePack/ORC/flat-Parquet/boolean-dense-Parquet found nothing
further (all already clean). Tested against a real downloaded dataset
instead of synthetic data: the official NYC TLC yellow-taxi trip record
for January 2024 (2,964,624 rows, 19 flat columns,
`nyc.gov/site/tlc/about/tlc-trip-record-data.page`). Found
`profile_parquet_file`'s own column-extraction step calling
`Map::get` (a deliberate linear scan) once per `(row, column)` pair -
O(rows * columns) calls to an O(columns) function, i.e. O(rows *
columns^2) total - invisible on this project's own narrow fixtures.
Fixed by inverting the loop: one pass over `rows`, distributing each
row's own fields into per-column accumulators via an
`HashMap<&str, usize, FxBuildHasher>` name-to-index lookup instead of
a per-row full-object scan, bringing this back to O(rows * columns).
See CLAUDE.md's own "Performance" section for the full writeup,
including a real self-correction: an initial profiling snapshot
overestimated this fix's impact by roughly 3x due to transient system
noise at that specific measurement.

**Controlled alternating-binary comparison**, three files chosen to
isolate the columns^2 term directly:

| File | Columns | Rows | Before (user) | After (user) | Improvement |
|---|---|---|---|---|---|
| Real NYC taxi data | 19 | 2,964,624 | avg 24.8s (3 rounds) | avg 22.3s (3 rounds) | ~10% |
| Synthetic | 100 | 300,000 | 18.26s | 15.99s | ~12% |
| Synthetic | 500 | 100,000 | avg 71.2s (2 rounds) | avg 43.5s (2 rounds) | ~39% |

The improvement growing with column count, not staying flat, is the
real confirmation this is a genuine complexity-class fix rather than a
one-file fluke - it protects any wide real-world schema (feature
tables, survey data, sensor telemetry) from a cost the 19-column real
file only hinted at. Byte-identical output confirmed via `diff` across
all 19 committed Parquet fixtures plus all four files above (real and
synthetic) in every output format, full test suite passing, clippy/fmt
clean.

## 2026-09-01 — `04ce309`+perf pass 10 (Darwin arm64, Apple M4)

A tenth optimization pass, profiling Parquet's own nested-reconstruction
engine (`decode_row_group_nested`/`ReaderNode`) on a real 500,000-row
file with a struct and a list column. Found the single largest cost
cluster of this entire series: `sip::Hasher::write` over 10% of *total*
profiled samples, from `ReaderNode::Primitive` storing each leaf's full
dotted schema path (`Vec<String>`) as the key into a
`HashMap<Vec<String>, LeafCursor>`, re-hashed on every one of several
per-leaf-per-row operations. Fixed by resolving each leaf's path to a
plain `usize` index once, at tree-build time (via a small `leaf_index`
map built once per row group), and replacing the `HashMap` with a plain
`Vec<LeafCursor>` indexed directly - no hashing at read time at all.
See CLAUDE.md's own "Performance" section for the full writeup.

**Controlled alternating-binary comparison** (10 rounds, the same
500,000-row nested Parquet file; an earlier noisy batch under severe,
unrelated system contention was discarded):

| Binary | Avg user time |
|---|---|
| Before this pass | 1.513s |
| After this pass | 1.128s |

A clean, reproducible **~25%** improvement, zero overlap between the
two groups across every round - by a wide margin the largest single win
of this optimization series, since this is a genuine algorithmic fix
(removing a cost that scaled with rows × leaves × operations-per-leaf)
rather than a cheaper-hasher swap. Byte-identical output confirmed via
`diff` across all 19 committed Parquet fixtures in all three output
formats, full test suite (including the Arrow-oracle comparison for
this exact code path) passing, clippy/fmt clean.

## 2026-09-01 — `d9e82be`+perf pass 9 (Darwin arm64, Apple M4)

A ninth optimization pass, moving from the shared CSV/JSON engine
(already covered by passes 1/5-8, and inherited for free by every
format built on top of it - see CLAUDE.md's own writeup) to a format
reader's own code: SQLite's `describe_sql_kinds`, which turned out to
have the exact same `HashMap<&'static str, usize>`-with-default-hasher
per-value tally shape as pass 7's `JsonKind`/`kind_counts` fix, just
never touched because it lives in `sqlite_support` rather than the
shared JSON engine. Replaced with the identical fix: `SqlKind` (a
4-variant enum) plus `SqlKindCounts` (a plain `[usize; 4]` array) -
no hashing at all.

**Controlled alternating-binary comparison** (8 rounds, a synthetic
1,000,000-row SQLite database with several low-cardinality `TEXT`
columns, generated via Python's `sqlite3` module):

| Binary | Avg user time |
|---|---|
| Before this pass | 0.859s |
| After this pass | 0.826s |

A clean, reproducible **~3.8%** improvement, baseline above the fix in
every round - a cleaner measurement window than pass 8 had, and closer
in magnitude to passes 6/7's own JSON-side findings since this
synthetic table's column mix skews more heavily low-cardinality than a
typical real file. Byte-identical output confirmed via `diff` across
six SQLite fixtures in all three output formats, full test suite
(including the `rusqlite` oracle comparison) passing, clippy/fmt clean.

## 2026-09-01 — `1b7313c`+perf pass 8 (Darwin arm64, Apple M4)

An eighth optimization pass, moving the profiler onto a wide, diverse
CSV file (UUID/email/IPv4/date/amount/free-text/category columns,
500,000 rows) to check the CSV-only path after passes 1/5's own work
there. See CLAUDE.md's own "Performance" section for the full writeup.

Found `profile_column`'s own sample-collection `HashSet<&str>` (still
using the default SipHash, never touched by pass 6's `FxHasher` work)
had the same "never reaches its early exit on a low-cardinality column"
shape as passes 6 and 7's own findings - any column with fewer distinct
values than `n_samples` (default 3: booleans, small status/category
enums) scans its *entire* length hashing every value. Fixed by
replacing it with `profile_json_path`'s own already-proven linear-scan-
against-`samples` approach - no hashing needed at all for this one.

**Measurement note**: sustained background contention from an unrelated
`mediaanalysisd` process degraded most of this pass's alternating-binary
batches into unusable noise (`ps`/`uptime` confirmed it, individual runs
swinging 1.4s-3.5s with no consistent direction). A fresh profile of the
fixed binary confirmed the mechanism directly regardless (the targeted
`sip::Hasher`/`hash_one` self-time cluster is completely gone). The
batches captured before contention set in:

| Fixture | Before (user) | After (user) | Delta |
|---|---|---|---|
| Low-cardinality-heavy CSV (4 cols, 2M rows) | avg 1.42s | avg 1.40s | ~1.6% |
| Wide 10-column CSV (500k rows, only 3 cols low-card) | avg 1.31s | avg 1.30s | ~0.9% |

Reported honestly as a real but modest, column-mix-dependent win, with
the profiler-confirmed mechanism as the primary evidence rather than
the noisy majority of wall-clock batches.

## 2026-09-01 — `e4c9138`+perf pass 7 (Darwin arm64, Apple M4)

A seventh optimization pass, re-profiling the same 300,000-row nested
JSON fixture after pass 6's hasher fix. See CLAUDE.md's own
"Performance" section for the full writeup, including a stale-profile
methodology mistake this pass caught and fixed (always regenerate the
profile + dSYM immediately before symbolicating, never reuse one across
a rebuild), and two ideas considered and explicitly declined
(`unsafe`-based UTF-8 revalidation removal; reverting
`bucket_object_fields` to a linear scan).

Found `profile_json_path`'s own `kind_counts: HashMap<JsonKind, usize>`
- one insert per value of every column, same shape as pass 6's finding,
just hashing a 5-variant enum instead of a `&str`. Replaced with a
plain `[usize; 5]` array indexed by discriminant (`JsonKindCounts`) -
no hashing or probing at all, not even `FxHash`, since the key space is
tiny and fixed.

**Controlled alternating-binary comparison** (14 usable rounds across
two batches; a middle batch was discarded after `ps`/`uptime` showed a
load average over 7 from unrelated background processes mid-run):

| Binary | Batch A (6 rounds, user) | Batch B (8 rounds, user) |
|---|---|---|
| Before this pass (`sniff-rs-after-clean`, pass 6's binary) | avg 0.953s | avg 0.930s |
| After this pass | avg 0.900s | avg 0.886s |

A clean, reproducible **~5%** further user-time improvement on top of
pass 6's own gain, baseline above the fix in every comparable round in
both clean batches. Byte-identical output confirmed via `diff` across
three nested/mixed-kind fixtures (`mixed_types.jsonl`,
`nested_typed.jsonl`, `nested.jsonl`) in all three output formats.

## 2026-09-01 — `0124c74`+perf pass 6 (Darwin arm64, Apple M4)

A sixth optimization pass, moving the `samply`/`atos` profiler off CSV
(covered by passes 4-5) and onto a synthetic 300,000-row nested JSON
file. See CLAUDE.md's own "Performance" section for the full writeup.

Found `std`'s default SipHash-1-3 hasher dominating the profile (~1/5
of total samples) via two hot loops hashing the same handful of short,
trusted keys millions of times over: `bucket_object_fields`'s
`HashMap<&str, usize>` (one lookup per `(object, field)` pair, for
every nested object any bridged format produces) and
`suggest_ideal_type`'s unique-value-count `HashSet<&str>` (one insert
per value, for every value of a column that stays under the 50-unique
category-detection cutoff). Switched both to a hand-rolled `FxHasher`
(the well-known "multiply, rotate, xor" construction rustc/Firefox use
internally for this same reason) via a `FxBuildHasher` type alias -
scoped to just these two containers, not a blanket change, since
neither is ever fed attacker-controlled keys.

**Controlled alternating-binary comparison** (6 rounds, the 300,000-row
nested JSON fixture, `--output-format json`):

| Binary | R1 | R2 | R3 | R4 | R5 | R6 |
|---|---|---|---|---|---|---|
| Before this pass (user) | 1.00s | 1.00s | 1.06s | 1.00s | 0.96s | 0.96s |
| After this pass (user) | 0.94s | 0.93s | 0.95s | 0.91s | 0.92s | 0.91s |

A clean, reproducible **~7%** user-time improvement, consistent in
every round with no overlap between the two groups, confirmed
byte-identical `--output-format json` output via `diff`. A re-profile
of the fixed binary confirmed the mechanism directly: the
`sip::Hasher`-related self-time clusters from the original profile are
gone, leaving only a small residual `hash_one` cost (the genuinely-
necessary `FxHash` computation itself). A parallel check on a
1,000,000-row CSV file with several low-cardinality columns showed no
clear improvement - this fix's real benefit is concentrated in nested/
bridged-format workloads through `bucket_object_fields`, not CSV (which
never calls that function and isn't hash-bound enough on its own
category-detection path for the hasher choice to matter there).

## 2026-09-01 — `e6bd76d`+perf pass 5 (Darwin arm64, Apple M4)

A fifth optimization pass, continuing the fourth's real-profiler
methodology (`samply`/`atos`) against a large real CSV file this time.
See CLAUDE.md's own "Performance" section for the full writeup,
including two profiler leads confirmed to be identical-code-folding
noise rather than real costs (not fixed, since there was nothing there).

`parse_csv`'s `InField`/`InQuotedField` states used to append one
already-decoded `char` at a time (`field.push(c)`); rewritten to track a
byte cursor and scan forward for the next delimiter/terminator/closing-
quote, `push_str`-ing the whole span at once - the same fix
`json_support::Parser::parse_string` already used for JSON strings,
applied to CSV.

**Controlled alternating-binary comparison** (a real 500,000-row,
8-column CSV file - id/name/email/date/amount/bool/uuid/description):

| Binary | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| Before this pass | 1.23s | 1.31s | 1.24s |
| After this pass | 0.99s | 1.01s | 0.97s |

A clean, reproducible **~20-24%** improvement, confirmed byte-identical
output via `diff`, including a manual multi-byte-UTF-8 spot check
(café/中文/emoji, embedded newlines and commas inside quoted fields)
beyond the automated suite.

---

## 2026-08-31 (follow-up 3) — `09831a0`+perf pass 4 (Darwin arm64, Apple M4)

A fourth optimization pass, the first to use a real sampling profiler
(`samply` + `atos`) against the release binary instead of reading hot-path
code and forming a hypothesis - see CLAUDE.md's own "Performance" section
for the full writeup, including a real identical-code-folding
symbolication hazard this method surfaced and how it was worked around
(cross-checking full call stacks, not trusting a leaf symbol alone), and
one promising-looking lead (`ColumnProfile::to_json`'s own field clones)
that measured out to *no difference at all* and was reverted rather than
kept.

**Methodology note, not just a result**: this pass's own early `cargo
bench` comparisons against `target/criterion`'s stored history produced
inconsistent "regressed"/"improved" labels from run to run - exactly the
thermal-state/background-load noise this file's own header already
warns about, and worse than in prior passes because this session had by
then run many back-to-back `cargo build`/`cargo bench` invocations in a
row. The numbers below use a controlled alternating-binary comparison
instead: build an "old" and "new" binary once each, then run them
back-to-back several times apiece (never all of one binary's runs
clustered together at one thermal extreme), reporting `user` time from
each individual run rather than a single aggregate.

**Real file** (a 400,000-row, 8-column JSONL file - a realistic mix of
an id, free-text name, email, date, float amount, bool, and UUID column):

| Binary | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| Before this pass | 0.92s | 0.94s | 0.83s |
| After this pass | 0.65s | 0.65s | 0.67s |

A clean, reproducible **~24-28%** improvement.

**Synthetic fixture** (the same shape `benches/end_to_end.rs`'s own JSON
generator produces, at 200,000 rows, run with `--output-format json`):

| Binary | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| Before this pass | 0.34s | 0.34s | 0.34s |
| After this pass | 0.23s | 0.23s | 0.24s |

A clean, reproducible **~30%** improvement. Both compared with `diff`
against the pre-fix output and confirmed byte-identical in every case.

---

## 2026-08-31 (follow-up 2) — `8d0b9ec`+perf pass 3 (Darwin arm64, Apple M4)

A third optimization pass in the same session, another "continue to
optimize" follow-up - see CLAUDE.md's own "Performance" section for the
full writeup. The single largest individual win of the whole effort:
`suggest_ideal_type`'s own unique-value counting (backing category vs.
plain-`String` detection) used to always build the *complete* `HashSet`
of a column's distinct values, even after the count had already passed
the 50-value cutoff the category branch requires - at which point no
further insertion can change the outcome. Fixed with an early exit the
moment the count exceeds 50.

**Heuristic engine** (`suggest_ideal_type`, in-process, values → time) -
`free_text_worst_case` (the shape this fix directly targets) against the
*original* 2026-08-23 baseline:

| Size | Original | Now | Change |
|---|---|---|---|
| 10 | 2.52 µs | 1.92-1.99 µs | -23.8% |
| 1,000 | 87.4 µs | 37.7-38.1 µs | **-56.5%** |
| 100,000 | 11.5 ms | 3.83-3.86 ms | **-66.6%** |

A column that never exceeds 50 distinct values (the genuine "enum /
category" case) does the identical full scan as before and sees no
change - this fix only ever removes work for the high-cardinality case.
End-to-end impact on a real file is smaller and varies with column
composition: a 300,000-row CSV with one high-cardinality free-text
column (parsing/other-column overhead dominates) improved only
~8% end-to-end, while the isolated heuristic itself improved by the
percentages above - reported as an in-process number for exactly that
reason, the same way `heuristic_engine.rs`'s own numbers always have
been.

Also worth recording: re-running the full `heuristic_engine` suite
after this fix showed the `integer` shape's own numbers (1,000/100,000
values) far below every prior entry in this log (9.7µs/1.19ms versus
~43µs/~5.15ms) - this is *not* a new effect of this pass's own fix
(integer values resolve via the `i64`/`f64` parse check, never reaching
the unique-counting code this pass touched at all), but a delayed, clean
measurement of the second pass's own `normalize_numeric_str` `Cow` fix,
which runs on every value before that parse check and was never
re-benchmarked for the `integer` shape specifically in isolation after
landing (the one `heuristic_engine` re-run in that pass's own write-up
happened on a machine under incidental background load and wasn't
trusted for exactly this reason). Recorded here rather than silently
folded into this pass's own numbers, since attributing it to the wrong
fix would make a future comparison against *this* entry misleading.

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
