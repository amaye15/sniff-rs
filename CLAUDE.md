# sniff-rs

A Rust CLI that profiles a data file and produces a data dictionary — one row
per column, with what type the data actually is, what type it *should* be,
missing %, sample values, and why. It reads CSV, TSV, JSON, JSON Lines,
Parquet, Arrow IPC/Feather, Avro, Excel, SQLite, MessagePack, TOML, YAML,
CBOR, INI, XML, fixed-width text, NumPy, Common/Combined Log Format access
logs, RFC 3164/5424 syslog, dBase, Stata, and SAS7BDAT — any of them gzip-
or zstd-compressed too — and writes Markdown, this tool's own rich JSON,
or json-schema.org-standard JSON.

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
./target/release/sniff-rs data.csv.gz                       # gzip decompressed transparently
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
| XML | `.xml` | `--features xml` | homogeneous same-tag children of the root = records; otherwise the root = one row; attributes become `@name` columns |
| Fixed-width text | *(none — `--format fixed-width` only)* | *(default)* | needs `--widths 10,5,20`; no delimiter, so boundaries are never guessed |
| gzip | any of the above + `.gz`/`.gzip` | *(default)* | transparently decompressed before the inner format's own reader runs |
| zstd | any of the above + `.zst`/`.zstd` | `--features zstd` | same as gzip |
| NumPy | `.npy` | `--features npy` | structured (record) dtype = one column per field; plain dtype = positional `col_0..col_N` (2D) or one `value` column (1D) |
| NumPy archive | `.npz` | `--features npy` | zip of named `.npy` arrays; one table per array, like SQLite (see below) |
| Common Log Format | *(none — `--format common-log` only)* | `--features weblog` | `host ident authuser [ts] "req" status bytes`; request splits into method/path/protocol |
| Combined Log Format | *(none — `--format combined-log` only)* | `--features weblog` | Common Log plus `"referer" "user-agent"` |
| Syslog (RFC 3164) | *(none — `--format syslog` only)* | `--features syslog` | `<PRI>Mmm dd hh:mm:ss host tag[pid]: msg`; PRI decodes to facility/severity names |
| Syslog (RFC 5424) | *(none — `--format syslog5424` only)* | `--features syslog` | structured variant: adds version/app-name/procid/msgid/structured-data |
| dBase | `.dbf` | `--features dbase` | soft-deleted records skipped (dBase's own convention); `current_type` can reveal a Numeric field that's really an integer |
| Stata | `.dta` | `--features stata` | every DTA release (102-119); Stata's own missing markers become missing values, not literal strings |
| SAS7BDAT | `.sas7bdat` | `--features sas7bdat` | `current_type` from the file's own declared type; SAS stores nearly all numerics as doubles, so `ideal_type` often narrows further |

`--features full` enables all of the above. `--format <name>` overrides
extension-based detection when a file is misnamed or ambiguous — fixed-width
text and all four log formats (web access + syslog) have no extension
convention reliable enough to infer from at all (logs are commonly
`.log`/`.txt`/no extension), so all five are reachable only via `--format`,
never auto-detected.

Every optional format has a matching Cargo feature that gates both the
dependency (`dep:x` in `[features]`) and the reader function itself
(`#[cfg(feature = "x")]` with a `#[cfg(not(...))]` stub that gives a clear
"rebuild with --features x" error rather than "unrecognized format"). Adding
a format should follow the same shape — see the cookbook below. Fixed-width
text is the one exception: it needs no new dependency (pure `std` string
slicing), so it's always compiled in, gated by nothing but the required
`--widths` flag.

gzip/zstd decompression (`decompress_if_needed`) isn't a format of its own —
it's a preprocessing step in front of every reader above, not a new
`InputFormat` variant. A `.gz`/`.zst` input gets decompressed into a real
temporary file (via `tempfile::NamedTempFile`, cleaned up on drop) *before*
format detection ever runs, so every reader keeps opening a plain file path
exactly as before, with zero per-format changes — including formats that
need actual random file access rather than a stream (Parquet, SQLite,
Excel). Detection and default output naming use the compression-stripped
logical name (`data.csv.gz` behaves like `data.csv`); the JSON/Markdown
`file` field still reports the real, original filename for traceability.
gzip is via `flate2` (pure Rust, no C toolchain, so always available); zstd
needs `--features zstd` since the `zstd` crate compiles a small vendored C
library.

NumPy's `npyz` crate is worth a dependency note: its `npz` feature (for
`.npz` archives) depends on the `zip` crate without trimming its default
features, which pulls in `bzip2` and a second, older copy of `zstd`
alongside this project's own. That's a handful of small, fast-compiling
extra crates — nothing like the DuckDB situation below — so it was judged
worth it for `.npz` support rather than treated as a reason to skip it.

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
`UUID`/`Email`/`IPv4`/`IPv6`/`URL`→`{"type": "string", "format": "uuid" /
"email" / "ipv4" / "ipv6" / "uri"}`,
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
(CSV, TSV, Excel, fixed-width text) or already fully typed with no nesting
concept (Parquet's scalar columns). A reader collects each column's
non-null raw string values plus a `current_type` label, and `profile_column`
runs the shared heuristic engine (`suggest_ideal_type`) over the raw
strings to produce a `ColumnProfile`. NumPy also lands here, but is the one
reader that decodes a binary layout by hand instead of leaning on a crate's
own typed API: `npyz::Deserialize` requires a Rust type known at compile
time, which doesn't work for an arbitrary user's `.npy` file whose dtype is
only known at runtime, so `npy_scalar_to_string`/`npy_value_to_string`
interpret each field's raw bytes directly from its `TypeStr`
(`TypeChar` + byte width + endianness) - int/uint/float by width, fixed-
width byte/unicode strings trimmed of right-zero-padding, and anything not
representable as a simple value (a sub-array field, `f16`/`f128`, the
pickled `object` dtype) falling back to a hex dump rather than fabricating
a value or failing the whole file. A structured (record) dtype gives one
`ColumnInput` per named field - the genuinely tabular case; a plain dtype
has no field names at all (numpy doesn't carry them), so it's treated like
a headerless CSV: 1D is a single `value` column, 2D gets positional
`col_0..col_N` columns (row-major/`C` or column-major/`Fortran` order both
handled), and anything higher-dimensional is a clear error rather than a
guessed flattening.

dBase is a more conventional flat reader (`columns_from_dbase`), but with
one thing worth calling out: column order comes from `Reader::fields()`
(the file's own field table) rather than iterating `Record`'s internal
`HashMap`, whose order isn't guaranteed stable. Soft-deleted records
(dBase's own "marked for deletion" flag) are skipped by the `dbase` crate
itself before this code ever sees them - that's the format's own
convention, not something this tool is choosing to hide. Its `Numeric`
field type doesn't distinguish int from float at the storage level, so
`current_type` reports the same `f64` for every numeric field regardless -
exactly the kind of gap `ideal_type`'s independent re-derivation from the
actual values exists to surface, the same way CSV's leading-zero check
does for a different reason.

Stata (`columns_from_stata`, via the `dta` crate) is architecturally the
same shape again, with its own version of the same lesson: a DTA file
marks each individual value present-or-missing explicitly (`.` through
`.z`, decoded by the crate itself), so a `Missing` value is simply omitted
from `raw_values` - the same treatment every other reader here already
gives an absent value - while a genuinely present value keeps going
through the normal `current_type`/`ideal_type` split (a Stata `double`
column pandas wrote to hold one `NaN` alongside otherwise-integer data is
exactly this tool's "missing values never fake a type change" principle
in someone else's file format). A `strL` long-string reference needs a
second read pass over a different file section to resolve, which this
tool doesn't do, so it's a visible placeholder rather than a silent drop.
Variable and value labels - Stata's own human-authored variable
descriptions and coded-value names (`1`/`2`/`3` meaning
`"male"`/`"female"`/`"other"`) - aren't surfaced; see Known limitations.

SAS7BDAT (`columns_from_sas7bdat`, via the `sas7bdat` crate) follows the
same shape, but its `current_type` comes straight from the file's own
`Dataset::columns()` metadata (a `LogicalType` per column) rather than
being inferred from row values, the same as `arrow_type_label` does for
Parquet/Arrow. That declared type is genuinely worth cross-checking: SAS
stores nearly all numeric data as 8-byte doubles internally regardless of
the value's real precision, so `current_type: "f64"` with `ideal_type`
correctly narrowing to `"i64"` for a whole-number column isn't a bug in
either the crate or this tool - it's the same "declared type is a hint,
not the truth" lesson Parquet/Avro/dBase/Stata all already demonstrate,
in one more format's own way of losing that distinction. SAS also has
per-column labels (same considered non-surfacing decision as Stata's).

Common/Combined Log Format also land here (`columns_from_weblog`), via a
fixed `regex` per format matching each grammar's exact field layout. The
quoted `"METHOD path PROTOCOL"` request is split into its own
`method`/`path`/`protocol` columns rather than kept as one opaque field -
a line whose request doesn't cleanly split into three tokens just gets
missing values there for that row, not a guessed split. `-` is each
format's own documented placeholder for "field not present", so it's
converted to a missing value rather than kept as a literal string. A line
that doesn't match the chosen format's grammar at all (e.g. a Combined
line read as `--format common-log`, which has two extra trailing quoted
fields the Common grammar doesn't expect) is a hard error naming the line
number, not a silent skip or a truncated parse.

Syslog (RFC 3164 and RFC 5424, `columns_from_syslog`) follows the exact
same shape as the web access logs - same crate, same "hard error naming
the line" behavior for a mismatched line, same `-`-as-nilvalue-so-missing
convention for RFC 5424's optional fields. Its one extra step is decoding
the leading `<PRI>`: `facility = PRI / 8`, `severity = PRI % 8` are the
RFC's own fixed numeric-to-name tables (`SYSLOG_FACILITIES`,
`SYSLOG_SEVERITIES`), not a guess, so both get mapped to their standard
names as separate columns rather than left as one opaque number. RFC
3164's timestamp has no year field at all - a real, well-known limitation
of that specific RFC, not something worth working around - so it's left
as a plain string instead of being forced through `matching_date_format`
(which needs a full date and would only ever fail on it).

**`profile_json_path` / `profile_json_records`** — for anything that can
nest (JSON, Avro, MessagePack, TOML, YAML, CBOR, XML, Parquet's Struct/List/Map columns). This recurses: objects
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
- XML is the one exception to "bridge via a ready-made dynamic Value type" -
  an XML element can carry attributes, text, and child elements all at
  once, which doesn't map onto a single generic enum the way
  toml::Value/serde_norway::Value/rmpv::Value/ciborium::Value do, so
  `xml_element_to_json` builds the bridge by hand from `xmltree`'s DOM tree:
  attributes become `@name` keys, text becomes a `#text` key (or, for a
  leaf element with only text and no attributes, the bare string, so
  `<name>Alice</name>` becomes `"Alice"` rather than `{"#text": "Alice"}`),
  and repeated same-name child elements pool into an array. Record
  detection then mirrors the other formats' own dual-mode choices: if the
  root's children are all the same tag (`<root><item/><item/></root>`),
  each is a record; otherwise the whole document is one record, TOML's and
  an INI section's choice for their own single-document shapes.
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

SQLite, Excel, INI, and `.npz` are architecturally different from the rest
(one file, many tables — SQLite's tables, Excel's sheets, INI's sections,
`.npz`'s named arrays), so `run()` normalizes *everything* — single-table
formats and these four alike — into `BTreeMap<String, Vec<ColumnProfile>>`
before rendering, so the Markdown/JSON renderers never know or care how
many tables a source had. `columns_from_xlsx`, `columns_from_ini`, and
`columns_from_npz` all follow the exact shape `columns_from_sqlite` already
established (`Vec<(String, Vec<ColumnProfile>)>`, one entry per table) —
Excel skips empty sheets and INI skips an absent default section the same
way SQLite skips its own internal `sqlite_%` tables, and none of the four
needed any new rendering logic (`.npz` and plain `.npy` share the exact
same per-array reading core, `columns_from_npy_reader` — a `.npz` is just a
zip of named `.npy` streams). INI additionally has no repeating-row concept
within a section (it's a flat set of `key=value` pairs), so each section is
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
- **CSV/TSV/fixed-width missing-value sentinels**: these three formats have
  no native null - every field is plain text, so a genuinely-missing value
  can only be an empty field or one of a handful of well-established
  placeholder conventions (`NA`, `N/A`, `NULL`, `None`, `NaN`, `-`, `?`,
  `unknown`, ...) - the exact same tokens pandas' `read_csv` treats as
  missing by default. Left unrecognized, a single stray `"NA"` in an
  otherwise-clean integer column used to derail `i64` detection for the
  *whole column* and undercount `missing_pct` at the same time - a real,
  reproducible loss, not a guess. `is_missing_sentinel` (matched
  case-insensitively against the trimmed field) catches this at read time
  in `columns_from_csv`/`columns_from_fixed_width`, the same place an empty
  field already becomes `None`. Deliberately *not* applied to JSON or any
  other format with a real native null - a JSON string field that literally
  contains `"NA"` (Namibia's ISO country code, notably) is a value someone
  chose to write, not a stand-in for absence.
- **Semantic type detection (UUID/Email/IPv4/IPv6/URL)**: five more
  `ideal_type` values beyond the storage-level ones, each backed by a fixed,
  unambiguous grammar rather than a fuzzy pattern - `is_uuid` checks exact
  length and dash positions, `is_ipv4`/`is_ipv6` lean on
  `std::net::Ipv4Addr`/`Ipv6Addr`'s own strict parsers, `is_email`/`is_url`
  are deliberately conservative (not RFC 5322/full-URL-grammar complete) so
  they only fire when there's no real ambiguity. Checked *before* the
  leading-zero heuristic in `suggest_ideal_type`, since leading-zero only
  looks at the first two characters and would otherwise misfire on a UUID
  that happens to start with a digit-then-digit prefix. All five map to
  `json-schema.org`'s own standard `format` keywords (`uuid`, `email`,
  `ipv4`, `ipv6`, `uri`) in `--output-format json-schema`, the same way
  `NaiveDate / DateTime` already maps to `format: date-time`. No new
  dependency: hand-rolled rather than pulling in `regex` (or a `uuid`/`url`
  crate) as an unconditional dependency of the *default* build, which today
  needs nothing beyond `std` for CSV/TSV/JSON — see "No DuckDB" below for
  why this project treats default-build dependency weight as worth
  protecting deliberately, not just incidentally.
- **Numeric formatting robustness**: `normalize_numeric_str` (feeding the
  existing i64/f64 parse checks, in place of the old bare
  `.replace([',', '$'], "")`) now also trims stray surrounding whitespace,
  recognizes parenthesized negatives (`"(123.45)"` → `-123.45`, standard
  accounting notation for a loss/deduction), strips `€`/`£`/`¥` alongside
  the existing `$`, and strips a trailing `%`. A stripped `%` gets its own
  note (`"'%' stripped from percentage values"`), kept separate from the
  existing `"numeric strings"` note - unlike currency/thousands-separator
  noise, a percentage changes what the number *means* (a column of `"45%"`
  becoming `45` is not the same claim as a column of `"$45"` becoming
  `45`), so the note says so explicitly instead of treating both as the
  same kind of formatting noise.

- **RFC 3339 / ISO 8601 timestamps with a 'Z' suffix or numeric offset**,
  e.g. `"2023-01-01T12:00:00Z"` or `"...+00:00"` - ubiquitous in JSON APIs,
  but unmatched by the older, offset-less `DATE_FORMATS` entries (which
  only covered the bare `"...T12:00:00"` form). `%.f` tolerates a value
  with no fractional seconds at all, and `%z` accepts a colon offset
  (`"+00:00"`) as well as the bare form (`"+0000"`) - both verified
  empirically against real chrono behavior before being relied on, per this
  project's usual practice, rather than assumed.
- **Time-of-day values with no date component** (`"14:30:00"`,
  `"2:30 PM"`) previously fell through every check straight to `String` -
  there was no time-only detection at all, only date/datetime.
  `matching_time_format`/`TIME_FORMATS` (parallel to
  `matching_date_format`/`DATE_FORMATS`) add a `NaiveTime` ideal type,
  mapped to json-schema's `format: time`.
- **Date/time detection now runs before the leading-zero heuristic**, not
  after. This fixed a real, pre-existing bug the time-of-day work above
  surfaced: `has_leading_zero` only inspects a value's first two
  characters, so a value like `"01/15/2024"` or `"09:00:00"` was being
  misclassified as "a numeric ID that lost a leading zero" before the
  date/time checks ever got a chance to run - even though the value fully,
  unambiguously matches a known date/time grammar. The more specific match
  now wins, the same principle already applied to placing UUID/Email/IPv4/
  IPv6/URL ahead of leading-zero.
- **Base-prefixed integer literals** (`"0x1A"`, `"0b1010"`, `"0o17"`) used
  to fall through to plain `String` with no note at all - `i64::parse`
  doesn't understand these prefixes. `parse_prefixed_int` recognizes them
  and resolves to `i64`, but deliberately *only* with an explicit `0x`/
  `0b`/`0o` prefix - a bare hex string with no prefix is exactly as
  ambiguous as a hash/opaque ID (see the UUID note above), so it's left
  alone.
- **MAC addresses** (`"00:1A:2B:3C:4D:5E"`, dash- or colon-separated) are a
  fixed IEEE 802 grammar - six exactly-2-hex-digit groups - checked ahead
  of IPv6 for the same "more specific match wins" reason as everything
  else in this list. Verified separately (not assumed) that this shape
  never parses as a valid `Ipv6Addr` in the first place (6 groups with no
  `"::"` is rejected by `std`'s own strict parser), so the two checks can
  never actually disagree on a real value regardless of ordering - the
  ordering just documents the intent.
- **IBAN and credit card numbers are checksum-validated, not shape-only.**
  `is_iban` implements the ISO 7064 mod-97-10 check (move the first 4
  characters to the end, expand each letter to its two-digit value, the
  result must be ≡ 1 mod 97 - computed digit-by-digit via a running
  remainder, since the expanded number is too big for a `u64`).
  `is_credit_card_number` implements the Luhn (mod 10) check. Both are
  meaningfully stronger evidence than a length/shape regex: a random digit
  string passes Luhn only 1 time in 10, and mod-97 similarly rejects the
  overwhelming majority of non-IBAN strings, so combined with the length
  bound there's essentially no false-positive risk - the same category of
  confidence UUID's fixed grammar already has, just via a checksum instead
  of a fixed dash pattern. Verified against three real IBANs (UK/Germany/
  France, including France's letter-containing BBAN) plus a deliberately
  corrupted checksum, and against several widely-published test card
  numbers, before being relied on.

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
2. **Is it flat/columnar with no nesting** (a new spreadsheet-like format)?
   Build `Vec<ColumnInput>` (name, current_type, raw string values, total
   count) and map through `profile_column`. See `columns_from_xlsx` or
   `columns_from_fixed_width` — the latter also shows what to do when
   there's no delimiter or schema to read structure from: require it
   explicitly (`--widths`) rather than guess at column boundaries. If the
   format is binary with a runtime-only-known layout (no crate ships a
   generic dynamic value type for it), see `columns_from_npy` /
   `npy_value_to_string` for the pattern: decode by hand from the format's
   own type descriptor, and fall back to a hex dump for anything not
   representable as a simple value rather than fabricating one. If it's a
   fixed text grammar with no delimiter-based structure (a log format, a
   report with a known layout), see `columns_from_weblog` or
   `columns_from_syslog` for the pattern: one `regex` per variant, split
   any compound fields into their own columns, decode any packed numeric
   codes against the format's own fixed lookup table rather than leaving
   them opaque, and hard-error with the line number on a line that doesn't
   match rather than skip or misparse it.
3. **Can one file hold multiple tables** (another embedded-database
   format, or anything with named sections/sheets/arrays)? Return
   `Vec<(String, Vec<ColumnProfile>)>` like `columns_from_sqlite`,
   `columns_from_xlsx`, `columns_from_ini`, or `columns_from_npz` and let
   `run()`'s `BTreeMap` unification handle the rest — don't special-case
   rendering.

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
section output and duplicate-key pooling, XML's homogeneous-children-as-
records detection and `@`-prefixed attribute columns, fixed-width text's
character-based column slicing and its actionable error when `--widths`
is missing, gzip/zstd transparently decompressing to their inner format
(plus an actionable error on a corrupt/mislabeled `.gz` and on `.zst`
without `--features zstd`), NumPy's structured-dtype-to-columns decoding
(including that `current_type` reflects the real declared dtype, not a
guess), its row-major positional-column reading of a plain 2D array,
`.npz`'s one-table-per-array output, Combined Log's request-splitting and
dash-as-missing conversion, Common Log's narrower column set, a
format-mismatched log line's actionable error, syslog RFC 3164's
PRI-to-facility/severity decoding and PID extraction, RFC 5424's
nilvalue-as-missing handling, dBase's current-vs-ideal-type gap on a
Numeric field that's really an integer, Stata's missing-marker-as-absent
handling and its own current-vs-ideal-type gap (a double column holding
one `NaN` alongside otherwise-integer data), SAS7BDAT being wired up
(see Known limitations for why it has no dedicated fixture), and that
Markdown output never has a trailing blank line.

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
- **No SPSS (`.sav`/`.zsav`).** Considered alongside Stata and SAS7BDAT,
  and declined for the same duplicate-dependency reason as DuckDB, just at
  smaller scale. `ambers` is the only pure-Rust SPSS reader on crates.io,
  and its `Cargo.toml` depends on `arrow` v57 *unconditionally* - no
  feature flag disables it - alongside the `arrow` v59.2 this project
  already depends on for Parquet/Arrow IPC; not deduped, just duplicated
  (~10-13 extra crates, though unlike DuckDB there's no HTTP client or
  other unrelated baggage riding along). The only other option,
  `polars-readstat-rs`, pulls in all of Polars instead, which is heavier
  still. Would reconsider if `ambers` makes `arrow` optional, or if
  there's a concrete need for `.sav` files.
- **Stata/SAS7BDAT variable/value labels aren't surfaced.** A `.dta` or
  `.sas7bdat` file can carry a human-authored description per variable (a
  "variable label") and, for Stata, a named mapping for coded values (a
  "value label", e.g. `1`/`2`/`3` → `"male"`/`"female"`/`"other"`) - both
  genuinely useful, authoritative metadata, not a guess. Deliberately out
  of scope for now: surfacing them well would mean either overloading the
  existing (always-empty) `description` field with format-provided text -
  a different kind of content than what it's documented to hold - or
  adding a new field to `ColumnProfile`, which is shared by every format's
  renderer and output shape. Worth adding if there's real demand;
  `Variable::label()` in the `dta` crate and `ColumnMeta::label` in the
  `sas7bdat` crate already expose it.
- **No SAS7BDAT test fixture is committed.** Unlike every other format in
  this project, there's no tool available in this development environment
  that can *write* a `.sas7bdat` file (it's a proprietary binary format;
  `pyreadstat`, the usual option, only writes `.dta`/`.sav`/`.xport`, not
  sas7bdat itself), and copying a third-party sample file of unclear
  provenance into the repo wasn't worth the licensing risk. The reader was
  manually verified against the `sas7bdat` crate's own bundled test
  fixture during development (schema, non-ASCII text, and the same
  `current_type`/`ideal_type` gap dBase and Stata already demonstrate);
  the committed test only confirms the format is wired up, the same
  fallback Feather already uses for the same underlying reason (no
  fixture, not a missing capability).
- **A dotted-quad value valid as IPv4 is always reported as IPv4**, even if
  it's semantically something else - a version string like `"1.2.3.4"` is
  indistinguishable from an address at the string level, and there's no
  column-name-based guessing here (see the design philosophy above) to
  break the tie. Same story, smaller stakes, for `"12:30"` legitimately
  being a ratio/score rather than a time.
- **`is_email`/`is_url` are intentionally not standards-complete.** They
  catch the overwhelmingly common shapes (one `@` and a dotted alphabetic
  TLD; an `http(s)://`/`ftp://` prefix with a non-empty rest) and reject
  anything with a real ambiguity, rather than implementing RFC 5322 or a
  full URL grammar by hand - a false negative just falls back to `String`
  (still correct, only less specific), which is the safer failure mode.
- **Category-detection threshold is fixed**, not configurable: ≤50 unique
  values *and* a uniqueness ratio under 5% of total rows. Works fine at
  hundreds-plus rows; on very small files it under-triggers (nothing to do
  about that mathematically — 3 unique values in a 5-row file is 60%
  cardinality no matter how you slice it). The one exception is a
  genuinely constant column (exactly 1 unique value) — that's flagged
  unconditionally regardless of row count, since "constant" doesn't need a
  ratio to be true no matter how few rows there are.
- **`missing_pct` is rounded to 1 decimal place** at construction time
  (`round1`), in both Markdown and JSON. This is a display choice, not a
  precision bug — full float precision was never meaningful here.
- **Date/time-format detection is a fixed candidate list** (`DATE_FORMATS`,
  `TIME_FORMATS`), not a fuzzy parser. Deliberate: a fuzzy parser can
  silently misparse a numeric ID as a date; a fixed list either matches
  every value in a column or reports nothing, which is a safer failure
  mode. Extend the list if a reasonable format is missing one.
