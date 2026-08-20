# sniff-rs

A Rust CLI that profiles a data file and produces a data dictionary — one row
per column, with what type the data actually is, what type it *should* be,
missing %, sample values, and why. It reads CSV, TSV, JSON, JSON Lines,
Parquet, Arrow IPC/Feather, Avro, Excel, SQLite, MessagePack, TOML, YAML,
CBOR, and INI, and writes Markdown, this tool's own rich JSON, or
json-schema.org-standard JSON.

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
| Parquet | `.parquet`, `.pqt` | `--features parquet` | full schema, recurses into Struct/List/Map |
| Arrow IPC / Feather | `.arrow`, `.feather` | `--features parquet` | shares Parquet's Arrow infrastructure |
| Avro | `.avro` | `--features avro` | recurses into records/arrays/unions |
| Excel | `.xlsx`, `.xls`, `.xlsb`, `.ods` | `--features xlsx` | one section per sheet, like SQLite (see below) |
| SQLite | `.db`, `.sqlite`, `.sqlite3` | `--features sqlite` | one section per table (see below) |
| MessagePack | `.msgpack`, `.mp` | `--features msgpack` | stream of concatenated records, or a single top-level array |
| TOML | `.toml` | `--features toml` | whole document = one row; array-of-tables flattens like a nested JSON array |
| YAML | `.yaml`, `.yml` | `--features yaml` | single mapping = one row, single sequence = array-of-records, `---`-multi-doc = one row per document |
| CBOR | `.cbor` | `--features cbor` | same convention as MessagePack: concatenated records, or a single top-level array |
| INI | `.ini` | `--features ini` | one section per table, like SQLite (see below); a repeated key pools into an array |

`--features full` enables all of the above. `--format <name>` overrides
extension-based detection when a file is misnamed or ambiguous.

Every optional format has a matching Cargo feature that gates both the
dependency (`dep:x` in `[features]`) and the reader function itself
(`#[cfg(feature = "x")]` with a `#[cfg(not(...))]` stub that gives a clear
"rebuild with --features x" error rather than "unrecognized format"). Adding
a format should follow the same shape — see the cookbook below.

## Output formats

`--output-format md` (default), `--output-format json`, or
`--output-format json-schema`. Pass `-` as the output path to write to
stdout instead of a file — the status line (`N tables, M columns -> ...`)
always goes to stderr, so stdout stays pure data for piping
(`... | jq .`, `... > out.json`).

JSON shape (`json`) — every format renders through the same structure, so a
consumer never needs to special-case SQLite's/Excel's multiple tables vs.
everything else's implicit single one:

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

JSON-Schema shape (`json-schema`) — a second, more interoperable JSON
rendering for consumers that want `json-schema.org` vocabulary instead of
this tool's own rich shape above. Each table becomes its own
`{"type": "object", "properties": {...}}` schema, keyed by table name under
`tables` the same way; `ideal_type` drives each property's `type`
(`i64`→`integer`, `f64`→`number`, `bool`→`boolean`,
`NaiveDate / DateTime`→`{"type": "string", "format": "date-time"}`,
`Vec<T>`→`{"type": "array", "items": ...}`); a column with any missing
values gets a `["type", "null"]` union instead of a bare type, and is left
out of the table's `"required"` list. Deliberately lossy wherever
`ideal_type` itself is lossy or ambiguous — `mixed(...)` current types,
flattened structs, and `enum / category` (only sample values are kept, not
the full domain) all fall back to an unconstrained `{}` schema rather than
guessing:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "file": "data.csv",
  "tables": {
    "data": {
      "type": "object",
      "properties": {
        "zip_code": { "type": "string" },
        "age": { "type": ["integer", "null"] }
      },
      "required": ["zip_code"]
    }
  }
}
```

## Architecture

Two shared building blocks carry almost the entire tool:

**`ColumnInput` → `profile_column`** — for formats that are naturally flat
(CSV, TSV, Excel) or already fully typed with no nesting concept (Parquet's
scalar columns). A reader collects each column's non-null raw string values
plus a `current_type` label, and `profile_column` runs the shared heuristic
engine (`suggest_ideal_type`) over the raw strings to produce a
`ColumnProfile`.

**`profile_json_path` / `profile_json_records`** — for anything that can
nest (JSON, Avro, MessagePack, TOML, YAML, CBOR, Parquet's Struct/List/Map columns). This recurses: objects
flatten into dot-notation sub-columns (`metadata.risk_score`), arrays get
unwrapped and pooled (nested arrays flatten transparently), and the result
is *this path's own row followed by every descendant row*, so nesting is
never force-fit into one opaque cell.

The load-bearing design decision is that **non-native nested formats are
bridged into `serde_json::Value` and handed to the exact same recursive
flattener**, rather than reimplementing recursion per format:

- Avro decodes each record to `serde_json::Value` (`avro_value_to_json`)
  and calls `profile_json_records` — identical code path to a `.json` file.
- MessagePack does the same (`msgpack_value_to_json`), reading a stream of
  concatenated top-level values (records are self-delimiting, so they don't
  need a separator the way JSON Lines needs newlines) - or, if the file
  holds exactly one top-level value and it's an array, that array's
  elements instead, mirroring how the JSON reader treats a single top-level
  `[...]`.
- TOML also bridges to `serde_json::Value` (`toml_value_to_json`), but with
  a twist: a TOML file is one document, not a table of many rows, so
  there's no natural row count to iterate. Rather than invent one, the
  whole document is profiled as a single record
  (`profile_json_records(&[record], ...)`, `total = 1`) - an array-of-tables
  section (`[[servers]]`) still becomes a `Vec<object>` column and flattens
  exactly like any other JSON array of objects would, since that part of
  the pipeline doesn't care how many top-level records it started with.
- YAML (via `serde_norway`, a maintained fork of the archived `serde_yaml`
  with the same API shape) bridges the same way (`yaml_value_to_json`), but
  `columns_from_yaml` picks its record list based on what's actually in the
  file rather than assuming one shape: a single top-level sequence is an
  array of records (JSON's `[...]` mode); a single top-level mapping is one
  record (TOML's whole-document-as-one-row choice); a `---`-separated
  multi-document stream is one record per document (YAML's own equivalent
  of JSON Lines, but self-delimiting rather than newline-delimited).
- CBOR does exactly what MessagePack does (`cbor_value_to_json`, via the
  `ciborium` crate): a stream of concatenated self-delimiting top-level
  values, or a single top-level array's elements. Same convention, same
  ~15 lines beyond the value-conversion helper.
- Parquet/Arrow IPC's Struct/List/Map columns get bridged through Arrow's
  own JSON writer (`arrow::json::writer::ArrayWriter`) into the same
  `serde_json::Map` shape, then call `profile_json_path` directly (a Map
  column becomes a JSON object per row and flattens by key exactly like a
  Struct does). Scalar columns skip this entirely and take the fast
  direct-stringify path (`array_value_to_string`) — the bridge only runs
  for files that actually have nested columns. Dictionary-encoded columns
  (Parquet's compact encoding for low-cardinality values, e.g. strings)
  aren't nested at all — `arrow_type_label` resolves them to their
  underlying value type recursively, so a dictionary-encoded string column
  reports as `String`, not as the encoding wrapping it.

Parquet and Arrow IPC additionally share one function,
`profile_arrow_batches`, since they both decode to the same `RecordBatch`
type — adding Arrow IPC support was a new file-opening call wired into
existing logic, not new logic.

SQLite, Excel, and INI are architecturally different from the rest (one
file, many tables — SQLite's tables, Excel's sheets, INI's sections), so
`run()` normalizes *everything* — single-table formats and these three
alike — into `BTreeMap<String, Vec<ColumnProfile>>` before rendering, so
the Markdown/JSON renderers never know or care how many tables a source
had. `columns_from_xlsx` and `columns_from_ini` both follow the exact shape
`columns_from_sqlite` already established
(`Vec<(String, Vec<ColumnProfile>)>`, one entry per table) — Excel skips
empty sheets and INI skips an absent default section the same way SQLite
skips its own internal `sqlite_%` tables, and none of the three needed any
new rendering logic. INI additionally has no repeating-row concept within
a section (it's a flat set of `key=value` pairs), so each section is
profiled as a single record via `profile_json_records`, the same choice
TOML/YAML make for their own single-document shapes — and a key repeated
within one section (which INI permits) pools into an array value rather
than the second occurrence silently overwriting the first.

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
   like Avro or MessagePack)? Decode each record to `serde_json::Value` (or
   bridge to one) and call `profile_json_records`. This is almost always
   the least code for a new format — see `columns_from_avro` or
   `columns_from_msgpack` for the pattern (both under ~40 lines including
   the value-conversion helper). If the format is naturally a single
   document rather than many records (TOML), or can be either depending on
   the file (YAML), see `columns_from_toml` / `columns_from_yaml` — profile
   a lone document as one record rather than inventing a fake row count,
   and pick the record list based on what's actually in the file (a
   top-level sequence vs. a top-level mapping vs., for formats that support
   it, a multi-document stream) rather than assuming one shape.
2. **Is it flat/columnar with no nesting** (fixed-width text, a new
   spreadsheet-like format)? Build `Vec<ColumnInput>` (name, current_type,
   raw string values, total count) and map through `profile_column`. See
   `columns_from_xlsx`.
3. **Can one file hold multiple tables** (another embedded-database
   format, or anything with named sections/sheets)? Return
   `Vec<(String, Vec<ColumnProfile>)>` like `columns_from_sqlite`,
   `columns_from_xlsx`, or `columns_from_ini` and let `run()`'s `BTreeMap`
   unification handle the rest — don't special-case rendering.

Then, regardless of which shape:

- Add the dependency with `cargo add <crate> --optional`, and a named
  feature in `[features]` (`Cargo.toml`) using `dep:` syntax — never make a
  new format's dependency non-optional. Add it to `full = [...]` too.
- Add the reader function behind `#[cfg(feature = "...")]`, with a
  `#[cfg(not(feature = "..."))]` stub returning
  `bail!("... isn't compiled in - rebuild with --features ...")`.
- Add a variant to `InputFormat`, wire it into `detect_format` (both the
  `--format` override arm and the extension-inference arm), give it a label
  in `InputFormat::as_str`, and add a dispatch arm in `run()`.
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
reporting, Parquet's no-data-loss-on-strings case, Parquet's Map-column
flattening and dictionary-encoding resolution, Excel's does-lose-data case
(the same value, opposite outcome, by design), Excel's one-table-per-sheet
output, SQLite's multi-table output and type-affinity detection, the
`json-schema` output's type mapping and nullability handling, MessagePack's
and CBOR's concatenated-records reading and their no-data-loss-on-strings
case, TOML's whole-document-as-one-row profiling and array-of-tables
flattening, YAML's multi-document-stream reading, INI's one-table-per-
section output and duplicate-key pooling, and that Markdown output never
has a trailing blank line.

The crate is a lib (`src/lib.rs`, exposing `pub fn run()`) plus a thin
binary (`src/main.rs` that just calls `sniff_rs::run()`), so besides the
black-box integration tests there's also a `#[cfg(test)] mod tests` at the
bottom of `lib.rs` unit-testing the heuristic functions directly
(`suggest_ideal_type`, `has_leading_zero`, `matching_date_format`,
`describe_kinds`) — they're the part most likely to grow subtle bugs under
a small direct test, and being private functions in the same file, they
don't need any `pub` just to be reachable from `#[cfg(test)]`.

## Known limitations / roadmap

- **No ORC.** Deliberately skipped — Rust ecosystem support is weak enough
  that it wasn't worth the dependency risk.
- **No DuckDB.** Considered and deliberately skipped for the same reason as
  ORC: dependency weight. `duckdb`'s `bundled` feature compiles its C++
  engine from a tarball shipped in the crate (no network fetch, same trust
  model as `rusqlite`'s own `bundled` feature) - that part is fine. But
  `libduckdb-sys` also carries an HTTP+TLS client (`ureq`, `rustls`, ...)
  plus `tar`/`zip`/`xattr` as *unconditional* build-dependencies purely to
  support a download fallback the bundled path never uses, and `duckdb`
  itself pulls in a second, different version of `arrow` as a plain runtime
  dependency alongside the one this project already depends on for
  Parquet/Arrow IPC - not deduped, just duplicated. ~40 extra crates and a
  duplicate Arrow stack for one format was judged not worth it here; would
  reconsider if the crate trims that footprint, or if there's a concrete
  need for `.duckdb` files.
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
