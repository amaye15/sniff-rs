# sniff-rs

A Rust CLI that profiles a data file and produces a data dictionary — one row
per column, with what type the data actually is, what type it *should* be,
missing %, sample values, and why. It reads CSV, TSV, JSON, JSON Lines,
Parquet, Arrow IPC/Feather, Avro, Excel, and SQLite, and writes Markdown or
JSON.

The point of the tool is schema extraction that doesn't trust anyone's
claims about the data — not the file extension, not the declared column
type, not the format's own type inference. It re-derives what's actually
there from the values themselves. See "Design philosophy" below; it's the
reason most of the code is shaped the way it is.

## Quick start

```bash
cargo build --release                      # CSV/TSV/JSON/JSONL only, ~5-35s
cargo build --release --features full      # every format, ~7-9 min clean cache

./target/release/sniff-rs data.csv
./target/release/sniff-rs events.jsonl out.md --samples 5
./target/release/sniff-rs warehouse.db - --output-format json | jq .
```

`cargo test` covers the default build; `cargo test --features full` covers
every format. See "Testing" below.

## Supported formats

| Format | Extensions | Needs | Notes |
|---|---|---|---|
| CSV / TSV | `.csv`, `.tsv` | *(default)* | `--delimiter` overrides the separator |
| JSON | `.json` | *(default)* | array-of-objects or JSON Lines, auto-detected by content |
| JSON Lines / NDJSON | `.jsonl`, `.ndjson` | *(default)* | same reader as JSON |
| Parquet | `.parquet`, `.pqt` | `--features parquet` | full schema, recurses into Struct/List |
| Arrow IPC / Feather | `.arrow`, `.feather` | `--features parquet` | shares Parquet's Arrow infrastructure |
| Avro | `.avro` | `--features avro` | recurses into records/arrays/unions |
| Excel | `.xlsx`, `.xls`, `.xlsb`, `.ods` | `--features xlsx` | first sheet only, treated like CSV once read |
| SQLite | `.db`, `.sqlite`, `.sqlite3` | `--features sqlite` | one section per table (see below) |

`--features full` enables all of the above. `--format <name>` overrides
extension-based detection when a file is misnamed or ambiguous.

Every optional format has a matching Cargo feature that gates both the
dependency (`dep:x` in `[features]`) and the reader function itself
(`#[cfg(feature = "x")]` with a `#[cfg(not(...))]` stub that gives a clear
"rebuild with --features x" error rather than "unrecognized format"). Adding
a format should follow the same shape — see the cookbook below.

## Output formats

`--output-format md` (default) or `--output-format json`. Pass `-` as the
output path to write to stdout instead of a file — the status line
(`N tables, M columns -> ...`) always goes to stderr, so stdout stays pure
data for piping (`... | jq .`, `... > out.json`).

JSON shape — every format renders through the same structure, so a consumer
never needs to special-case SQLite's multiple tables vs. everything else's
implicit single one:

```json
{
  "file": "data.csv",
  "format": "csv",
  "tables": {
    "data": [
      {
        "name": "zip_code",
        "current_type": "String",
        "ideal_type": "String",
        "description": "",
        "missing_pct": 0.0,
        "sample_values": ["02134", "90210"],
        "notes": "leading zeros in raw values (likely an ID/code)"
      }
    ]
  }
}
```

`description` is always empty — intentionally left for a human (or an agent
downstream of this one) to fill in; no heuristic should be guessing what a
column *means*.

## Architecture

Two shared building blocks carry almost the entire tool:

**`ColumnInput` → `profile_column`** — for formats that are naturally flat
(CSV, TSV, Excel) or already fully typed with no nesting concept (Parquet's
scalar columns). A reader collects each column's non-null raw string values
plus a `current_type` label, and `profile_column` runs the shared heuristic
engine (`suggest_ideal_type`) over the raw strings to produce a
`ColumnProfile`.

**`profile_json_path` / `profile_json_records`** — for anything that can
nest (JSON, Avro, Parquet's Struct/List columns). This recurses: objects
flatten into dot-notation sub-columns (`metadata.risk_score`), arrays get
unwrapped and pooled (nested arrays flatten transparently), and the result
is *this path's own row followed by every descendant row*, so nesting is
never force-fit into one opaque cell.

The load-bearing design decision is that **non-native nested formats are
bridged into `serde_json::Value` and handed to the exact same recursive
flattener**, rather than reimplementing recursion per format:

- Avro decodes each record to `serde_json::Value` (`avro_value_to_json`)
  and calls `profile_json_records` — identical code path to a `.json` file.
- Parquet/Arrow IPC's Struct/List columns get bridged through Arrow's own
  JSON writer (`arrow::json::writer::ArrayWriter`) into the same
  `serde_json::Map` shape, then call `profile_json_path` directly. Scalar
  columns skip this entirely and take the fast direct-stringify path
  (`array_value_to_string`) — the bridge only runs for files that actually
  have nested columns.

Parquet and Arrow IPC additionally share one function,
`profile_arrow_batches`, since they both decode to the same `RecordBatch`
type — adding Arrow IPC support was a new file-opening call wired into
existing logic, not new logic.

SQLite is architecturally different (one file, many tables), so `main()`
normalizes *everything* — single-table formats and SQLite alike — into
`BTreeMap<String, Vec<ColumnProfile>>` before rendering, so the
Markdown/JSON renderers never know or care how many tables a source had.

## Design philosophy: trust observed data, not declared types

Every format has some way of telling you what type a column is — a CSV
library's inference, a JSON value's own type, Parquet's schema, SQLite's
declared column affinity. This tool treats all of those as *hints*, and
verifies them against the actual values wherever verification is possible.
That's the whole reason for `Current Type` vs `Ideal Type` as separate
columns, and it has caught real, format-specific data-loss bugs during
development that a naive "just show me the schema" tool would have missed
entirely:

- **CSV**: `pd.read_csv()`-style naive parsing silently turns `"02134"`
  into `2134` before you ever see it — `has_leading_zero` catches this by
  checking the raw string, and the note distinguishes "already lost" (numeric
  current type) from "just a heads-up" (already a string, nothing lost).
- **Excel**: writing `"02134"` through openpyxl/Excel converts it to the
  number `2134` *when the file is written* — same symptom, different root
  cause, correctly flagged as already-lost since there's nothing to recover
  at read time.
- **Parquet/Avro string columns**: the same value stored as a proper string
  type has genuinely lost nothing, and the notes say so.
- **SQLite**: type affinity is a suggestion, not a constraint — a column
  declared `REAL` can legally hold `TEXT`. `describe_sql_kinds` tracks the
  *actual* storage class per value, so this shows up as
  `mixed(String: 1, f64: 2)` instead of silently trusting the schema.
- **JSON/Avro schema drift**: the same field can be a string in one record
  and an integer in another. `describe_kinds` reports every observed type
  *with counts* (`mixed(String: 2, bool: 1)`), not just a flag that
  something's inconsistent — the counts tell you whether it's one bad
  record or a real 50/50 split.
- **Missing values never fake a type change.** Pandas upcasts a
  `int64` column with one `NaN` to `float64`; this tool never does that
  fakery anywhere, because nothing here uses NumPy-style arrays — a column
  with missing values just gets a `has missing values -> wrap in Option<T>`
  note alongside its real type.

If you're adding a heuristic, ask "does this catch a real, reproducible
loss-of-information event, or am I guessing at intent?" The leading-zero and
type-affinity checks are the former. There's deliberately no heuristic that
tries to guess column *meaning* from its name — that's what `description` is
for, and it's left to a human.

## Adding a new format: a cookbook

Three questions determine the shape of a new reader:

1. **Is it naturally row-oriented and possibly nested** (a record format
   like Avro, TOML with tables, MessagePack)? Decode each record to
   `serde_json::Value` (or bridge to one) and call `profile_json_records`.
   This is almost always the least code for a new format — see
   `columns_from_avro` for the ~20-line pattern.
2. **Is it flat/columnar with no nesting** (fixed-width text, a new
   spreadsheet-like format)? Build `Vec<ColumnInput>` (name, current_type,
   raw string values, total count) and map through `profile_column`. See
   `columns_from_xlsx`.
3. **Can one file hold multiple tables** (another embedded-database
   format)? Return `Vec<(String, Vec<ColumnProfile>)>` like
   `columns_from_sqlite` and let `main()`'s `BTreeMap` unification handle
   the rest — don't special-case rendering.

Then, regardless of which shape:

- Add the dependency with `cargo add <crate> --optional`, and a named
  feature in `[features]` (`Cargo.toml`) using `dep:` syntax — never make a
  new format's dependency non-optional. Add it to `full = [...]` too.
- Add the reader function behind `#[cfg(feature = "...")]`, with a
  `#[cfg(not(feature = "..."))]` stub returning
  `bail!("... isn't compiled in - rebuild with --features ...")`.
- Add a variant to `InputFormat`, wire it into `detect_format` (both the
  `--format` override arm and the extension-inference arm), give it a label
  in `InputFormat::as_str`, and add a dispatch arm in `main()`.
- Build a small fixture, run it, and eyeball the output before trusting it —
  this project's whole history is heuristics that looked right and weren't
  (see "Design philosophy"). Then add a `tests/fixtures/` file and a
  `#[cfg(feature = "...")]`-gated test in `tests/integration.rs`.
- If the new dependency is heavy (anything pulling in more than a handful
  of transitive crates), budget real time for the first clean build —
  `arrow`+`parquet` alone take ~7-9 minutes from a cold cache in a
  constrained environment. Build in the background and poll rather than
  blocking; a first `cargo build` for a new heavy dependency can exceed a
  single command's time limit in some sandboxes.

## Testing

```bash
cargo test                    # default build: csv/tsv/json/jsonl only
cargo test --features full    # everything, including format-gated tests
```

`tests/integration.rs` runs the *compiled binary* against fixtures in
`tests/fixtures/` (via `std::process::Command`, deliberately no
`assert_cmd`/`predicates` dependency — keeping test deps as lean as the tool
itself) and asserts on parsed JSON output. Coverage includes: the
leading-zero and date-format heuristics on CSV, nested object + array-of-
objects flattening with the local missing-% calculation, mixed-type count
reporting, Parquet's no-data-loss-on-strings case, Excel's does-lose-data
case (the same value, opposite outcome, by design), SQLite's multi-table
output and type-affinity detection, and that Markdown output never has a
trailing blank line.

The crate is a lib (`src/lib.rs`, exposing `pub fn run()`) plus a thin
binary (`src/main.rs` that just calls `sniff_rs::run()`), so besides the
black-box integration tests there's also a `#[cfg(test)] mod tests` at the
bottom of `lib.rs` unit-testing the heuristic functions directly
(`suggest_ideal_type`, `has_leading_zero`, `matching_date_format`,
`describe_kinds`) — they're the part most likely to grow subtle bugs under
a small direct test, and being private functions in the same file, they
don't need any `pub` just to be reachable from `#[cfg(test)]`.

## Known limitations / roadmap

- **No JSON-Schema-standard output.** The JSON mode is this tool's own
  shape (rich: current/ideal type, notes, samples), not
  `json-schema.org`'s `{"type": "object", "properties": {...}}` vocabulary.
  Could be added as a third `--output-format` if a consumer specifically
  needs the standard.
- **Excel: first sheet only.** No multi-sheet support yet; would follow the
  same `BTreeMap<String, Vec<ColumnProfile>>` pattern SQLite already
  established if added.
- **Parquet nested types stop at Struct/List.** Map and dictionary-encoded
  types aren't specifically handled (they'd likely fall through the
  `is_nested_arrow_type` check as unrecognized and get stringified via the
  scalar path, which is untested).
- **No ORC.** Deliberately skipped — Rust ecosystem support is weak enough
  that it wasn't worth the dependency risk.
- **Category-detection threshold is fixed**, not configurable: ≤50 unique
  values *and* a uniqueness ratio under 5% of total rows. Works fine at
  hundreds-plus rows; on very small files it under-triggers (nothing to do
  about that mathematically — 3 unique values in a 5-row file is 60%
  cardinality no matter how you slice it).
- **`missing_pct` is rounded to 1 decimal place** at construction time
  (`round1`), in both Markdown and JSON. This is a display choice, not a
  precision bug — full float precision was never meaningful here.
- **Date-format detection is a fixed candidate list** (`DATE_FORMATS`), not
  a fuzzy parser. Deliberate: a fuzzy parser can silently misparse a
  numeric ID as a date; a fixed list either matches every value in a column
  or reports nothing, which is a safer failure mode. Extend the list if a
  reasonable format is missing one.
