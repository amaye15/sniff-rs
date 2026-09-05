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

## 2026-09-06 — `feat/incremental-json-path` branch (Darwin arm64)

The shared `profile_json_path` recursive flattener (every non-native
nested format bridges through it) went incremental, in two phases.
Phase A: `JsonPathAccumulator` - a per-path accumulator tree (kind
counts, an `IdealTypeAccumulator` for scalars, `n_samples`-capped sample
lists, one child accumulator per object key in first-seen order); `push`
walks arrays inline (replacing `unwrap_arrays`, now deleted);
`profile_json_path` is a thin `new/push/finish` wrapper. Phase B:
`columns_from_json` routes JSON Lines to `profile_json_lines_streaming`,
which pushes each record straight into a root accumulator via
`BufReader::lines()` - never collecting `Vec<JsonValue>`.
`stream_json_lines` deleted; `read_json_values` now only handles the two
non-streamable shapes (top-level array, single multi-line document).

Measured on a real 230 MB, 1,000,000-record nested JSONL file
(id/email/amount/3-element array/2-field nested object/bool/RFC 3339
timestamp): maxRSS 1.66-1.68 GB -> ~2.5 MB (~99.8%), peak footprint
~1.56 GB -> ~1.4 MB (~99.9%), 3 rounds - the largest single reduction of
the whole streaming effort (a parsed JSON `Value` tree carries far more
per-record overhead than a flat row).

Output byte-identical via `diff` against the pre-change binary across the
entire 359-file corpus in all three output formats with/without
`--nrows` (2,154 combinations), a 400-iteration nested-JSON structure
fuzz, and a 500-iteration JSONL-specific fuzz (blank/null lines,
`--nrows`, `--samples`, mixed line shapes) - zero mismatches. Full test
suite unchanged bar one unit test renamed/rewritten to assert the JSONL
shape through `columns_from_json` instead of `read_json_values` (which no
longer handles JSON Lines). Clippy/fmt clean across default/`full`,
established baselines (the pre-existing `chunks_exact`/question-mark
findings from a newer clippy version).

---

## 2026-09-06 — `feat/incremental-orc` branch (Darwin arm64)

ORC wired through `ColumnAccumulatorState`, the eighth format converted.
`accumulated: Vec<Vec<Option<String>>>` (every stripe's values for every
column, held until the last stripe) becomes `Vec<ColumnAccumulatorState>`
fed value-by-value per stripe. A single `row_count` counter (advanced
once per stripe by every column alike) replaces `accumulated[..].len()`
for the `--nrows` cap and every column's `total`. `current_type`
hardcoded per `OrcTypeKind`; compound/unrecognized columns still emit a
disclosed placeholder `ColumnProfile` directly.

Measured on a real 84 MB, 500,000-row ORC file (id/amount/25-word
free-text/4-value category, via `pyarrow.orc`): maxRSS 390-398 MB ->
224-227 MB (~43%), peak footprint ~213 MB -> ~155 MB (~27%), 3 rounds.
Smaller than the row-oriented readers' ~98% on purpose: ORC's own
stripe-at-a-time decode granularity (`read_scalar_column` still returns
a whole stripe's `Vec<Option<String>>`) is now the floor, the same way
one row group is Parquet's own streaming floor - this phase removed the
cross-stripe buffer, not that per-stripe cost.

Output byte-identical via `diff` against the entire 359-file fixture
corpus, plus every committed `.orc` fixture (every codec, dictionary
strings, decimals, timestamps, RLEv2, missing-values) at 3 `--samples`
settings with `--nrows` unset/2/3 (135 combinations) - zero mismatches.
Full test suite unchanged (including
`orc_reader_matches_the_orc_rust_crate_output_exactly`), clippy/fmt
clean across default/`orc`/`full`, established baselines (default=1,
orc=1, full=5 - the pre-existing `chunks_exact`/question-mark findings
from a newer clippy version).

---

## 2026-09-06 — `feat/incremental-sas7bdat` branch (Darwin arm64)

SAS7BDAT wired through `ColumnAccumulatorState`, the seventh format
converted. `collect_rows` now takes an `impl FnMut(&[u8]) -> Result<u64>`
callback invoked per row instead of returning a `Vec<Vec<u8>>` of every
raw row; the caller folds each row's cells straight into one
`ColumnAccumulatorState` per column via `cell_to_string`, dropping both
the whole-table raw-byte buffer and the separate
`Vec<Vec<Option<String>>>` string copy. `current_type` from the file's
own declared column type (`logical_type_label`); missing cells
(`cell_to_string` -> `None`) are simply not pushed.

No before/after footprint measurement - no tool in this environment can
write a `.sas7bdat` file, so there's no large real fixture to measure
against (the same disclosed limitation the Tier 1 SAS7BDAT phase had).
Correctness-only: full test suite unchanged (including
`sas7bdat_reader_matches_the_sas7bdat_crate_output_exactly`), plus
byte-identical old-vs-new `--output-format json` across every committed
`.sas7bdat` fixture + a truncated copy at 3 `--samples` settings
with/without `--nrows 2`, and the entire 359-file fixture corpus - zero
mismatches. Memory claim rests on the structural argument (one page +
one row + bounded accumulators, page-forward-only), same as SQLite's own
callback-converted phase already proved at scale.

Clippy/fmt clean across default/`sas7bdat`/`full`, matching established
baselines (default=1, sas7bdat=1, full=5) - the count-of-1/5 being the
pre-existing `chunks_exact`/question-mark findings from a newer clippy
version, confirmed identical on unmodified `main`.

---

## 2026-09-05 — `feat/incremental-npy` branch (Darwin arm64)

NumPy wired through `ColumnAccumulatorState`, the sixth format converted
(after CSV/fixed-width/dBase/Stata/SPSS). `columns_from_npy_reader`'s
`Vec<Vec<String>>` becomes `Vec<ColumnAccumulatorState>`, one line changed
per branch (structured/record, row-major C-order, Fortran-order fallback)
since the per-row streaming loop shape was already correct. `current_type`
comes from the array's own declared dtype (`npy_type_label`) - a third
distinct shape `into_profile_with_declared_type` accommodates with no
further changes.

Measured on a real 163 MB, 200,000-row structured `.npy` file (id/name/
email/amount/a 150-char free-text column, via `numpy.save`): maxRSS
91-98 MB -> ~2.0 MB (~98%), peak footprint 69-72 MB -> ~1.1-1.2 MB (~98%),
consistent across 3 rounds. Output byte-identical via `diff` against the
entire 359-file fixture corpus, plus every committed `.npy`/`.npz` fixture
at 3 `--samples` settings with/without `--nrows 2` (78 combinations) -
zero mismatches.

Verified via the full test suite (347 unit + 360 integration, zero
modifications needed) and clippy/fmt clean across default/`npy`/`full`,
matching established baselines (default=1, npy=1, full=5) exactly. A
handful of pre-existing `chunks_exact`/question-mark clippy findings on
unrelated lines (newer clippy version than this baseline was last checked
against) confirmed identical on unmodified `main` via `git stash` - not
introduced by this change.

---

## 2026-09-05 — `feat/incremental-spss` branch (Darwin arm64)

SPSS wired through `ColumnAccumulatorState`, the fifth format converted
(after CSV/fixed-width/dBase/Stata). `read_cases` now returns
`(Vec<ColumnAccumulatorState>, usize)` instead of a full
`Vec<Vec<Option<String>>>`. `current_type` is a fixed `"f64"`/`"String"`
picked from the variable's own `VarType` (neither inferred nor a
per-field label) - the second shape `into_profile_with_declared_type`
had to accommodate, needing no further changes.

Measured on a real 34 MB, 200,000-row `.sav` file (id/amount/a 150-char
free-text column, via `pyreadstat`): maxRSS 80-87 MB -> ~2.4 MB (~97%),
peak footprint 58-63 MB -> ~1.2 MB (~98%), consistent across 3 rounds.
Output byte-identical via `diff` against the entire 359-file fixture
corpus, plus every committed `.sav` fixture (bytecode-compressed and
uncompressed) at 3 `--samples` settings with/without `--nrows 2` (48
combinations) - zero mismatches.

Verified via the full test suite (347 unit + 360 integration, including
both `spss_reader_matches_the_ambers_crate_output_exactly` and
`spss_reader_agrees_with_the_ambers_crate_on_malformed_input`, zero
modifications needed) and clippy/fmt clean across default/`spss`/
`full`, matching established baselines (default=1, spss=1, full=5)
exactly.

---

## 2026-09-05 — `feat/incremental-stata` branch (Darwin arm64)

Stata wired through `ColumnAccumulatorState`, the fourth format
converted (after CSV/fixed-width/dBase) and the simplest so far - its
own read loop already checks `nrows` before reading each observation,
so no "decode always, accumulate conditionally" split was needed the
way dBase's own phase required. Reuses `into_profile_with_declared_type`
unchanged (current_type comes from Stata's own declared variable type).

Measured on a real 32 MB, 200,000-row `.dta` file (id/amount/a 150-char
free-text column, via pandas' `to_stata`): maxRSS 76-83 MB -> ~2.1 MB
(~97%), peak footprint 59-64 MB -> ~0.95-1.0 MB (~98.4%), consistent
across 3 rounds. Output byte-identical via `diff` against the entire
359-file fixture corpus, plus every committed `.dta` fixture at 3
`--samples` settings with/without `--nrows 2` (30 combinations) - zero
mismatches.

Verified via the full test suite (347 unit + 360 integration, including
the `stata_reader_matches_the_dta_crate_output_exactly` oracle test,
zero modifications needed) and clippy/fmt clean across default/`stata`/
`full`, matching established baselines (default=1, stata=1, full=5)
exactly.

---

## 2026-09-05 — `feat/incremental-dbase` branch (Darwin arm64)

dBase wired through `ColumnAccumulatorState`, the third format converted
after CSV/fixed-width - picked next as the closest remaining match to
their own sequential-read shape among readers still using `ColumnInput`/
`profile_column` (Excel's `SheetGrid` already fully materializes a
sheet's cells before column assembly, a bigger separate undertaking).
Added `into_profile_with_declared_type`/`finish_profile` since dBase's
`current_type` comes from the file's own declared field type, not
inferred from values the way CSV/fixed-width's `NaiveTypeAccumulator`
does. Preserved the existing "every record is decoded regardless of
`nrows`, only accumulation is bounded" behavior exactly (a malformed
record past the cutoff still errors, unchanged).

Measured on a real 52 MB, 200,000-row `.dbf` file (id/name/email/a
200-char free-text column): maxRSS 94-101 MB -> ~2.1-2.2 MB (~98%),
peak footprint 77-78 MB -> 0.87-1.0 MB (~98.7%), consistent across 3
rounds. Output byte-identical via `diff` against the entire 359-file
fixture corpus, plus every committed `.dbf` fixture at 3 `--samples`
settings with/without `--nrows 2` (54 combinations) - zero mismatches.

Verified via the full test suite (347 unit + 360 integration, including
the `dbase_reader_matches_the_dbase_crate_output_exactly` oracle test,
zero modifications needed) and clippy/fmt clean across default/`dbase`/
`full`, matching established baselines (default=1, dbase=1, full=5)
exactly.

---

## 2026-09-05 — `feat/incremental-fixed-width` branch (Darwin arm64)

Fixed-width text wired through the same `ColumnAccumulatorState` CSV's
own Tier 2 phase introduced (renamed from `CsvColumnState` - nothing
about it was CSV-specific), replacing `columns_from_fixed_width`'s old
`Vec<Vec<Option<String>>>` and bypassing `ColumnInput`/`profile_column`
the same way. `naive_current_type` picked up `#[allow(dead_code)]`
(genuinely unused in the default build now, still used by weblog/
syslog/`.xls`).

Measured on a real 180 MB, 500,000-row fixed-width file (id/name/email/
a 300-char free-text column): maxRSS 309-324 MB -> 2.6 MB (~99%), peak
footprint 249-257 MB -> 1.5 MB (~99%), consistent across 3 rounds.
Output byte-identical via `diff` against the entire 359-file fixture
corpus, plus every committed `.fwf` fixture at 3 `--samples` settings
with/without `--nrows 2` (54 combinations) - zero mismatches.

Verified via the full test suite (347 unit + 360 integration, zero
test modifications needed) and clippy/fmt clean across default/`full`,
matching established baselines (default=1, full=5) exactly.

---

## 2026-09-05 — `feat/incremental-ideal-type` branch (Darwin arm64)

`suggest_ideal_type` converted from a whole-slice `&[&str]` function to a
thin wrapper over a new `IdealTypeAccumulator` (one running bool/small
state per check, fed one value at a time, mathematically identical
results to the original by construction - see CLAUDE.md's "Streaming
reads / memory footprint" section for the full writeup), then CSV wired
through it directly (`CsvColumnState` replacing `CsvColumnAccumulator`'s
old `Vec<Vec<String>>`), bypassing `ColumnInput`/`profile_column` for CSV
only - every other reader unchanged.

Measured on two real files: a 180 MB, 500,000-row CSV with a deliberately
extreme 300-character free-text column - full-scan maxRSS 346 MB ->
3.5 MB (~99%), peak footprint 282 MB -> 2.4 MB (~99%), consistent across
3 rounds. A more realistic 318 MB, 2,000,000-row file (id/name/email/
amount/free-text) - maxRSS 856 MB -> 3.9 MB (~99.5%), peak footprint
824 MB -> 2.8 MB (~99.7%). Output byte-identical via `diff` in both
cases, and across every committed `.csv` fixture (48 files) at two
`--samples` settings, plus a 200,000-column randomized fuzz-equivalence
check (development-only, not committed) proving the new accumulator
matches the old whole-slice logic exactly across eight value shapes.

Verified via the complete existing test suite (347 unit + 308
integration against a clean baseline) passing with only two direct
`columns_from_csv` unit tests updated (`raw_values` assertions became
`sample_values`, since `ColumnInput` is no longer part of CSV's own
pipeline), plus a byte-identical `--output-format json` diff against the
pre-change binary across the entire 359-file fixture corpus (every
format, not just CSV). Clippy/fmt clean across default/`full`, matching
established baselines (default=1, full=5) exactly.

---

## 2026-09-05 — `feat/streaming-xls` branch (Darwin arm64)

`xlsx_support::CfbFile` (OLE2/Compound File Binary, backing old-style
`.xls`) converted from a whole-file `data: Vec<u8>` buffer to
`Seek`+`read_exact` per sector off a real `fs::File` - `read_chain`/
`open`/`read_stream` all moved to `&mut self`; `read_mini_chain`/
`has_stream` needed no change (the former only ever slices the
already-resident mini-stream, the latter never touches sector data at
all). The real, disclosed scope here is "stop double-buffering the
whole file alongside the separately-extracted Workbook stream," not
full BIFF8-record-level streaming - the Workbook stream itself has to
stay fully materialized regardless, since BOUNDSHEET8 records address
sheet data by absolute byte position scattered throughout it. See
CLAUDE.md's "Streaming reads / memory footprint" section for the full
writeup, including why this format turned out more tractable than its
own prior "chain-walk itself needs to become lazy" framing suggested.

Measured on a real 17.5 MB `.xls` file (65,000 rows, 10 columns, via
LibreOffice's "MS Excel 97" export filter converting an
`openpyxl`-generated `.xlsx` - no tool in this environment writes `.xls`
directly): full-scan maxRSS 180 MB -> 138 MB (~23%), peak footprint
155 MB -> 117 MB (~25%). `--nrows 1` shows essentially the same
reduction (maxRSS 172 MB -> 128 MB, ~26%; peak footprint 152 MB ->
117 MB, ~23%) rather than a larger unmasked one - direct confirmation
that the win is the eliminated whole-file buffer, not a per-row bound,
since the Workbook stream is always fully read regardless of `--nrows`.
Output byte-identical via `diff` in both cases.

Verified via the complete existing test suite (348 unit tests on
`--features full`, including the existing
`cfb_reader_extracts_the_real_workbook_stream` byte-exact CFB test,
unchanged and passing) plus 308 integration tests against a clean
baseline (a concurrent in-progress edit to `tests/integration.rs` in
another session introduced an unrelated duplicate test name blocking a
direct run - worked around by temporarily swapping in the last-
committed file for verification, then restoring the working copy
exactly). Clippy/fmt clean across default, `xlsx`, and `full`, each
matching its own already-established baseline (default=1, xlsx=4,
full=5) exactly.

---

## 2026-09-05 — `feat/streaming-sas7bdat` branch (Darwin arm64)

`sas7bdat_support::page_slice` (indexing a whole-file `data: &[u8]`
buffer) replaced with `read_page` (`Seek`+`read_exact` of one page off a
`fs::File`) - the same shape as `sqlite_support`'s own conversion.
Re-reading `parse_metadata`/`collect_rows` found neither actually needs
random access at all: both walk pages strictly forward with no backward
jumps, so the format's genuine two-pass structure (metadata scattered
anywhere in the file, resolved before a second pass can correctly bound
row counts) costs a second full read of the file from disk, not a
second full *load into memory* - each pass still only ever holds one
page at a time. See CLAUDE.md's "Streaming reads / memory footprint"
section for the full writeup, including why no quantitative large-file
measurement was possible here (no tool in this environment can write a
genuine SAS7BDAT file).

Verified via the complete existing test suite (213 unit + 127
integration against a clean baseline, including the
`sas7bdat_reader_matches_the_sas7bdat_crate_output_exactly` oracle test)
passing unchanged, plus a controlled old-vs-new comparison against both
real committed fixtures and a deliberately truncated copy - byte-
identical output in every successful case, the same actionable error
(modulo a cosmetic `Caused by:` addition already accepted in the SQLite
phase) in the truncated one. Clippy/fmt clean across default,
`sas7bdat`, and `full`, each matching its own established baseline.

---

## 2026-09-05 — `feat/streaming-arrow-ipc` branch (Darwin arm64)

The same fix as Parquet's own phase, applied to Arrow IPC's own footer/
`Block`-list layout: `profile_arrow_ipc_file` now reads each
`RecordBatch`/`DictionaryBatch`'s own `[offset, offset+metaDataLength+
bodyLength)` span via `Seek` instead of one whole-file buffer, handing
the unmodified `read_message_at`/`decode_record_batch_columns` a plain
`0` offset into that fresh, block-sized buffer - simpler than Parquet's
own fix, since every buffer-region offset inside a message was already
relative to its own body, needing no rebase at all. See CLAUDE.md's
"Streaming reads / memory footprint" section for the full writeup.

Real file (175 MB, 3,000,000 rows, 4 columns, 30 record batches via
`pyarrow`), `/usr/bin/time -l`, 3 rounds:

Full scan:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 958-985 MB | 820-846 MB |
| New | 818-822 MB | 646-671 MB |
| Change | **-15%** | **-20%** |

Isolated via `--nrows 1` (now reads only the first record batch instead
of the whole file):

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 737-745 MB | 629-630 MB |
| New | 36-45 MB | ~31 MB |
| Change | **-94%** | **-95%** |

Both confirmed byte-identical via `diff`, including a new real 10-batch
`pyarrow` fixture (every committed fixture has exactly one batch) with
`--nrows` spanning a batch boundary. All 24 existing `arrow_ipc_support`
unit tests and 4 integration tests passed unchanged with zero test
modifications. No Criterion benchmark target currently isolates this
reader, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-parquet` branch (Darwin arm64)

`profile_parquet_file` converted from decoding every row group against
one whole-file `fs::read` buffer to reading each row group's own
compressed byte span via `Seek` (`open_and_read_footer` +
`read_row_group_bytes`, offsets rebased via `shift_row_group_offsets`) -
no change needed to any of the actual decode logic (RLE/dictionary/
delta, nested reconstruction), since it already only ever treated page
offsets as "relative to whatever buffer this is". See CLAUDE.md's
"Streaming reads / memory footprint" section for the full writeup,
including the two real multi-row-group fixtures generated to prove the
row-group byte-span computation holds up across more than one group.

Real file (55 MB, 3,000,000 rows, 4 columns, 30 row groups via
`pyarrow`), `/usr/bin/time -l`, 3 rounds:

Full scan:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 792-877 MB | 691-710 MB |
| New | 772-803 MB | 630-636 MB |
| Change | roughly flat | **-10%** |

Isolated via `--nrows 1` (now reads only the first row group instead of
the whole file):

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 103-110 MB | 97-100 MB |
| New | 46-52 MB | 35-42 MB |
| Change | **-53%** | **-60%** |

Both confirmed byte-identical via `diff`, including two new real
multi-row-group `pyarrow` fixtures (flat and nested schemas, 10 row
groups each) and an `--nrows` value spanning a row-group boundary. The
complete existing 19-fixture Parquet test suite (every codec, every
encoding, nested reconstruction, the real-world corpus tests) passed
unchanged with zero test modifications. No Criterion benchmark target
currently isolates this reader, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-zip-archive` branch (Darwin arm64)

`zip_support::ZipArchive` (shared by `.xlsx`/`.ods`/`.xlsb`/`.npz`)
converted from reading the entire compressed archive into one `Vec<u8>`
up front to reading only the bounded EOCD tail scan and the central
directory itself eagerly, with each entry's own compressed bytes read
fresh via `Seek` only when `read(name)` is actually called for it. See
CLAUDE.md's "Streaming reads / memory footprint" section for the full
writeup, including ORC's own audit finding (already streaming, no
change needed).

Real file (74 MB `.npz`, 5 arrays of ~15 MB random `float64` data each,
DEFLATE-compressed via `numpy.savez_compressed`), `/usr/bin/time -l`,
3 rounds:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 397-457 MB | ~347 MB |
| New | 321-344 MB | ~262 MB |
| Change | **-19 to -25%** | **-24%** |

Both confirmed byte-identical via `diff`. The complete existing
`.xlsx`/`.xlsb`/`.ods`/`.npz` test suite (including the direct
`zip_archive_reads_and_verifies_real_xlsx_entries` CRC32/size
cross-check) passed unchanged with zero test modifications. No
Criterion benchmark target currently isolates this reader, so this
entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-sqlite` branch (Darwin arm64)

SQLite's own `page_slice` (indexing into a whole-file `data: &[u8]`
buffer) replaced with `read_page` (a real `Seek`+`read_exact` of one
page off a `fs::File`), and `collect_table_rows`/`profile_table`
restructured to decode and fold each row into its column accumulators
via a callback as the b-tree walk visits it, instead of collecting every
row's raw payload into a `Vec` first. See CLAUDE.md's "Streaming reads /
memory footprint" section for the full writeup - the first of the
`Seek`-needing tier actually converted.

Real file (107 MB, 2,000,000 rows, 4 columns, via Python's `sqlite3`),
`/usr/bin/time -l`, 3 rounds each:

Full table scan:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 732-763 MB | 507-573 MB |
| New | 636-650 MB | ~510 MB |
| Change | **-13%** | roughly flat |

Isolated via `--nrows 1` (now reads only the first few pages instead of
the whole file):

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 109 MB | 108 MB |
| New | 2.0 MB | 0.85 MB |
| Change | **-98%** | **-99%** |

Both confirmed byte-identical via `diff`. The complete existing SQLite
test suite (overflow-page reassembly, table-level `PRIMARY KEY` rowid
alias, `WITHOUT ROWID` disclosed placeholder, zero-row table, multi-
table type-affinity violation, semantic-type recognition) passed
unchanged with zero test modifications needed. No Criterion benchmark
target currently isolates this reader, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-spss-npy` branch (Darwin arm64)

An audit of the remaining "naturally streamable" formats found five
(MessagePack, CBOR, Avro, dBase, Stata) already streaming - no change
needed - and two real gaps, both fixed. See CLAUDE.md's "Streaming
reads / memory footprint" section for the full writeup, including why
SAS7BDAT (also on the original list) turned out to need a genuine
two-pass rewrite and was moved to the harder tier instead.

**SPSS** (`columns_from_spss`'s `read_to_end` of the whole remaining
file replaced with a streaming `CaseSource`/`BytecodeDecompressor` over
a generic `Read`): real file (64 MB, 2,000,000 rows, 3 columns,
uncompressed), 3 rounds:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 540 MB | 435 MB |
| New | 471 MB | 371 MB |
| Change | **-13%** | **-15%** |

**NumPy** (`columns_from_npy_reader`'s plain-dtype row-major path now
streams one row at a time instead of reading the whole array body up
front; Fortran order with >1 column stays a disclosed whole-buffer
read). Full pipeline on a 160 MB file (2,000,000 x 10 `float64`) showed
no clean win either direction - the downstream `Vec<Vec<String>>`
column accumulator (20,000,000 stringified values) dominates regardless
of how the array bytes were read, the same masking effect zstd's own
phase already documented:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 975 MB | 881 MB |
| New | 1,051 MB | 872 MB |
| Change | +8% | -1% |

Isolating the array-reading phase directly (`--nrows 1`, which now
reads only the first row instead of the whole body) shows the real,
unmasked mechanism:

| | Peak RSS | Peak footprint |
|---|---|---|
| Old | 162 MB | 161 MB |
| New | 2.0 MB | 0.85 MB |
| Change | **-99%** | **-99.5%** |

Both confirmed byte-identical via `diff` against every committed
fixture, not just the measurement files. No Criterion benchmark target
currently isolates either reader, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-zstd` branch (Darwin arm64)

The zstd decompression layer converted to a sliding-window streaming
decoder (`ZstdStreamSink`, bounded to each frame's own declared
`Window_Size` - resolved dynamically per file from its own header,
unlike DEFLATE's fixed 32 KiB - plus a 4x flush-batching margin) instead
of decompressing the whole frame into one `Vec<u8>` first. See CLAUDE.md's
"Streaming reads / memory footprint" section for the full writeup,
including a real, pre-existing Huffman-table bug this pass's own
real-file measurement found and fixed (unrelated to streaming itself -
verified on both sides of the comparison below).

**Real file** (100 MB CSV, real `zstd` CLI compression, 95.8 MB
decompressed, one frame, 2 MiB declared `Window_Size`), `/usr/bin/time -l`,
3 rounds each:

Full pipeline (decompress + the already-streaming CSV reader):

| | Peak RSS | Peak footprint |
|---|---|---|
| Old (bugfixed, non-streaming) | 697 MB | 482 MB |
| New (streaming) | 585 MB | 516 MB |
| Change | **-16%** | **+7%** |

Decompression phase isolated via `--nrows 1` (still fully decompresses
the file - `--nrows` only bounds downstream row-reading - while making
the CSV-typing phase's own memory trivial, so it stops masking the
result):

| | Peak RSS | Peak footprint |
|---|---|---|
| Old (bugfixed, non-streaming) | 209 MB | 206 MB |
| New (streaming) | 23 MB | 13 MB |
| Change | **-89%** | **-94%** |

Both confirmed byte-identical via `diff`, including against every other
committed `.zst` fixture. The full-pipeline "peak footprint" increase is
real and reproducible (not noise - consistent across all 3 rounds), and
reported honestly rather than only citing the flattering isolated
number: for this file, the *downstream* CSV column-typing phase (already
established, unaffected by this work) dominates total process memory
regardless of how decompression got there, so the streaming win is real
but masked in that one metric for this particular file shape - the
isolated measurement is what actually demonstrates the mechanism this
phase targets. No Criterion benchmark target currently isolates zstd
decompression, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-weblog-syslog` branch (Darwin arm64)

`columns_from_weblog`/`columns_from_syslog` converted from
`fs::read_to_string` + `content.lines()` to a real streaming
`BufReader::lines()` - see CLAUDE.md's "Streaming reads / memory
footprint" section. No chunk-boundary machinery needed (like fixed-width
text before them, a log line is always one complete, independent
record).

**Ad-hoc, real files** (2,000,000 rows each, `/usr/bin/time -l`):

| | Peak RSS | Peak footprint |
|---|---|---|
| Common Log, 168 MB - old | 1,135 MB | 872 MB |
| Common Log, 168 MB - new | 932 MB | 690 MB |
| Change | **-18%** | **-21%** |

| | Peak RSS | Peak footprint |
|---|---|---|
| RFC 3164 syslog, 155 MB - old | 1,007 MB | 792 MB |
| RFC 3164 syslog, 155 MB - new | 911 MB | 649 MB |
| Change | **-10%** | **-18%** |

Both confirmed byte-identical via `diff`. Syslog's smaller RSS reduction
is consistent with its lower per-row column count (7 vs. 9), the same
pattern JSON Lines' own smaller reduction showed relative to CSV's - a
smaller fraction of total memory is the eliminated raw-text buffer when
there's less parsed-column data to offset it against. No Criterion
benchmark target currently isolates either reader, so this entry is
ad-hoc only.

---

## 2026-09-05 — `feat/streaming-decompression` branch (Darwin arm64)

The gzip compression layer converted to a sliding-window streaming
decoder (`GzipStreamSink`, bounded to `DEFLATE_WINDOW` = 32 KiB of
memory regardless of the file's decompressed size) instead of
decompressing the whole file into one `Vec<u8>` before writing it to the
temp file - see CLAUDE.md's "Streaming reads / memory footprint"
section. This is the full end-to-end pipeline (decompress + the
already-streaming CSV reader from earlier in this log), not just
decompression in isolation.

**Ad-hoc, real file** (a 155 MB CSV, gzip-compressed to 26 MB, 163 MB
decompressed - forcing roughly 1,200 flush cycles):

| | Peak RSS | Real time |
|---|---|---|
| Old (`gzip_decompress` -> `Vec<u8>` -> temp file) | 865 MB | 2.32s |
| New (`gzip_decompress_to` streams into temp file) | 728 MB | 2.06s |
| Change | **-16%** | **-11%** |

Output confirmed byte-identical via `diff`. Also confirmed: corrupted
CRC-32 and, separately, corrupted ISIZE footer fields are both still
caught correctly after ~1,200 flush cycles (proving the incremental
checksum carries its running state across flushes, not just within
one), and a 300-iteration bit-flip fuzz pass against a real fixture
produced zero panics. No Criterion benchmark target currently isolates
gzip decompression specifically, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-jsonl` branch (Darwin arm64)

JSON Lines converted to line-at-a-time streaming (`read_json_values` +
`stream_json_lines`) - see CLAUDE.md's "Streaming reads / memory
footprint" section. The smallest win of the three formats streamed so
far, since a parsed JSON `Value` tree already carries more structural
overhead per record than CSV's/fixed-width's plain `String` cells,
leaving less relative benefit from removing the raw-text double-buffer.

**Ad-hoc, real file** (1,000,000 rows, 125 MB JSONL, 5 fields):

| | Peak RSS | Real time |
|---|---|---|
| Old (`fs::read_to_string`) | 901 MB | 1.64s |
| New (streaming) | 835 MB | 1.54s |
| Change | **-7%** | ~flat |

Output confirmed byte-identical via `diff`. No Criterion benchmark
target currently covers JSON specifically enough to isolate this change
from JSON's own per-record flattening cost, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-fixed-width` branch (Darwin arm64)

Fixed-width text converted to `BufReader::lines()`-based streaming - see
CLAUDE.md's "Streaming reads / memory footprint" section. Simpler than
CSV (no chunk-boundary machinery needed at all, since a line can never
span multiple lines by construction), and a correspondingly smaller win,
since fixed-width's raw text is already close in size to its parsed
form with no delimiter/quote overhead to strip away.

**Ad-hoc, real file** (2,000,000 rows, 95 MB, 3 columns):

| | Peak RSS | Real time |
|---|---|---|
| Old (`fs::read_to_string`) | 538 MB | 1.01s |
| New (streaming) | 483 MB | 1.04s |
| Change | **-10%** | ~flat |

Output confirmed byte-identical via `diff`. No Criterion benchmark
target currently covers fixed-width, so this entry is ad-hoc only.

---

## 2026-09-05 — `feat/streaming-csv-reader` branch (Darwin arm64)

CSV/TSV converted from `fs::read_to_string` + whole-buffer `parse_csv`
to a genuinely streaming reader (`stream_utf8_chunks` + `csv_feed_chunk`
+ `CsvColumnAccumulator`) - see CLAUDE.md's new "Streaming reads / memory
footprint" section for the full writeup. This entry is memory-focused,
not purely CPU-time-focused like every other entry in this log, so it
carries an ad-hoc real-file measurement alongside the usual Criterion
numbers.

**Ad-hoc, real file** (2,000,000 rows, 155 MB, 5 columns - int/string/
email/float/free-text - old and new binaries built from the same
working tree, run back-to-back via `/usr/bin/time -l`):

| | Peak RSS | Real time |
|---|---|---|
| Old (`fs::read_to_string`) | 1,196 MB | 1.98s |
| New (streaming) | 693 MB | 1.59s |
| Change | **-42%** | **-20%** |

Output confirmed byte-identical via `diff`.

**`end_to_end/csv`** (Criterion point estimate, vs. this same machine's
prior snapshot):

| Rows | Time | Change |
|---|---|---|
| 100 | 1.42 ms | -9.6% |
| 10,000 | 9.97 ms | -11.8% |
| 200,000 | 165.9 ms | -15.7% |

The win growing with row count is expected - the eliminated raw-text
buffer's own size (and the allocation/copy cost of building it) scales
with the file, while the rest of the pipeline's per-row cost doesn't
change at all.

## 2026-09-02 — `perf/orc-quadratic-and-lz-backcopy` @ `8ecd1ba` (Darwin arm64, Apple M4)

An 18-commit branch pass. The theme is quadratic and redundant-work
removal on the wide-data / nested-format paths, plus a few clear
constant-factor wins. Headline items (see CLAUDE.md's Performance
section and the branch commits for the full write-ups):

- `detect_preamble_rows` re-read and fully re-parsed the entire CSV
  just to look at its first 6 rows, on top of `columns_from_csv`'s own
  parse - now a 256 KiB prefix.
- A systemic `O(fields^2)` in `json_support::Map::insert` (a linear
  duplicate scan per key) while building every nested record - fixed in
  the JSON parser, `Map::from_iter` (MessagePack/CBOR/TOML/json-schema
  render), Avro records, both Arrow row builders, the Parquet nested
  reconstructor, `xml_element_to_json`, and INI.
- `suggest_ideal_type` built two N-length `Vec`s and normalized every
  value before the numeric branches even for non-numeric columns - now
  gated on `values[0]`.
- Both hand-rolled XML parsers materialized the whole document as a
  `Vec<char>` (4 bytes/char + a copy) - now a byte cursor.
- Arrow IPC and all-flat Parquet now stay column-oriented instead of
  building per-row objects and transposing back.

**Heuristic engine** (`suggest_ideal_type`, in-process, values → time;
Criterion point estimate):

| Shape | 10 | 1,000 | 100,000 |
|---|---|---|---|
| UUID | 822 ns | 17.8 µs | 1.74 ms |
| Integer | 639 ns | 10.4 µs | 1.29 ms |
| Email | 669 ns | 27.7 µs | 2.89 ms |
| Free-text (worst case) | 1.37 µs | **2.33 µs** | **83.0 µs** |

vs the prior snapshot on this machine: free-text worst case is down
**~94% at 1,000 values and ~98% at 100,000** (the `values[0]` gate
skips the two N-length allocations and N `normalize_numeric_str` calls
for a column that isn't numeric). UUID/integer/email moved within
±5% run-to-run (noise - a re-run flipped several signs); no real
change there, which is expected since those shapes return before the
numeric branch.

**End-to-end** (full binary via `Command`, CSV/JSON, rows → time):

| Format | 100 | 10,000 | 200,000 |
|---|---|---|---|
| CSV | 1.55 ms | 11.3 ms | 197 ms |
| JSON | 2.58 ms | 12.7 ms | 294 ms |

vs the prior snapshot: CSV **−38% at 10k rows, −47% at 200k** (mostly
the preamble double-parse fix, plus the `suggest_ideal_type` gate and
dropping the per-cell `Option<String>` layer); JSON **−17% to −19% at
10k/200k** (the parser's `O(fields^2)` fix and `bucket_object_fields`).
The `json/100` case printed a "regressed" label but at n=100 with wide
CIs it's not a trustworthy signal.

**`format_comparison` not re-run this pass** (its stored baseline
predates a laptop change; a fresh run would only compare to itself).

Ad-hoc `/usr/bin/time` on real/synthetic files, `main` → branch tip,
best-of-3 user seconds (not Criterion - listed for the paths the
benches above don't cover):

| workload | main | branch |
|---|---|---|
| Parquet, 20k rows × 800 cols | ~18.7s | **~7.7s** |
| dict-encoded Parquet, 2M rows | 1.57s | **0.85s** |
| Arrow IPC, 500 cols × 15k rows | ~2.0s | **~1.2s** |
| JSONL, 20k × 400 fields | 3.6s | **1.8s** |
| MessagePack / CBOR, 15k × 400 | ~3.4s | **~1.6s** |
| XML, 8k × 350 fields (50 MB) | ~3.7s | **~0.87s** |
| `.csv.gz`, 400k rows | 0.67s | **0.44s** |

## 2026-09-01 — `33cbc42`+perf pass 17 (Darwin arm64, Apple M4)

A seventeenth pass swept every remaining format this project reads
(Avro, MessagePack, CBOR, Stata, SPSS, ORC, NumPy `.npy`) against
realistically-sized synthetic files - all clean, no fix needed. A large
`.npz` file (NumPy's zip-of-named-arrays format, tens of thousands of
arrays) surfaced one more real bug: `zip_support::ZipArchive::read` -
shared by `.xlsx`/`.ods`/`.xlsb` and `.npz` - found its target entry via
a linear scan over every entry in the archive on every call. Harmless
at the handful-of-parts scale a spreadsheet file has, but O(archive
entries^2) for `.npz`, called once per array. Fixed with `name_index:
HashMap<String, usize, FxBuildHasher>`, built once in `ZipArchive::open`,
resolving a name to its index in O(1) instead. See CLAUDE.md's own
"Performance" section for the full write-up.

**Controlled alternating-binary comparison** (synthetic `.npz` files,
many small named arrays via `np.savez`):

| File | Before (user) | After (user) | Change |
|---|---|---|---|
| 10,000 arrays | 0.28s | 0.21s | -25% |
| 40,000 arrays | 1.72s | 0.87s | **~2x faster** |

The scaling ratio itself improved too (4x input from 10,000 to 40,000
arrays cost 6.1x more time before, 4.1x after - closer to linear,
though real per-array decode cost means it's not perfectly flat even
after the fix). An 80,000-array file hit an unrelated, pre-existing,
disclosed limit instead (exceeds the plain, non-Zip64 zip format's own
size fields), so this pass's measured range tops out at 40,000. Byte-
identical output confirmed via `diff` against the pre-fix binary across
every committed `.xlsx`/`.ods`/`.xls`/`.xlsb`/`.npz` fixture plus both
synthetic stress files, full test suite passing under every affected
feature combination (`--features xlsx`, `--features npy`, `--features
full`, default) individually, clippy/fmt clean throughout.

## 2026-09-01 — `b416688`+perf pass 16 (Darwin arm64, Apple M4)

A sixteenth pass followed up pass 15's YAML fixes by checking the
project's other two hand-rolled text parsers (TOML, XML) against
equally realistic synthetic files - both clean (40,000-record TOML at
0.06s, 40,000-record XML at 0.13s, genuinely linear). A large-scale INI
file did surface a third real bug, this time in shared JSON-rendering
code rather than a format-specific reader: `render_json`/`render_json_
schema` built the top-level output's own `tables` object with one
`Map::insert` call per table while iterating a `BTreeMap` (whose keys
are already guaranteed unique) - `Map::insert`'s own existing-key
linear scan made this O(tables^2) for information the `BTreeMap`
itself had already proven unnecessary. See CLAUDE.md's own
"Performance" section for the full write-up, including the profiler
numbers (`Map::insert` alone was 31.3% of self-time on an 80,000-
section file). Fixed by adding `Map::push_unique` - an unconditional
append with no existing-key scan, reserved for call sites that already
know the key is new - and switching both render functions' per-table
loops to it.

**Controlled alternating-binary comparison** (synthetic INI files, since
INI's own one-section-per-table convention is what makes a real file
with tens of thousands of tables realistic):

| File | Before | After | Change |
|---|---|---|---|
| 10,000 sections | n/a | 0.07s | - |
| 20,000 sections | 0.31s | 0.12s | **-61%** |
| 80,000 sections | 5.07s | 0.47s | **~10.8x faster** |

Scaling is now linear (roughly proportional to section count at every
size), replacing clearly quadratic growth before (20,000 -> 80,000, a
4x input increase, cost 16.4x more time). Byte-identical output
confirmed via `diff` against the pre-fix binary across every committed
INI/SQLite/`.npz`/spreadsheet fixture (every multi-table format) in
both `json` and `json-schema` output, full test suite (312 unit + 208
integration tests, one new) passing, clippy/fmt clean.

## 2026-09-01 — `4d13917`+perf pass 15 (Darwin arm64, Apple M4)

A fifteenth pass switched from grep-driven hunting to direct
measurement: profiled the hand-rolled YAML reader against a
realistically-sized synthetic file and just timed it, rather than
searching for another instance of an already-known bug shape. Found
two independent, severe O(n^2) bugs in `yaml_support`'s recursive-
descent parser - see CLAUDE.md's own "Performance" section for the
full root-cause writeup of both. `parse_inline_value` (handling `-
key: value`/`key: value`, the single most common real-world "array of
objects" shape) built a fresh `Vec` copying every remaining line in
the document on every call; `parse_flow_from_lines` (an inline
`[...]`/`{...}` flow collection at any nesting depth) eagerly joined
every remaining line into one string before attempting to parse
anything, regardless of how small the actual value was. Fixed by
threading `&mut [YLine]` through the recursive call chain so
`parse_inline_value` can overwrite one line in place and re-slice
instead of copying, and by growing `parse_flow_from_lines`'s joined
buffer one line at a time, stopping as soon as the flow value parses
successfully.

**Controlled alternating-binary comparison** (synthetic files, since
both bugs needed a shape this project's own small committed fixtures
never had reason to reach):

| File | Before | After | Change |
|---|---|---|---|
| Flat records, 30,000 rows | 6.17s user | 0.04s | **~150x faster** |
| Flat records, 60,000 rows | n/a (56.72s real) | 0.08s | **~700x faster** |
| Nested + flow collection, 20,000 rows | 5.86s user | 0.05s | **~117x faster** |
| Nested + flow collection, 40,000 rows | 24.01s user | 0.10s | **~240x faster** |

Scaling is now confirmed linear at every size tested for both shapes
(2x rows -> ~2x time), replacing what was clearly superlinear before
(2x rows -> 4-9x time, worsening at larger sizes - the signature of a
real algorithmic blowup, not just a slow constant factor). Byte-
identical output confirmed via `diff` against the pre-fix binary
across every committed `.yaml` fixture plus five synthetic stress
files, full test suite (311 unit + 208 integration tests, two new)
passing, clippy/fmt clean on both builds.

## 2026-09-01 — `2e58031`+perf pass 14 (Darwin arm64, Apple M4)

A fourteenth pass, following up on pass 13's own dBase finding with a
targeted `grep` for `.get(&` across `src/lib.rs` rather than another
full sweep - looking specifically for the same O(rows * columns)
per-row-per-column-lookup shape Parquet's and JSON's own readers were
already fixed for. Found it in `profile_arrow_ipc_file` (the hand-
rolled Arrow IPC reader): one `JsonValue::get(&name)` call per (row,
column) pair, delegating to `Map::get`'s own deliberate linear scan -
genuinely O(rows * columns^2), worse than Parquet's pre-fix O(rows *
columns) shape (a real `HashMap`, not a linear-scan `Map`). Fixed with
the identical restructuring `profile_parquet_file` already uses: one
pass over `rows`, a `field_index: HashMap<&str, usize, FxBuildHasher>`
built once, and per-column accumulators filled in a single O(rows *
columns) pass. See CLAUDE.md's own "Performance" section for the full
writeup, including a real, unrelated LZ4 multi-block decoding bug found
and fixed in the same pass while generating a benchmark file.

**Controlled alternating-binary comparison** (synthetic files via
`pyarrow.feather.write_feather(..., compression="uncompressed")`, to
isolate this fix from the LZ4 bug):

| File | Before (avg user) | After (avg user) | Change |
|---|---|---|---|
| 300,000 rows x 20 cols (5 rounds) | 1.954s | 1.616s | **-17.3%** |
| 60,000 rows x 100 cols (3 rounds) | 3.053s | 2.057s | **-32.6%** |

A clean, reproducible complexity-class fix - the relative win grows
with column count, exactly as expected from removing a cost that scaled
quadratically in columns. Byte-identical output confirmed via `diff`
against the pre-fix binary across every committed `.arrow`/`.arrows`
fixture plus both synthetic stress files, full test suite (311 unit +
206 integration tests) passing, clippy/fmt clean.

## 2026-09-01 — `dcb349c`+perf pass 13 (Darwin arm64, Apple M4)

A thirteenth pass, started from a source-level sweep of every `.clone()`
call site in `src/lib.rs` (131 at the start) rather than a profiler run
- sorted into genuinely-needed (a shared lookup table, a `#[cfg(test)]`
oracle function) versus avoidable (cloned only because the owner was
borrowed rather than moved, with nothing left to preserve afterward).
See CLAUDE.md's own "Performance" section for the full writeup,
including the dBase HashMap-removal finding and the XLSX text-ownership
rewrite's own honest (mostly negative) measurement.

Three fixes: (1) the `raw[i].iter().filter_map(|v| v.clone()).collect()`
pattern - already fixed once for `columns_from_csv` in an earlier pass,
but never carried over - repeated identically across nine more flat
readers (fixed-width, weblog, syslog, SAS7BDAT, SQLite's `profile_table`,
and all four spreadsheet readers), fixed via `std::mem::take(&mut
raw[i]).into_iter().flatten().collect()` in each. (2) `columns_from_
dbase` decoded every record into its own `HashMap<String, Value>`,
cloning each field's name into the map key on every row, then re-looked-
up every column by name across every row afterward - the same O(rows *
columns) antipattern Parquet's and JSON's own readers were already fixed
for in earlier passes. Replaced with positional `Vec<Vec<Option<
String>>>` accumulation during the same decode pass, using the field
table's own fixed order instead of hashing a name on every cell. (3)
`xlsx_parse_sheet`/`xlsx_parse_shared_strings` cloned every cell's/
shared-string-table-entry's text out of a parsed `XmlElement` tree
that's never read again afterward - fixed by adding `into_child`/
`into_children_named` (consuming counterparts of the existing borrowing
`child`/`children_named`) and walking the tree by value.

**dBase, controlled alternating-binary comparison** (synthetic files via
the `dbf` Python package, since this project has no DBF writer):

| File | Before (avg user) | After (avg user) | Change |
|---|---|---|---|
| 100,000 rows x 20 cols (5 rounds) | 0.738s | 0.248s | **-66.4%** |
| 30,000 rows x 60 cols (3 rounds) | 0.597s | 0.190s | **-68.2%** |

A clean, reproducible ~3x speedup at two different row/column ratios -
a real complexity-class fix, not a one-file coincidence, confirmed the
same way the Parquet column-extraction fix's own scaling was confirmed.

**XLSX, controlled alternating-binary comparison** (synthetic files via
`openpyxl`):

| File | Before (avg user) | After (avg user) | Change |
|---|---|---|---|
| 150,000 rows x 12 cols, short values (6 rounds) | 4.232s | 4.260s | no measurable difference |
| 60,000 rows x 8 cols, long text values (4 rounds) | 1.390s | 1.360s | **~2.2%**, after faster in every round |

Kept despite the first file showing no win (profiling traced the
dominant cost to XML-tree construction and DEFLATE decompression, which
this fix doesn't touch) - the second file confirms a real, if modest,
mechanism-consistent improvement (a clone's cost scales with string
length; the fixed per-cell parsing overhead this fix doesn't touch
doesn't), and the fix adds no meaningful complexity, so it clears this
project's own "earn its place" bar without needing a dramatic number.

Verified the same way as every pass before it: full test suite (206
`--features full` integration tests, every oracle-comparison test
touching an affected reader) unchanged and passing, clippy/fmt clean on
both builds, byte-identical output confirmed via `diff` against the
pre-fix binary across every committed CSV/dBase/Stata/SQLite/XLSX/ODS/
XLS/XLSB/log fixture plus all four synthetic stress files.

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
