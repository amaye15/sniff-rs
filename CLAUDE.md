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
| CSV / TSV | `.csv`, `.tsv` | *(default)* | `--delimiter` overrides the separator; `--skip-rows` skips N leading rows before the header (auto-detected when not given - see below) |
| JSON | `.json` | *(default)* | array-of-objects, a single (optionally pretty-printed) object, or JSON Lines, auto-detected by content; a top-level array/stream of non-object values profiles as one `value` column |
| JSON Lines / NDJSON | `.jsonl`, `.ndjson` | *(default)* | same reader as JSON |
| Parquet | `.parquet`, `.pqt` | `--features parquet` | full schema, recurses into Struct/List/Map |
| Arrow IPC / Feather | `.arrow`, `.feather` | `--features parquet` | shares Parquet's Arrow infrastructure |
| Avro | `.avro` | `--features avro` | recurses into records/arrays/unions |
| Excel | `.xlsx`, `.xls`, `.xlsb`, `.ods` | `--features xlsx` | one section per sheet, like SQLite (see below) |
| SQLite | `.db`, `.sqlite`, `.sqlite3` | `--features sqlite` | one section per table (see below) |
| MessagePack | `.msgpack`, `.mp` | `--features msgpack` | stream of concatenated records, or a single top-level array |
| TOML | `.toml` | `--features toml` | whole document = one row; array-of-tables flattens like a nested JSON array |
| YAML | `.yaml`, `.yml` | `--features yaml` | single mapping = one row, single sequence = array-of-records, `---`-multi-doc = one row per document; a non-mapping document/sequence-of-scalars profiles as one `value` column |
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
never auto-detected. A missing or unrecognized extension on any *other*
format first falls back to content-based sniffing before giving up and
asking for `--format` — see "Content-based format auto-detection" below.

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
temporary file (via the hand-rolled `TempFile` guard - see the Dependency
footprint section below), cleaned up on drop) *before* format detection
ever runs, so every reader keeps opening a plain file path exactly as
before, with zero per-format changes — including formats that need actual
random file access rather than a stream (Parquet, SQLite, Excel).
Detection and default output naming use the compression-stripped logical
name (`data.csv.gz` behaves like `data.csv`); the JSON/Markdown `file`
field still reports the real, original filename for traceability. gzip is
via a hand-rolled DEFLATE/gzip decoder (pure `std`, no dependency at all -
see the Dependency footprint section); zstd is likewise a hand-rolled RFC
8878 decoder (`zstd_support`, pure `std` too) behind `--features zstd` -
the feature flag now gates the module's own code rather than a real
dependency, since the `zstd` crate itself moved to a dev-only
cross-verification role (see the Dependency footprint section).

NumPy (`.npy`/`.npz`) is via a hand-rolled reader (`npy_support`, pure
`std` too) behind `--features npy` - the `npyz` crate it used to depend on
moved to a dev-only cross-verification role (see the Dependency footprint
section).

## Content-based format auto-detection

`detect_format` tries the file extension first, exactly as described above.
When that fails — no extension, or one this tool doesn't recognize — it
falls back to `sniff_format`, which reads a small prefix of the file's own
bytes (and, for Parquet, its last 4 bytes too) looking for a magic number or
other structural signature, before giving up and asking for `--format`. This
is the same "declared type is a hint, not the truth" principle the rest of
this file already applies to every column, turned on the file format itself
— a misnamed file, or one with no extension at all (a downloaded artifact, a
piped temp file, a database backup someone renamed `.bak`), doesn't have to
be guessed at by a human if its content carries a real signal. A *recognized*
extension is never second-guessed by content — `data.csv` always reads as
CSV regardless of what sniffing might think, so this is a strict superset of
the previous behavior: nothing that used to work changed, and cases that
used to hard-error now succeed automatically.

Deliberately conservative, the same way every heuristic in this file is —
only formats with a fixed magic number, or a multi-field structural check
strong enough to be confident rather than a guess, are attempted:

- **SQLite, Avro, Arrow IPC, NumPy (`.npy`)** — each has a short, fixed,
  unambiguous byte prefix (`"SQLite format 3\0"`, `"Obj\x01"`, `"ARROW1"`,
  `"\x93NUMPY"` respectively). Checked directly against each reader crate's
  own source rather than assumed from memory.
- **SAS7BDAT** — a fixed 32-byte magic (`SAS7BDAT_MAGIC`), copied verbatim
  from the `sas7bdat` crate's own `probe.rs` (`SAS7BDAT_MAGIC_NUMBER`) — the
  same "verified against the source" discipline as the rest of this list,
  notable here specifically because this format has no committed test
  fixture to cross-check against (see Known limitations), so the crate's own
  source was the only verification path available.
- **Parquet** — `"PAR1"` at both the very start of the file *and* the last 4
  bytes (`"PARE"` for an encrypted footer is also accepted) — checking only
  the header would let a truncated or unrelated file that happens to open
  with those 4 bytes false-positive; requiring the footer too closes that
  gap.
- **Old-style `.xls`** (pre-2007, OLE2/Compound File Binary) — its container
  magic (`D0 CF 11 E0 A1 B1 1A E1`) is shared with old `.doc`/`.ppt`, but
  this tool reads no other format that magic could mean, so there's no
  practical ambiguity.
- **Stata `.dta`** — the modern XML-like container (release 117+) opens with
  a literal `<stata_dta>` tag. The older binary format (102–116) has no
  fixed string at all, just a numeric release byte — so this combines it
  with the byte-order byte that always immediately follows (only 0, 1, or 2
  are real) into one two-field check, confident enough on its own that
  neither field alone would be.
- **dBase `.dbf`** — no fixed magic string either, just a version byte that
  on its own is nowhere near unique (roughly a dozen accepted values out of
  256 possible). Paired with three more of the header's fixed-offset fields
  — a valid month/day in the "last updated" date, and a header length and
  record length that are internally consistent with a real dBase file — the
  combined false-positive rate is negligible. Verified against the `dbase`
  crate's own header layout (`header.rs`'s `Header::read_from` and
  `Version::from`), the same "stack independent weak signals into one
  confident check" approach Stata's binary format needed for the same
  underlying reason (no single fixed string available).
- **Zip-based formats** (`.xlsx`/`.xlsb`/`.ods`, `.npz`) all share the same
  outer `PK\x03\x04` magic, so telling them apart needs a peek at what's
  actually packed inside. A zip's local file header stores each entry's
  filename as plain, uncompressed ASCII, so a substring search over the same
  head buffer — no real zip/central-directory parsing needed — reliably
  tells them apart: an OOXML spreadsheet always carries an `xl/`-prefixed
  entry (verified empirically against this project's own committed `.xlsx`
  fixtures — `xl/workbook.xml`, `xl/worksheets/sheet1.xml`, etc.), an ODF
  spreadsheet's first entry is a literal `mimetype` file whose content names
  its type (`application/vnd.oasis.opendocument.spreadsheet`), and an
  `.npz` archive's entries are always named `<array-name>.npy`.
- **JSON and XML** — the only two plain-text formats sniffed, because
  they're the only two with an unambiguous leading character: `{`/`[` (after
  skipping leading whitespace) is JSON's own grammar, not a guess, and
  covers JSON Lines too (each line still opens with `{`). XML requires the
  character right after `<` to be a valid tag-name start (an ASCII letter,
  `_`, or the `?` of an XML declaration) specifically so it can't collide
  with an RFC 3164 syslog line, which also opens with `<` but is always
  followed by a PRI digit (`"<34>Oct 11 ..."`) — a digit is never a legal
  XML tag-name start.

**CSV, TSV, TOML, YAML, and INI are deliberately left un-sniffed.** Plain
delimited or key-value text carries no fixed magic number or unambiguous
leading character, so guessing between them would mean guessing at intent —
exactly what this project's heuristics never do (see "Design philosophy"
below). This is the same category of disclosed, irreducible ambiguity as a
dotted-quad value being valid as both IPv4 and a version string. Fixed-width
text and the four log formats aren't attempted either, for the reason
already stated above: no delimiter or magic number distinguishes them from
generic text at all, which is exactly why they're `--format`-only in the
first place, not a gap specific to sniffing.

One further, explicitly out-of-scope case: an extensionless file that's
*also* gzip- or zstd-compressed. `compression_from_extension` (feeding
`decompress_if_needed`) is still purely extension-based (`.gz`/`.gzip`/
`.zst`/`.zstd`) and runs *before* format detection, so a compressed file
with no extension at all skips decompression entirely and `sniff_format`
sees raw compressed bytes it has no signature for — a clear `--format`-
demanding error, not a silent misdetection, but not an automatic pass
either. Extending sniffing to compression itself was considered and set
aside as a separate concern from identifying the *inner* data format, the
thing this feature was actually asked to solve.

Tested at two levels, the same split this project uses for every other
heuristic: `sniff_format` itself has direct unit tests in `lib.rs`'s
`#[cfg(test)]` module (one file per format via synthetic byte buffers,
including near-miss cases — a bad month byte for dBase, an out-of-range
release byte for Stata, a truncated Parquet footer, a syslog line's PRI
digit rejected by the XML check), and `tests/integration.rs` proves the
*full* pipeline end-to-end (extension missing or wrong → sniff → correct
reader dispatch → correct profile) by copying real fixtures to
extensionless/misnamed paths and running the compiled binary against them.

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
reader that decodes a binary layout entirely by hand (`npy_support` - see
the Dependency footprint section for why, and for the npyz-crate history):
an arbitrary user's `.npy` file has a dtype only known at runtime, not at
compile time, so `npy_scalar_to_string`/`npy_value_to_string` interpret
each field's raw bytes directly from its own hand-parsed `TypeStr`
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

dBase is a more conventional flat reader (`columns_from_dbase`, via
`dbase_support` - a hand-rolled reader now, see the Dependency footprint
section), but with one thing worth calling out: column order comes from
the file's own field descriptor table (in file order) rather than a
HashMap's iteration order, which isn't guaranteed stable. Soft-deleted
records (dBase's own "marked for deletion" flag) are skipped before this
project's own heuristics ever see them - that's the format's own
convention, not something this tool is choosing to hide. Its `Numeric`
field type doesn't distinguish int from float at the storage level, so
`current_type` reports the same `f64` for every numeric field regardless -
exactly the kind of gap `ideal_type`'s independent re-derivation from the
actual values exists to surface, the same way CSV's leading-zero check
does for a different reason.

Stata (`columns_from_stata`, via `stata_support` - a hand-rolled reader
now, see the Dependency footprint section) is architecturally the same
shape again, with its own version of the same lesson: a DTA file marks
each individual value present-or-missing explicitly (`.` through `.z`),
so a `Missing` value is simply omitted
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

SAS7BDAT (`columns_from_sas7bdat`, via `sas7bdat_support` - a hand-rolled
reader now, see the Dependency footprint section) follows the same shape,
but its `current_type` comes straight from the file's own column-format
metadata (a numeric type code plus an optional format name, resolved to
a logical type) rather than being inferred from row values, the same as
`arrow_type_label` does for Parquet/Arrow. That declared type is
genuinely worth cross-checking: SAS stores nearly all numeric data as
8-byte doubles internally regardless of the value's real precision, so
`current_type: "f64"` with `ideal_type` correctly narrowing to `"i64"`
for a whole-number column isn't a bug in either the reader or this tool -
it's the same "declared type is a hint, not the truth" lesson Parquet/
Avro/dBase/Stata all already demonstrate, in one more format's own way of
losing that distinction. SAS also has per-column labels (same considered
non-surfacing decision as Stata's).

Common/Combined Log Format also land here (`columns_from_weblog`, via
`weblog_support` - a hand-rolled parser now, see the Dependency footprint
section; each grammar's exact field layout used to be matched with a fixed
`regex`, and still is, mechanically, just without the crate). The
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

Syslog (RFC 3164 and RFC 5424, `columns_from_syslog`, via `syslog_support`)
follows the exact same shape as the web access logs - same hand-rolled-
parser-over-a-fixed-grammar approach, same "hard error naming the line"
behavior for a mismatched line, same `-`-as-nilvalue-so-missing
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

Recursion never trades away typing precision: a pooled array of scalars
(`unwrap_arrays`, which walks arbitrarily-nested arrays down to their
non-array, non-null leaves) is run through the exact same `suggest_ideal_type`
engine a top-level scalar column gets, wrapped as `Vec<T>` (a column of UUID
strings resolves to `Vec<UUID>`, not a generic `Vec<String>`), and every
dot-notation sub-column produced by flattening an object - at any nesting
depth - is itself profiled from scratch the same way, so a value three
levels deep (`object -> object -> array of objects -> leaf`) gets exactly
as precise a type as a flat top-level column would. Verified directly via
`profile_json_path_types_a_plain_array_of_scalars_precisely`,
`profile_json_path_types_every_field_of_an_array_of_objects`, and
`profile_json_path_resolves_a_leaf_three_levels_deep` in `lib.rs`'s
`#[cfg(test)]` module, and end-to-end via
`nested_arrays_and_objects_are_recursively_typed_at_every_leaf` in
`tests/integration.rs`. The one deliberate exception: an array that mixes
raw scalars and objects together (`[1, 2, {"x": 1}]`) can't honestly claim
one precise scalar type for the column as a whole - some elements are
structurally objects, not near-misses of the same scalar type - so the
scalar portion falls back to `String`/`Vec<String>` with a note explaining
why, the same "no partial credit" rule `suggest_ideal_type`'s `.all(...)`
checks already enforce everywhere else in this file (e.g. a "mostly UUID"
column isn't a trustworthy UUID column). The object portion of a mixed
array is still recursed into and typed normally regardless -
`profile_json_path_does_not_overclaim_a_precise_type_for_a_scalar_and_object_mix`
locks this exact tradeoff in.

The load-bearing design decision is that **non-native nested formats are
bridged into `serde_json::Value` and handed to the exact same recursive
flattener**, rather than reimplementing recursion per format:

- Avro decodes each record straight to `serde_json::Value`
  (`avro_support::decode_to_json` - a hand-rolled reader now, see the
  Dependency footprint section) and calls `profile_json_records` —
  identical code path to a `.json` file — or, if not every decoded value
  is an object (a real, valid shape: an Avro RPC response file, for
  instance, decodes to a bare scalar), the same top-level-scalar fallback
  the JSON/YAML readers use, profiling the whole set as one `value` column
  instead. Unlike every other bridge in this list, decoding and JSON
  conversion happen in a *single* pass rather than two: since the
  `Schema` is already in hand at every step of decoding (unlike the
  dynamic-value-tree bridges below, which decode first and only then walk
  the result alongside a schema), there's no separate co-recursion step
  needed to resolve a logical type like `decimal` - Avro stores only the
  unscaled two's-complement integer in the value, and the scale that says
  where the decimal point goes comes directly from the schema node already
  being decoded against, converted to a string via hand-rolled schoolbook
  long division (not a bignum library - see that reader's own dependency-
  footprint entry for why plain digit-shifting arithmetic is enough here).
  `timestamp-millis`/`-micros`/`-nanos`, their `local-*` counterparts, and
  `time-millis`/`-micros` all get resolved to real date-time/time-of-day
  strings the same way `date` already was, rather than being left as
  opaque epoch integers - see the design philosophy section for how this
  was found (cloud-platform Avro producers like Kinesis Firehose, Event
  Hubs Capture, and Pub/Sub lean heavily on exactly these logical types).
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
  of JSON Lines, but self-delimiting rather than newline-delimited). Not
  every document/sequence-element has to be a mapping either - the same
  "no field names, but still a genuine single column" fallback the JSON
  reader uses for a top-level array of scalars applies here too (see the
  design philosophy section below).
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
  an INI section's choice for their own single-document shapes. XML is also
  the one nested format whose crate has no recursion guard of its own -
  `xmltree`'s own tree-building recurses once per nesting level with
  nothing capping it, confirmed to genuinely stack-overflow the compiled
  binary on a 50,000-level-deep adversarial document (a real SIGABRT, not
  a catchable error) before `xml_nesting_too_deep` was added. That function
  runs a conservative pre-parse scan of the raw text - not a full
  tokenizer, just enough state to walk past comments/CDATA/processing
  instructions/DOCTYPE (whose content must never affect the depth count)
  and tell an opening tag from a closing or self-closing one - and refuses
  to hand `xmltree` anything nested past `MAX_XML_DEPTH` (512), the same
  clean-error-instead-of-a-crash contract every other nested format already
  gets from its own parsing crate (serde_json's built-in limit,
  toml_edit's `#![recursion_limit = "256"]`, serde_norway's, rmpv's,
  ciborium's - each verified directly against that crate's own source, not
  assumed, before trusting it as already-safe).
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
  `arrow_batch_to_json_rows` tries the whole batch through `ArrayWriter` in
  one call first (the fast, common path), but falls back to converting
  each nested column separately if that fails - a real, encountered case
  being a Map column with non-UTF8 keys (`Map<Int32, T>` is legal Parquet/
  Arrow, e.g. a numeric-code lookup table), which the JSON writer refuses
  outright for the whole batch regardless of what every other column looks
  like. A column that still fails even in isolation gets a disclosed
  "nested content could not be converted for typing" note rather than
  silently losing the column or failing the whole file over one
  unsupported column - see the design philosophy section below.

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
- **A much wider `DATE_FORMATS` list**: European/international dot-
  separated dates (`"15.01.2024"`, `"2024.01.15"`), full month names
  alongside the existing abbreviated (`%b`) forms (`"January 15, 2024"`,
  `"15 January 2024"`), RFC 2822/1123 - the HTTP and email `Date` header's
  own format (`"Mon, 15 Jan 2024 10:00:00 +0000"`) - Unix `date`/`ctime()`'s
  default textual format (`"Mon Jan 15 10:00:00 2024"`, also git log's
  default), Oracle's own default `NLS_DATE_FORMAT` (`"15-Jan-2024"` /
  `"15-Jan-24"`), two-digit-year variants of the existing `%m/%d`/`%d/%m`
  forms, datetime combinations with no seconds field (including the
  literal shape an HTML5 `<input type="datetime-local">` submits), and
  compact/"Basic" ISO 8601 with no punctuation at all
  (`"20240115T100000"`). Verifying RFC 2822 surfaced a detail worth
  confirming rather than assuming: chrono actually cross-validates the
  `%a` weekday token against the parsed date, rejecting e.g. a date
  correctly computed as a Monday if the string itself claims Tuesday,
  rather than treating `%a` as a shape-only three-letter token.
  Adding the two-digit-year forms surfaced a real, pre-existing
  correctness gap in chrono itself, not something newly introduced: `%Y`
  accepts variable-width numeric input while *parsing* (it only zero-pads
  to 4 digits on *output*), so `NaiveDate::parse_from_str("01/15/24",
  "%m/%d/%Y")` silently succeeds as `0024-01-15` - year 24 AD - rather than
  failing outright. Confirmed directly, not assumed, alongside the
  corresponding fact that made the fix free: `%y` correctly *rejects* an
  actually-4-digit year with a "trailing input" error rather than
  truncating it. `DATE_FORMATS` places every `%y` form immediately before
  its `%Y` counterpart specifically so a genuinely 2-digit year matches
  the honest interpretation first, while an already-4-digit year still
  falls through to `%Y` exactly as before - this ordering constraint is
  general to any future `%Y`-anchored entry that gains a `%y` sibling, not
  just the ones added here.
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
- **ISBN-10, ISBN-13, and EAN-13/UPC-A barcodes** share one checksum
  function (`ean_check_digit_valid`) for the latter two: UPC-A is exactly
  an EAN-13 with an implicit leading zero (0 contributes nothing to the
  weighted sum either way, verified by hand against a real UPC-A number),
  and ISBN-13 is just a 978/979-prefixed EAN-13. ISBN-10 uses an older,
  different mod-11 scheme (`is_isbn10`), including its own quirk - the
  check digit can be the letter `X`, standing for 10. All three checked
  *ahead* of the broader-range Credit Card Number check, since they only
  match an exact 10/12/13-digit length: the more narrowly-scoped match
  should win a tie. That tie is real, not hypothetical - a 13-digit number
  can in principle satisfy both a card issuer's Luhn check and EAN-13's
  mod-10 check by coincidence, which is genuinely undecidable from the
  digits alone without domain context, the same category of irreducible
  ambiguity as a dotted-quad value being valid as both IPv4 and a version
  string (see below). Every checksum here was hand-verified against known-
  real numbers (a real EAN-13, a real UPC-A, two real ISBNs of the same
  book in both formats) plus a deliberately tampered one, before being
  relied on - not derived and trusted on the first attempt, since check-
  digit weighting is exactly the kind of arithmetic that's easy to get
  subtly wrong (an off-by-one in which position gets which weight).
- **SemVer** (`"1.2.3"`, `"2.0.0-beta.1"`, `"1.0.0+build.5"`) - a
  reasonably faithful but not 100%-spec-exhaustive check of semver.org's
  own grammar (MAJOR.MINOR.PATCH, each numeric with no leading zero unless
  the identifier is literally `"0"`, plus an optional `-prerelease` and/or
  `+build` suffix). Deliberately requires exactly 3 dot-separated core
  components so it can never collide with `is_ipv4`'s 4-octet grammar -
  but it carries the exact same kind of irreducible ambiguity IPv4 already
  has with a dotted version string: a plain 3-part dotted numeric code
  that isn't actually a software version is indistinguishable from a real
  one at the string level, and there's no column-name-based guessing here
  to break the tie.
- **Embedded JSON in a text cell** - a CSV/text value that's itself a
  serialized JSON object or array (`is_embedded_json`, via `serde_json`,
  already a core dependency - no new one needed). Deliberately excludes a
  bare scalar (`"5"`, `"true"`, `"\"hello\""` are all technically valid
  JSON too), since those are already correctly handled by the numeric/bool
  checks - this only fires on the object/array case those can't already
  explain. `ideal_type` stays `String` (it genuinely still is one), but
  gets a note flagging it's worth parsing separately - the same treatment
  a genuinely-nested source column already gets in `profile_json_path`
  (`"nested value (array/object) - consider flattening before typing"`),
  just discovered from the string content instead of the format's own
  type system.
- **Literal `"infinity"`/`"nan"` text silently became a clean-looking f64
  column with zero warning** - a real bug found via adversarial testing,
  not a hypothetical. Rust's own `f64::from_str` accepts `"inf"`,
  `"infinity"`, and `"nan"` (any case, optionally signed) as legitimate
  IEEE-754 values, not a parse error, so a stray `"Infinity"` typed into an
  otherwise-clean numeric column (a serialization bug, an overflow marker
  from another system, a human data-entry mistake) sailed straight through
  to `ideal_type: f64` with nothing to indicate anything was unusual.
  `ideal_type` still resolves to `f64` - it genuinely is a legal float
  value, and some domains do use ±infinity deliberately - but the f64
  branch of `suggest_ideal_type` now checks `!f.is_finite()` on every
  parsed value and appends an explicit note when it fires, so the surprise
  is disclosed rather than hidden.
- **A column of digit strings too large for `i64` silently lost precision
  as f64, also with zero warning** - the second bug adversarial testing
  found. Once a value's magnitude overflows `i64` (~9.2e18) it falls
  through to the f64 check, but f64 can only represent integers *exactly*
  up to 2^53 (~9e15) - three orders of magnitude smaller - so the fallback
  silently rounds real digits away. `is_plain_integer_literal` identifies a
  value that's a bare integer literal (no `.`, no exponent) which
  *individually* failed `i64::parse` (not just present in a column some
  *other*, differently-shaped value blocked from the i64 branch - the two
  are easy to conflate and an earlier draft of this fix did exactly that,
  caught by `suggest_ideal_type_does_not_flag_precision_loss_for_ordinary_i64_sized_values`)
  and adds a note when it does, since by construction such a value is
  already too large for `i64`'s range, which is itself well past f64's
  exact-integer range - precision loss is guaranteed, not just risked.
- **A dedicated adversarial/robustness test suite** exists specifically to
  keep catching this class of bug. It covers three things distinct from
  the correctness tests above: (1) every validator survives a grab-bag of
  hostile input - empty strings, control characters, multi-byte unicode
  including 4-byte emoji, SQL/shell/template-injection-style payloads, and
  a 100,000-character string - without panicking, regardless of what it
  returns; (2) `is_iban` and `normalize_numeric_str` both do raw
  byte-offset string slicing (`&s[4..]`, `&s[1..len-1]`) that's only safe
  because a preceding check guarantees the bytes at those offsets are
  single-byte ASCII - proven directly with adversarial multi-byte input at
  exactly those code paths, not just reasoned about; (3) every checksum
  and fixed-grammar check is proven to genuinely discriminate, not just
  check shape - a real, valid IBAN/credit-card/ISBN-10/ISBN-13/EAN-13
  number with exactly one digit tampered is confirmed rejected, and
  `suggest_ideal_type`'s `.all(...)` semantics are confirmed to veto a
  whole column's classification on a single non-conforming value (a
  "mostly UUIDs" column is not a trustworthy UUID column) rather than
  taking a majority vote.
- **A deeply-nested XML document could crash the whole process with a real
  stack overflow, not a handled error** - a third bug this project's
  adversarial-testing practice found directly, the same way the infinity/
  NaN and i64-precision-loss bugs below were found: not reasoned about in
  advance, but discovered by deliberately trying to break the tool.
  `malformed_deeply_nested.json` already proved JSON survives unbounded
  nesting depth cleanly, so the natural adversarial question was whether
  every *other* nested format (TOML, YAML, MessagePack, CBOR, XML) had the
  same protection - each format's own parsing crate was checked directly
  rather than assumed safe by analogy, and every one of them does have an
  explicit recursion/depth guard **except** `xmltree`, confirmed by
  actually constructing a 50,000-level-deep adversarial XML document and
  watching the compiled binary abort with a genuine stack overflow.
  `xml_nesting_too_deep` (see the Architecture section's XML paragraph)
  closes this with a pre-parse scan that refuses anything nested past a
  fixed depth, restoring the same "clean error, never a crash" contract
  every other format already had - verified not to false-positive on
  legitimate complex XML either (comments/CDATA containing literal
  `<`/`>` characters, self-closing tags, thousands of wide-but-shallow
  siblings).
- **Avro's `timestamp-millis`/`-micros` logical types were silently reduced
  to opaque epoch integers, and `decimal` rendered as unusable Rust Debug
  output** - found while checking whether files produced by cloud-platform
  data services (Kinesis Firehose, Event Hubs Capture, Pub/Sub, all of
  which lean on Avro logical types for timestamps and precise numerics)
  actually read correctly, not by auditing the code in the abstract.
  `avro_value_to_json`'s old match arm merged `TimestampMillis`/
  `TimestampMicros` in with plain `Long` (`AvroValue::Long(i) |
  AvroValue::TimestampMillis(i) | ... => JsonValue::Number((*i).into())`),
  discarding the exact semantic information the schema's logical-type
  annotation exists to carry - a real, if quieter, cousin of the leading-
  zero and infinity/NaN bugs elsewhere on this list: the schema *told* the
  reader this was a timestamp, and the reader threw that away anyway. The
  Decimal case was worse - `apache_avro::types::Value`'s catch-all fallback
  (`other => JsonValue::String(format!("{other:?}"))`) rendered a decimal
  field as `"Decimal(Decimal { value: 12345, len: 2 })"`, confirmed by
  actually writing an Avro file with a `decimal` field and reading it back.
  Both are fixed now (see the Architecture section's Avro paragraph) and
  verified against positive, negative, zero, and zero-padded-fraction
  decimal values, plus the same logical type nested inside a record, an
  array, and a nullable union, to prove the schema co-recursion resolves
  scale correctly at every nesting shape it could plausibly appear in, not
  just the flat top-level case that was tested first.
- **`\N` (literal backslash-N) joined the missing-value sentinel list** -
  not a bug, but a genuine gap found the same way: checking what cloud
  data-warehouse export tools actually write for NULL. MySQL's `SELECT
  INTO OUTFILE`, Hive's default text SerDe, and Redshift's `UNLOAD ...
  NULL AS '\N'` all use it, and none of pandas' own default `na_values`
  (which the rest of this list deliberately mirrors) happen to include it -
  so this one entry is justified on its own real-world-convention merits
  rather than by that usual "matches pandas" reasoning.
- **Hex colors and IMEI** - two more precise, low-risk checks. `is_hex_color`
  is the `parse_prefixed_int` pattern again: a `#` prefix
  plus exactly 3/4/6/8 hex digits (RGB/RGBA/RRGGBB/RRGGBBAA) is essentially
  zero-ambiguity, checked at the very top of `suggest_ideal_type` alongside
  the other prefix-disambiguated checks. `is_imei` reuses
  `luhn_checksum_valid` directly rather than a second implementation - an
  IMEI is a 15-digit Luhn-checksummed identifier, the exact same algorithm
  a credit card number uses, just at a fixed length - checked ahead of the
  broader-range credit card check for the same narrower-match-wins-a-tie
  reason ISBN/EAN already are. Verified against a real, widely-cited
  reference IMEI (`"490154203237518"`) plus a tampered counterpart, and
  both fixtures (`type_detection.csv`, `adversarial.csv`) got matching
  valid/near-miss columns as part of the same change, not added later -
  every new type in this project now gets its near-miss coverage
  immediately, per the adversarial-testing section above.
- **JWT (JSON Web Tokens)** - three dot-separated base64url segments where
  the header and payload segments must each decode to a valid JSON
  *object* (RFC 7519 defines both as always objects). This is a much
  stronger signal than "three base64-ish segments separated by dots" -
  proven directly by a test where all three segments are individually
  valid base64url but decode to plain text, not JSON (`is_jwt` correctly
  rejects it). `base64url_decode` is hand-rolled (RFC 4648 §5) rather than
  adding the `base64` crate as an unconditional dependency of the default
  build - the same UUID/email/URL/hex-color tradeoff made throughout this
  file. The signature segment is intentionally only checked for a valid
  base64url charset, never decoded as JSON - it's arbitrary bytes by
  design (an HMAC or signature), not structured data. Verified against
  jwt.io's own canonical example token.
- **Geographic coordinate pairs** (`"40.7128,-74.0060"`) - deliberately the
  most conservative check in the file, and checked *last* among the
  "precise grammar" tier for exactly that reason. Unlike everything else
  above it, there's no checksum or fixed prefix ruling out coincidence: a
  plain pair of small decimals is structurally identical to a real
  coordinate. Requiring a decimal point in *both* components (real
  coordinate data essentially always carries fractional precision) plus
  the standard ±90/±180 range rules out plain integer pairs and
  out-of-range values, but doesn't eliminate the ambiguity - "1.5,2.5"
  still passes. This tradeoff was made deliberately and with the user's
  explicit awareness (flagged as the most ambiguous of a batch of four
  candidates before it was picked), the same spirit as the IPv4-vs-
  version-string and SemVer ambiguities already documented above:
  disclosed and accepted, not hidden.
- **Hash-digest length is a note, deliberately never a type promotion.**
  `hash_digest_kind` classifies a value by exact hex-digest length alone
  (MD5=32, SHA-1=40, SHA-256=64 hex chars) - checked *after* geographic
  coordinates, i.e. dead last, because there's even less signal here than
  anywhere else in the file: no checksum, no prefix, not even a range
  constraint, just "this many hex characters." A bare, undashed UUID is
  itself exactly 32 hex characters, so this would misfire constantly if
  promoted to a confident type the way UUID/IMEI/IBAN are. Instead it only
  ever adds a note to an otherwise-plain `String` column
  (`"matches MD5 hex-digest length (32 hex chars) - shape only, not a
  validated hash"`) - informative without asserting something the tool
  can't actually verify. Requires every value in the column to share the
  *same* digest length, not just "some hex-shaped length" - a column
  mixing a 32-char and a 40-char value doesn't get the note either,
  confirmed by a dedicated adversarial fixture column.
- **VIN (Vehicle Identification Number)** - the most algorithmically
  complex check in this file: 17 characters, transliterated through
  NHTSA's own letter-to-digit table (I/O/Q are never valid VIN characters
  at all, excluded from the standard itself to avoid confusion with
  1/0/0), each multiplied by a fixed per-position weight, summed and
  reduced mod 11 - the result must equal the check digit at position 9
  (itself weighted 0, since it's what's being checked against, not part of
  the sum). Given the complexity, this got extra-careful verification: the
  canonical reference VIN used throughout VIN-checksum documentation
  (`"1HGCM82633A004352"`) was recomputed *by hand*, digit by digit, not
  just trusted from `is_vin`'s own output - the full working is preserved
  as a comment directly above the test that encodes it
  (`is_vin_validates_the_canonical_reference_vin_and_rejects_a_tampered_one`).
  This is honestly a smaller verification set than IBAN/ISBN got (which
  were checked against 3+ independent real numbers) - a second recalled
  VIN failed validation during development and was discarded rather than
  investigated, specifically *because* it couldn't be confirmed as a real,
  correctly-issued VIN (recalled from memory, not sourced), so it wasn't
  trustworthy evidence either way. Checked ahead of the credit card number
  range for the same narrower-match-wins-a-tie reason as ISBN/EAN/IMEI.

- **CIDR notation** (`"192.168.1.0/24"`, `"2001:db8::/32"`) - the cheapest
  addition in this file: `is_cidr` reuses `is_ipv4`/`is_ipv6` directly for
  the address part, and just adds the prefix-length range CIDR notation
  itself defines (0-32 for IPv4, 0-128 for IPv6). No new parsing logic, no
  new ambiguity beyond what IPv4/IPv6 already carry.

- **ULID** - 26 Crockford-base32 characters encoding a 128-bit value (48-bit
  timestamp + 80 bits of randomness), a growing alternative to UUID.
  `26 * 5 = 130` bits, 2 more than the 128 actually used - those 2 extra
  bits live at the top of the first character, so a real, non-overflowing
  ULID's first character can only be `'0'`-`'7'`, never `'8'` or higher.
  This detail is checked (not just the alphabet and length), and is
  flagged honestly as a smaller-confidence detail than most of this file:
  it's widely documented in the ULID spec itself and the canonical example
  starts with `'0'`, but if it were ever misremembered the failure mode is
  a false negative (falls back to `String`), the safe direction. Checked
  *ahead of* `parse_prefixed_int` further down: a ULID beginning `"0X..."`
  (a real, valid Crockford digit sequence) would otherwise get intercepted
  by the `"0x"` hex-literal prefix check first, since that only needs a
  2-character match versus this check's full 26 - a concrete instance of
  the same "more specific match wins" principle applied throughout this
  file.

- **WKT (Well-Known Text) geometry** (`"POINT(30 10)"`,
  `"LINESTRING(30 10, 10 30, 40 40)"`) - a real OGC keyword followed by a
  balanced, parenthesized body containing only coordinate-safe characters.
  Deliberately structural, not a full WKT parser - it doesn't validate a
  well-formed ring/point-count, just that the keyword is real and the body
  is balanced. **`GEOMETRYCOLLECTION` is deliberately excluded from the
  keyword list**, and this was found empirically, not just reasoned about
  in advance: unlike the other six WKT types, its body legitimately nests
  *other* geometry keywords (`"GEOMETRYCOLLECTION(POINT(4 6))"`), which
  isn't just coordinate characters - a real fixture value with it caused
  the whole test column to fail the coordinate-only character check the
  first time this was tried. Properly supporting it needs actual recursive
  parsing, a meaningfully bigger scope than "keyword + balanced coordinate
  body." Rather than either overclaim support that silently breaks on
  nesting, or loosen the character check for every keyword (raising
  false-positive risk for the other six), it's just left out - a
  `GEOMETRYCOLLECTION` value falls back to `String`, confirmed by its own
  test. Checked well ahead of the weaker `is_lat_lon_pair` check, since the
  OGC keyword is a much stronger, more specific signal.

- **Cron expressions** (`"0 0 * * *"`, `"*/15 * * * *"`) - a standard
  5-field schedule (minute/hour/day-of-month/month/day-of-week), each
  field a `*`, a number, a comma-separated list, a range `N-M`, or a step
  `*/N`/`N-M/N`, each checked against its field's real valid range (minute
  0-59, hour 0-23, day-of-month 1-31, month 1-12, day-of-week 0-7 where
  both 0 and 7 mean Sunday). Deliberately does not support named months/
  weekdays (`JAN`, `MON`, ...) - kept to the numeric grammar most cron
  implementations share, rather than a larger, harder-to-verify keyword
  table (a false negative just falls back to `String`). Checked at the
  same tier as `is_lat_lon_pair`, and carries the same kind of disclosed,
  irreducible ambiguity: five arbitrary small integers in range
  (`"1 2 3 4 5"`) are indistinguishable from a real cron schedule - no
  checksum or prefix rules out coincidence here either.

- **Boundary-value tests are a distinct category from near-miss tests, and
  both exist.** A near-miss test (already covered throughout this file)
  proves a value just *past* a valid range is rejected; a boundary test
  proves the *inclusive* edge of that same range is still accepted -
  every range check in this file uses `..=`, and that's exactly where an
  accidental `<` instead of `<=` would hide, silently. Covered: IPv4's
  `0.0.0.0`/`255.255.255.255`, IPv6's all-zero/all-`f` forms, CIDR's `/0`
  and `/32`/`/128`, a credit card at exactly 12 and exactly 19 digits
  (both Luhn-valid, constructed and verified via a throwaway harness, not
  assumed), an IBAN at exactly 15 and exactly 34 characters (mod-97-valid,
  same verification discipline), `is_lat_lon_pair` at exactly ±90/±180,
  every cron field at its real minimum and maximum simultaneously
  (`"0 0 1 1 0"` and `"59 23 31 12 7"` - note day-of-week 7 meaning Sunday,
  the same as 0, is exercised specifically), the actual decision
  boundary behind the i64-overflow precision-loss note (`i64::MAX` itself
  gets no note since it fits exactly, `i64::MAX + 1` does - not an
  arbitrary digit-count threshold, the literal boundary the code checks),
  and the category-detection threshold's two independent edges (unique
  value count crossing exactly 50 with the ratio held fixed and tiny; the
  uniqueness ratio crossing exactly 5% with the unique count held fixed at
  10 - the check is a strict `<`, so precisely 5.0% is confirmed to *not*
  count).

- **A title/banner row above the real header - a common shape in
  human-authored spreadsheets exported to CSV - used to silently become the
  header itself, demoting the real header row to a data value.** Found via
  real-world testing rather than reasoned about in advance: a public,
  35,000-row salary survey (Ask A Manager's) has an instructions banner
  line above its real header, and independently, the HPI Pollock data-
  loading benchmark's own `file_preamble.csv` fixture (from its survey of
  245,000+ real open-data-portal CSVs) showed the exact same shape - two
  unrelated real-world sources landing on one bug. `detect_preamble_rows`
  fixes this the same way every other heuristic in this file is held to a
  confident-signal bar: it only ever fires on row *structure*, never on
  cell content or column-name guessing. A leading row counts as preamble
  only if it has at least two fields and at most one of them is non-empty
  (a real header virtually always names every column; requiring >= 2
  fields specifically rules out misreading a genuine single-column
  dataset, whose every row is trivially "1 of 1 fields populated"), and
  the run of such rows must be immediately followed by a row where *every*
  field is populated - the strongest available signal that row is the
  real header rather than just another sparse one. `MAX_PREAMBLE_SCAN`
  (5) bounds the run so a genuinely sparse dataset can never have an
  unbounded chunk silently skipped, and a header that legitimately has an
  empty/unlabeled column correctly leaves auto-detection off rather than
  misfiring (verified via a dedicated test) - either signal failing to
  hold leaves `skip_rows` at 0, the same old behavior, rather than
  guessing. Auto-detection only runs when `--skip-rows` isn't given
  explicitly, and always discloses what it did to stderr (`detected N
  preamble row(s) before the header - skipping`) rather than silently
  changing the output - the same "never hidden" treatment every other
  auto-behavior in this file gets (compare content-based format sniffing).
  Implementing `--skip-rows` correctly surfaced a real, separate bug of
  its own along the way: seeking a fresh strict (non-flexible) `csv::Reader`
  to resume past the skipped rows calls the crate's own `byte_headers()`
  internally, which - confirmed directly against the `csv` crate's own
  source (`Reader::seek`, `ReaderState::add_record`), not assumed - reads
  a record from *byte 0 of the file* purely to populate its own cache
  before the seek happens, silently seeding the crate's internal
  ragged-row tracker from whatever that first record's field count was
  (the preamble's, not the header's) and never re-seeding it from the real
  header afterward. On a real file whose header and data rows happen to
  have different field counts (also found via the same real-world sweep,
  not hypothesized - three files hit this exact shape), this let a
  genuinely ragged row silently pass through instead of erroring, causing
  an out-of-bounds panic downstream rather than the clean error this
  project always guarantees for malformed CSV. The fix sidesteps the
  crate's internal state machine entirely rather than fighting it: the
  resumed reader is `flexible(true)`, and every record's length is checked
  explicitly against `headers.len()` in this project's own code - stating
  the actual invariant ("every row matches the header") directly instead
  of depending on which record happened to seed an internal tracker first.
  A follow-up, more exhaustive real-world pass (running the *entire*
  crawled-CSV survey rather than a sample, after an initial pass had
  mistakenly only sampled part of a related pollution corpus - see "Real-
  world corpus validation" below) went back to those same three files that
  had just been turned into a clean error by the fix above, and asked
  whether that error was actually correct or just the best this project
  could do at the time. It wasn't: all three are a genuine, common,
  parseable shape - a scientific/numeric export where line 1 is a row
  count, not a header (`"868\n0,0.0\n0.0025,0.0992676486197\n..."`), not
  corrupted data. Signal A above can't catch this - the line is a real,
  non-empty value, not padding, so it never qualifies as a candidate under
  that signal's rules. A second, independent signal (also gated behind
  `MAX_PREAMBLE_SCAN`) instead trusts a field-count mismatch between the
  leading row and a *stable* run of what immediately follows - every one
  of the next several rows sharing one consistent field count, not just
  the very next row, specifically so a single coincidentally-matching
  neighbor in an otherwise-genuinely-ragged file can't trigger it (a
  dedicated test locks this in: a body that agrees for two rows and then
  diverges on the third must not fire). Requiring at least 3 corroborating
  body rows before trusting the mismatch is the same "don't act on weak
  corroboration" discipline used elsewhere in this list (compare the
  hash-digest-length note, which never promotes to a real type on shape
  alone). With both signals in place, every one of the 3,712 real files in
  Pollock's own crawled survey now resolves successfully - zero errors,
  zero panics.

- **The JSON reader used to reject the majority of valid JSON documents it
  could have profiled - a top-level array of scalars, or a hand-authored/
  pretty-printed single object spanning multiple lines - with "expected an
  array of objects"/"expected one JSON object per line".** Found via a
  real-world sweep against `nst/JSONTestSuite`, a JSON parser conformance
  corpus (played the same role for the JSON reader this pass that the HPI
  Pollock benchmark played for CSV): only 13 of its 95 valid-JSON test
  files were accepted before this fix. Two distinct, additive gaps, both
  closed without weakening what the reader already correctly rejects
  (`serde_json` itself still does 100% of actual JSON *syntax* validation
  - nothing here second-guesses that):
    1. **Top-level array/stream of non-object values** (`[1, 2, 3]`, or
       one bare ID per line) has no field names to extract as columns, but
       is still a genuine single column of data - the same "no natural
       row-record shape, so treat what's there as one column" choice a
       headerless CSV (`file_no_header.csv`, see above) and NumPy's plain
       1D array (`columns_from_npy`) already make elsewhere in this file.
       `columns_from_json` now checks whether every top-level value is an
       object; if not, the whole set is profiled as a single `"value"`
       column through `profile_json_path` - the exact same recursive
       engine an already-nested array-of-scalars sub-column goes through,
       so a mixed scalar/object array gets the same documented "no partial
       credit" and object-fields-still-recursed treatment at the top level
       that it already got when nested. One real gotcha caught before it
       shipped: `profile_json_path` expects its caller to have already
       filtered nulls out of the `values` it's handed (its own existing
       recursive call site does this) - `unwrap_arrays` only drops nulls
       it finds *inside* a nested array, not from a flat top-level list -
       so the new top-level call site filters nulls itself; skipping that
       would have been a real, reproducible panic on `[1, null, 3]`, not
       a hypothetical, caught by reasoning through the existing contract
       before trusting it rather than after.
    2. **A single JSON document that doesn't start with `[`** (almost
       always an object) was unconditionally treated as JSON Lines mode
       and split by newline - fine for a *compact* single-line object,
       but a pretty-printed one (`{\n  "a": "b"\n}`, the overwhelmingly
       common shape for a hand-authored config or a response saved to
       disk by any JSON-pretty-printing tool) then fails line-by-line,
       since `"{"` alone isn't valid JSON. `read_json_values` now tries
       parsing the *whole* content as one JSON value first - the same
       "whole document = one record" choice TOML and YAML's single-
       mapping mode already make for their own single-document shapes -
       and only falls through to per-line parsing if that fails. This is
       provably a pure fallback, not a competing interpretation: a
       genuine multi-record JSON Lines stream *must* fail the whole-
       content parse, since `serde_json::from_str` rejects trailing
       content after a complete value ("trailing characters" - confirmed
       directly against the crate before relying on it, not assumed).
  With both fixes, `JSONTestSuite`'s valid-JSON files go to 95/95 accepted
  (its invalid-JSON and implementation-defined-edge-case files are
  unaffected - still 186/188 correctly rejected, and 35/35 survive without
  a panic, exactly as before), and a separate real-world sweep against 43
  genuinely nested JSON/JSONL datasets from the RealNest benchmark
  (real-world GitHub Archive events, AWS public blockchain and genomics
  data, OpenStreetMap, cord-19 research-paper parses - up to 14,703
  flattened columns on the deepest one) completed with zero failures and
  zero panics, confirming the recursive flattening engine holds up on
  real production-scale nested payloads, not just synthetic fixtures.

- **The YAML reader had the exact same "must be a mapping" gap the JSON
  reader had, found the same way** - a real-world sweep, this time against
  `yaml/yaml-test-suite` (the YAML spec compliance corpus, playing the
  same role for YAML this pass that JSONTestSuite played for JSON): a
  top-level sequence of scalars, or a bare top-level scalar document, was
  rejected with "expected each YAML document/record to be a mapping" even
  though both are real, valid, unambiguous YAML. Before the fix, only
  137/312 of the suite's valid-YAML cases were accepted; `columns_from_yaml`
  now applies the identical fallback the JSON reader already uses - bridge
  every document/sequence-element to `serde_json::Value` first via the
  existing `yaml_value_to_json`, then check whether *all* of them are
  objects; if not, profile the whole set as one `"value"` column through
  `profile_json_path`, same as a top-level JSON array of scalars. After:
  231/312 accepted (94 more, exactly the count of mapping-shape
  rejections found), zero panics throughout, and the invalid-YAML bucket
  essentially unaffected in spirit - still 72/94 correctly rejected by
  the underlying parser itself. `yaml_document_to_record`, the helper that
  used to enforce the mapping-only rule, is gone entirely rather than kept
  around unused - the mapping check it did is now just one path through
  the shared JSON-shaped dispatch, not a separate gate.

  The remaining gap is real but out of scope: 81 of the suite's valid-YAML
  cases still fail, and a further 22 of its invalid-YAML cases are
  incorrectly accepted - both are the underlying `serde_norway` crate's
  own YAML-1.2-spec-compliance limits (complex mapping keys, certain
  directive/tag combinations, lenient handling of missing whitespace
  before comments, tabs in block context, and similar corners), not a gap
  in this project's own code. Every remaining failure was checked
  individually (error text like `"did not find expected ..."`, `"found
  unknown directive"` - genuine parser-level rejections, not something
  downstream of a successful parse) to confirm this before accepting it
  as a boundary rather than chasing it - the same "delegate real parsing
  to the crate, don't reimplement it" principle behind not writing a CSV
  dialect sniffer or a second JSON validator anywhere else in this file.
  Separately verified against 128 real Kubernetes manifests from the
  ContainerSolutions/kubernetes-examples collection (ConfigMaps, Services,
  NetworkPolicies, Istio resources, and more) - zero failures, zero
  panics, confirming the fix holds on genuine production YAML and not
  just synthetic spec-compliance fixtures.

- **A Parquet/Arrow file with one Map column whose keys aren't strings
  used to fail to read entirely, losing every other column in the file
  along with it - even columns with nothing wrong with them.** Found via a
  real-world sweep against the official `apache/parquet-testing` corpus
  (its `data/` directory: 79 files that should all read cleanly). Root
  cause: `Map<Int32, T>` (or any non-UTF8 key type) is legal Parquet and
  Arrow, but `arrow::json::writer::ArrayWriter` - the crate's own JSON
  writer this project bridges nested columns through - refuses to
  serialize it at all ("Only UTF8 keys supported by JSON MapArray
  Writer"), and the old code ran that writer once over the *entire batch*
  (every nested column together), so one unsupported column's error
  aborted the whole read. `arrow_batch_to_json_rows` now tries that fast,
  whole-batch path first (the common case, unaffected), but on failure
  falls back to converting each nested column through its own isolated
  single-column `ArrayWriter` call - a column that still can't be
  converted gets a clean, disclosed note
  (`"nested content could not be converted for typing: ..."`) on just
  that column, while every other column in the file profiles normally.
  This is the same principle behind every other partial-failure case in
  this file (a "mostly UUID" column still isn't silently promoted, a
  mixed scalar/object array still types its object portion) applied to a
  new axis: one column's *format-level* unsupportability shouldn't cost
  the rest of an otherwise perfectly readable file.

  The same sweep also found a related but separately-caused failure:
  `nested_structs.rust.parquet` failed with `"Invalid timezone \"UTC\":
  only offset based timezones supported without chrono-tz feature"` -
  Arrow's JSON writer needs the IANA timezone database to resolve a
  *named* timezone (as opposed to a raw numeric offset like `+00:00`) on
  a Timestamp column, and this project's `arrow` dependency didn't enable
  that optional feature. Checked before assuming it was expensive
  (`cargo tree` before/after): enabling `chrono-tz` on the `arrow`
  dependency adds exactly three crates (`chrono-tz`, `phf`, `phf_shared`)
  - nothing like the DuckDB or SPSS dependency-weight tradeoffs elsewhere
  in this file - so it was simply turned on rather than worked around.

  The remaining 4 of the 79 `data/` files still fail (`alp_extended.zstd`
  - an experimental encoding this version of the `parquet` crate doesn't
  yet decode; `dict-page-offset-zero` and `large_string_map.brotli` -
  page/buffer-decoding errors; `nation.dict-malformed` - an `EOF: Invalid
  page header`, consistent with its name), and 7 of the 22 intentionally
  adversarial files in `bad_data/` (named after real `apache/arrow-rs`
  GitHub issues - corrupted dictionary sizes, out-of-range offsets,
  oversized primitives) are still accepted rather than rejected. Every
  one of these was confirmed to fail (or succeed) *before* this project's
  own bridging/typing code ever runs - inside the `parquet`/`arrow`
  crate's own metadata parsing or batch decoding - so, same as the
  YAML-1.2-spec-compliance gap above, these are the underlying crate's
  own decoding limits, not this project's, and zero panics were produced
  by any of them either way.

- **Two more real gaps, found the same way, this time for Avro**: a
  real-world sweep combining the Apache Avro project's own interop test
  data with the widely-used "userdata" sample Avro dataset (real-shaped
  synthetic user records - names, emails, IPs, credit card numbers,
  registration timestamps). Before any fix, 10 of 19 files read
  successfully, zero panics.
    1. **Every file compressed with Snappy - Avro's most common
       production codec, especially in Kafka/Hadoop pipelines - failed
       outright** (`"Codec 'snappy' is not supported/enabled"`), including
       all 5 real userdata files and the Apache Avro project's own
       `weather-snappy.avro`/`weather-zstd.avro` interop fixtures (zstd
       failed the same way). `apache-avro`'s `snappy` and `zstandard`
       features weren't enabled. Checked the cost before assuming it was
       expensive, the same discipline the Parquet `chrono-tz` fix above
       used: `zstd` dedupes for free (this project already depends on the
       exact same version, `0.13.3`, for its own top-level `--features
       zstd`), and enabling both together adds exactly two more crates
       (`crc32fast`, `snap`) - cheap enough to simply turn both on.
    2. **A non-record top-level Avro schema was rejected outright** - the
       same "must decode to an object" gap the JSON and YAML readers had,
       found in the exact same corpus: the Apache Avro project's own
       "hello world" RPC interop fixture decodes to the bare string
       `"Hello World"`, not an object, and `columns_from_avro` bailed with
       `"expected each Avro record to decode to an object"` rather than
       accepting it. Fixed with the identical fallback already used for
       JSON/YAML: collect every decoded value first, and if not all of
       them are objects, profile the whole set as one `value` column
       instead of rejecting a real, valid Avro file.
  With both fixes, all 19 files in the combined corpus succeed. Spot-
  checking the real userdata output by hand also confirmed something
  worth recording as a *non*-finding: its `email` column stays plain
  `String` rather than resolving to `Email`, which looked at first like a
  possible gap - checked directly (`is_email` correctly accepts every
  real domain shape in the dataset, including ones with digits and
  hyphens like `163.com`/`t-online.de`/`51.la`) before concluding the
  real cause is a handful of genuinely empty-string email values mixed
  into the column, which correctly veto the whole column under this
  project's existing "no partial credit" rule (see `is_email`'s entry in
  the list above) - the heuristic was already doing exactly the right
  thing, not a bug to fix.

- **MessagePack and CBOR had the exact same "must decode to a map" gap
  JSON, YAML, and Avro all had** - both bridge into the same
  `serde_json::Value` shape those three do, so once the bug class was
  known, checking the other two bridge formats directly (rather than
  assuming it was JSON/YAML/Avro-specific) was the obvious next step, and
  it was there in both. Found with genuine, real-shaped data: a bare
  top-level array of five sensor-reading floats
  (`[23.5, 24.1, 23.8, 22.9, 24.4]`, modeling the IoT/telemetry payloads
  both formats are commonly chosen for specifically because they're
  compact binary encodings) failed outright on both readers with
  `"expected each MessagePack/CBOR record to decode to a map"`, even
  though the identical shape had already been fixed for JSON and YAML.
  Fixed with the same fallback: collect every decoded top-level value
  first, and if not all of them are objects, profile the whole set as one
  `"value"` column via `profile_json_path` (with top-level nulls filtered
  out first, the same precondition every other caller of that function
  already satisfies) instead of rejecting a real, valid file.

  This pass also surfaced a genuine format-specific quirk in the existing
  adversarial coverage, not a new bug: `malformed_garbage.msgpack` (the
  "readable text, wrong structure" convention every other format's
  `malformed_garbage.<ext>` fixture relies on) turns out not to be invalid
  MessagePack at all. MessagePack's positive-fixint encoding defines the
  single-byte range `0x00`-`0x7f` as meaning the integer of that same
  value - byte-for-byte identical to the 7-bit ASCII range, so any plain
  ASCII text is, by construction, already a legal MessagePack value
  stream (a sequence of small non-negative integers: `'t'` = 116, `'h'` =
  104, `'i'` = 105, ...). The fixture's test only ever passed by accident,
  via the unrelated "must decode to a map" bail this fix removes - not
  because the bytes were genuinely malformed. `malformed_msgpack_fails_cleanly`
  now points at a new `malformed_truncated.msgpack` fixture (a `str8`
  header declaring a 200-byte string with only 3 bytes actually supplied -
  a real, verified-to-error truncation) instead, and the original
  fixture's surprising-but-correct decode is its own locked-in test rather
  than an accidental pass. CBOR's own `malformed_garbage.cbor` needed no
  equivalent fix - confirmed, not assumed, that it still fails cleanly
  after this change: CBOR's major-type encoding (the top 3 bits of each
  byte select the type) doesn't share MessagePack's coincidental overlap
  with printable ASCII.

- **RFC 2822 dates with a literal named zone (`"GMT"`) instead of a
  numeric offset were left as plain `String`.** Found via a real-world
  sweep of live RSS feeds: BBC News's `<pubDate>` field uses
  `"Mon, 15 Jan 2024 10:00:00 GMT"`, not the `+0000` form the existing
  RFC 2822 entry already handled - and this isn't an outlier convention,
  it's what RFC 7231's own HTTP `Date`-header "IMF-fixdate" grammar
  mandates. Confirmed directly before assuming it: chrono's `%z` rejects
  `"GMT"` outright ("input contains invalid characters" - it's a named
  zone, not a numeric offset, and `%Z` is display-only in chrono, not
  reliable for parsing), but a literal `"GMT"` written directly into the
  format string matches it as plain text, which is exactly correct here
  since GMT's offset is always zero - nothing is lost treating the result
  as naive. A new `DATE_FORMATS` entry
  (`"%a, %d %b %Y %H:%M:%S GMT"`) handles it, kept as a separate entry
  from the numeric-offset RFC 2822 form rather than trying to make one
  format string cover both (a column mixing the two shapes correctly
  matches neither, the same "no partial credit" rule the rest of this
  file already applies everywhere else - confirmed by a dedicated test).
  Two of the three real feeds swept (NASA, Hacker News) already used the
  numeric-offset form and needed no fix; only BBC's did - a reminder that
  "found in one real source out of three" is still worth fixing when the
  source in question is backed by the actual HTTP spec, not treated as
  too narrow a sample to act on.

- **A native Excel date/datetime cell was silently rendered as a
  meaningless raw integer (Excel's own internal day-count serial, e.g.
  `44652`) instead of a date - the single most impactful bug this whole
  real-world-testing effort found, because date columns are close to
  ubiquitous in real spreadsheets.** Found via a real-world sweep against
  three genuinely real `.xlsx` files (a cyclone-tracking dataset,
  Microsoft's own "Financial Sample" demo workbook, Kaggle's
  `MessyData.xlsx`) - every single one of them had at least one date
  column affected. `type_detection.xlsx`'s own `signup_date` column never
  caught this, because it was authored as a plain date-*shaped string*
  rather than a genuine native Excel date cell - a real gap between what
  the committed fixture tested and what the format actually produces in
  practice, not just a missing assertion. Root cause, confirmed directly
  against `calamine`'s own source rather than assumed: a date-formatted
  cell decodes to `calamine::Data::DateTime(ExcelDateTime)`, but that
  type's own `Display` impl is `write!(f, "{}", self.value)` - `self.value`
  being the *unresolved* numeric serial, not a calendar date - and this
  project's Excel reader was stringifying every cell via that same
  generic `Display` path (`cell.to_string()`), scalar and date-typed
  alike. `calamine` itself already carries the fix - `.as_datetime()`
  correctly resolves the serial to a real `chrono::NaiveDateTime` - but
  it's gated behind `calamine`'s own optional `chrono` Cargo feature,
  which this project hadn't enabled (checked the cost first, the same
  discipline as the Parquet `chrono-tz` and Avro codec dependency
  decisions above: exactly 2 more lines in `cargo tree`, since this
  project's own `chrono` dependency already satisfies calamine's version
  requirement and dedupes for free). `xlsx_cell_to_string` now checks
  whether calamine itself already flagged the cell as a date/time
  (`is_datetime`/`is_datetime_iso`) before resolving it - deliberately
  never applied to every numeric cell, so a plain integer column (a
  serial number, an ID) is never reinterpreted as a date just because its
  magnitude happens to look like one. Time-of-day is dropped from the
  rendered string only when it's exactly midnight, the closest available
  signal to "this cell's own format had no time component" that
  `calamine`'s resolved value actually exposes. Verified this was a real,
  regression-catchable fix rather than assumed: temporarily reverting to
  the old `cell.to_string()` call reproduced the exact original bug
  (raw serials `"45306"`/`"45306.4375"`) against the new
  `edge_xlsx_native_date_cells.xlsx` fixture (written with real
  `datetime.date`/`datetime.datetime` values via `openpyxl`, specifically
  because a string-based fixture can't exercise this code path at all)
  before the fix was restored.

- **A single `.npz` array with an unsupported shape used to abort every
  other array in the same archive, not just its own.** Found via a
  real-world sweep against TensorFlow's own MNIST dataset (the actual
  `mnist.npz` its own `tf.keras.datasets.mnist` API downloads): `x_train`/
  `x_test` are genuine 3-D image arrays (`(60000, 28, 28)`/
  `(10000, 28, 28)`) - correctly and deliberately rejected, per this
  file's own already-documented "anything higher-dimensional is a clear
  error rather than a guessed flattening" boundary, not a new bug - but
  `y_train`/`y_test` in that *exact same file* are perfectly ordinary 1-D
  label arrays that should read cleanly. Before this fix, hitting the
  first unreadable array (`columns_from_npz`'s loop propagated its error
  immediately via `?`) meant the whole archive read failed, so the two
  perfectly good label arrays never got profiled either - a real user
  pointing this tool at a real, extremely common ML dataset shape would
  have gotten nothing at all, not a partial, honest answer. Each array's
  read is now caught independently; one that fails gets a single
  disclosed placeholder column on its own table
  (`"array 'x_train' could not be profiled: ..."`) instead of taking
  every other array down with it - the same "one bad part shouldn't sink
  everything else" principle already applied to a single unconvertible
  nested Parquet/Arrow column above. `tests/fixtures/
  edge_npz_mixed_readable_and_unreadable.npz` (a genuinely 3-D array
  alongside an ordinary 1-D one, generated directly with `numpy` -
  mirroring MNIST's own shape at fixture scale) and its integration test
  lock this in.

- **RFC 3164 syslog required a `<PRI>` prefix that virtually no real
  on-disk syslog file actually has, and its `tag` grammar rejected any
  program name containing a space.** Found via a real-world sweep against
  `loghub`'s `Linux_2k.log` - a genuine excerpt of an actual production
  `/var/log/messages`-style file, not synthetic data. Two distinct gaps
  in the same 1,999-line file:
    1. **PRI.** RFC 3164 nominally includes `<PRI>` as part of every
       message, but PRI is fundamentally a wire-protocol artifact - it's
       how a receiving syslog daemon knows the sender's facility/severity
       over the network. The *local* syslog daemon writing its own file
       to disk (the overwhelming majority of real syslog data anyone
       would actually want to profile) has no reason to include it, and
       doesn't. The original regex made `<PRI>` mandatory, so it rejected
       essentially the single most common real-world shape this format
       appears in - confirmed directly against the real file, not
       theorized. `syslog_regex` now wraps `<PRI>` in a non-capturing
       optional group; when it's absent, `facility`/`severity` correctly
       come out as missing rather than a guessed value - the same
       "disclose the gap, don't guess" rule applied everywhere else in
       this file, just triggered by an absent field instead of an
       ambiguous one.
    2. **The tag grammar.** The same real file has a recurring line
       (7 times in fewer than 2,000 lines - not a one-off, since it's
       `sysklogd`'s own hardcoded restart announcement, which fires on
       every daemon restart): `"syslogd 1.4.1: restart."`. RFC 3164's
       `TAG` field is conventionally a single no-whitespace program name,
       and the old regex enforced exactly that - but here the "tag" is a
       program name *and version* separated by a space. Changed the tag
       capture from a whitespace-excluding character class to a
       non-greedy one that only excludes `:`/`[` (letting it span
       whitespace, matched non-greedily so it still stops at the
       earliest valid `tag[pid]:`/`tag:` boundary rather than swallowing
       message content). This is a real precision/coverage trade-off,
       disclosed rather than hidden: a genuinely malformed line with no
       colon anywhere still correctly fails to match at all (verified
       against the existing `syslog_line_that_does_not_match_the_grammar_
       is_an_actionable_error` test, unchanged), but a line with *some*
       colon somewhere now has a wider space of tag interpretations than
       before. Checked empirically before trusting it, the same
       discipline as everywhere else in this file: ran the *entire* real
       2,000-line file through the loosened grammar and read every
       resulting `tag` value by hand, not just confirmed it didn't error
       - 29 of 30 sampled values were unambiguously correct real program
         names (`sshd(pam_unix)`, `logrotate`, `syslogd 1.4.1`, `kernel`,
       `irqbalance`, ...); the one exception (a line with a genuinely
       missing tag field in the *source* log itself - a double space
       where a real program identifier should have been) reflects
       already-ambiguous source data, not a parsing defect introduced by
       the looser grammar.
  Separately, the same pass ran a real, unrelated 10,000-line Apache
  access log (Elastic's own published example dataset) through
  `--format combined-log` and found zero gaps in the *existing* reader -
  every field (IP, timestamp, method, status, referer as `URL`) resolved
  correctly, and the one line that failed to parse turned out to be
  genuine data corruption in the source file itself (a literal missing
  closing quote mid-line, confirmed by reading the raw bytes) - exactly
  the "hard error naming the line" contract this project already
  guarantees, working as intended on a real corrupted line, not a gap.

- **`"On"`/`"Off"` - a common real INI/config boolean convention (PHP's
  own directive style in `php.ini`, also seen in Apache and
  Windows-style configs) - wasn't recognized as boolean at all.** Found
  via a real-world sweep against `php.ini-production`, the actual file
  shipped and deployed as-is on countless real PHP servers: every
  `On`/`Off` directive (`engine = On`, `display_errors = Off`, dozens
  more) resolved to a plain untyped `enum / category` instead of `bool`,
  since `is_bool_word` only recognized `true`/`false`/`yes`/`no`/`y`/`n`.
  Added `on`/`off` to the same set - zero new ambiguity risk, since
  nothing else in this file's type-detection order would ever claim
  those two words for something else. A real, unrelated Samba
  `smb.conf` swept in the same pass needed no fix - its own boolean
  convention (`yes`/`no`) already worked.

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
   report with a known layout), see `weblog_support`/`syslog_support` for
   the pattern: one hand-rolled forward-scanning parser per variant (no
   `regex` dependency needed for a small, fixed number of known grammars -
   see the Dependency footprint section for why and how this was verified
   against the real `regex` crate before trusting it), split any compound
   fields into their own columns, decode any packed numeric codes against
   the format's own fixed lookup table rather than leaving them opaque,
   and hard-error with the line number on a line that doesn't match rather
   than skip or misparse it.
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
flattening and dictionary-encoding resolution, Avro's logical-type
resolution (`date`/`timestamp-millis`/`timestamp-micros`/decimal,
including decimal nested inside a record/array/nullable union - see the
Cloud-platform file compatibility section), Excel's does-lose-data case
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

**Every format also has at least one test proving a precise-grammar
semantic type (UUID/Email/IPv4/date, not just a bare scalar shape) survives
that format's own reader.** `suggest_ideal_type` itself only needs proving
once (CSV's `type_detection.csv`/`adversarial.csv` and JSON's
`nested_typed.jsonl` already do this exhaustively, since the function is
format-agnostic - it only ever sees raw strings), so the risk this layer
actually covers is narrower and more concrete: does *this specific format's
reader* hand a value to that engine unmangled? That's a real, format-
specific failure mode this project has hit before (Excel silently turning
a leading-zero zip code into a number is exactly this class of bug, for a
different heuristic) - and before this layer existed, roughly half the
format readers (Parquet, Arrow IPC, Avro, SQLite, MessagePack, TOML, INI,
CBOR, NumPy, dBase, Stata, fixed-width text, and the log formats) had never
had a single assertion checking anything beyond `current_type`/column shape.
`tests/fixtures/type_detection.<ext>` (one per format, generated with
pandas/pyarrow/fastavro/openpyxl/dbf/etc. and each verified by hand against
the compiled binary before being trusted) carries the same five columns -
`id`/`user_uuid`/`contact_email`/`ip_address`/`signup_date` - so expectations
line up across formats; dBase's copy alone renames the three longest to fit
its own 10-character field-name limit. Common/Combined Log's `host` and
`referer` columns needed no new fixture at all - the existing sample logs
already carry real IPv4/URL data, just previously unasserted. This layer
also caught a genuine, previously-unverified interaction between two
already-documented design decisions: RFC 5424 permits either a literal `Z`
suffix or an explicit numeric offset on its timestamp, and `DATE_FORMATS`
requires one single candidate format to match every value in a column - so
`sample_rfc5424.log`'s existing fixture (which mixes both forms across its
3 lines, per the RFC's own canonical example) correctly leaves `timestamp`
as a plain `String` rather than forcing a guess, verified directly against
chrono rather than assumed; a second, uniformly-`Z`-formatted fixture
(`edge_rfc5424_uniform_timestamps.log`) proves the positive case still
resolves correctly when a real sender's format is actually consistent.
Arrow IPC/Feather additionally went from zero real read coverage (only
feature-wiring was checked, since Parquet already proved the shared
`profile_arrow_batches` path) to a genuine `.arrow` fixture.

The `#[cfg(test)] mod tests` block in `lib.rs` additionally has an
adversarial/robustness section (see the design-philosophy note above) that
every validator must survive without panicking - hostile unicode, control
characters, injection-style payloads, extreme length - plus proof that
every checksum genuinely discriminates near-miss values rather than just
checking shape.

`tests/integration.rs` mirrors this at the full-pipeline level (reader +
heuristics + renderer together, via the compiled binary) with its own
adversarial section, backed by two kinds of dedicated fixtures:
`tests/fixtures/adversarial.csv` packs one column per semantic type with
values deliberately corrupted away from it (a UUID one character short, a
credit card with its Luhn digit tampered, an IPv4 octet of 256, a "mostly
UUID" column with one non-UUID value mixed in, SQL/shell/template-
injection-style payloads, heavy unicode/emoji/CJK/zero-width-space
content) plus the two silent-data-loss cases described above (a literal
"infinity"/"NaN" value, a digit string beyond i64's range) - every single
column is asserted to *not* resolve to the type it was corrupted away
from, and the two loss cases are asserted to carry their explicit notes.
A family of `tests/fixtures/malformed_*` files (an empty file, a header
with zero data rows, an all-whitespace file, a UTF-8 BOM-prefixed file, a
file with a ragged/inconsistent column count, a file with genuinely
invalid UTF-8 bytes, and a 200-levels-deep nested JSON document) each get
a test asserting the tool either produces sane output or fails with a
clean, actionable error naming the actual problem - never a panic, and
for the deeply-nested JSON case specifically, never a stack overflow
(serde_json's own built-in recursion limit is what actually protects
`profile_json_path`'s own recursive flattening here, confirmed rather
than assumed). A matching `malformed_deeply_nested.xml` fixture covers the
same adversarial shape for XML - the one nested format that needed an
actual code fix (`xml_nesting_too_deep`) rather than just a test locking in
already-correct behavior, since `xmltree` has no recursion guard of its own
(see the design philosophy section above for how this was found).

Every other format also has its own malformed-input test now (CSV/JSON's
own dedicated fixtures above predate this and are more varied; every other
format gets one `tests/fixtures/malformed_garbage.<ext>` fixture -
readable text with the right extension but none of the real format's
structure - and one `#[cfg(feature = "...")]`-gated test asserting a
clean, actionable error rather than a panic, via the shared
`assert_fails_without_panicking` helper). This was verified empirically
against every format *before* being written up as a test, not assumed:
feeding each reader random garbage, and separately a truncated-halfway
copy of a real fixture, confirmed every one of them already propagates
the underlying crate's own error through `?`/`with_context` rather than
unwrapping - so none of this needed a code fix, only the coverage locking
it in. One genuinely interesting, non-obvious finding from that
verification: TOML/YAML/INI can *fail to error at all* on a truncated
file if the cut happens to land at a syntactically valid boundary (e.g.
right after a complete `key = value` line) - not a bug, just how lenient
plain-text formats degrade, the same way a CSV truncated after a complete
row also "just works" with fewer rows.

A third `tests/fixtures/edge_*` family covers structurally *valid* but
degenerate input, as opposed to malformed/garbage: a zero-row Parquet
file (schema present, no data), a zero-record Avro file, a SQLite table
with no rows, an Excel workbook with a header-only sheet plus a separate
sheet with unicode content (café/日本語) in the same file, an XML document
whose root element has no children (a clean "nothing to profile" error,
not a crash) alongside a separate unicode-content XML document that does
work, a zero-length NumPy array, a JSON `[]`/`{}` top-level value, a JSON
field that's `null` in every record, and a genuinely zero-byte file for
every format lenient enough to treat that as "zero records" (MessagePack/
CBOR/TOML/YAML) versus INI, whose own reader treats zero sections as an
error instead - a real, documented difference between formats' own
conventions, not an inconsistency in this tool. Rounding out the same
family: a zero-byte fixed-width file (a hard, correct error - there's no
header to derive column meaning from even with `--widths` given, unlike
every other empty-file case here which at least has a schema or fixed
column set to fall back on), a gzip file whose *decompressed* content is
itself zero bytes (transparent decompression still applies before the
empty-CSV case is reached), and empty Common Log/syslog files (both fall
back to their own fixed, format-defined column set, same shape as the
header-only-CSV case). Every case here was verified empirically first,
the same as the malformed-input family: none of it needed a code fix, all
of it was already handled correctly and simply lacked a test locking the
behavior in.

**Real-world corpus validation** is a distinct exercise from the fixture-
based testing above: rather than synthetic or hand-crafted files, this ran
the compiled binary against large *external* corpora of real, messy data no
one on this project authored, specifically to check for crashes and
correctness gaps that only show up at real-world scale and variety. Two
corpora, neither committed to the repo (both are large, externally-hosted,
and not this project's to redistribute - the fixtures the findings produced
*are* committed, see below): the HPI Pollock data-loading benchmark's
`survey/csv` directory (3,712 real CSVs crawled from government/open-data
portals as part of their own published study of 245,000+ files' RFC-4180
violations) and the public, 35,000-row "Ask A Manager" salary survey (real
human-entered free text - job titles, locations, currencies, salaries with
thousands separators).

The first pass through Pollock only sampled part of its second corpus -
`polluted_files/csv`, 2,290 synthetic structural-pollution files - testing
just the 21 unique whole-*file*-level pollution variants (delimiter/quote/
escape character changed, multi-row headers, a preamble, a multi-table
file, no header, an empty file, CRLF/CR/LF-only line endings, no trailing
newline) plus the unmodified `source.csv`, on the mistaken assumption that
the remaining 2,268 files were near-duplicate variants of one already-
covered shape. They weren't: that remainder is actually three more
pollution families entirely (`row_more_sep_row*`/`row_less_sep_row*`/
`row_field_delimiter_*`, each injecting a ragged row at every possible
row/column position in the base file), never sampled or even enumerated in
the first pass. A corrected, genuinely exhaustive run covers all 2,290
files: zero panics, 260 parse successfully, and 2,030 fail with a clean,
actionable `found record with N fields, but the header has M fields`
error. That high error rate is the *correct* outcome, not a gap - these
three families are Pollock's own adversarial fuzzing, deliberately
injecting a single ragged row at a random position in an otherwise-clean
84-row file specifically to test whether a system correctly *rejects*
genuinely inconsistent data. Silently tolerating them would mean guessing
which of two shapes is right with no structural basis for the guess -
exactly what this project refuses to do everywhere else in this file.

The `survey/csv` corpus (all 3,712 files, run exhaustively both times) is a
better test of legitimate real-world messiness rather than deliberate
fuzzing: every one of the 3,712 now resolves successfully - zero errors,
zero panics. That wasn't true on the first pass (3,709 succeeded, 3 failed
with a clean ragged-row error) - going back and asking whether that
error was actually the right answer, rather than treating "clean error,
no panic" as good enough, is what led to the second preamble-detection
signal documented above. Pollock's 21 unique whole-file pollution variants
also produced zero crashes on every pass - delimiter/quote/escape
mismatches correctly surface as a clean ragged-row error rather than a
silent misparse, exactly as documented above (this tool doesn't auto-sniff
CSV dialect; `--delimiter` is the escape hatch).

This pass is what found all three real bugs described in the preamble-
detection entries above (the previously-undetected header-swallowed-by-
banner-row behavior, the seek-poisoning ragged-row panic introduced while
fixing it, and the row-count-line-misread-as-ragged-data gap found by
going back and re-examining what "clean error" was actually hiding) - real-
world testing here served the same role adversarial fixture testing
already serves elsewhere in this file: surfacing failures no one would
have thought to write a synthetic test for in advance, and - just as
importantly - surfacing the gap in the *validation methodology itself*
(an unverified assumption about which files a large external corpus
contains) the same way this file's adversarial-testing discipline exists
to surface gaps in the *product*. `tests/fixtures/preamble.csv`,
`tests/fixtures/row_count_preamble.csv`, and their five integration tests
distill both findings into small, permanent, committed regression tests,
so the external corpora themselves don't need to be present for `cargo
test` to keep proving the fixes hold.

The same exercise was repeated for the JSON reader, against two more
external corpora, neither committed for the same redistribution reason as
above: `nst/JSONTestSuite` (318 files - 95 valid-JSON files that a
compliant parser must accept, 188 invalid-JSON files it must reject, 35
implementation-defined edge cases where either outcome is acceptable but a
crash never is) and 43 real nested JSON/JSONL tables from the RealNest
benchmark's 1024-row sample data (GitHub Archive events, AWS public
blockchain and genomics data, OpenStreetMap, cord-19 research-paper
parses). Before the two fixes described in the design philosophy section
above, only 13/95 valid files were accepted; after, 95/95, with the
invalid/edge-case buckets unaffected (186/188 rejected, 35/35 survive) and
zero panics throughout. The RealNest sweep found zero failures on any of
the 43 real tables, up to 14,703 flattened columns on the deepest one
(`cord-19-document_parses`), confirming the recursive flattening engine
holds up on real production-scale nested payloads rather than just
synthetic fixtures - spot-checking one output by hand
(`gharchive-PushEvent`) also showed the semantic-type heuristics firing
correctly on genuine production API data for the first time in testing
(`commits.sha` correctly flagged as matching SHA-1 hex-digest length,
`commits.url` resolved to `URL`). `tests/fixtures/
edge_pretty_printed_single_object.json`,
`tests/fixtures/edge_top_level_scalar_array.json`, and their own
integration tests (plus five `#[cfg(test)]` unit tests directly on
`read_json_values`/`columns_from_json`) lock both JSON fixes in
permanently, the same pattern as the CSV findings above.

The same exercise, a third time, for the YAML reader: `yaml/yaml-test-suite`
(406 test cases extracted from its own meta-YAML format via a small
one-off Python script, not committed - see the design philosophy section
above for the exact before/after numbers) played the role JSONTestSuite
played for JSON, and found the identical class of gap. Separately, 128
real Kubernetes manifests from the `ContainerSolutions/kubernetes-examples`
collection - zero failures, zero panics. One genuinely interesting
methodology wrinkle surfaced along the way, worth recording since it's a
trap any future real-world-corpus pass could fall into again: a plain
sweep script built around `while IFS= read -r f; do ...; done < file_list`
(or the equivalent with a bash array populated via process substitution)
intermittently hung indefinitely partway through the k8s manifest sweep in
this environment, with zero panics, zero errors, and zero progress output
- not a sniff-rs bug (confirmed by then re-running the same 128 files
through a small Python `subprocess.run(..., timeout=10)` wrapper instead,
which completed cleanly with zero failures - the per-call timeout alone
is proof no individual invocation ever actually hung). The likely cause
was an interaction between this session's shell-command hook tooling and
a long-running loop construct, not anything YAML- or sniff-rs-specific,
but the lesson generalizes: prefer a subprocess-based sweep script with an
explicit per-file timeout over a raw shell loop for any future exhaustive
corpus pass, since it's both more reliable *and* gives a genuine hang a
way to be caught and reported instead of silently stalling the whole
sweep. Fixing the YAML gap also surfaced a stale test assumption, not a
product bug: `tests/fixtures/malformed_garbage.yaml` (a hand-written
sentence of plain prose, meant to simulate "readable text, no real
structure") was quietly valid YAML all along - any single unquoted line
is a legal YAML plain scalar - so it only ever passed
`malformed_yaml_fails_cleanly` as a side effect of the mapping-only gate
this pass removed. Replaced with genuinely invalid YAML (an unterminated
quoted scalar), verified directly against `serde_norway` before trusting
it, rather than loosening the test's own intent.
`tests/fixtures/edge_yaml_scalar_sequence.yaml` and its own integration
test, plus three `#[cfg(test)]` unit tests on `columns_from_yaml`, lock
the YAML fix in the same way.

A fourth pass, for Parquet/Arrow: the official `apache/parquet-testing`
corpus (79 files in `data/` that should all read cleanly, 22 deliberately
adversarial files in `bad_data/` named after real `apache/arrow-rs`
GitHub issues). Before any fix: 72/79 good files read successfully, zero
panics on either directory. After the two fixes described in the design
philosophy section above (per-column JSON-conversion isolation, and
enabling `chrono-tz`): 75/79, with the remaining 4 (plus 7 of the 22
adversarial files still being accepted rather than rejected) confirmed to
be limits of the underlying `parquet`/`arrow` crate's own metadata/batch
decoding, not this project's code - the same "delegate real parsing to
the crate" boundary the YAML pass drew. `tests/fixtures/
edge_map_non_string_key.parquet` and `tests/fixtures/edge_named_timezone.parquet`
(both generated with `pyarrow`, matching this project's own established
fixture-generation convention rather than vendoring a third-party file
even though `parquet-testing` is Apache-2.0 licensed and permits it) and
their integration tests lock both fixes in - the named-timezone fixture's
test was added in a follow-up coverage audit after the original pass
verified the fix manually but hadn't yet turned it into a committed
regression test; its own comment records that the fix was re-confirmed by
temporarily reverting `chrono-tz` and rebuilding before the test was
trusted, not just assumed correct from the earlier manual check.

A fifth pass, for Avro: the Apache Avro project's own interop test data
(`share/test/` in the `apache/avro` repo - weather data in several
compression codecs, RPC request/response fixtures) combined with the
`userdata1-5.avro` files from Teradata/kylo's sample-data collection - a
widely-reused, real-shaped synthetic dataset (names, emails, IPs, credit
cards, timestamps) rather than something built for this project. Before
any fix: 10/19 files read successfully, zero panics. After the two fixes
described in the design philosophy section above (Snappy/zstd codec
support, non-record top-level schemas): 19/19, and the same real-userdata
sweep is also what surfaced the `is_email` non-finding documented there -
checking a heuristic's output against real data and confirming it's
already correct is as much a part of this validation pass as finding
things that are actually broken. `tests/fixtures/edge_avro_snappy_codec.avro`
and `tests/fixtures/edge_avro_scalar_records.avro` (both generated with
`fastavro`, matching this project's fixture-generation convention) plus
two integration tests lock both fixes in.

A follow-up coverage audit across all five passes above (prompted by an
explicit request to double-check every finding actually got a committed
regression test, not just a fix and a manual verification) closed two
more real gaps it found: the Parquet named-timezone fix (see above) had
been verified by hand but never turned into a test, and the JSON reader's
zero-byte-file behavior (`n_structure_no_data.json`/`n_single_space.json`
from the JSONTestSuite sweep both being treated as zero records, matching
every other lenient format) had been confirmed correct during that sweep
but was the one format in that family without a committed
`edge_empty_doc.<ext>` test locking it in - `json_zero_byte_file_produces_an_empty_table_not_a_crash`
closes that gap alongside `msgpack`/`cbor`/`toml`/`yaml`'s existing
equivalents. A third addition, `is_email_accepts_real_domains_with_digits_and_hyphens`,
turns the Avro pass's `is_email` non-finding from a claim in this document
into something a future change to that function would actually have to
break a test to get wrong.

A sixth pass covered two more formats with clean results. **SQLite**:
two well-known real sample databases, Chinook (a ~1MB digital-media-store
schema, 11 tables) and a SQLite port of Northwind (~25MB, 13 tables).
Both read correctly on the first try - real email/date/URL columns
resolved to their semantic types, a genuine SQLite type-affinity mix
(Northwind's `Order Details.UnitPrice` storing both integer and float
values interchangeably) was correctly reported as `mixed(...)`, and a
real table name containing a space (`"Order Details"`) round-tripped
without incident. The one thing worth locking in rather than just
confirming by hand: Northwind ships 18 real SQL Server-style VIEWs
(several also space-named) alongside its 13 real tables, and none of
them leaked into the output - `columns_from_sqlite`'s own
`WHERE type='table'` query already excludes them structurally, but
nothing had ever tested that a real database with real views actually
exercises this correctly. `tests/fixtures/edge_sqlite_view_excluded.sqlite`
(a table plus a view built from it, both space-named) and
`sqlite_excludes_views_and_handles_table_names_with_spaces` now do.

**TOML**: the official `toml-lang/toml-test` conformance suite (268
files that must be accepted, 509 that must be rejected) plus 41 real
`Cargo.toml` files pulled from this machine's own crates.io registry
cache (including this project's own). Result: 268/268 valid files
accepted, 41/41 real files accepted, zero panics anywhere. 500 of the
509 invalid files were correctly rejected; the remaining 9 (a datetime
missing seconds, a trailing comma in an inline table, an invalid `\x`
string escape) were checked individually and confirmed to be the
underlying `toml`/`toml_edit` crate's own parsing leniency - the exact
same "delegate real parsing to the crate, don't second-guess it" boundary
already drawn for YAML above, not a gap in this project's code. No fix
was needed for TOML at all - the cleanest result of any format pass in
this document, and worth recording as such rather than only writing up
passes that found something broken.

A seventh pass, for XML: six genuinely real files rather than a
synthetic or crawled corpus - live RSS feeds from BBC News, NASA, and
Hacker News, a real, large Maven `pom.xml` (Apache Maven's own, 244
flattened columns, nesting seven levels deep through real plugin
configuration), a real sitemap.xml, and a real SVG icon. All six read
correctly with zero panics - deeply nested real configuration, mixed
scalar/object arrays (BBC's RSS mixes a plain-text `<link>` with an
Atom-namespaced `<link href="..."/>` under the same flattened name,
correctly triggering the documented "no partial credit" mixed-array
fallback), and genuine multi-dot attribute names (Maven's own
`combine.children` inheritance directive) all handled without incident.
One real, valuable gap this pass did find: see
`matching_date_format_recognizes_rfc2822_with_literal_gmt_zone` in the
design philosophy section below.

An eighth pass, for Excel: three genuinely real `.xlsx` files (a
cyclone-tracking dataset, Microsoft's own "Financial Sample" demo
workbook, and Kaggle's `MessyData.xlsx`). All three read successfully
with zero panics - but every single one of them exposed the same real,
high-impact bug: a native Excel date/datetime column silently rendered
as a meaningless raw day-count serial (`44652`) instead of a date. See
the `xlsx_cell_to_string`/`ExcelDateTime` entry in the design philosophy
section above for the full root-cause and fix writeup - it's flagged
there as the single most impactful finding of this entire real-world
validation effort, since date columns are close to ubiquitous in real
spreadsheets and every one of them was affected before the fix.
`tests/fixtures/edge_xlsx_native_date_cells.xlsx` (written with real
Python `datetime` values via `openpyxl`, deliberately not the
date-shaped-string approach `type_detection.xlsx`'s existing
`signup_date` column already used, which is exactly why that fixture
never caught this) and its integration test lock the fix in.

A ninth pass, for NumPy: TensorFlow's own real MNIST `.npz` (the exact
file `tf.keras.datasets.mnist.load_data()` downloads) alongside four
more real files generated directly from scikit-learn's bundled real-
world datasets (Iris as a structured/record-dtype `.npy`, Diabetes as a
plain 2-D `.npy`, Wine as an `.npz`, California Housing as a larger
~20,000-row `.npy`). All five reads succeed with zero panics - and one
real gap this pass found: see the `.npz`-per-array-isolation entry in
the design philosophy section above. MNIST's own two label arrays
(`y_train`/`y_test`) went from "never profiled at all, because the
archive's other two arrays are 3-D images" to profiling normally
alongside a clear, disclosed explanation for the two that still
correctly can't be.

A tenth pass covered the log formats: `loghub`'s real `Linux_2k.log` (a
genuine excerpt of an actual production `/var/log/messages`) for
`--format syslog`, and a real 10,000-line Apache access log (Elastic's
own published example dataset) for `--format combined-log`. The access
log needed no fix at all - every field resolved correctly, and its one
parse failure was confirmed to be genuine corruption already present in
the source file. The syslog file found two real gaps; see the `<PRI>`-
optional and tag-grammar entries in the design philosophy section above
for the full writeup, including the empirical check (reading every
resulting `tag` value from the *entire* real file by hand, not just
confirming it didn't error) behind trusting the loosened grammar.
`tests/fixtures/sample_rfc3164_no_pri.log` (three lines lifted directly
from the real file's own shapes: a PRI-less line with `[pid]`, the
recurring `"syslogd 1.4.1: restart."` space-containing-tag line, and a
kernel-style message with embedded brackets/colons that must not be
mistaken for tag/pid structure of its own) and its integration test lock
both fixes in together.

An eleventh pass covered three more formats. **Stata**: three real,
official Stata-press example datasets (`auto.dta`, the canonical
teaching dataset; `census.dta`, real US state-level demographic data;
`nlswork.dta`, a genuinely large ~28,000-row longitudinal survey with
missing-value rates up to 32.6% on some columns) - all three read
correctly with zero panics and zero fixes needed. **dBase**: a real US
Census Bureau TIGER/Line shapefile's `.dbf` component (56 US states/
territories, extracted from the actual ZIP the Census Bureau publishes)
- also zero panics, zero fixes needed; FIPS codes correctly flagged for
leading zeros, land/water area columns correctly showing the
declared-`f64`-but-really-`i64` gap this project's dBase reader is
specifically built to surface. **INI**: `php.ini-production` (the real,
1,878-line file PHP ships and countless servers deploy verbatim) and a
real Samba `smb.conf` - one real gap found; see the `on`/`off` boolean
entry in the design philosophy section above.

A twelfth pass covered MessagePack and CBOR, found via the same reasoning
that motivated every other bridge-format check in this project: both
formats decode into a `serde_json::Value`-shaped tree the same way JSON,
YAML, and Avro do, so a structural bug found in one of those readers'
"must decode to an object" dispatch logic was worth checking for in the
other two directly, rather than assuming it was JSON/YAML/Avro-specific.
It was there. Confirmed with genuine, real-shaped data rather than a
synthetic edge case: a bare top-level array of five sensor-reading floats
(`[23.5, 24.1, 23.8, 22.9, 24.4]`, generated with Python's `msgpack` and
`cbor2` libraries, modeling the IoT/telemetry payloads both formats are
commonly chosen for specifically because they're compact binary
encodings) failed outright on both readers with `"expected each
MessagePack/CBOR record to decode to a map"`, even though the exact same
shape - a top-level array of scalars - had already been fixed for JSON
and YAML. Both `columns_from_msgpack` and `columns_from_cbor` now apply
the identical fallback: if every top-level value is a JSON object, profile
as records as before; otherwise profile the whole set as a single
`"value"` column via `profile_json_path`, with top-level nulls filtered
out first (the same precondition `profile_json_path` already requires of
every other caller). Locked in with `tests/fixtures/
edge_msgpack_scalar_array.msgpack` / `edge_cbor_scalar_array.cbor`
(the same five-float sensor-reading data used to find the bug, generated
directly into the fixtures directory rather than left as a scratch file).

This pass also surfaced a genuine, format-specific quirk in the existing
adversarial coverage, not a new bug: `malformed_garbage.msgpack` (the
same "readable text, wrong structure" convention every other format's
`malformed_garbage.<ext>` fixture uses) turns out to *not* be invalid
MessagePack at all. MessagePack's positive-fixint encoding defines the
single-byte range `0x00`-`0x7f` as meaning the integer of that same
value - which is byte-for-byte identical to the 7-bit ASCII range, so any
plain ASCII text is, by construction, already a legal MessagePack value
stream: a sequence of small non-negative integers (`'t'` = 116, `'h'` =
104, `'i'` = 105, ...). Before this pass's fix, the fixture's test only
passed by accident, via the unrelated "must decode to a map" bail this
pass just removed - not because the bytes were actually malformed. Now
that a non-map top-level stream correctly falls back to a `"value"`
column instead of erroring, the fixture decodes successfully and
correctly, so `malformed_msgpack_fails_cleanly` was repointed at a new
`tests/fixtures/malformed_truncated.msgpack` (a `str8` header declaring a
200-byte string with only 3 bytes actually supplied - a genuine,
verified-to-error truncation), and the previous fixture's real, surprising
behavior is now its own locked-in test,
`msgpack_ascii_garbage_text_decodes_as_a_stream_of_small_integers`, rather
than an accidental pass. CBOR's own `malformed_garbage.cbor` needed no
equivalent fix - CBOR's major-type encoding (top 3 bits of each byte) does
not have the same overlap with printable ASCII, confirmed by checking that
fixture still fails cleanly after this pass's fix, not assumed from the
MessagePack finding.

The crate is a lib (`src/lib.rs`) plus a thin binary (`src/main.rs` that
just calls `sniff_rs::run()`), so besides the black-box integration tests
there's also a `#[cfg(test)] mod tests` at the bottom of `lib.rs`
unit-testing the heuristic functions directly (`suggest_ideal_type`,
`has_leading_zero`, `matching_date_format`, `describe_kinds`) — they're
the part most likely to grow subtle bugs under a small direct test. Most
of these stay private, reachable from `#[cfg(test)]` without needing
`pub`; `suggest_ideal_type` itself is the one exception, made `pub` for
`benches/heuristic_engine.rs` to call directly - see "Benchmarking" below.
`run()` remains the crate's only *supported* entry point; the rest of the
public surface (currently just that one function) exists for benchmarking
access, not as a general-purpose library API for other crates to build on.

## Benchmarking

```bash
cargo bench                                    # heuristic_engine + end_to_end (no features needed)
cargo bench --features parquet,sqlite,xlsx     # + format_comparison
cargo bench --bench heuristic_engine           # just one target
open target/criterion/report/index.html        # HTML report, after any run
```

`BENCHMARKS.md` (repo root) is a manually-updated log of past runs - no CI
runner behind this project, deliberately (see its own header for why: cross-
machine numbers aren't comparable anyway, and a committed, human-curated
log was judged more valuable here than automating something whose output
still needs a human to interpret). Append a new entry there after a
performance-relevant change, so it can be checked against a prior snapshot
on the same machine instead of relying on memory.

Three targets, each answering a different question, all via
[Criterion](https://docs.rs/criterion) (`[dev-dependencies]` only - never
touches the shipped binary or a consumer's build, the one place in this
project that reaches for a big, full-featured crate instead of hand-
rolling something leaner, since statistical rigor - variance, outlier
detection, regression comparison across runs - is exactly what a
benchmark needs and a `std::time::Instant` harness can't easily give it):

- **`benches/heuristic_engine.rs`** — "how fast is the type-detection
  logic itself?" Calls `suggest_ideal_type` directly, in-process, across a
  few realistic column shapes (UUIDs, integers, emails, and a high-
  cardinality free-text column - the actual worst case, since it has to
  fail *every* precise-grammar check before falling back to `String`) at
  10/1,000/100,000 values each. This is the one target that needed a
  public-API change (`suggest_ideal_type` is `pub` specifically for this,
  see above) - the alternative would be measuring it only through a full
  subprocess run, which buries the actual heuristic cost under process-
  spawn and file-I/O noise.
- **`benches/end_to_end.rs`** — "how does the CLI actually perform for a
  user?" Black-box, via `Command` against the compiled binary - the same
  approach `tests/integration.rs` already uses, and for the same reason
  (no assert_cmd dependency; keeping bench tooling as lean as the test
  tooling). CSV and JSON only, generated synthetically at 100/10,000/
  200,000 rows (written to a `tempfile::tempdir()`, never committed) -
  these two need no optional feature, so this target always runs.
- **`benches/format_comparison.rs`** — "which format does this tool read
  fastest, for the *same* data?" Not just the same row count -
  `benches/fixtures/generate.py` writes one pandas DataFrame to five
  formats at once (CSV/JSON/Parquet/SQLite/Excel), so the values are
  byte-identical across formats, not just similarly shaped. Gated behind
  `required-features = ["parquet", "sqlite", "xlsx"]` in `Cargo.toml`, so
  a plain `cargo bench` skips it rather than failing to build. A real
  run's own numbers on this repo's dev machine (10,000 rows, illustrative
  only - always reproduce locally before trusting a specific figure):
  SQLite and CSV read fastest (~14-15ms), Parquet close behind (~15ms),
  JSON slower (~22ms, the recursive per-record flattening path has more
  overhead than the flat-columnar readers), Excel slowest by a clear
  margin (~29ms, zip decompression plus XML parsing on top of everything
  else).

## Cloud-platform file compatibility

This tool never touches the network - no cloud SDKs, no credentials, no
`s3://`/`az://`/`gs://` URIs. "Cloud compatibility" here means something
narrower and more concrete: once a file produced by an AWS/Azure/GCP-native
data service has been downloaded locally, does it read the same as any
other file of that format? A deliberate pass was made checking exactly
that, empirically rather than by inspection - some things already worked
and just needed confirming, some formats needed a real fix (see the
design philosophy section above for the Avro logical-type, `\N`-sentinel,
and Parquet `chrono-tz`/Map-key entries).

**Already correct, verified rather than assumed:**

- **Parquet compression codecs** - Snappy (the near-universal default
  across Athena/Glue/EMR/BigQuery/Synapse), gzip, Brotli, LZ4, and Zstd all
  read correctly. The `parquet` crate's own `default` feature set already
  enables every codec (`snap`, `brotli`, `flate2-zlib-rs`, `lz4`, `zstd`),
  and this project's `Cargo.toml` never disables default features on it -
  confirmed by actually writing the same small dataset through pandas
  with each codec and reading every file back.
- **Legacy Parquet `INT96` timestamps** - still common from Hive/Athena/
  EMR-lineage writers despite being deprecated in favor of the logical
  `timestamp` type. Reads identically to a modern `INT64`-encoded
  timestamp column, confirmed by writing the same data both ways
  (`use_deprecated_int96_timestamps=True/False` in pyarrow) and diffing
  the output.
- **Parquet `DECIMAL128`** - unlike Avro's decimal (see below), this
  already renders correctly (`"123.45"`, `"-45.67"`, `"0.00"`) because it
  goes through Arrow's own mature `Decimal128Array` type, not a hand-
  rolled bridge - confirmed with the same positive/negative/zero values
  used to verify the Avro fix.

**Found broken and fixed** (both detailed in the design philosophy section
above): Avro's `timestamp-millis`/`-micros`/`-nanos` and their `local-*`
counterparts were being silently reduced to opaque epoch integers, and
`decimal` rendered as unusable Rust Debug output
(`"Decimal(Decimal { value: 12345, len: 2 })"`) instead of a real number -
both real risks for cloud-streaming Avro producers (Kinesis Firehose,
Event Hubs Capture, Pub/Sub) that lean on exactly these logical types.
`\N`, the literal NULL marker MySQL/Hive/Redshift `UNLOAD` all write in
text exports, is now recognized as a missing-value sentinel. A Parquet
Timestamp column carrying a *named* timezone (`"UTC"`, as opposed to a
raw numeric offset) - exactly what Spark/Hive writers commonly attach -
used to fail outright ("only offset based timezones supported without
chrono-tz feature"); the `arrow` dependency now enables `chrono-tz`
(three extra crates, checked before assuming it was expensive - see the
design philosophy section above). Separately, a Parquet/Arrow Map column
with non-string keys (`Map<Int32, T>`, a real, legal shape for a numeric-
code lookup table) used to fail the whole file's read, not just that one
column - fixed by isolating each nested column's own JSON conversion
rather than converting a batch's nested columns as one atomic unit. Any
Avro file compressed with Snappy - the default/most common codec for
Kafka- and Hadoop-pipeline-produced Avro specifically - failed outright
with every single one of this project's own real-world Snappy-compressed
test files (`apache-avro`'s `snappy` feature wasn't enabled); zstd-
compressed Avro had the identical gap. Both are now enabled - see the
design philosophy section above for the exact dependency-cost check.

**Not covered, and out of scope for this pass:** Avro's `Duration` logical
type (months/days/milliseconds, a compound value with no single natural
string form) still falls through to the same best-effort Debug-formatted
fallback every truly-unhandled `Value` variant gets - genuinely rare in
practice compared to decimal/timestamp, and left as a disclosed gap rather
than guessed at. `BigDecimal` (Avro's newer, unscaled-in-the-schema
decimal variant) delegates to the `bigdecimal` crate's own `Display` impl,
which should already be correct since (unlike the fixed-scale `Decimal`)
it carries its own scale - but this specific path has lower verification
confidence than the rest of this section, since `fastavro` (the tool used
to generate every other test fixture here) doesn't support writing
`big-decimal` test data the way it does for `decimal`.

## Dependency footprint

This project already declines whole formats over dependency weight (see
DuckDB/SPSS below) - the same instinct applies one level down, to
individual crates within formats that *are* supported. A handful of
dependencies were hand-rolled away entirely once actually looked at,
because what they provided was a small, bounded piece of functionality
this project could just implement directly rather than depend on:

- **`tempfile` → `TempFile`/`TempDir`.** The only thing `decompress_if_needed`
  actually needs is a real on-disk scratch file (several readers need
  genuine random file access, not a stream - Parquet, SQLite, Excel) that's
  cleaned up on drop. `TempFile` (in `src/lib.rs`, right above
  `decompress_if_needed`) does this in about 40 lines: `create_new` (fails
  if the path already exists, rather than silently truncating or
  following it) for the same collision/symlink-race safety `tempfile`
  provides internally, with a pid + nanosecond-timestamp + call-counter
  name standing in for `tempfile`'s RNG-backed uniqueness - overkill for a
  single-process CLI, but the safety property (atomic create-or-fail, not
  a check-then-create race) is the part actually worth keeping.
  `tests/integration.rs` and `benches/end_to_end.rs` each carry their own
  near-identical `TempDir` (a whole scratch *directory*, recursively
  removed on drop) for the same reason, duplicated rather than shared
  since tests/benches/the lib are three separate compilation units here.
- **`clap` → a hand-written arg loop.** This CLI has no subcommands and no
  short flags besides the automatic `-h`/`-V`, just nine long options plus
  two positionals - `Args::parse_from` (above the `Args` struct) is a
  single `while` loop over `std::env::args()` handling `--flag value` and
  `--flag=value` forms, with the numeric/char/comma-list parsing each
  option already needed anyway. The one deliberate behavior change: clap
  prints its own error and exits with its own reserved code before this
  project's `main` ever runs; the hand-rolled version returns this
  crate's own `Result<Args>` instead (at the time this was written, still
  backed by `anyhow` - see that dependency's own entry further below for
  why it was removed too, afterward), so a bad flag flows through the
  exact same `Error: ...`-and-exit-1 path every other error in this tool
  already uses - one less special case, not a regression, and nothing
  currently depends on clap's specific exit code.
- **`serde`'s `derive` feature dropped.** The only two places this project
  ever used `#[derive(Serialize)]` were `ColumnProfile` and a small local
  `DataDictionary` wrapper in `render_json` - both replaced with a direct,
  hand-written `impl serde::Serialize` using `serialize_struct`. This
  surfaced a real, easy-to-miss trap worth recording: the first attempt at
  this used `serde_json::Value`/the `json!` macro instead (building the
  output as a `Map` rather than implementing `Serialize` by hand), and
  `serde_json::Map` is backed by a plain `BTreeMap` unless the
  `preserve_order` feature is enabled (which pulls in `indexmap` - the
  wrong direction for this exercise entirely) - so it silently re-sorted
  every field alphabetically (`current_type, description, ideal_type,
  missing_pct, name, notes, sample_values` instead of the documented
  `name, current_type, ideal_type, ...` order shown earlier in this file).
  None of the existing tests caught this - they parse JSON output and
  look values up by key, which is correct behavior for JSON's own
  semantics (object key order isn't meaningful) but meant a real,
  visible regression against this project's own documented output shape
  went unnoticed by `cargo test`. Only caught by actually running the
  binary and reading the output by hand before trusting the change - the
  same discipline this file's design-philosophy section applies
  everywhere else, here applied to a build-system change rather than a
  heuristic.
- **`flate2` → a hand-rolled DEFLATE/gzip decoder.** gzip is the one
  compression format this project reads unconditionally (no feature
  gate), so it's the one place hand-rolling pays off without touching an
  optional feature's own dependency budget. `inflate`/`gzip_decompress`
  (in `src/lib.rs`, right above the "Transparent gzip/zstd decompression"
  section) implement RFC 1951 (DEFLATE: stored/fixed/dynamic Huffman
  blocks) and RFC 1952 (the gzip container: header, optional FEXTRA/
  FNAME/FCOMMENT/FHCRC fields, and a CRC32+ISIZE footer actually verified
  against what was decompressed, not just parsed) - decode-only, since
  this tool never writes gzip, closely following the structure of
  `puff.c` (Mark Adler's own minimal reference inflate implementation)
  rather than inventing an approach from first principles. Real DEFLATE
  decoders are notoriously easy to get subtly wrong, so this got
  correspondingly heavier verification before being trusted: byte-exact
  diffed against Python's independent `zlib`/`gzip` modules across eight
  real files generated with the system `gzip` command at multiple
  compression levels (empty, a 2-byte file, highly repetitive content,
  random/incompressible content forcing stored blocks, and a 28MB/
  300,000-row CSV to check performance isn't pathological - all correct,
  0.54s end-to-end), plus a hand-built file exercising all four optional
  gzip header fields at once (FHCRC/FEXTRA/FNAME/FCOMMENT - none of which
  the system `gzip` command ever sets on its own) and two deliberately
  checksum-corrupted files to confirm the CRC32/ISIZE verification
  actually catches real corruption rather than just being present in the
  code. `tests/fixtures/edge_gzip_dynamic_huffman.csv.gz` (3,000 rows,
  large/repetitive enough that zlib's encoder reaches for multiple
  dynamic Huffman blocks, not just the trivial single-block case
  `sample.csv.gz` already covers), `edge_gzip_all_optional_header_fields
  .csv.gz`, and `malformed_gzip_checksum.csv.gz` carry the smaller,
  representative slice of that verification forward as permanent,
  committed regression coverage (`src/lib.rs`'s own `#[cfg(test)]`
  block plus two new `tests/integration.rs` cases), rather than relying
  on the throwaway scratch files the fuller manual verification pass used.
- **`anyhow` → a hand-rolled `Error`/`Context`/`Result`/`bail!`.** Done
  last, and done anyway despite `anyhow` itself having *zero* transitive
  dependencies of its own (confirmed via `cargo tree` before starting -
  the rewrite was always going to net exactly one crate removed, a poor
  ratio purely by dependency count), because the actual rewrite turned
  out to be low-risk and mechanical rather than something to weigh
  against that ratio: `Error` (message + optional boxed `source`, the
  same shape anyhow's own context chain has), a `Context` trait
  implemented once for any `Result<T, E: Into<Error>>` *and* for
  `Option<T>`, and `bail!`/`anyhow!` as local `macro_rules!` wrapping
  `format!` - about 100 lines total, replacing `use anyhow::{Context,
  Result, bail};` at the top of `src/lib.rs`. The one piece worth calling
  out: `impl<E: std::error::Error> From<E> for Error` is a *single*
  blanket impl, and it's what makes `?` keep working unchanged at every
  one of this project's ~150 explicit `.context()`/`.with_context()`/
  `bail!`/`anyhow!` call sites (plus every bare `?` on a lower-level
  error) across every reader - `io::Error`, `serde_json::Error`,
  `csv::Error`, `chrono::ParseError`, `rusqlite::Error`,
  `apache_avro::Error`, `parquet`/`arrow`'s errors, `calamine`'s,
  `toml_edit`'s, `serde_norway`'s, `dbase`'s, `dta`'s, `sas7bdat`'s, and
  more all already implement the standard `std::error::Error` trait (a
  very standard convention almost every published crate follows), so one
  impl bridges all of them at once - no per-crate special-casing needed,
  confirmed by both the default and `--features full` builds compiling
  clean *on the first attempt* after the swap, with all 122+60 (default)
  and 131+156 (full) tests still passing unchanged. `Error` deliberately
  does *not* implement `std::error::Error` itself - the same choice
  anyhow's own `Error` type makes, and for the same reason: it would
  conflict with core's reflexive `impl<T> From<T> for T` once `Error:
  Into<Error>` is also relied on by the `Context` impl covering the
  "wrap a `Result<T, Error>` this project's own code already returned"
  case. `Error`/`Result` are `pub` (a small, deliberate extension of this
  crate's otherwise-minimal public surface - see the "only supported
  entry point" note elsewhere in this file) purely because `main.rs`
  needs to name `run()`'s return type, the same reason `anyhow::Result`
  used to appear there. Manually spot-verified across several real,
  multi-level error chains before trusting the mechanical pass alone
  (a missing file, a truncated gzip header nested three levels deep
  through `decompress`/`read gzip header`/the underlying io error, a
  corrupted Parquet footer, a wrong Avro magic number, a broken xlsx
  zip archive, and a TOML parse error whose multi-line pointer-diagram
  `Display` output survives the `.to_string()` bridge intact) - nothing
  byte-identical to anyhow's own `Debug` formatting was required, since
  no test in this project ever asserted on that exact chain layout, only
  on specific substrings appearing somewhere in stderr.
- **`csv` → a hand-rolled `parse_csv`.** The riskiest hand-roll in this
  list, precisely because - unlike `anyhow` - this one *is* real,
  correctness-sensitive parsing logic, of exactly the kind this section
  elsewhere argues against touching. Done anyway, but only after
  confirming the actual behavior needed (not a naive delimiter-split)
  directly against `csv-core`'s own `reader.rs` state machine
  (`transition_nfa`) rather than assumed from RFC 4180 alone - real CSV
  in the wild, and several of this project's own committed fixtures,
  lean on behavior RFC 4180 doesn't even specify: CRLF/bare-LF/bare-CR
  all independently ending a record, a genuinely blank line producing no
  record at all (not a one-empty-field row), content immediately after a
  quoted field's closing quote appending to the same field rather than
  erroring, and a leading UTF-8 BOM stripped only at the very start of
  the file. `parse_csv` (`src/lib.rs`, right above `columns_from_csv`) is
  a straightforward character-by-character state machine matching that
  behavior exactly, operating on `char`s rather than raw bytes so
  multi-byte UTF-8 content already read into memory via `fs::
  read_to_string` (which also gets UTF-8 validation, and gzip-file-style
  BOM handling, for free) is never split mid-character. Reading the
  whole file up front rather than streaming also let `columns_from_csv`
  drop the two-pass seek-and-resume dance the old `csv`-crate-based
  version needed to skip preamble rows without losing the header - see
  that function's own git history for the seek-poisoning bug that
  approach caused - since "skip N rows, then strictly length-check the
  rest against the header" is now just slicing an already-parsed
  `Vec<Vec<String>>`. Verified two ways before being trusted: the
  existing test suite's *own* adversarial CSV coverage (near-miss
  checksums for IBAN/credit-card/ISBN/EAN/VIN/IMEI, WKT geometry and
  coordinate pairs with embedded commas inside quotes, embedded JSON with
  escaped quotes, BOM handling, ragged rows, both preamble-detection
  signals) passed unchanged with zero code changes needed beyond the
  parser swap itself, and five shapes that suite's fixtures don't happen
  to stress - a newline embedded in a quoted field, content after a
  closing quote, mixed CRLF/LF/CR in one file, consecutive blank lines,
  and an unterminated quote (confirmed to consume the rest of the file
  as one field rather than hang or panic) - were checked by hand and
  turned into permanent unit tests on `parse_csv` directly rather than
  left as one-off manual checks.
- **`chrono` → a hand-rolled civil-calendar and date/time parser -
  everywhere except `xlsx`.** The most research-heavy hand-roll of this
  whole effort, precisely because it looked the *most* dangerous going
  in: 40+ independently-verified date formats, RFC 2822's weekday
  cross-validation, and the already-documented %y/%Y quirks this file's
  own comments rely on getting right. Done by reading chrono's own
  source (`format/parse.rs`, `format/scan.rs`, `format/parsed.rs`)
  directive-by-directive rather than assuming strptime conventions carry
  over exactly - several details only surfaced this way: `%Y` scans 1-4
  digits, not exactly 4 (the literal reason the %y-before-%Y ordering
  trick elsewhere in this file works at all); `%y`'s real pivot is
  `< 70` (00-69 -> 2000s, 70-99 -> 1900s), which turned out to be one
  year off from this file's own older paraphrase of the same rule -
  fixed in place once found; `%z` accepts any run of `:`/whitespace
  between hour and minute digits, not just a single optional colon; and
  `%a` is genuinely cross-validated against the parsed date's *computed*
  weekday, not just shape-matched. The civil-calendar conversion itself
  (`days_from_civil`/`civil_from_days`, backing both the format parser's
  weekday check and the Avro/SAS7BDAT epoch-to-date paths) is Howard
  Hinnant's well-known algorithm, not derived from scratch - verified
  against Python's `datetime` module across leap-year and century-
  boundary cases (year 1, year 9999, 1600/1900/2000/2100/2400) before
  anything else was built on top of it. `EpochDate`/`EpochTime`/
  `EpochDateTime` (near `matches_date_format`) cover the separate,
  simpler need the Avro/SAS7BDAT readers have: turning a stored epoch
  offset into one of a handful of fixed output strings, not general
  parsing. One real bug the adversarial test suite caught immediately:
  the first attempt at the month/weekday-name scanner (`&v[..3]`)
  panicked on multi-byte UTF-8 input (💥, repeated é) with "byte index 3
  is not a char boundary" - fixed by checking `str::is_char_boundary`
  first (which doubles as the length check, per its own documented
  contract, and correctly fails the match rather than panicking when a
  3-byte prefix would land mid-character - an all-ASCII month/weekday
  name could never match there anyway). All 11 of this file's own
  existing `matching_date_format`/`matching_time_format` tests -
  covering RFC 3339 with `Z`/numeric offset, international and
  full-month variants, RFC 2822 and Unix `ctime()` forms, RFC 2822 with
  a literal `GMT` zone, the weekday-mismatch rejection, the two-digit-
  year ordering trick, Oracle-style and compact-ISO forms, and 12h/24h
  time - passed unchanged the moment the parser was wired in, before the
  UTF-8 fix was even found (the adversarial-input tests are what caught
  it, not these). `chrono` didn't disappear entirely, though: it's now
  an *optional* dependency, gated behind `xlsx` only, because calamine's
  own `as_datetime()` API returns a real `chrono::NaiveDateTime`
  regardless of what this project's own code does - `xlsx_cell_to_string`
  was already fully-qualifying `chrono::NaiveTime::MIN` and needed no
  changes at all, it just lost its unconditional top-level `use`.
  Verified across every affected feature combination individually, not
  just the two usual endpoints - `--features avro` alone, `--features
  sas7bdat` alone, and `--features xlsx` alone all needed their own
  clean-build and clippy check, since (unlike every earlier dependency
  removed in this section) chrono's removal wasn't simply "gone or not
  gone" across the default/full split; it depended on *which specific
  feature* was active.

- **`calamine` → hand-rolled, one format at a time - `.xlsx` first.**
  Unlike every other dependency in this section, `calamine` isn't one
  format's worth of parsing - it bundles four genuinely different file
  formats (OOXML `.xlsx`, legacy binary BIFF8/OLE2 `.xls`, binary-record
  `.xlsb`, and ODF `.ods`) behind one `--features xlsx` flag. `.xlsx`
  went first because it's both the most common in practice and the most
  tractable: a ZIP archive of XML documents, and this project already
  had (or could cheaply build) both halves. `ZipArchive` (`src/lib.rs`,
  right after `gzip_decompress`) is a from-scratch ZIP reader following
  PKWARE's own APPNOTE.TXT spec - reads exclusively from the central
  directory (the archive's authoritative index, at the end of the file)
  rather than trusting local file headers in file order, since a local
  header's size/CRC fields aren't even guaranteed reliable (the "data
  descriptor" flag bit means they can be zeroed there, with the real
  values written *after* the compressed data instead) - and reuses
  `inflate` directly for compression method 8 (DEFLATE), since a ZIP
  entry's compressed stream is byte-for-byte the same format gzip's own
  body already is. `xml_parse` is a second from-scratch piece, a minimal
  DOM parser scoped deliberately to what well-formed, machine-generated
  OOXML/ODF XML actually needs (no DTD/external-entity support, no
  namespace-URI resolution) - not a general replacement for the separate
  `xmltree` crate this project's own `xml` feature still depends on for
  arbitrary user-supplied XML, which remains untouched.

  The genuinely hard part was Excel's own date-serial system: day 1 =
  1900-01-01, with Lotus 1-2-3's fictitious 1900-02-29 preserved for
  backward compatibility (the well-known "Excel 1900 leap year bug").
  `xlsx_serial_to_ymd` converts this with a deliberately simple
  epoch-shift rule (shift serials below 60 forward by a day, then treat
  day 0 as 1899-12-30 uniformly, special-casing serial 60 itself since
  it has no real Gregorian equivalent) reusing the same
  `days_from_civil`/`civil_from_days` civil-calendar functions the
  hand-rolled date/time engine already has - rather than porting
  calamine's own considerably more elaborate 400/100/4/1-year-block
  algorithm (`excel_to_standard_datetime` in its `datatype.rs`, built
  that way specifically to avoid floating-point precision loss at large
  serial values). Verified by extracting calamine's *entire own* test
  suite for this (203 date-only reference cases spanning 1899-9999, 99
  datetime cases with millisecond precision) into a throwaway harness
  and confirming every single case matched before trusting the simpler
  approach - not spot-checked, the complete set. Number-format date
  detection (`xlsx_is_date_format_code`/`xlsx_is_builtin_date_format_id`)
  is a direct, line-by-line port of calamine's own
  `detect_custom_number_format`/`builtin_format_by_id` (`formats.rs`) -
  a precise, already-solved state machine (bracketed `[Red]`/elapsed-time
  sections, quoted literal text, the `_`/`\` escape and `*` fill
  characters, AM/PM markers) worth taking from a verified-correct
  reference rather than re-deriving; calamine's own 22-case test suite
  for it (itself ported from openpyxl) was ported over too, and caught a
  real regression during a clippy-driven refactor (merging two identical
  `return true` branches) before it shipped - confirming the port stayed
  correct even after being rewritten for a lint.

  `columns_from_xlsx` now dispatches on file extension: `.xlsx` goes to
  this new reader, everything else `--features xlsx` still covers
  (`.xls`/`.xlsb`/`.ods`) still goes to calamine, completely unchanged -
  the feature flag's own boundary and behavior are identical either way,
  this is purely an internal swap. Verified two ways before the dispatch
  was wired in: every one of this project's own real `.xlsx` fixtures
  (inline strings, shared strings with real deduplication, native
  date/datetime cells across multiple number formats, unicode content,
  multi-sheet workbooks, formula-result and error cells) produced
  *identical* output - table names, column names, current/ideal types,
  missing percentages, and sample values - through both the new reader
  and calamine side by side, and every feature combination that could
  plausibly differ (`--features xlsx` alone, `--features full`, and the
  default build with neither) was rebuilt and clippy-checked separately,
  not just the usual two endpoints.
- **`.ods` next, reusing the same ZIP/XML infrastructure.** ODF's own
  spreadsheet schema turned out considerably simpler than OOXML's for
  this project's purposes: a cell states its own value type directly
  (`office:value-type="date"`) and a date's value is already a clean ISO
  8601 string (`office:date-value="2024-01-15"`) - no epoch-serial
  arithmetic at all, unlike Excel's own system. The one genuinely tricky
  real-world convention is cell/row compression:
  `table:number-columns-repeated`/`table:number-rows-repeated` let a
  writer represent a long run of identical (almost always empty) cells
  or rows without spelling each one out, and real LibreOffice-authored
  files routinely pad a sheet out to ODF's own actual maximum dimensions
  this way (1,048,576 rows x 16,384 columns) - naively expanding every
  repeat into a real, materialized cell would be a genuine memory-blowup
  risk, not a hypothetical one; `ods_parse_sheet` tracks logical row/
  column position as repeats are walked but only ever records a sparse
  entry for a cell that actually has content, so an empty repeat, however
  large, never allocates anything. Verified directly against this exact
  pathological shape, not just reasoned about: a hand-built fixture with
  a `table:number-rows-repeated="1048573"` trailing block (over 17
  billion logical empty cells) plus a repeated *empty* cell sitting in
  the middle of a real data row (to prove the gap doesn't misalign the
  columns after it) both resolve correctly and complete essentially
  instantly. `ods_cell_text`'s attribute-priority order and
  `columns_from_ods`'s overall shape were checked directly against
  calamine's own `ods.rs` (`read_row`/`get_datatype`) before being
  trusted, the same "verify against the source" discipline as `.xlsx`.

  This pass also caught a real, pre-existing latent bug in the `.xlsx`
  reader itself, found via the exact same calamine-comparison test that
  had already passed cleanly for every `.xlsx` fixture: calamine parses
  a numeric cell's raw value through `fast_float2::parse` and
  re-stringifies the resulting `f64` (confirmed directly against both
  its `xlsx.rs` and `ods.rs` source) rather than passing the original
  XML text through verbatim, which silently normalizes away a written
  trailing `.0` on a whole number (`"30.0"` in the XML becomes the
  displayed value `"30"`). Every `.xlsx` fixture tested so far happened
  to be written by a tool (openpyxl, xlsxwriter) that never emits an
  unnecessary trailing `.0` for a whole-number cell, so this gap was
  invisible until the `.ods` fixture's own writer (odfpy) did emit one
  and the comparison test caught the mismatch immediately - both readers
  now parse-and-re-stringify numeric values the same way calamine does,
  not just the one that happened to surface it first.

- **`.xls` (legacy BIFF8/OLE2) - the largest piece of this whole effort,
  in two genuinely separate layers.** Unlike `.xlsx`/`.ods` (one archive
  format, ZIP, wrapping one document format, XML), a `.xls` file is a
  binary *container* format (OLE2/Compound File Binary, [MS-CFB] - a
  small filesystem-in-a-file, unrelated to ZIP) holding a binary
  *content* format inside it (BIFF8 records, [MS-XLS]) - each needed its
  own from-scratch reader, verified independently before being wired
  together.

  Getting a genuine test fixture came first and needed real, explicit
  user authorization: no tool already available in this environment can
  write a real `.xlsb`, and (it turned out, after investigation) none can
  write a real `.xls` either - `openpyxl`/`xlsxwriter` (used for every
  `.xlsx` fixture so far) only write the modern OOXML format. The user
  chose to install LibreOffice specifically to solve this
  (`brew install --cask libreoffice`, a real ~1GB+ application install,
  confirmed with the user first given its size and system-modifying
  nature), whose own "MS Excel 97" export filter genuinely does write
  BIFF8/OLE2 - every `.xls` fixture in this project, including the ones
  reused below, is a real LibreOffice-produced conversion of this
  project's own existing, already-trusted `.xlsx` fixtures, not a
  synthetic byte-level construction.

  **The container layer** (`CfbFile`, right after the ZIP/XML
  infrastructure): [MS-CFB]'s 512-byte header, a FAT (File Allocation
  Table - sector-chain pointers, assembled from a 109-entry table
  embedded in the header plus any number of overflow DIFAT sectors for
  larger files), a directory stream (128-byte entries: UTF-16LE name,
  object type, start sector, size), and - the genuinely tricky part - a
  *mini*-FAT/mini-stream subsystem for any stream under 4096 bytes
  (stored 64 bytes at a time inside the root directory entry's own
  regular stream, with its own separate mini-FAT). Confirmed directly,
  not assumed, that this isn't a rare corner case: even a small,
  realistic spreadsheet's own "Workbook" stream lands in the mini-stream
  path. Verified byte-exact against a real file - header, FAT, mini-FAT,
  directory entries, and the first 20 bytes of the extracted "Workbook"
  stream (a BOF record, the mandatory first record of any BIFF8 stream)
  all independently re-derived in Python and diffed against the Rust
  reader's own output. That verification caught a real lesson worth
  recording: an early version of the test itself failed on what looked
  like an off-by-one in the reader, and turned out instead to be a manual
  hex-transcription error in the *test's own* expected-bytes literal, not
  the reader - re-deriving the expected bytes fresh a second time (rather
  than trusting the first hand-transcription) is what caught it. `.xls`
  detection in the `columns_from_xlsx` dispatcher checks for a real
  "Workbook" (or the older name, "Book") stream inside a valid OLE2
  container, the same content-over-extension principle `sniff_format`
  and the `.xlsx`/`.ods` dispatch already use.

  **The content layer** (BIFF8 records) was researched the same way
  every hand-rolled format in this project is - reading calamine's own
  `xls.rs`/`cfb.rs`/`formats.rs` source field-by-field before writing a
  line of Rust, not recalled from memory. A BIFF record is simple framing
  (2-byte type + 2-byte length + that many bytes), but any record can be
  followed by CONTINUE records carrying its overflow past an ~8KB
  per-record cap - and, confirmed directly against calamine's own
  dispatch code rather than assumed, only the shared string table (SST)
  actually reads a value that spans a CONTINUE boundary; a LABEL cell's
  inline string and a FORMAT record's custom format code are each read
  from a single non-continuing buffer (truncating cleanly if the record
  runs short, mirroring `XlsEncoding::decode_to`'s own behavior) exactly
  because calamine's own `parse_label`/`parse_format` do the same - this
  reader matches that boundary rather than exceeding it in an untested
  way. RK's compact 4-byte numeric encoding (the 4 bytes become an IEEE
  double's high 32 bits, with the low 2 bits of the first byte
  repurposed as "divide by 100"/"this is a shifted 30-bit integer" flags)
  and date detection (an XF record's format index resolved through
  either a FORMAT record's custom format-code text or a fixed
  built-in-ID range) both reuse machinery already built and verified for
  `.xlsx` - `xlsx_is_date_format_code` directly, and the same
  `xlsx_serial_to_ymd`/`xlsx_format_serial` epoch-conversion functions,
  since `.xls` uses the identical 1900 date-serial system. A FORMULA
  cell's cached result value is read (row/col plus an 8-byte tagged
  field: a plain double, or - signalled by a trailing `0xFFFF` - a
  bool/error/blank/"string follows in the next STRING record" marker);
  like the `.xlsx` reader, this project deliberately reads only that
  cached value, not the formula's own token stream, into a formula
  string.

  Two scope boundaries were chosen deliberately, both disclosed rather
  than silently assumed: this reader targets BIFF8 only (Excel 97-2003 -
  the version every writer anyone would actually feed this tool today
  produces, LibreOffice's own filter included; an older BIFF2-5 stream is
  a clear, actionable error rather than guessed-at, since there's no
  fixture to verify that path against - the same "no fixture, no trust"
  boundary SAS7BDAT already draws). And "compressed" (1-byte-per-char)
  string content decodes as Latin-1 rather than through a real
  per-codepage charset table the way calamine's own `XlsEncoding` does -
  the same "not standards-complete, correct for the overwhelming common
  case" tradeoff already made for `is_email`/`is_url` elsewhere in this
  project; it only differs from true Windows-1252 in the rare 0x80-0x9F
  control range, and "uncompressed" (real UTF-16LE) content - what any
  modern writer uses for non-ASCII text - decodes exactly regardless of
  codepage, confirmed directly against a real fixture carrying café/日本語
  content converted through LibreOffice.

  Verified against calamine's own output on six real fixtures - reusing
  this project's own existing, already-trusted `.xlsx` fixtures
  (converted through LibreOffice rather than authored from scratch, since
  they were already known-good): the standard `type_detection` fixture
  (UUID/Email/IPv4/date semantic types all surviving the BIFF8 path),
  native date *and* datetime cells (the same high-impact "raw day-count
  serial instead of a real date" bug class documented for `.xlsx` above,
  proven fixed on the BIFF8 path on the first attempt), shared strings
  (SST + LABELSST), formula and error cells (FORMULA/STRING/BOOLERR,
  including a real `#DIV/0!` cached error value), a multi-sheet workbook,
  and unicode content. Every one matched calamine's output exactly -
  table names, column names, current/ideal types, missing percentages,
  and sample values - before the dispatcher was wired to prefer this
  reader over calamine for `.xls`.

- **`.xlsb` (Excel Binary Workbook, BIFF12) - initially declined, then
  revisited once a real verification fixture turned out to be reachable
  after all.** This was first written up as a permanent gap: LibreOffice
  (the only tool available in this environment that can write legacy
  Excel binary formats at all) has no working `.xlsb` export filter -
  confirmed through genuine effort, not assumed from one failure (a plain
  `--convert-to xlsb` reports no export filter found; explicitly naming
  the filter, `xlsb:Calc MS Excel 2007 Binary`, is accepted but fails at
  the actual file-write step, `SfxBaseModel::impl_store failed`). But
  "no tool here can *write* one" and "no genuine `.xlsb` file is
  reachable at all" turned out to be two different claims - the second
  one was never actually checked before the gap was declared permanent.
  It doesn't hold: Apache POI's own `test-data/spreadsheet/` directory
  (Apache-2.0 licensed, the same license family already trusted for
  `parquet-testing` elsewhere in this file) ships several real,
  genuinely-Excel-produced `.xlsb` files as part of POI's own test suite.
  Four were vendored into this project's own `tests/fixtures/`
  (`poi_simple.xlsb`, `poi_date.xlsb`, `poi_sample.xlsb`,
  `poi_various.xlsb` - see `tests/fixtures/poi_xlsb_PROVENANCE.md` for
  the exact source and license) specifically because no tool anywhere in
  this environment can generate a synthetic one - the same "vendor a
  real file when self-generation is genuinely impossible" call already
  made for the OLE2/CFBF layer of `.xls` verification, just one level
  further since even *conversion* wasn't available here.

  **The container layer needed nothing new.** `.xlsb` uses the exact
  same OPC ZIP-of-parts layout `.xlsx` already does (`xl/workbook.bin`,
  `xl/worksheets/*.bin`, `xl/sharedStrings.bin`, `xl/styles.bin`, and -
  still plain XML even here - `xl/_rels/*.rels`), so this reuses
  `ZipArchive` and `xml_parse` directly; only each part's own *content*
  differs (binary BIFF12 records instead of XML elements). BIFF12's own
  record framing turned out simpler than BIFF8's: a 1- or 2-byte
  variable-length record type (the first byte's high bit signals a
  second byte) followed by a 1-to-4-byte base-128 varint length - no
  fixed 16-bit length cap the way BIFF8 has, so there's no CONTINUE-
  record concept to handle at all. RK's compact numeric encoding turned
  out byte-for-byte identical to BIFF8's own, so `xls_rk_decode` is
  reused directly rather than reimplemented - confirmed field-by-field
  against calamine's `cells_reader.rs`, not assumed from the similar name.

  **This pass surfaced three real, independently-confirmed bugs - two in
  calamine 0.36.1's own `.xlsb` reader, and one in this project's own
  first draft, caught the same way this project catches everything else:
  by testing against real files and treating any mismatch as worth
  understanding, not dismissing.**
    1. A `BrtBundleSh` [MS-XLSB 2.4.316] record's relationship-ID string
       is prefixed by a fixed header whose documented size (Microsoft's
       own published spec example) is 8 bytes (`hsState` + `itabID`, 4
       bytes each). `poi_sample.xlsb` genuinely uses that 8-byte form -
       but `poi_simple.xlsb`, an equally real file, has 4 extra reserved
       bytes there instead (12 bytes total), confirmed by hand-decoding
       its raw bytes against its own `xl/_rels/workbook.bin.rels`
       contents until the relationship ID and sheet name both came out
       clean. calamine hardcodes the 8-byte offset and panics on this
       file (`"no entry found for key"`, indexing straight into a
       `HashMap` with `[]` rather than `.get()`) - and, checked
       specifically because a second, independent implementation is
       better evidence than one library's own bug, Python's `pyxlsb`
       hardcodes the identical assumption and fails the identical way
       (`KeyError`). `xlsb_parse_bundle_sh` tries the documented 8-byte
       header first and only falls back to 12 if the resulting
       relationship ID isn't actually present in the already-parsed
       relationships map - a real structural corroboration check against
       known-good data, not a guess, matching this project's usual
       "verify before trusting a fixed offset" discipline (compare the
       preamble-detection and dBase/Stata version-sniffing heuristics).
       `xlsb_reader_succeeds_on_a_real_file_that_breaks_calamine_and_pyxlsb`
       locks this in, independently re-verified by hand (a from-scratch
       byte-level scan of `poi_simple.xlsb`'s own worksheet parts, not
       just "the code didn't crash") to confirm the resolved content -
       one real header cell, two genuinely empty sheets - is actually
       correct, not merely non-panicking.
    2. This project's own first draft of the styles-table reader
       collected every `BrtXF` [MS-XLSB 2.4.826] record in the file into
       one flat list. That's wrong: `styles.bin` has *two* separate XF
       tables sharing that same per-entry record type - `cellStyleXfs`
       (named style definitions like "Normal", never referenced by a
       cell directly) and `cellXfs` (the real per-cell format table a
       cell's own style reference indexes into) - and a flat scan lets
       `cellStyleXfs`'s own entries shift every later cell's index by
       however many style-only entries came first. Found via a genuine
       mismatch against calamine on `poi_various.xlsb`: a real date cell
       rendered as its raw, unresolved serial number instead of a date.
       Fixed by mirroring calamine's own two-phase read exactly -
       `BrtBeginFmts`/`BrtBeginCellXFs` each declare their own entry
       count up front, and only entries immediately following the
       *right* marker, up to that count, are ever collected.
    3. Having fixed its own bug, this project's reader then disagreed
       with calamine on the very same file (`poi_various.xlsb`) a second
       time, on a *different* column - and this time the mismatch traced
       back to calamine itself, not this project's code. calamine's
       `next_cell` (the function `worksheet_range()` actually uses) has
       a match arm for `BrtCellError` (a literal error value) but none
       at all for `BrtFmlaError` (a *formula* cell whose cached result is
       an error) - it falls into that function's own catch-all
       `_ => continue`, silently dropping the cell instead of surfacing
       `"#NAME?"`. Confirmed directly against `xlsb/cells_reader.rs`'s
       source. This reader treats `BrtFmlaError` the same as
       `BrtCellError`, matching the `.xls`/`.xlsx` readers' own existing
       formula-error handling rather than reproducing calamine's gap.
    4. A fourth, separate calamine bug surfaced independently while
       chasing the *first* attempted fix for bug 2 above: `poi_date.xlsb`
       still failed to resolve its one real cell's date even after this
       project's own styles-table fix, so the file's `xl/styles.bin` was
       checked directly against calamine's `read_styles` source rather
       than assumed correct. That function's top-level dispatch loop
       calls `iter.read_type()` for every record, but only calls
       `fill_buffer()` - the call that actually advances the reader past
       a record's *body* - inside its `0x0267`/`0x0269` match arms; every
       other record type's body is silently never consumed. The first
       non-zero-length, non-matching record in the file (and
       `poi_date.xlsb`'s `styles.bin` has several - fonts, fills, and
       more - before its real `BrtBeginCellXFs`) permanently desyncs the
       rest of the stream, so calamine's own CLI-facing output for this
       file shows the cell's raw, unresolved serial (`"41286"`) instead
       of a date. This reader's own `Biff12RecordIter` has no equivalent
       bug - every record's length is consumed unconditionally in one
       place, `next()` itself, regardless of what type it is - so it
       resolves the date correctly.
       `xlsb_reader_resolves_a_date_calamine_fails_to_because_of_its_own_stream_desync_bug`
       and `xlsb_reader_captures_a_formula_error_cell_calamine_silently_drops`
       lock bugs 3 and 4 in as permanent regression tests, each with the
       full diagnosis in its own doc comment - and
       `xlsb_reader_matches_calamine_output_exactly` was narrowed to just
       `poi_sample.xlsb`, the one fixture with no known calamine bug
       affecting it, rather than comparing against a now-known-wrong
       oracle on the other three.

- **`calamine`/`chrono` demoted from real dependencies to dev-only ones,
  once `.xlsb` closed the last format still reading through them.** With
  every format `--features xlsx` documents (`.xlsx`/`.ods`/`.xls`/
  `.xlsb`) now dispatched by content to its own hand-rolled reader,
  `columns_from_xlsx_calamine`/`xlsx_cell_to_string` had exactly one job
  left: producing the "expected" side of the
  `*_matches_calamine_output_exactly` cross-verification tests. Rather
  than delete that verification capability outright, both moved to
  `[dev-dependencies]` in `Cargo.toml` and both functions became
  `#[cfg(all(test, feature = "xlsx"))]` - the exact same "dev-only, never
  touches the shipped binary" treatment this project's own benchmarking
  section already gives `criterion` (see "Benchmarking" below), applied
  here for the first time to a crate that used to be load-bearing at
  runtime. Confirmed with `cargo tree --features xlsx -e normal` (no
  `calamine`/`chrono` anywhere in the shipped build's dependency graph)
  versus `cargo tree --features xlsx -e normal,dev` (both present) -
  the distinction is real, not just a `Cargo.toml` comment, and
  `cargo build --features xlsx`/`--features full` compile with zero
  trace of either crate. All four `*_matches_calamine_output_exactly`
  tests, and the three tests documenting real calamine bugs found while
  building `.xlsb`, keep working completely unchanged - `cargo test`
  still links calamine for test binaries, exactly the way `cargo bench`
  already links criterion. The one real behavior change: `columns_from_xlsx`'s
  previous last-resort fallback (hand a file matching none of the four
  known content signatures to calamine, in case it's some other
  OOXML/OLE2-flavored format this project doesn't otherwise recognize)
  is now a direct, disclosed error instead - since the file was already
  routed here as `InputFormat::Xlsx` by extension or OLE2-magic
  sniffing, reaching this point with no signature matching means a
  corrupted file or a genuinely unsupported structure, not a case worth
  silently deferring to a crate no longer in the build at all.

- **`serde_norway` → a hand-rolled YAML parser, no intermediate value
  type.** Every other nested-format bridge in this file leans on a
  ready-made dynamic `Value` type from the crate being kept
  (`toml::Value`, `rmpv::Value`, `ciborium::Value`) to convert into
  `serde_json::Value` - this is the one exception, since the crate being
  *removed* was that ready-made type itself. `yaml_support::parse_yaml_documents`
  produces `serde_json::Value` directly: a line-based, indentation-aware
  recursive-descent parser (`YLine` - indent + content + line number, the
  same shape `xlsx_support`'s CFB/BIFF8 line-oriented parsing already
  established the pattern for), with a character-stream sub-parser for
  flow collections (`{}`/`[]`, not indentation-sensitive, so they're free
  to span physical lines without any special handling - the consumed
  span's own newline count tells the line-based parser how far to
  advance). The trickiest structural piece: an inline value right after
  `- `/`key: ` (`- key: value`, `key: [1, 2]`, a nested mapping whose
  *later* keys align to a column with no dash/key prefix on their own
  line) is handled uniformly by re-anchoring that text as a synthetic
  `YLine` at the column it actually starts on and delegating straight
  back into the normal recursive parser (`parse_inline_value`), rather
  than duplicating block-mapping/sequence/scalar logic for the "already
  mid-line" case.

  Scoped deliberately, the same "confident common case, disclosed gap on
  the rest" discipline as everywhere else in this file: this project's
  own former `serde_norway`-based reader already only passed ~74% of the
  `yaml-test-suite` spec-compliance corpus (see the real-world-corpus-
  validation section above), so 100% fidelity was never the bar. Covered:
  block and flow mappings/sequences at arbitrary depth, literal (`|`)
  and folded (`>`) block scalars with chomping and an explicit indent
  indicator, a folded multi-line plain scalar, single/double-quoted
  scalars (full backslash-escape grammar), `#` comments respecting quote
  state, `---`/`...` multi-document streams, leading `%`-directives, the
  five `!!core` tags (forcing `str`/`int`/`float`/`bool`/`null`
  interpretation - including on an explicitly-quoted scalar, e.g.
  `!!int "45"`), and YAML 1.2's core-schema null/bool/int/float
  resolution. Deliberately *not* YAML 1.1's `yes`/`no`/`on`/`off` boolean
  words - checked directly, not assumed, that this matches the crate
  being replaced: `serde_norway` is a maintained fork of the archived
  `serde_yaml`, and its name is a direct nod to the classic "Norway
  problem" (a bare `NO` silently resolving to `false`) that fork exists
  to avoid, so *not* coercing those words is the correct continuation of
  existing behavior, not a new limitation. An anchor's own value
  (`key: &name ...`) is read completely normally; only *dereferencing*
  it elsewhere via an alias (`*name`) or merge key (`<<: *name`) is out
  of scope, producing a clear, disclosed error - see below for why that
  split, rather than a flat "anchors unsupported," is what shipped.
  Explicit complex mapping keys (`? key\n: value`) and any non-core tag
  beyond a best-effort strip-and-parse-the-rest remain out of scope,
  each a real, disclosed gap rather than a guess.

  Verified two ways before being trusted, mirroring this project's own
  established rigor for every hand-rolled reader: this project's *own*
  existing YAML fixtures and unit tests (including the multi-document
  `sample.yaml`, the top-level-scalar-sequence edge case, and the
  malformed-input test) passed unchanged on the first attempt: and,
  since no synthetic fixture set substitutes for genuinely messy
  real-world YAML, three real files were pulled and cross-checked
  against Python's independent `PyYAML` library - a real Docker Compose
  file (`docker/awesome-compose`), a real Kubernetes Deployment manifest
  (`kubernetes/website`'s own example), and a real GitHub Actions
  workflow (`actions/starter-workflows`). This surfaced three real bugs,
  each fixed and locked in as a permanent test before the parser was
  trusted:
    1. **Block scalars measured against the wrong reference indentation.**
       `key: |` re-anchored the scalar's body-indentation check against
       the *synthetic* column right after `key: ` (an artifact of the
       inline-value delegation described above) rather than the key's
       own real indentation - producing an empty string, and - worse -
       silently orphaning the *next* key entirely, since the body lines
       never satisfied that wrong reference and were left unconsumed for
       the outer mapping loop to choke on. Fixed by threading a second,
       separate `parent_indent` through the block-node/scalar functions
       specifically for this purpose, distinct from the structural
       `indent` the inline-delegation mechanism needs for its own,
       different job (aligning a nested mapping/sequence's *later*
       keys/items to a real column) - the two coincide everywhere except
       through that one delegation path, which is exactly where the bug
       was hiding.
    2. **A block sequence indented the *same* as its own key** (`key:`
       immediately followed by `- item` with no extra indentation, not
       more) - found in the real Kubernetes manifest's own `containers:`
       field, a real, common style YAML explicitly permits as an
       exception to its usual "children more indented than parent" rule.
       The mapping-value logic required strictly-greater indentation
       before treating what followed as a nested value, so the sequence
       was skipped entirely and its content misread as if it were a
       sibling mapping key one level up. Fixed with `is_nested_value_line`,
       which extends the exception specifically (and only) to a
       same-indent line that's itself a sequence item - a same-indent
       *mapping* key still isn't given this exception, since that would
       be genuinely ambiguous.
    3. **`on`/`off`/`yes`/`no` silently becoming booleans** - not a bug
       in this project's own parser, but the *opposite* finding: cross-
       checking the real GitHub Actions workflow against PyYAML's
       default `safe_load` showed its own top-level `on:` key resolving
       to the literal boolean `True`, exactly the "Norway problem" this
       project's own design choice (above) was built to avoid. Confirmed
       directly, not assumed, and recorded as positive evidence the
       design choice was correct, not just theoretically justified.
  A fourth issue was a real correctness gap rather than a cross-tool
  mismatch, found by deliberately testing the declared-out-of-scope
  anchor/alias case rather than just documenting it and moving on: an
  anchor's own value (`defaults: &defaults` followed by a nested block)
  was, before this fix, misread as the literal string `"&defaults"`,
  and - the more serious half - the block it should have introduced was
  silently dropped from the output entirely (never consumed by anything,
  so it just vanished, taking the *next* sibling key down with it too).
  That's a real violation of this project's own "never silently
  misread" principle, not an acceptable shape for an out-of-scope
  feature to fail in. `strip_anchor_prefix` closes the gap for the
  anchor's own value (the tag carries no information the type-detection
  heuristics need, so simply discarding it and reading the value
  underneath normally is both correct and free), while a bare `*alias`
  reference - genuinely unresolvable without anchor-table bookkeeping
  this parser doesn't do - now produces a clear, actionable error
  instead of either silently misreading it as a literal string or
  losing data around it. `tests/fixtures/edge_yaml_same_indent_sequence.yaml`
  and its own integration test lock in the Kubernetes-manifest finding
  at the full-pipeline level; the block-scalar, anchor-value, and
  alias-error findings are locked in as `#[cfg(test)]` unit tests
  directly on `yaml_support::parse_yaml_documents`, alongside the
  now-confirmed `on`/`off`/`yes`/`no` non-coercion behavior.

- **`rust-ini` → a hand-rolled INI parser, the smallest and lowest-risk
  hand-roll in this whole effort.** INI's own grammar is far smaller
  than any nested format this project has replaced so far - no
  indentation sensitivity, no nested collections, just sections and
  flat `key=value`/`key:value` lines - so `ini_support::parse_ini`
  (`src/lib.rs`) is a straightforward line-oriented parser: a line is a
  comment (`;`/`#`), a `[section]` header, blank, or a key-value pair,
  with a name-to-index map alongside an ordered `Vec` of sections so
  that re-opening a `[section]` already seen earlier in the file appends
  into that *same* section rather than creating a duplicate - matching
  rust-ini's own `ListOrderedMultimap`-backed behavior, confirmed
  directly against its source rather than assumed. Section (and,
  within a section, key) order is real, observable output shape here,
  the reason for the ordered `Vec` instead of a plain `HashMap`.

  Value parsing mirrors rust-ini's own quoting/escaping rules, also
  checked directly against its source: leading whitespace is skipped
  once, then a value is built from zero or more `"..."`/`'...'` quoted
  segments interleaved with unquoted trailing text - a real, if unusual,
  rust-ini convention this replicates exactly (its own documented
  example, `key='Single Quote' with extra value`, resolves to
  `Single Quote with extra value`: the text right after a closing quote
  is *not* re-trimmed of its own leading whitespace, only trailing, so
  the space between the quoted segment and the trailing text survives
  into the concatenated result). The backslash-escape grammar itself
  (`\0 \a \b \t \r \n \xHHHH`, an escaped newline as a line-continuation
  contributing nothing to the value, any other `\c` reducing to the
  literal character `c`) is shared identically between quoted and
  unquoted text, matching rust-ini's own single shared implementation
  for both rather than two separate ones.

  One deliberate divergence, made and disclosed rather than silently
  matched: rust-ini requires a comment's `;`/`#` to be the *literal*
  first character of the line (checked against its source: with the
  `inline-comment` Cargo feature off, which is this project's own
  configuration, a `;`/`#` preceded by even one space is a hard parse
  error, not a recognized comment) - this parser is deliberately more
  lenient, treating a comment as `;`/`#` after leading whitespace is
  trimmed, the more standard and expected INI convention, since no real
  file this project tested against ever exercised rust-ini's stricter
  (and, on inspection, likely accidental) rule.

  Verified two ways: a dedicated `ini_reader_matches_rust_ini_output_exactly`
  test cross-checks this parser against `rust-ini` itself (kept as a
  dev-only oracle, the same treatment calamine/chrono and rust-ini's own
  YAML-era counterpart already get) on this project's existing fixtures
  plus a new `tests/fixtures/edge_ini_quoting_and_escapes.ini` covering
  every quoting/escaping rule above in one small, committed file; and,
  transiently and not committed (matching this project's usual large-
  external-corpus practice), against two real files already referenced
  elsewhere in this document's own real-world-corpus-validation
  write-up - a genuine `php.ini-production` (1,878 lines) and a real
  Samba `smb.conf.default` (223 lines). Both matched `rust-ini`'s own
  output exactly (after filtering empty sections on both sides, the same
  filter `columns_from_ini` itself already applies - rust-ini eagerly
  creates an empty `Properties` entry for every `[header]` line even
  with no keys following, while this parser creates a section lazily on
  its first key; a real, confirmed difference that never surfaces past
  that existing filter) - zero hand-rolled-parser bugs found, the
  cleanest result of any hand-roll in this whole effort.

- **`xmltree` → a *second*, independent hand-rolled XML parser, not a
  shared one.** This project already had one hand-rolled XML parser -
  `xlsx_support`'s own, built earlier for reading OOXML/ODF parts inside
  `.xlsx`/`.ods` archives - but it couldn't simply be reused here, for
  two real reasons rather than a style preference. First, `xml` and
  `xlsx` are independently toggleable Cargo features (a build with
  `--features xml` alone must compile and work without anything gated
  behind `xlsx`), so nothing in one feature's code can reference the
  other's. Second, and more fundamentally, the two need genuinely
  *different* behavior: the OOXML/ODF parser deliberately *preserves* a
  namespace prefix verbatim (`r:id` is looked up as literally the string
  `"r:id"`, since this project already knows OOXML's own fixed schema
  prefixes), while a general-purpose reader for arbitrary, unknown,
  real-world XML needs to *strip* prefixes instead - confirmed
  empirically, not assumed: `xmltree::Element.name` is documented as
  excluding namespace info entirely, and a synthetic file mixing a plain
  `<link>` with a namespaced `<atom:link>` and a namespaced `xsi:type`
  attribute produced a single merged `link` column and a plain `@type`
  attribute through the old xmltree-based reader - independently
  consistent with this project's own prior real-world validation, which
  found the identical merge on a real BBC RSS feed mixing a plain
  `<link>` with an Atom-namespaced one under one flattened name (see the
  "seventh pass, for XML" entry in this document's own real-world-
  corpus-validation write-up above). `xml_support` (`src/lib.rs`) is
  accordingly a second, separately-scoped implementation - substantial
  structural overlap with the OOXML/ODF one (both are hand-rolled
  recursive-descent parsers over the same core XML grammar: entities,
  CDATA, comments, processing instructions, attributes, nesting), but a
  real and deliberate divergence in namespace handling, matching the
  same "controlled duplication across independently-gated format
  modules" precedent this project already accepts elsewhere (e.g.
  `zip_read_u16` and `CfbFile::read_u16` inside `xlsx_support` itself).

  Namespace handling here is real URI-based resolution's cheap,
  deliberately scoped stand-in: any element or attribute name containing
  a colon has everything up to and including the first colon stripped -
  no validation that the prefix was ever actually declared via an
  `xmlns:prefix="..."` binding, and no real scoping (a prefix is
  stripped the same way regardless of which element declared it, or
  whether it was ever redeclared partway through the document). Real
  namespace resolution requires tracking a stack of prefix-to-URI
  bindings as the document is descended - genuine complexity this
  project's own reader has no use for, since it never reads the resolved
  URI at all, only ever the bare local name. `xmlns`/`xmlns:*`
  attributes themselves are dropped rather than exposed as regular
  `@xmlns...` attributes, matching `xml-rs`'s own behavior (`xmltree`'s
  own underlying parsing crate, confirmed empirically) of treating them
  as namespace bindings, not attribute data.

  Depth protection became strictly better as a side effect of hand-
  rolling, not just an afterthought carried over: the *old* xmltree-
  based reader needed a separate pre-parse text scanner
  (`xml_nesting_too_deep`) specifically because `xmltree::Element::parse`
  has no recursion limit of its own (confirmed empirically: a
  50,000-level-deep adversarial document reliably stack-overflowed the
  compiled binary), and that scanner's own doc comment disclosed a real,
  narrow false-negative gap - a literal unescaped `>` inside an
  attribute value could end its tag scan early. This project's own new
  recursive-descent parser needs no equivalent pre-scan at all: it
  carries an explicit depth counter through its own real recursion and
  bails cleanly the instant it's exceeded, a strictly stronger guarantee
  than a heuristic text scan can offer, since it's tracking genuine
  parse state rather than guessing at tag boundaries from raw text.
  `xml_nesting_too_deep` is gone entirely, not kept alongside the new
  parser - the four tests that used to exercise it directly now exercise
  the new parser's own depth guard through its real public entry point
  instead, and every existing depth-related fixture and integration test
  (including the 50,000-level adversarial one) passed against the new
  parser unchanged.

  Verified two ways: a dedicated `xml_reader_matches_xmltree_output_exactly`
  test cross-checks this parser against `xmltree` itself (kept as a
  dev-only oracle, the same treatment every other replaced crate in this
  section already gets) on this project's existing fixtures plus a new
  `tests/fixtures/edge_xml_namespaces.xml` covering the exact plain/
  namespaced-element and namespaced-attribute merge described above; and,
  transiently and not committed (matching this project's usual large-
  external-corpus practice), against the same four real files this
  project's own original xmltree-based validation used - live BBC News
  and NASA RSS feeds, Apache Maven's own real `pom.xml`, and a real SVG
  icon using a *default* (unprefixed) namespace declaration, a
  meaningfully different shape from the prefixed-namespace cases above
  since it exercises elements that have no colon in their name at all.
  All four matched `xmltree`'s own output exactly, including Maven's
  `pom.xml` producing the identical 244 flattened columns this project's
  own prior validation already documented - zero hand-rolled-parser bugs
  found on real content, the cleanest large-scale real-world result of
  any hand-roll in this effort alongside `rust-ini`'s.

- **`zstd` → a hand-rolled RFC 8878 decoder - the most algorithmically
  complex hand-roll of this whole effort, and the last one needed to get
  the default build down to genuinely zero non-`std` dependencies for
  every format this project reads.** Unlike DEFLATE/gzip (already
  hand-rolled, needing only Huffman coding), Zstandard needs a second,
  independent entropy coder - FSE (Finite State Entropy / tANS) - plus an
  LZ77-style sequence-execution stage with its own three-slot
  repeat-offset state machine, layered as frame → block → literals
  section → sequences section. Every algorithmic piece was verified
  against RFC 8878's own text first, then cross-checked against the
  actual vendored C reference source inside the `zstd-sys` crate for the
  parts where the RFC's prose alone left real ambiguity:
    - **Bitstream direction.** FSE- and Huffman-coded data is read
      *backward* from a sentinel bit (the highest set bit of the buffer's
      last byte); a `BackwardBitReader` walks a single global LSB-first
      bit index downward from there. FSE table *descriptions* (the
      probability distributions themselves) are read *forward* instead -
      a second, much simpler `ForwardBitReader` - confirmed by tracing
      RFC 8878's own "0145" Huffman worked example (bytes `0x10, 0x0D`)
      by hand, bit by bit, before trusting either reader.
    - **FSE table construction.** `fse_read_ncount` (the probability-table
      parser) and `fse_build_table` (the spread-and-assign decode-table
      builder) were checked digit-for-digit against RFC 8878's own worked
      examples (its Accuracy_Log=8 probability-decoding example, and its
      Table 21 baseline/Number_of_Bits worked example) *and* against
      `FSE_readNCount_body`/`FSE_buildDTable_internal` in zstd's vendored
      `entropy_common.c`/`fse_decompress.c` - the RFC's own more manual
      "sort states, assign widths" description and the C reference's
      simpler incremental-per-symbol-counter approach were proven
      equivalent by hand before the simpler one was implemented.
    - **The predefined LL/ML/OF distributions** (RFC 8878 3.1.1.3.2.2) are
      hand-transcribed as `const` arrays, but their fully-*built* decode
      tables are also hardcoded directly from RFC 8878's own Appendix A
      (which states outright that its tables exist "to crosscheck that an
      implementation has built its decoding tables correctly") - a unit
      test builds each from its raw distribution via this project's own
      `fse_build_table` and asserts the result matches Appendix A's
      tables exactly, so the general table-builder (needed anyway for
      `FSE_Compressed` mode) gets proven correct against an independent
      source, not just self-consistency.
    - **The repeat-offset state machine** (RFC 8878 3.1.1.5) - resolving
      offset codes 0 and 1 into one of three "repeat" slots, with a
      documented but easy-to-mistranspose exception when the current
      sequence's literals length is zero - was the one place the RFC's
      own worked example (Table 18) didn't reconcile with a first attempt
      at the prose description. Rather than keep guessing at the table,
      `ZSTD_decodeSequence` in the vendored `zstd_decompress_block.c` was
      read directly: offset codes ≥2 resolve via a precomputed `OF_base`
      array that already has the RFC's "-3" folded in
      (`OF_base[code] = (1<<code) - 3`, confirmed against several code
      values by hand); codes 0 and 1 resolve through `prevOffset[]`
      indexed directly by a small computed selector (`ll0`, or
      `1 + ll0 + extrabit`), which - once traced through by hand for
      every one of its four sub-cases (code 0 with ll≠0/ll==0, code 1
      with ll≠0/ll==0) - matches the RFC's *prose* description of the
      shift-by-one exception exactly, even though it never reconciled
      with the specific numbers in the RFC's own Table 18 (left
      unresolved rather than chased further, since the C reference is the
      actual, deployed, battle-tested implementation and the real
      correctness arbiter used throughout this validation - real,
      independently-produced compressed files - is what a hand-transcribed
      table's numbers can't substitute for).
    - **A genuine, sequence-vs-FSE-internal field mix-up** was caught before
      it ever reached real-file testing: a literals-length/match-length/
      offset *value*'s own extra-bit count (`LL_bits`/`ML_bits`/the offset
      code itself) is completely separate from the FSE table entry's own
      state-transition `nb_bits`/`baseline` (used only to advance to the
      *next* FSE state) - conflating the two, an easy mistake since both
      are just "some bits associated with this table entry," was found and
      fixed by re-reading `ZSTD_decodeSequence`'s own `llBits`/`llnbBits`-
      style dual naming before it shipped.

  **Real-world corpus validation** (the same practice used for every
  other format's own validation pass, see below): every one of this
  project's 129 committed test fixtures, compressed via the real `zstd`
  CLI at three levels (1/3/19) each - 387 files total - decompresses
  byte-exact through this decoder. A purpose-built larger fixture
  (`tests/fixtures/edge_zstd_dynamic_tables.csv.zst`, 3,000 rows) is what
  actually found this decoder's one real, shipped bug: an off-by-one in
  `fse_read_ncount`'s accuracy-log recompute
  (`nbbits = bit_length(remaining - 1)` instead of the correct
  `bit_length(remaining)`) that only misbehaves when `remaining` lands
  exactly on a power of 2 - invisible on every small fixture (none of
  which happened to hit that exact boundary) and even on RFC 8878's own
  worked example (which happens not to cross a power-of-2 boundary
  either), but real and reproducible on large, real-content files, the
  same "small fixtures alone don't stress this" lesson every other
  hand-roll in this project has already hit at least once. A second, real
  discrepancy - not a bug in the decoder itself - was found by comparing
  behavior against the actual `zstd` CLI: a genuinely zero-byte `.zst`
  file was being silently treated as valid empty content, while real
  `zstd -d` correctly rejects it ("unexpected end of file"), since even
  an empty stream needs its mandatory 4-byte magic number; fixed to match.
  A 500-iteration bit-flip fuzz pass (1-20 random bit flips each, across a
  real 158 KB compressed source-code fixture) produced zero panics and
  zero out-of-bounds accesses - every corrupted input failed with a
  clean, actionable error - confirming the decoder's error handling, not
  just its happy path, holds up under adversarial input the same way
  every other hand-rolled reader in this project has already been proven
  to. Content-checksum verification (RFC 8878's optional XXH64 trailer,
  hand-rolled the same as CRC32 was for gzip) was confirmed to genuinely
  discriminate, not just be present in the code, via a deliberately
  corrupted checksum byte that's correctly rejected.
  `tests/fixtures/malformed_zstd_checksum.csv.zst` and
  `zstd_with_a_corrupted_checksum_gives_an_actionable_error_not_a_panic`
  lock this in as a permanent regression test, alongside
  `zstd_with_fse_compressed_tables_reads_correctly_end_to_end` (the
  dynamic-tables fixture, through the full CLI pipeline) and
  `zstd_reader_matches_the_zstd_crate_output_exactly` (a direct
  byte-for-byte cross-verification against the real `zstd` crate, kept as
  a dev-only oracle the same way every other hand-roll in this section
  keeps its own replaced crate around for exactly this purpose).

  Deliberately out of scope, matching this decoder's actual real-world use
  case here (decompressing whatever `.zst` file a user points this tool
  at, never producing one): dictionary support (this project's own `.zst`
  reading never needs it), and encoding of any kind.

- **`npyz` → a hand-rolled NumPy `.npy`/`.npz` reader (`npy_support`).**
  Unlike most of this list, npyz's own on-disk format is small and
  fully specified rather than algorithmically complex - the real work
  here was replacing its typed Rust API (`DType`/`TypeStr`/`Field`,
  built from a general Python-literal parser via the separate
  `py_literal` crate) with narrow, purpose-built equivalents that only
  need to understand the exact grammar `numpy.save` actually emits, the
  same "just enough, not a general evaluator" scope every other small
  parser in this project already keeps (`ini_support`, the OOXML-scoped
  half of `xml_support`). A minimal recursive-descent `PyParser` reads
  only what an NPY header dict can ever contain - a top-level `{...}`
  dict, nested `(...)`/`[...]` sequences (treated identically; NPY never
  cares which), single/double-quoted strings with backslash escapes,
  `True`/`False`, and signed integers - verified against four real
  `numpy.save` outputs inspected byte-for-byte (a plain 1D array, a
  structured/record array, a Fortran-order 2D array, and a record field
  with its own fixed-size sub-array shape, e.g.
  `('vec', '<f4', (3,))`) rather than assumed from the NPY format spec's
  prose alone. `TypeStr` parsing (endianness character + type character +
  byte width, with `U`'s own byte-width-is-4×code-point-count rule) and
  the record-dtype-from-a-list-of-2/3-tuples conversion were checked
  field-by-field against npyz's own `type_str.rs`/`header.rs` before being
  trusted, the same "verify against source" discipline as every other
  hand-roll in this file - not just re-derived from the spec and assumed
  correct.

  `.npz` reuses `zip_support::ZipArchive` (see that module's own entry
  above) rather than a second ZIP reader or the `zip` crate npyz's own
  `npz` feature depended on - a real simplification this hand-roll
  enabled, since `xlsx` and `npy` are independently-toggleable features
  that both only ever needed a small, generic "open by name, decompress,
  CRC-check" ZIP reader with no format-specific divergence between them
  (unlike the deliberately *duplicated* OOXML-scoped and general-purpose
  XML parsers, which do genuinely disagree on namespace handling - see
  that pair's own entry above for why duplication was the right call
  there and sharing is the right call here). `ZipArchive` itself moved
  out of `xlsx_support` into a new `zip_support` module gated
  `#[cfg(any(feature = "xlsx", feature = "npy"))]` so either feature
  alone still compiles it without requiring the other.

  Verified two ways: dedicated cross-verification tests
  (`npy_reader_matches_the_npyz_crate_output_exactly`,
  `npz_reader_matches_the_npyz_crate_output_exactly`) compare this
  reader's output against `npyz` itself (kept as a dev-only oracle, the
  same treatment every other replaced crate in this section already
  gets) on this project's own fixtures plus a new
  `tests/fixtures/edge_npy_big_endian_and_subarray.npy` - covering two
  real numpy shapes with zero prior test coverage in this project, found
  while auditing the new reader against npyz's own source: a non-native
  (`>`) byte order (no existing fixture used one) and a fixed-size
  sub-array field (`DType::Array` nested inside a `DType::Record` - no
  existing fixture had one either); and, transiently and not committed
  (matching this project's usual real-world-corpus practice), against
  real scikit-learn-bundled datasets (Iris as a structured array, the
  Diabetes dataset as a plain 2D array, Wine via `np.savez`), a
  `savez_compressed`-produced archive (genuinely DEFLATE-compressed
  `.npz` entries, not stored), a big-endian array, and Unicode-string and
  fixed-width-byte-string arrays - all matched the `npyz` oracle exactly,
  and a 600-file bit-flip fuzz pass (300 each against a real `.npy` and a
  real `.npz`) produced zero panics.

- **`rmpv`/`rmp` → a hand-rolled MessagePack decoder (`msgpack_support`).**
  MessagePack's own wire format (msgpack.org's spec) is small and
  completely specified by one table - `rmp`'s own `marker.rs`, checked
  directly rather than recalled from memory, gives every marker byte's
  exact meaning in one place (fixint/fixmap/fixarray/fixstr's bit-packed
  ranges, the `0xc0`-`0xdf` fixed-purpose bytes, and the big-endian
  multi-byte length/value encodings) - so this hand-roll was mostly
  mechanical, a single `match` on the marker byte. The hand-rolled
  `Value` enum deliberately isn't a byte-for-byte port of `rmpv::Value`:
  integers collapse to a plain `Int(i64)` with a separate `UInt(u64)`
  used only for the one case that genuinely can't fit `i64` (a `uint64`
  marker whose value exceeds `i64::MAX`), mirroring the
  `as_i64().or_else(as_u64())` fallback every call site in this project's
  own `msgpack_value_to_json` already used - so the *behavior* matches
  exactly without needing `rmpv::Integer`'s own richer internal
  representation.

  Two adversarial-safety details were carried over deliberately, not
  independently discovered but *confirmed* by reading `rmpv`'s own
  decoder before trusting the port: a string/binary length field
  (`str32`/`bin32`'s length is a full `u32`) reads via
  `Read::take(len).read_to_end(&mut buf)` with only a 64 KiB
  pre-allocation cap, so a few bytes claiming gigabytes can't force a
  huge allocation before any real data backs it up - and, more
  significantly, `read_array`/`read_map` build their `Vec` with `.push()`
  in a loop rather than `(0..len).map(...).collect()`, because the latter
  would let an attacker-controlled `array32`/`map32` length (again, a
  full `u32`) size an eager allocation via the iterator's own size hint
  *before* a single element is decoded. This is a real, previously-fixed
  issue in this exact ecosystem, not a theoretical one - `rmpv`'s own
  `read_array_data`/`read_map_data` carry an identical fix with a comment
  linking a GitHub issue (3Hren/msgpack-rust#151) recording the original
  report. Both were verified as still closed in the new code, not just
  copied on faith: a handful of bytes claiming a `u32::MAX`-element
  array, string, or map each fail cleanly with a peak memory footprint
  under 1 MB (measured, not assumed).

  **Real-world/adversarial testing found one genuine bug, in the deeper
  pipeline this decoder feeds into, not in the decoding itself.** A
  MessagePack-decoded `serde_json::Value` tree never passes through
  `serde_json`'s own parser, so it never benefits from that parser's
  built-in recursion limit (the protection every plain `.json`/`.jsonl`
  file gets for free, confirmed directly: feeding the equivalent
  1-key-per-level nesting as JSON *text* fails cleanly at parse time with
  serde_json's own "recursion limit exceeded", well before reaching
  `profile_json_path` at all). `rmpv::decode::MAX_DEPTH` is 1024, and this
  project's own first draft matched it exactly on the theory that
  swapping decoders shouldn't change what depth a file is willing to
  accept - but real testing (a hand-built, genuinely 900-level-deep
  MessagePack structure, since Python's own `msgpack` packer recurses
  and hits *its own* limit before producing anything deeper) found a real
  stack overflow through the *compiled binary itself*, not just a unit
  test - confirmed to be a debug-build-specific stack-frame-size issue,
  not a logic bug (the identical structure decodes correctly in a
  `--release` build all the way past 1024, where the depth guard then
  correctly and cleanly rejects it) - but `cargo test` and a plain
  `cargo build`/`cargo run` both default to the debug profile, so this
  was a real, reachable gap, not a hypothetical one. Binary-searching the
  actual crash boundary in a debug build placed it between 700 and 900
  levels - and independently, `ciborium` (at the time still a real
  dependency, for this project's own CBOR reader - it's a dev-only
  cross-verification oracle now, see below) defaults to a 256-level
  recursion limit for exactly this reason, with its own doc comment
  reading "Set a high
  recursion limit at your own risk (of stack exhaustion)!" - real,
  independent corroboration this is a known risk class for this shape of
  recursive decoder, not something specific to this project's own code.
  `MAX_DEPTH` was lowered to 256 to match, giving comfortable margin
  under the empirically found danger zone while remaining far deeper than
  any legitimate MessagePack document would plausibly nest.
  `tests/fixtures/malformed_deeply_nested.msgpack` (50,000 levels, hand-
  built the same way the crash-reproducing fixture was, matching the
  scale of this project's own `malformed_deeply_nested.xml`) and
  `deeply_nested_msgpack_fails_cleanly_instead_of_a_stack_overflow` lock
  the fix in as a permanent regression test, run through the compiled
  binary rather than in-process specifically because that's how the
  original crash was actually reproduced.

  Verified two ways: `msgpack_reader_matches_the_rmpv_crate_output_exactly`
  cross-checks this decoder against `rmpv` itself (kept as a dev-only
  oracle, the same treatment every other replaced crate in this section
  already gets) on this project's existing fixtures plus a new
  `tests/fixtures/edge_msgpack_wide_markers.msgpack` - this project's
  existing MessagePack fixtures are all small enough to only ever
  exercise the fix-sized marker ranges (fixstr/fixarray/fixmap/fixint),
  so the wider str8/str16, array16, map16, bin8, and the `uint64`-
  exceeding-`i64::MAX` case had zero prior coverage, found the same way
  the big-endian/sub-array gaps were found while auditing the NumPy
  reader against its own predecessor's source; and, transiently and not
  committed (matching this project's usual real-world-corpus practice), a
  500-record synthetic dataset exercising nested maps/arrays/negative
  integers/large integers together, a `str16`/`array16`/`map16`-forcing
  document, an explicit `uint64` beyond `i64::MAX`, `bin8`/`bin16` binary
  payloads, and an `f32`-forced float - all matched the `rmpv` oracle
  exactly - plus a 400-file bit-flip fuzz pass against the 500-record
  dataset, zero panics.

- **`toml` → a hand-rolled TOML parser (`toml_support`).** The largest
  hand-roll in this section by grammar surface, not by algorithmic
  complexity - TOML's on-disk format is plain, well-specified text, but
  correctly implementing its *redefinition* rules (which key/table paths
  may be extended, and by which of dotted-keys vs. `[header]` vs.
  `[[header]]`, and in which order) turned out to be considerably subtler
  than the tokenizing itself, and got the rules wrong on a first read of
  the spec's own prose - see below. Bridges straight to `serde_json::Value`
  like `serde_norway`'s YAML replacement did (no intermediate `toml::Value`
  stand-in, since there's no behavior of the old dependency worth
  preserving beyond "produces the right JSON shape"), except for the
  *document-structure* layer (`[header]`/`[[header]]`/dotted-key
  resolution), which needs its own small tree type (`TomlNode`/
  `TomlTable`) carrying two extra bits of state per table beyond its
  entries.

  **The redefinition rules needed toml-test's own fixtures to get right,
  not just the spec text.** A first reading of the spec's prose
  ("dotted keys create tables... the `[table]` form can be used to define
  sub-tables within tables defined via dotted keys") led to implementing
  a single `explicit: bool` per table, closing it to *both* further
  dotted-key extension *and* `[header]` redefinition once set - which
  is wrong, confirmed by toml-test's own `append-with-dotted-keys-*`
  fixtures and, on a closer second read, by the spec's own worked example
  immediately preceding that sentence (`[fruit]` / `apple.color = "red"` /
  `apple.taste.sweet = true` / commented-out `# [fruit.apple]  # INVALID`)
  - the commenting-out convention used throughout the spec for "do not do
  this" examples was misread the first time as showing valid syntax. The
  real rule needs *two* independent flags: `via_header` (set only by
  `[header]`/`[[header]]`, closes the table to *dotted-key* traversal from
  any later statement, but not to more header-based sub-tables nested
  under it - the standard "supertable" pattern) and `dotted_owned` (set by
  *any* dotted-key statement touching the table, closes it to a *later*
  `[header]` redefinition, but never to more dotted-key extension - `a.b`
  can always be extended by further `a.b.x = ...` statements, a real and
  common pattern the spec's own worked example relies on). Both flags gate
  the same header-redefinition check (`via_header || dotted_owned`); only
  `via_header` gates the dotted-key-traversal check. Every
  `append-with-dotted-keys-*`/`redefine-*`/`duplicate-key-*` fixture in
  toml-test - the ones that actually exercise this interaction - passes
  with this two-flag model; the single-flag version failed several of
  them.

  **Multi-line string closing needed the same "verify against the
  corpus, not just the prose" treatment.** A run of quote characters at
  the end of a multi-line string can include up to 2 *literal* quotes
  immediately before the real 3-quote closing delimiter (`""""` closes as
  "1 literal quote + delimiter", per the spec's own `str4`/`str5`
  examples) - a first implementation generalized this as "however long
  the run, the last 3 close and everything before is literal," which
  wrongly *accepts* toml-test's own `multiline-quotes-01` invalid fixture
  (a run of exactly 6 quotes, which the spec intends as unrepresentable:
  the first 3 would already form a valid close, leaving 3 dangling quotes
  with nothing to attach to). Fixed by capping the literal-quote
  allowance at 2 (a run of 6+ is a hard error) - verified against
  toml-test's own `string/multiline-quotes.toml` (the positive case, runs
  of 4 and 5) and `multiline-quotes-01.toml` (the negative case, a run of
  6) together, not just whichever one was checked first.

  **Targets TOML 1.1.0** (matching the `toml = "1.1.4+spec-1.1.0"` crate
  being replaced), which relaxes several 1.0.0 rules: optional seconds in
  local time/datetime values (`13:37`, not just `13:37:00`), newlines and
  a trailing comma inside inline tables (previously single-line-only with
  no trailing comma), and two new string escapes (`\e` for ESC, `\xHH`
  for the first 256 code points). Datetime values are parsed into a small
  structured representation and *re-serialized* to match
  `toml_datetime::Datetime`'s own `Display` impl exactly (checked
  directly against its source, not assumed) rather than kept as the
  original substring - the reference crate always normalizes the
  date/time separator to `T` regardless of whether the source used a
  space (RFC 3339 permits both), and right-trims trailing zeros from
  fractional seconds, both of which a verbatim substring copy would get
  wrong for the cross-verification oracle test to catch.

  **Real-world/adversarial testing found the same class of stack-safety
  gap this pass already knew to check for** (having just found it in
  MessagePack's own decoder): TOML's array and inline-table grammar
  recurses with no depth limit of its own, and a hand-built
  `[[[...]]]`-nested array genuinely stack-overflowed a debug build
  somewhere between 5,000 and 10,000 levels - deeper than MessagePack's
  own danger zone (different recursion shape, lighter per-frame cost),
  but real and reachable all the same, since `cargo test`/`cargo run`
  both default to the debug profile that actually exhibits it. Capped at
  512 (`MAX_TOML_DEPTH`), matching this project's own XML depth guard for
  the same "comfortable margin, far deeper than any real document would
  nest" reasoning.

  Verified three ways: **(1)** the full `toml-lang/toml-test` conformance
  suite, filtered to exactly the files that list applies to a 1.1.0
  parser (`tests/files-toml-1.1.0`, itself part of the suite) rather than
  the full valid/invalid directories, which mix in 1.0.0-only fixtures
  this parser correctly disagrees with (e.g. the 1.0.0 suite marks
  optional-seconds datetimes and inline-table trailing commas as
  *invalid*, which 1.1.0 - and this parser - correctly accept) - 220/220
  valid files accepted, 494/494 invalid files rejected, zero panics.
  **(2)** `toml_reader_matches_the_toml_crate_output_exactly` cross-checks
  this parser against the real `toml` crate (kept as a dev-only oracle,
  the same treatment every other replaced crate in this section already
  gets) on this project's own fixtures plus a new
  `tests/fixtures/edge_toml_v1_1_features.toml` (the 1.1.0 features
  above, all with zero prior coverage in this project's fixtures); and,
  transiently and not committed (matching this project's usual real-
  world-corpus practice), against 41 real `Cargo.toml` files pulled from
  this machine's own crates.io registry cache plus this project's own -
  all matched the `toml` crate's output exactly, zero panics. **(3)** a
  400-file bit-flip fuzz pass against a real fixture, zero panics.
  `tests/fixtures/malformed_deeply_nested.toml` (50,000 levels, matching
  the scale of this project's other `malformed_deeply_nested.*`
  fixtures) and `deeply_nested_toml_fails_cleanly_instead_of_a_stack_overflow`
  lock the depth-guard fix in as a permanent regression test, run through
  the compiled binary rather than in-process, matching how the original
  crash was actually reproduced.

- **`ciborium` → a hand-rolled CBOR decoder (`cbor_support`).** RFC 8949's
  own byte format is a single small table, the same shape as MessagePack's
  - verified directly against `ciborium-ll`'s own `hdr.rs`/`dec.rs`
  (`pull_title`), not recalled from memory: an initial byte splits into a
  3-bit major type (0-7) and a 5-bit "additional info" that's either the
  value itself (0-23), a length-prefixed follow-on read (24/25/26/27 mean
  1/2/4/8 more big-endian bytes), reserved (28-30), or "indefinite length"
  (31). `read_value`/`read_value_from` are split the way they are
  specifically because a generic `Read` has no peek/pushback: decoding an
  indefinite-length array/map/bytes/text has to read the next byte to check
  for the `0xFF` break marker, and - if it isn't one - feed that
  already-consumed byte back into `read_value_from` rather than reading a
  fresh one.

  A genuinely different wire format from MessagePack under the surface
  similarity, in three specific ways that shaped the implementation:
    1. **Wider integers.** CBOR's own integer range is asymmetric and wider
       than `i64` on *both* ends (an unsigned major-type-0 value up to
       `u64::MAX`, a negative major-type-1 value down to `-1 - u64::MAX`) -
       confirmed directly against `ciborium::value::Integer`'s own internal
       `i128` representation (and its own test data,
       `neg!(-18446744073709551616)` round-tripping through raw bytes
       `3bffffffffffffffff`) rather than assumed, so `cbor_support::Value::Integer`
       is `i128`-based too, preserving the old ciborium-based reader's own
       already-correct `i64::try_from(..).unwrap_or_else(|_| i128::from(..).to_string())`
       overflow-to-string fallback exactly.
    2. **Real indefinite-length chunking** (RFC 8949 §3.2.3), a legitimate
       encoding no MessagePack marker has an equivalent for: an
       indefinite-length bytes/text value is a sequence of *definite*-length
       chunks of the *same* major type, concatenated, terminated by the
       break byte - a chunk of the wrong major type is a hard, disclosed
       error, matching the spec's own prohibition and verified directly
       against `ciborium`'s own `deserialize_byte_buf`/`deserialize_string`.
       UTF-8 is validated once over the fully-assembled text bytes, and -
       deliberately unlike MessagePack's looser hex-dump-on-invalid-UTF8
       fallback - invalid UTF-8 in a CBOR text item is a hard error, matching
       both RFC 8949's own requirement and `ciborium`'s confirmed behavior
       (`core::str::from_utf8`, propagated as an error, not lossily
       recovered).
    3. **Major type 7's mixed grab-bag** (booleans, null, "undefined",
       unassigned simple values, and three float widths sharing one major
       type) needed its own research pass on how `ciborium::Value` -
       which has no dedicated `Simple`/`Undefined` variant at all -
       represents each case, since the old reader's oracle behavior had to
       be matched exactly. Confirmed by reading `ciborium`'s own
       deserialization dispatch (`de/mod.rs`): `false`/`true` map to `Bool`,
       and - the one genuinely non-obvious finding - both `null` (info 22)
       *and* `undefined` (info 23) collapse to the identical `Value::Null`,
       routed through the same `deserialize_option`/`visit_none` path
       internally. Any *other* simple value (an unassigned info 0-19, or an
       out-of-range byte following the info-24 escape) is a hard decode
       error in `ciborium` (`Err(h.expected("known simple value"))`), not a
       silent fallback - `cbor_support` matches this exactly rather than
       guessing at a value for an encoding CBOR itself leaves undefined.
       Half-precision (binary16) floats (info 25) need their own
       conversion with no existing dependency to lean on (the `half` crate
       isn't otherwise used anywhere in this project, and adding it would
       be a new dependency contrary to the point of this hand-roll) -
       `f16_to_f64` converts via plain floating-point arithmetic rather
       than bit manipulation, hand-verified against eight known reference
       bit patterns (1.0, 2.0, -2.0, +inf, -inf, NaN, the smallest
       subnormal at 2^-24, the smallest normal at 2^-14) before being
       trusted.

  Tag handling (major type 6) was checked, not assumed, to need no special
  casing: `ciborium::value::de` only special-cases specific tags (bignum,
  etc.) when deserializing a `Value` *into some other target type* - when
  the target type *is* `Value` itself, every tag decodes uniformly
  (`Value::Tag(tag, Box::new(inner))`) regardless of its number, confirmed
  directly in its source. `cbor_support` does the same, and `value_to_json`
  keeps the old reader's own best-effort JSON rendering
  (`{"tag(N)": <inner>}`) unchanged for oracle compatibility.

  `MAX_DEPTH` (256) was set from the start to match `ciborium`'s own
  documented default recursion limit, rather than discovered after a crash
  the way MessagePack's was - by this point in the dependency-removal
  effort the debug-build stack-safety risk class (a hand-rolled decoder
  that bridges to `serde_json::Value` without ever passing through
  `serde_json`'s own parse-time recursion guard) was already known from
  the MessagePack and TOML hand-rolls, so this one was designed defensively
  up front. Verified empirically anyway, not just assumed safe by
  design: a hand-built 50,000-level-deep definite-length array
  (`tests/fixtures/malformed_deeply_nested.cbor`) fails cleanly on a debug
  build with no crash, and a boundary check (255 levels succeeds, exactly
  256 fails) confirmed the guard fires precisely where intended.

  Verified two ways: `cbor_reader_matches_the_ciborium_crate_output_exactly`
  cross-checks this decoder against `ciborium` itself (kept as a dev-only
  oracle, the same treatment every other replaced crate in this section
  already gets) on this project's existing fixtures; and, transiently and
  not committed (matching this project's usual real-world-corpus practice,
  via Python's independent `cbor2` library), a 20-record realistic dataset
  mixing negative/positive integers, floats, nested maps/arrays, byte
  strings, and a tag (date-time), plus dedicated hand-built-bytes fixtures
  for indefinite-length arrays/maps/bytes/text, integers beyond `i64`'s
  range on both ends, and half-precision floats - every one matched the
  expected values by hand before being trusted, and three of these
  (indefinite-length chunking, the `i128` integer range, and half-precision
  floats) are now permanent fixtures/tests
  (`edge_cbor_indefinite_length.cbor`, `edge_cbor_big_integers.cbor`,
  `edge_cbor_float16.cbor`) since none of them had any prior coverage even
  under the old ciborium-based reader. A 500-iteration bit-flip fuzz pass
  against the realistic dataset produced zero panics and zero hangs -
  every corrupted input either decoded successfully by coincidence or
  failed with a clean, actionable error.

- **`regex` → hand-rolled forward-scanning parsers (`weblog_support`/
  `syslog_support`).** The one dependency in this whole effort that never
  needed a general-purpose engine in the first place: `weblog`/`syslog`
  only ever construct a small, fixed number of hardcoded patterns (never a
  user-supplied or dynamically-built regex), so replacing `regex::Regex`
  meant writing purpose-built parsers for those specific grammars, not a
  general regex engine - a much smaller scope than every other hand-roll
  in this section, closer in spirit to `csv`'s hand-rolled state machine
  than to a from-scratch reimplementation of `regex` itself.

  Every field in all four patterns (Common Log, Combined Log, the nested
  `"METHOD path PROTOCOL"` request, RFC 5424) is delimited by a literal
  character, a fixed-width run, or an unambiguous dash-vs-something
  choice - so a single left-to-right scan (read a token, expect a literal,
  repeat) reproduces each pattern exactly with no backtracking required at
  all. `\d{3}` immediately followed by a literal space doesn't even need
  special handling for "what if there's a 4th digit": reading exactly 3
  digits and letting the *next* `expect_char(' ')` call reject a 4th digit
  in place reproduces the regex's own greedy-with-no-shorter-backtrack
  behavior for that specific shape - confirmed by reasoning through why
  `\d{3}` (fixed count, not a range) has no shorter alternative to
  backtrack to, not assumed. `<(\d{1,3})>`, by contrast, genuinely does
  need its explicit cap respected as a hard boundary (not "read digits
  then let the next check reject"): the group literally cannot consume a
  4th digit as part of itself regardless of what follows, so
  `read_digits_capped` enforces the max directly.

  **RFC 3164's `([^:\[]+?)(?:\[(\d+)\])?:` looked like it would need real
  backtracking (a non-greedy quantifier plus a trailing optional group),
  and turned out not to.** TAG's own character class already excludes `:`
  and `[` outright - not just "avoided while greedy," genuinely
  unable to consume either - so its true end is a hard structural
  boundary (the first `:` or `[` encountered scanning forward), not
  something a backtracking search has to discover by trial and error.
  Landing on `[` *requires* the bracket-digits-close pattern to fully
  match right there, with no fallback: TAG cannot extend past the `[` to
  try a different split point if the bracket doesn't parse cleanly, so a
  malformed bracket at that position fails the whole line, matching the
  regex's own lack of any viable backtrack there. This was reasoned
  through carefully before being trusted (not just implemented and hoped
  for) precisely because it's the one place in this hand-roll where
  getting the backtracking-equivalence argument wrong would have been
  easy to miss in testing - real syslog tags are simple enough that a
  subtly-wrong implementation could still pass on ordinary input.

  Verified two ways: **(1)**
  `weblog_reader_matches_the_regex_crate_output_exactly` and
  `syslog_reader_matches_the_regex_crate_output_exactly` cross-check both
  parsers against the real `regex` crate (kept as a dev-only oracle, the
  same treatment every other replaced crate in this section already gets)
  on this project's existing fixtures, including the trickiest real cases
  already on file - RFC 3164 with no `<PRI>` prefix, a tag containing a
  space (`"syslogd 1.4.1"`), a message with embedded colons and brackets
  (`"[12345.678] eth0: link up"`), RFC 5424 with and without a numeric UTC
  offset. **(2)** transiently and not committed (matching this project's
  usual real-world-corpus practice), against the exact same two real
  files this project's own prior real-world validation pass already used
  for these formats (see the "tenth pass" entry above) - `loghub`'s real
  1,999-line `Linux_2k.log` (production `/var/log/messages` excerpt) for
  RFC 3164, and Elastic's real 10,000-line Apache Combined Log dataset -
  both re-fetched fresh rather than assumed unchanged. Every RFC 3164 line
  matched the `regex` oracle exactly; the Apache dataset matched on all
  9,999 well-formed lines and failed on the *same* single line both times
  (line 8899, a literal missing closing quote on the user-agent field -
  genuine pre-existing data corruption, not a parser disagreement,
  confirmed identical to this project's own prior documented finding for
  this exact file). **(3)** a 1,200-line randomized-mutation fuzz pass
  (character insertion/deletion/replacement/swap applied to one baseline
  line per grammar, 300 mutations each) compared *both* parsers' verdicts
  on every mutated line - not just "does it panic," but "do the two
  parsers agree on match-or-no-match, and on every extracted field when
  they do" - with zero disagreements across all 1,200 cases, and
  separately zero panics/hangs confirmed via the compiled binary directly
  on the same corpus.

- **`dbase` → a hand-rolled reader (`dbase_support`).** Unlike every other
  crate declined in this project's own "No DuckDB"/"No SPSS" reasoning
  below, dBase's own on-disk format turned out genuinely simple to hand-
  roll once actually read rather than assumed complex by association with
  "a database format": a fixed 32-byte header, a fixed 32-byte-per-field
  descriptor table (count derived arithmetically from the header's own
  declared offset to the first record, not a scan for the conventional
  `0x0D` terminator byte - which is read but, matching the `dbase` crate's
  own explicit choice, never actually checked against that value), and
  fixed-length ASCII/binary records with a single leading deletion-flag
  byte. Verified field-by-field against the `dbase` crate's own
  `header.rs`/`field/mod.rs`/`field/types.rs`/`reading.rs`/`file.rs`
  rather than assumed from a spec summary - this surfaced several
  behaviors worth calling out because they're easy to get wrong by
  reasoning from "how would I design this" instead of reading the actual
  crate:
    1. **The record's real on-disk size is recomputed from the field
       table's own summed lengths, and the header's own separately-stored
       record-length field is read but never trusted** - confirmed
       directly in `open_dbase`'s own comment ("Some files seem not to
       include the DELETION_FLAG_SIZE into the record size, but we rely on
       it"). This reader does the same recomputation, not a shortcut that
       happened to look equivalent.
    2. **A DateTime field's on-disk representation is a Julian Day Number
       plus a milliseconds-since-midnight word, not this project's own
       Unix-epoch-day convention** - but the two are related by exactly
       one fixed, well-known constant (JDN 2,440,588 is 1970-01-01 itself,
       confirmed directly against the `dbase` crate's own
       `Date::to_unix_days`), so `dbase_support` reuses this project's
       already-verified `civil_from_days` after one subtraction, rather
       than re-deriving the crate's own separate Julian-day arithmetic
       (Howard Hinnant's algorithm proving out again, the same way it
       already did for Excel's 1900-epoch date serials).
    3. **Text decoding is genuinely limited in the exact same way the
       `dbase` crate's own *default* build already is, not a new gap this
       hand-roll introduces.** A dBase file's header carries a code-page
       marker byte; correctly decoding any of the ~20 *named* legacy
       single-byte code pages (CP437, CP1252, CP932, ...) needs a real
       per-codepage byte-to-codepoint table, which the `dbase` crate only
       provides behind two *optional* features (`yore`/`encoding_rs`) -
       and this project's own prior `Cargo.toml` entry for the crate never
       enabled either. Confirmed directly in `CodePageMark::to_encoding`
       (`header.rs`): without those features, every named code page
       resolves to `None`, and `open_dbase` hard-errors with
       `UnsupportedCodePage` before a single record is ever read - only
       the UTF-8 marker (strict) and the undefined/unrecognized-byte case
       (lossy) ever actually worked. `dbase_support` reproduces this exact
       boundary rather than "fixing" it into a bigger hand-roll than
       verification could support - a real, disclosed limitation
       inherited faithfully, not silently narrowed further or quietly
       widened without the codepage tables to back it up.
    4. **Memo fields (external `.dbt`/`.fpt` files) are a disclosed,
       clear error** rather than an attempt at a second binary format
       this project has no committed fixture to verify against - the same
       "no fixture, no trust" boundary already drawn for SAS7BDAT and
       old-style BIFF2-5 `.xls`.
    5. **`trim_field_data`'s one real quirk was worth preserving exactly,
       not smoothing over**: its leading/trailing-space scan stops dead at
       the *first* NUL byte encountered anywhere in a field (not just a
       trailing one), so content after an embedded NUL is silently
       excluded - confirmed directly in the crate's own implementation,
       including the fact that only the `BeginEnd` trim variant (the
       crate's own default, and the only one this project's code ever
       uses) actually needs implementing.

  Verified two ways: **(1)**
  `dbase_reader_matches_the_dbase_crate_output_exactly` cross-checks this
  reader against the real `dbase` crate (kept as a dev-only oracle, the
  same treatment every other replaced crate in this section already gets)
  on this project's existing fixtures. **(2)** transiently and not
  committed (matching this project's usual real-world-corpus practice,
  re-fetched fresh rather than assumed unchanged), against the same real
  US Census Bureau TIGER/Line shapefile's `.dbf` component this project's
  own prior real-world validation pass already used (see the "eleventh
  pass" entry above) - 56 real state/territory records, zero panics,
  matching the `dbase` crate's output exactly via the same oracle test,
  and reproducing the identical FIPS-leading-zero and declared-`f64`-but-
  really-`i64` land/water-area findings that prior pass already
  documented. A 500-iteration bit-flip fuzz pass against that same real
  file produced zero panics and zero hangs. Four scenarios this project's
  existing fixtures never exercised - a soft-deleted record, genuine
  multi-byte UTF-8 field content, a named-code-page file (a disclosed
  error), and a Memo-field file (also a disclosed error) - are now
  permanent fixtures/tests (`edge_dbase_deleted_records.dbf`,
  `edge_dbase_unicode.dbf`, `malformed_dbase_unsupported_codepage.dbf`,
  `malformed_dbase_memo_field.dbf`), the first two additionally verified
  against the real `dbase` crate via the same oracle before being trusted.

- **`dta` → a hand-rolled reader (`stata_support`).** The largest hand-roll
  in this whole effort by scope, not by algorithmic complexity - Stata's
  own `.dta` format has been revised 18 times (releases 102-119, the
  `dta` crate's own documented ReadStat-derived range) across two
  genuinely different container shapes (a fixed binary layout for
  102-116, an XML-tagged layout for 117+), with real byte-width changes
  to variable names, display formats, and variable labels scattered
  across that history. Verified field-by-field against the `dta` crate's
  own `release.rs` (every version-dependent field width lives there as
  one clean comparison table, not scattered through the reader), rather
  than assumed from a spec summary - this is the same
  "read the actual crate before implementing" discipline as every other
  hand-roll in this section, just applied across a much wider version
  matrix than any format tackled here before.

  Several things worth calling out, each confirmed by reading the crate's
  own source rather than inferred from the format's reputation:
    1. **This project's usage never needs 23 of the crate's own 90-plus
       source files** - `columns_from_stata` only ever reads a header, a
       schema, and records, never writing, never the separate `.dct`
       dictionary format, never async I/O, never value labels or
       characteristics *content* (only their byte *extent*, to skip past
       them), and never strL *resolution* (just recognizing the reference
       shape). Scoping to exactly this usage surface - rather than porting
       the crate wholesale - is what kept an otherwise 18-release format
       tractable to hand-roll at all.
    2. **Missing-value detection never needs the crate's own 27-variant
       `MissingValue` enum.** This project's `raw_values` already treats
       every missing value identically (`None`, filtered out) regardless
       of *which* of Stata's 27 missing codes (`.`, `.a`-`.z`) a value
       carries - so `stata_support` only ever needs a boolean "is this
       value missing," derived directly from each numeric type's own
       range/bit-pattern check (verified against the `dta` crate's
       `stata_byte.rs`/`stata_int.rs`/`stata_long.rs`/`stata_float.rs`/
       `stata_double.rs`, each carrying its own precise sentinel table).
       Double's own history is the most intricate of the five: V104/V105
       used the exact bit pattern `0x54C0_0000_0000_0000` (2^333) as system
       missing - a value that falls *inside* the normal valid `f64` range,
       so it must be matched exactly rather than range-checked - while
       V106-112 switched to "any positive value above pandas' own
       `OLD_VALID_RANGE` maximum," and 113+ moved to reserved NaN bit
       patterns. All three eras are handled, not just the modern one.
    3. **A DTA `DateTime` field's on-disk value is a Julian Day Number
       plus a milliseconds-since-midnight word - the same underlying
       problem this project's dBase hand-roll already solved, and the
       same fix applies again**: JDN 2,440,588 is 1970-01-01, so
       subtracting that constant and reusing `civil_from_days` sidesteps
       porting the crate's own separate Julian-day arithmetic a second
       time in the same session.
    4. **Pre-118 files decode text as Windows-1252, and - unlike dBase's
       ~20 named legacy codepages, which needed a disclosed
       "unsupported" boundary - this needed no equivalent gap at all.**
       Stata only ever has this one legacy single-byte encoding to
       support (verified directly against `encoding_rs`'s own `data.rs`
       table, the same crate the `dta` dependency itself already used for
       this), so `stata_support` embeds that exact 128-entry table
       instead. The one genuinely non-obvious detail, also confirmed
       against the source rather than assumed: five bytes with no real
       windows-1252 assignment (`0x81`/`0x8D`/`0x8F`/`0x90`/`0x9D`) map to
       their own C1-control code point under the WHATWG encoding standard
       `encoding_rs` implements, rather than erroring or falling back to a
       replacement character the way Python's stricter `cp1252` codec
       does - cross-checked against Python's `cp1252` for the other 123
       bytes' *assigned* mappings before trusting the five-byte
       divergence as real rather than a transcription error.
    5. **A DoS-safety guard genuinely new to this hand-roll, not present
       in the crate it replaces**: unlike dBase's field count (inherently
       bounded by a `u16` header offset) or CBOR/MessagePack's per-value
       length prefixes (already guarded by this project's own established
       `PREALLOC_MAX` pattern), Stata's V119 variable count is a real,
       unbounded `u32` with nothing in the file format itself capping it -
       a corrupted or adversarial header could claim millions of
       variables and force a huge upfront allocation before a single byte
       of real schema data is read. `MAX_VARIABLES` (1,000,000, far above
       any real Stata file's variable count) and `MAX_ROW_LEN` (100 MB,
       guarding against that many maximum-width string variables even
       under the first cap) are checked before any buffer sized from
       either value is allocated - the same class of guard this project
       has now added independently to three different hand-rolled binary
       readers for the same underlying reason.
    6. **Skipping the characteristics section replicates the crate's own
       forward-compatibility rule exactly, not just the one entry type
       this reader knows about**: a binary-format expansion field's type
       byte can be `0` (terminator), `1` (a real characteristic), or -
       per the format's own documented allowance for future extension -
       anything else, and all three of the non-zero-terminator cases are
       skipped identically by byte count. Confirmed directly against the
       `dta` crate's own `characteristic_reader.rs`, which draws exactly
       this "any unrecognized type is safe to skip" boundary rather than
       treating an unknown type as a format violation.

  Verified two ways: **(1)**
  `stata_reader_matches_the_dta_crate_output_exactly` cross-checks this
  reader against the real `dta` crate (kept as a dev-only oracle, the
  same treatment every other replaced crate in this section already
  gets) on this project's existing fixtures - `sample.dta` (release 118,
  XML) and `type_detection.dta` (release 114, binary), the same two
  container shapes this format actually has. **(2)** transiently and not
  committed (matching this project's usual real-world-corpus practice),
  against six real files spanning four format releases and both
  container shapes: the same three official Stata-press teaching
  datasets this project's own prior real-world validation pass already
  used (`auto.dta`, `census.dta`, `nlswork.dta` - re-fetched fresh from
  Stata 18's current data page, landing on releases 118/117/118
  respectively, not necessarily the exact releases that earlier pass
  saw), plus three older exports of the same `auto.dta` teaching dataset
  pulled from Stata-press's own archived per-version data pages
  specifically to reach the *binary* container (releases 8/9's export is
  release 113, release 10-12's is 114, release 13's is already
  XML/117) - closing a real gap the three modern files alone would have
  left (all XML). Every one of the six matched the `dta` crate's own
  output exactly, including `nlswork.dta`'s ~28,000 rows. A 600-iteration
  bit-flip fuzz pass (300 each against a real binary-format and a real
  XML-format file) produced zero panics and zero hangs. No fix was
  needed anywhere in this pass - the cleanest real-world result of any
  hand-roll in this effort alongside TOML's own equally clean pass.

- **`apache-avro` (+ `num-bigint`) → a hand-rolled reader (`avro_support`).**
  Avro's own binary encoding turned out to be one of the *simplest* wire
  formats hand-rolled in this whole effort - no Huffman coding, no
  dictionary encoding, no page structure, just zigzag varints and
  concatenated fields - but its *schema* is one of the most elaborate:
  named-type registries, recursive self-reference, unions, and a dozen
  logical types layered on top of a handful of primitives. Verified
  directly against the `apache-avro` crate's own `reader/block.rs`
  (Object Container File structure), `util.rs` (zigzag varint encoding),
  `decode.rs` (per-type binary layout and the array/map negative-block-
  count convention), `codec.rs` (which codecs mean what, and - critically -
  that `apache-avro`'s own `Cargo.toml` already declares `snappy`/
  `zstandard` as real, always-enabled features of this project's existing
  dependency, not optional ones a user could have left off), and
  `schema/name.rs` (namespace resolution) - not assumed from the public
  Avro spec alone, since a hand-roll aiming for byte-exact oracle parity
  needs to match the *crate's* specific choices, not just "a" valid
  reading of the spec.

  Several things worth calling out, each a direct consequence of reading
  the source first:
    1. **Schema parsing and binary decoding merge into a single pass**,
       unlike every other nested format this project bridges through an
       intermediate dynamic value type (JSON, YAML, MessagePack, CBOR,
       TOML all decode to a generic value tree *first*, then flatten it
       against the schema *separately* - `avro_value_to_json`'s own old
       two-argument `(value, schema)` co-recursion, kept only as this
       hand-roll's oracle now). Avro doesn't need that: the schema is
       already in hand at every step of decoding, so `decode_to_json`
       converts straight to `serde_json::Value` as it reads, with no
       intermediate `Value` enum of its own at all.
    2. **A self-referential schema (a record naming itself inside one of
       its own fields - a real, common shape for tree/list-like data, not
       a contrived edge case) needs no `Rc`/`RefCell` graph-building
       trick.** Every named type (record/enum/fixed) is registered by its
       fully-qualified name into a flat `HashMap<String, Schema>` as it's
       parsed; a bare-name reference anywhere in the tree - forward,
       backward, or to itself - becomes a `Schema::Ref(name)` marker that
       is *never* resolved during parsing, only looked up lazily once
       decoding (which only starts after the whole schema has been fully
       parsed) actually reaches it. Since the name table is complete by
       the time any lookup happens, there's no chicken-and-egg ordering
       problem to solve at all - confirmed against a real, three-level-
       deep self-referential recursion (`edge_avro_named_type_refs.avro`,
       an "employee has a manager, who is also an employee" chain)
       flattening correctly at every level.
    3. **Two of Avro's three codecs this project already supported were
       already free.** Deflate is Avro's own raw-RFC-1951 (no gzip/zlib
       wrapper, no checksum) - exactly what this project's own `inflate`
       (built for gzip) already implements, reused directly. Zstandard is
       a standard zstd frame - exactly what `zstd_support` (built for the
       top-level `.zst` format) already decodes, reused directly by
       widening that module's own feature gate from `zstd` alone to
       `any(zstd, avro)`, the same "one decoder, two independent features"
       arrangement `zip_support` already has for `xlsx`/`npy`. Only
       Snappy - Avro's *most common* production codec in Kafka/Hadoop
       pipelines - needed a genuine new hand-roll.
    4. **Snappy's raw block format (not its separate, higher-level "frame"
       format, confirmed `apache-avro` uses the former via `snap::raw::*`)
       is a simple literal/copy scheme**, verified directly against the
       `snap` crate's own `decompress.rs` and the bit-layout table its
       `build.rs` generates: a tag byte's low 2 bits select a literal
       (length in the remaining 6 bits, or - if that field reads 60-63 -
       the real length minus one follows as 1-4 raw little-endian bytes)
       or a back-reference copy with a 1/2/4-byte offset. The trailing
       4-byte big-endian CRC32 checksum Avro's own snappy codec appends is
       the *standard* IEEE polynomial (`crc32fast`, confirmed in
       `codec.rs`) - not the Castagnoli variant `snap`'s own internal
       frame-format machinery uses elsewhere - so this project's own
       `crc32` (already hand-rolled for gzip) verifies it directly, no
       second checksum implementation needed.
    5. **A decimal's unscaled two's-complement bytes are converted to a
       scaled string via hand-rolled schoolbook long division**, not a
       bignum library - the same "just enough, not a general-purpose
       dependency" scoping as every other precisely-bounded conversion in
       this project. `apache-avro`'s own `BigDecimal` extension (whose
       scale is embedded *in the value*, via a nested length-prefixed
       bytes-plus-zigzag-long encoding, unlike the schema-carried scale of
       standard `decimal`) reuses the identical digit-shifting logic with
       a value-supplied, possibly negative, scale. This path carries
       lower verification confidence than the rest of this reader,
       matching the same disclosed gap this project's prior real-world
       Avro pass already recorded: no tool available while building this
       project's own fixtures can write `big-decimal` test data, so it's
       implemented directly from the crate's source rather than cross-
       checked against a real file - the same honest boundary already
       drawn for `Duration`'s own best-effort rendering.

  Verified two ways: **(1)**
  `avro_reader_matches_the_apache_avro_crate_output_exactly` cross-checks
  this reader against the real `apache-avro` crate (kept as a dev-only
  oracle, the same treatment every other replaced crate in this section
  already gets) on this project's existing fixtures plus a new
  `edge_avro_named_type_refs.avro` (generated with `fastavro`, matching
  this project's established fixture-generation convention) covering the
  named-type-reference/self-recursion mechanism above, which had zero
  prior coverage even under the old apache-avro-based reader. **(2)**
  transiently and not committed (matching this project's usual real-
  world-corpus practice), against the same corpora this project's own
  prior Avro validation pass used - the Apache Avro project's own
  `weather.avro`/`weather-snappy.avro`/`weather-zstd.avro` interop
  fixtures and the five `userdata*.avro` files from the widely-used
  Teradata/kylo sample-data collection - all re-fetched fresh, all eight
  matching the `apache-avro` crate's own output exactly via the same
  oracle. A 900-iteration bit-flip fuzz pass (300 each against the
  uncompressed, snappy, and zstd real files) produced zero panics and
  zero hangs - the low proportion of inputs that still decoded
  successfully after mutation (most bit flips landed in the schema's own
  JSON text, correctly producing a clean parse error rather than a
  crash) is expected for a format whose header is largely human-readable
  text, not a gap.

- **`rusqlite` → a hand-rolled reader (`sqlite_support`).** The one
  crate in this whole effort where "read the crate's own source" - the
  discipline behind every other hand-roll in this file - genuinely
  doesn't apply: `rusqlite` is a thin FFI binding over the real, linked
  SQLite C library, not a Rust reimplementation of the on-disk format, so
  there's no Rust source tree that actually describes the file's byte
  layout. The authoritative source here was SQLite's own published file-
  format specification (sqlite.org/fileformat2.html, cross-checked
  against sqlite.org/datatype3.html for affinity rules) instead - fetched
  and quoted directly rather than recalled from memory, the same
  discipline every other hand-roll in this file applies to a crate's
  source, just pointed at a different kind of authoritative document.

  Scope is deliberately narrow, matching exactly what this project's own
  usage ever asked SQLite to do: list user tables from `sqlite_master`,
  then an unfiltered, unordered full table scan (`SELECT * FROM t`) per
  table - no `WHERE`/`JOIN`/aggregation/index lookups of any kind. That
  narrowness is what makes a full embedded-database engine tractable to
  hand-roll at all: only the **table b-tree** (never an index b-tree)
  ever needs walking, and a plain depth-first left-to-right traversal of
  its cells is already exactly the rowid-ascending order a real, index-
  free `SELECT *` returns.

  The format itself layers cleanly: a 100-byte file header (magic string,
  page size - including the special `0x0001` encoding for 65536, and
  reserved-bytes-per-page, which together give the *usable* page size
  every offset formula below is computed against); a big-endian varint
  (up to 9 bytes, the first 8 contributing 7 bits each behind a
  continuation bit, the 9th contributing a full 8 bits with none) used
  throughout for lengths, rowids, and record header fields; b-tree pages
  (an 8- or 12-byte header plus a cell-pointer array) of four types, of
  which only two - table leaf (`0x0d`) and table interior (`0x05`) -
  are ever walked, since a table using the other two (an index b-tree)
  means `WITHOUT ROWID` storage (see below); a table leaf cell (varint
  payload size, varint rowid, initial payload bytes, and - only past a
  computed local-payload threshold - a 4-byte pointer into a linked list
  of overflow pages, each a 4-byte next-page pointer plus payload bytes);
  and, inside that payload, SQLite's own record format (a varint-prefixed
  header of one serial-type varint per column, each code mapping to a
  fixed-width integer/float, one of two zero-width shortcuts for the
  integers 0/1, or a length-derived BLOB/TEXT). Every formula (`X = U -
  35` for the max local payload, `M = ((U-12)*32/255) - 23` for the
  minimum, `K = M + ((P-M) % (U-4))` for the actual local size once a
  payload overflows) is sqlite.org's own, quoted directly rather than
  re-derived, the same "verify against source" bar every other hand-roll
  in this file already holds itself to.

  Two things genuinely beyond raw b-tree/record decoding were needed to
  match `SELECT *`'s real behavior, both found by testing against
  `rusqlite` rather than reasoned out in advance:
    1. **Column names have no home in the record format at all** - a row
       is just positional values, so the table's own `CREATE TABLE`
       statement (stored as a `TEXT` column in `sqlite_master`) has to be
       parsed to get column names in order. `parse_create_table` is
       deliberately not a general SQL parser, the same "just enough, not
       a general evaluator" scope every other small hand-rolled parser in
       this project keeps (`ini_support`, `toml_support`'s document-
       structure layer): a quote-and-comment-aware character scanner
       finds the top-level `(...)` column-list span, splits it into
       comma-separated items at paren-depth zero (so a `CHECK(a > 0)` or
       `DECIMAL(10,2)`'s own internal comma never causes a false split),
       and classifies each item as a table-level constraint (skipped) or
       a column definition (first token = the name, quoted via
       `"..."`/`` `...` ``/`'...'`/`[...]` or bare). This also has to
       resolve two more real SQLite behaviors the record format alone
       can't reveal: an `INTEGER PRIMARY KEY` column (inline, or a
       single-column table-level `PRIMARY KEY(col)` referencing an
       `INTEGER`-typed column, per SQLite's own documented rule - checked
       for a trailing `DESC`, which specifically disables the alias) is a
       rowid alias, so a `NULL` serial type in *that* column's record
       position means "use the cell's own rowid," not a genuine null; and
       `WITHOUT ROWID` in the table's tail (after the column list) means
       the table is stored as an index b-tree keyed by its declared
       primary key instead - a real, disclosed, unsupported shape (see
       below), not a guess.
    2. **A real oracle-comparison mismatch, not a hypothetical one,
       caught a genuine SQLite storage-format subtlety the file-format
       page alone doesn't mention**: this project's own `sample.sqlite`
       fixture's `amount REAL` column, cross-checked against `rusqlite`,
       disagreed on one row's `current_type` - `mixed(String: 1, f64: 1,
       i64: 1)` from this reader versus `mixed(String: 1, f64: 2)` from
       `rusqlite`. Tracing it down (and confirming against
       sqlite.org/datatype3.html directly rather than assuming) landed on
       a specific, well-documented optimization: "a column with REAL
       affinity... forces integer values into floating point
       representation... small floating point values with no fractional
       component and stored in columns with REAL affinity are written to
       disk as integers in order to take up less space and are
       automatically converted back into floating point as the value is
       read out. This optimization is completely invisible at the SQL
       level and can only be detected by examining the raw bits of the
       database file" - exactly the raw bits this reader examines
       directly. `column_affinity_is_real` implements sqlite.org's own
       five-rule "Determination Of Column Affinity" algorithm (checked in
       order: `"INT"` → INTEGER, `"CHAR"`/`"CLOB"`/`"TEXT"` → TEXT,
       `"BLOB"` or empty → BLOB, `"REAL"`/`"FLOA"`/`"DOUB"` → REAL,
       otherwise NUMERIC - substrings quoted verbatim from the spec, not
       paraphrased) against each column's declared type (bounded to just
       the type-name span via `first_keyword_boundary`, since a type can
       be multiple words like `DOUBLE PRECISION` or `UNSIGNED BIG INT`,
       and a real column-constraint keyword like `DEFAULT`/`CHECK` must
       not be swept into the affinity check), and `apply_affinity`
       converts an integer-serial-type value back to a float exactly when
       the owning column has REAL affinity - matching the one affinity
       documented to do this on *read*, deliberately not extended to
       NUMERIC affinity's own similar write-time integer-storage
       optimization, which sqlite.org does *not* document as being
       converted back (a well-known asymmetry, and the reason "declare
       REAL if you want floats back" is common SQLite advice).

  **WAL (write-ahead log) reconciliation is a deliberate, disclosed scope
  boundary, not a silent gap.** The real SQLite C library transparently
  merges a `-wal` sibling file's committed-but-not-yet-checkpointed
  frames into what a reader sees on every open; reimplementing that would
  mean parsing a second file format and its own frame/checksum layout for
  a case this project's own usage (profiling a data file, typically
  captured or shipped at rest) rarely exercises. Rather than silently
  serve stale data - a wrong answer with no indication anything was
  missed - `check_no_pending_wal` looks for a sibling `-wal` file and, if
  it carries more than just its own 32-byte header (i.e. at least one
  real frame), hard-errors with an actionable message (checkpoint the
  database, or close every connection cleanly, first) rather than guess.
  A `WITHOUT ROWID` table gets the same "clean, disclosed boundary" over
  a silent wrong answer, but the *other* failure-isolation treatment this
  project already uses elsewhere (a bad Parquet nested column, a bad
  `.npz` array): one table using it doesn't take down the rest of the
  file - `profile_table`'s error is caught per-table and turned into a
  single placeholder column carrying a clear note, exactly like the
  `.npz` per-array isolation pattern.

  Verified two ways: **(1)**
  `sqlite_reader_matches_the_rusqlite_crate_output_exactly` cross-checks
  this reader against `rusqlite` itself (kept as a dev-only dependency for
  exactly this purpose - genuinely a different codebase, not just a
  different Rust parser of the same spec, unlike every other oracle in
  this file) on this project's existing fixtures plus two new ones this
  pass added: `edge_sqlite_overflow_pages.sqlite` (a 15,000-byte `TEXT`
  value, well past the ~4,061-byte local-payload threshold on a default
  4,096-byte page, alongside an ordinary short value in the same table -
  exercising both the overflow-chain-assembly path and the plain local-
  payload path together) and `edge_sqlite_table_level_primary_key.sqlite`
  (a table-level `PRIMARY KEY(id)` rowid alias, as opposed to the inline
  `INTEGER PRIMARY KEY` form). A third new fixture,
  `edge_sqlite_without_rowid.sqlite`, deliberately isn't run through the
  oracle comparison - a `WITHOUT ROWID` table is expected to diverge from
  `rusqlite`'s real data by design - and instead gets its own dedicated
  test confirming the disclosed-placeholder shape. **(2)** transiently
  and not committed (matching this project's usual real-world-corpus
  practice), against two well-known real sample databases already
  referenced elsewhere in this document - a fresh copy of Chinook (246
  pages, ~1 MB) and the Northwind SQLite port (6,031 pages, ~23.6 MB,
  large enough to force genuine multi-level b-tree interior pages and
  real overflow chains) - both matched `rusqlite`'s output exactly via
  the same oracle, including the just-fixed REAL-affinity conversion on
  Northwind's own numeric columns. A 500-iteration bit-flip fuzz pass (1-
  20 random bit flips each) against the real Chinook file, run through
  the compiled release binary, produced zero panics, zero hangs, and no
  unexpected exit codes.

- **`sas7bdat` → a hand-rolled reader (`sas7bdat_support`).** SAS
  Institute never published a file-format specification, so - unlike
  every hand-roll before it in this list, and like SQLite's own spec-vs-
  FFI-binding distinction but for a different underlying reason - there
  was no single authoritative document to verify against. The reference
  crate's own extensively-commented source stood in for one instead: many
  of its comments record a specific real-world fixture that caught a
  specific bug, which is exactly the kind of hard-won detail worth
  carrying forward rather than re-deriving from first principles. Scope
  is deliberately narrower than the reference crate's own: that crate is
  built for high-throughput batch/SIMD scanning across several
  performance-motivated execution-class fast paths; this reader collapses
  all of that back into the one underlying mechanism every file actually
  uses - walk a page's subheader pointers (if any), decompress or borrow
  whatever they reference, then fall back to whatever contiguous row
  bytes remain on the page - since a single straightforward full-table
  read has no need for a fast-path split at all. Variable/value labels
  aren't surfaced, the same considered decision as Stata's own.

  The format layers similarly to Stata's own binary form (unsurprising,
  since both are proprietary statistical-package formats of a similar
  vintage): a fixed header (32-byte magic, an alignment-offset byte pair
  that determines 32- vs 64-bit pointer width, an endianness byte, a
  numeric text-encoding code, header/page sizes each independently
  sanity-range-checked); pages classified by a bitmasked type field into
  Meta/Data/Mix/Amd/Meta2/Comp/CompTable/Unknown; a page's subheaders
  (if any) reached via a pointer array immediately following its header,
  each pointer carrying an offset/length/compression-mode triple *plus* a
  separate flag byte (`is_compressed_data`); and, inside a subheader, a
  fixed 4-byte signature (0xF7F7F7F7 for row-size metadata, 0xF6F6F6F6
  for column count, and five more `0xFFFF_FFxx`-shaped constants for
  column name/attribute/format/text entries) dispatching to one of a
  handful of known layouts. Every constant, offset, and struct layout was
  read directly from the reference crate's own `probe.rs`/`pages.rs`/
  `layout.rs`/`internal.rs` before being trusted, the same discipline
  applied to every other hand-roll's source crate in this list.

  Two real bugs shipped in this reader's first draft, both found only
  because the oracle comparison was run against genuine files rather than
  trusted on the strength of the port - the exact scenario this project's
  own real-world-validation discipline exists for:
    1. **A missing pointer field, not a missing case.** The reference
       crate's row-extraction pointer struct carries a 4th field
       (`is_compressed_data`) that this reader's first draft simply never
       read, having been modeled on the *metadata*-parsing pointer shape
       (which only ever needs 3 fields, since it never reaches the
       "otherwise treat as row data" fallback at all). Without that flag,
       a compression-mode-0 pointer whose signature wasn't one of the
       known metadata signatures was *always* treated as raw row data -
       when the real rule requires `is_compressed_data` to be set too.
       The result was real, reproducible corruption on real files
       (`productsales.sas7bdat`, `cars.sas7bdat`, `load_log.sas7bdat`):
       metadata bytes sliced into row-length chunks and decoded as
       numeric values, producing a handful of giant near-zero floating-
       point strings (`f64::from_bits` on effectively-random bytes)
       spliced into otherwise-correct columns, and non-existent extra
       text values inflating `missing_pct`.
    2. **Two fields with the same name, different scope.** A `Mix`
       page's trailing data row is capped by `rows_per_page` - a
       *file-wide* value from the ROW_SIZE metadata subheader - while a
       `Data` page's rows are capped by its own *per-page* header field.
       This reader's first draft used the per-page field for both page
       kinds (since a Mix page's own header carries a superficially
       similar-looking count field at the same offset), and had never
       parsed `rows_per_page` from the ROW_SIZE subheader at all. Without
       the correct file-wide cap, a Mix page's leftover unused space past
       its one genuine trailing row - padding, whenever it happened to be
       an exact multiple of the row length - read as extra phantom rows.
       This produced the identical symptom as bug 1 on the same real
       files (garbage values spliced into real columns), which is what
       made it easy to mistake for a single bug at first - only after
       fixing the pointer-flag issue and re-running the oracle comparison
       did the *second*, independent cause become visible on the same
       fixtures.
  A third, non-panic-inducing but still real finding: `encoding_rs` (the
  crate the reference implementation delegates text-encoding resolution
  to) implements the WHATWG Encoding Standard, which - for real-world
  web-compatibility reasons documented in the standard itself, since
  virtually all content ever labeled "ISO-8859-1" in practice is actually
  Windows-1252 - defines `"iso-8859-1"` as a plain *alias* for the
  `windows-1252` decoder, not genuine Latin-1. This reader's first draft
  implemented true Latin-1 instead (a reasonable-looking, wrong
  assumption), caught by an oracle mismatch on a real fixture
  (`test16.sas7bdat`) whose CJK/Cyrillic/Hangul text only diverged from
  the reference crate's own output in the specific high-byte range this
  quirk affects - confirmed directly against `encoding_rs` itself
  (`Encoding::for_label(b"iso-8859-1").name()` returns `"windows-1252"`)
  before trusting the fix. Text decoding is otherwise the same disclosed-
  boundary choice dBase's own reader already makes for its ~20 legacy
  codepages: UTF-8/US-ASCII and Windows-1252 (reusing the exact table
  already verified for Stata's own hand-roll) are decoded directly; every
  other named encoding SAS can declare (~70 more, including several
  genuinely complex multi-byte/stateful schemes - Shift-JIS, EUC-JP/KR,
  Big5, GB18030, ISO-2022-\*) is a clear, disclosed error rather than a
  guess, the same dependency-weight tradeoff already declined elsewhere
  in this file (see "No SPSS"/"No DuckDB").

  RLE (`"SASYZCRL"`) and RDC/binary (`"SASYZCR2"`) row decompression are
  ported algorithm-for-algorithm from the reference crate's own
  `compression.rs`, including its own worked byte-level test cases as
  this reader's own verification - the `wild_copy`/`wild_fill` SIMD-style
  buffer-overrun tricks are omitted (pure throughput optimizations,
  irrelevant to a single-pass full read), but every length/bounds check
  they sit alongside is kept, including the specific DoS-safety fix the
  reference crate's own comments document: RDC's fill/copy tokens can
  amplify a few input bytes into thousands of output bytes, so every
  emit is bounds-checked against the row's declared length *before*
  writing, not just compared after the fact. A SAS date/datetime/time
  value only converts to a real date if it's a whole number within the
  target field's integer range (`i32` for date/time, `i64` for datetime)
  - ported from the reference crate's own `try_i64_from_f64`/
  `try_i32_from_f64` - and, when it isn't (a genuinely fractional value,
  or one too extreme to represent), falls back to rendering as a plain
  number instead of a date. This fallback was a real gap in this reader's
  first draft (a plain epoch-offset conversion with no fallback and, more
  urgently, an `i64::MAX`/`MIN`-adjacent addition that could panic on
  overflow on adversarial input), found by an oracle mismatch on
  `dates_null.sas7bdat`'s own deliberately-extreme test value
  (`"253717747199.999"`, a genuinely fractional datetime the reference
  crate renders as a raw number rather than forcing a wrong date) and
  fixed to match, using saturating/checked arithmetic throughout so an
  out-of-range value degrades to the same fallback rather than crashing.

  Verified two ways: **(1)**
  `sas7bdat_reader_matches_the_sas7bdat_crate_output_exactly` cross-checks
  this reader against the real `sas7bdat` crate (kept as a dev-only
  dependency for exactly this purpose) on `sas7bdat_people_nonascii
  .sas7bdat` - a real file vendored from the reference crate's own MIT-
  licensed test fixtures (see `tests/fixtures/sas7bdat_PROVENANCE.md`),
  the same "vendor a real file when self-generation is genuinely
  impossible" call already made for the POI `.xlsb` fixtures, and a real
  gap this format had standing since it was first wired up (see Known
  limitations) - no tool available in this environment can *write* a
  genuine `.sas7bdat` file at all, so this is the first non-malformed-
  input fixture this format has ever had. **(2)** transiently and not
  committed (matching this project's usual real-world-corpus practice),
  against two corpora: the reference crate's own remaining bundled
  fixtures (its adversarial fuzz-regression files, each confirmed to
  fail identically on both readers rather than merely "not crash" on
  either) and, more substantially, all 30 files in pandas' own real
  `.sas7bdat` test corpus (`pandas/tests/io/sas/data` - genuinely diverse
  real and edge-case files: a corrupt file, zero-row and zero-variable
  datasets, wide multi-hundred-column tables, a multi-page metadata
  file, real production log data, non-ASCII text in several legacy
  encodings). Both fixes above were found on real files in this corpus,
  not synthesized in advance. Of the 30, 26 matched the `sas7bdat` crate's
  output exactly after both fixes; the remaining 4 use a text encoding
  outside this reader's deliberately-scoped support (`ISO-8859-15` on 3,
  `WINDOWS-1251` on 1) and fail with a disclosed, actionable error on
  both readers' own terms (this reader refuses cleanly; the oracle
  decodes them, which is expected - the scope boundary is intentional,
  not a bug). A 1,000-iteration bit-flip fuzz pass (500 each against two
  different real files, run through the compiled release binary)
  produced zero panics, zero hangs, and no unexpected exit codes.

- **`arrow`/`parquet` → a hand-rolled reader, in progress
  (`parquet_support`).** Unlike every entry above this one, this is not a
  finished hand-roll - it's the one still underway, explicitly chosen by
  the user over two narrower alternatives (a flat-columns-only reader
  leaving nested types and Arrow IPC/Feather on the `arrow`/`parquet`
  crates, or stopping the campaign here and keeping both crates outright)
  specifically because of its size: `arrow`+`parquet` together are the
  largest dependency in this project by a wide margin, and - uniquely
  among everything hand-rolled so far - Parquet's own footer metadata is
  encoded with Thrift's compact protocol, a real general-purpose
  serialization framework, not a single bespoke binary layout the way
  every other format here has been. Arrow IPC/Feather adds a *second*,
  entirely separate general-purpose framework (FlatBuffers) on top, not
  yet started. This entry will keep growing across sessions as more of
  the reader lands; treat it as a running log, not a finished writeup the
  way every other entry in this section is.

  **Phase A (this session): the Thrift compact protocol's read side, and
  Parquet's footer schema.** `parquet` itself hand-rolls its own minimal
  Thrift decoder rather than depending on a general Thrift library or
  code-generating from the official `.thrift` IDL at build time
  (documented in the crate's own `THRIFT.md`) - for the same reasons
  every other hand-roll in this project exists, per that file's own
  words: "performance and flexibility." That made it this phase's
  authoritative source, the same role a pure-Rust crate's source has
  played for every hand-roll before it: every field ID, enum
  discriminant, and union-variant encoding below was read directly from
  `parquet_thrift.rs` (the wire protocol itself) and
  `file/metadata/thrift/mod.rs` / `basic.rs` (Parquet's own struct/enum
  definitions, written as comments alongside the crate's hand-written
  serialization code, since there's no separate `.thrift` file in the
  crate to read structs from directly) before being trusted.

  The wire protocol: ULEB128 varints and zigzag-encoded signed integers
  (identical shape to Avro's own, a format this project already hand-
  rolled a Thrift-free decoder for); struct fields identified by a 1-byte
  header packing either a 4-bit field-ID *delta* from the previous field
  or (when the delta doesn't fit, i.e. exceeds 15, or the field ID
  actually decreases) a zero-delta marker followed by the field's full
  zigzag `i16` ID - with one real compact-protocol-specific quirk found
  by reading the reference source rather than assumed from a generic
  Thrift description: a `bool` *struct field*'s value is encoded directly
  in the field-type nibble itself (`BooleanTrue`/`BooleanFalse` are
  distinct field types), not as a separate payload byte the way every
  other primitive type is. List/set headers have the analogous packed-
  nibble-with-varint-overflow shape. Every unrecognized field ID is
  skipped via a single recursive `skip` function keyed only on the wire
  type (not needing to know what the field *means*), which is what lets
  every struct reader here stay a short, flat loop with no exhaustive
  field list to keep in sync as Parquet's own schema gains fields over
  time (bloom filters, page indexes, geospatial statistics, and others
  are all skipped this way, since this project doesn't currently surface
  any of them).

  Parquet's own footer: `FileMetaData` (num_rows, `created_by`, and a
  flat, depth-first-traversal-order list of `SchemaElement`s - Thrift has
  no native support for a recursive/nested struct, so Parquet's own
  schema tree is linearized with a `num_children` count driving
  reconstruction, not reconstructed in this phase yet) -> `RowGroup` ->
  `ColumnChunk` -> `ColumnMetaData` (physical type, encodings used,
  compression codec, value/byte counts, data/dictionary page offsets).
  `LogicalType` - the modern (2.4.0+) per-column type-annotation
  mechanism (superseding the older `ConvertedType` enum, still read
  alongside it since older files only ever set that one) - is a genuine
  Thrift *union*: exactly one of 18 fields is present, selected by field
  ID rather than a separate discriminant byte, with several variants
  (`Decimal`/`Time`/`Timestamp`/`Integer`, plus three - `Variant`/
  `Geometry`/`Geography` - this reader doesn't need individually and
  collapses to one disclosed `Other` catch-all) carrying their own nested
  struct payload rather than being empty.

  **A real, if minor, bug found via real-world testing, the same
  methodology every other hand-roll in this project already used**: this
  phase's own oracle-comparison test (`footer_matches_parquet_crate_metadata`,
  cross-checking against the real `parquet` crate's own
  `ParquetMetaDataReader` - still a live runtime dependency at this phase,
  so no dev-only gating needed yet) passed cleanly on this project's own 6
  committed Parquet fixtures, but a further sweep against the official
  `apache/parquet-testing` corpus (`data/`, 79 files - the same corpus
  this project's prior real-world Parquet validation pass already used,
  see the fourth-pass entry elsewhere in this document) surfaced one real
  gap: `unknown-logical-type.parquet` carries a `LogicalType` union
  variant id of 2555 - not one of Parquet's 18 currently-defined variants,
  clearly a deliberately-adversarial forward-compatibility test case. The
  reference crate's own macro for this exact union is named
  `thrift_union_with_unknown!`, specifically to stay forward-compatible
  with a future format version's new variant; this reader's first draft
  hard-errored on an unrecognized id instead, fixed to match by falling
  back to `Other` (still correctly advancing the reader past the unknown
  variant's own payload bytes, using its wire type - a real requirement,
  not just error-message politeness, since the reader would otherwise
  desync on every field after it). Locked in with a vendored copy of the
  real file (`tests/fixtures/parquet_unknown_logical_type.parquet` - see
  `tests/fixtures/parquet_PROVENANCE.md`), the same "vendor a real file
  when self-generation is genuinely impossible" call already made for the
  POI `.xlsb` and `sas7bdat` fixtures, since no ordinary writer tool can
  produce a file with a not-yet-assigned union variant id to re-derive
  this fixture synthetically.

  With that fix, 77 of the 79 real corpus files match the oracle exactly
  (transient, not committed, matching this project's usual real-world-
  corpus practice). The remaining 2 are both cases where the *oracle
  itself* fails to parse the footer, not this reader:
  `alp_extended.zstd.parquet` (an experimental encoding value this
  version of the `parquet` crate doesn't recognize either - this reader
  fails identically, for the same reason, matching CLAUDE.md's own prior
  documented finding for this exact file) and, more interestingly,
  `dict-page-offset-zero.parquet` - already documented elsewhere in this
  file as a known limitation of the *current*, crate-based reader
  ("page/buffer-decoding errors") - where this hand-rolled footer parser
  succeeds cleanly (1 row group, 2 schema elements, 39 rows) where the
  reference crate's own footer parser itself rejects the file
  ("Expected list element type of I64 but got I16"). This is not yet
  locked in as a passing regression test, since there's no oracle left to
  cross-check the *decoded values* against until this reader can read
  page data too (the next phase) - a footer that parses without error
  doesn't yet prove the row group/column chunk offsets it found are being
  interpreted correctly, only that they were read without crashing.

  **Deliberately not started yet, in order**: reconstructing the nested
  `SchemaElement` list into a real tree (needed before any column can be
  matched to its schema node); page header parsing (the same Thrift
  primitives, a much smaller additional struct); the actual value
  encodings (PLAIN first, then RLE/bit-packing hybrid for definition/
  repetition levels and dictionary indices, then `RLE_DICTIONARY` -
  together the overwhelming common case for real files - with
  `DELTA_BINARY_PACKED`/`DELTA_LENGTH_BYTE_ARRAY`/`DELTA_BYTE_ARRAY`/
  `BYTE_STREAM_SPLIT` after); compression codec wiring (Snappy, Gzip, and
  Zstd are *already* hand-rolled elsewhere in this project for other
  formats and only need plugging in here; LZO/Brotli/LZ4/LZ4_RAW are not
  yet hand-rolled anywhere in this project and would each be a new,
  separate undertaking); nested Struct/List/Map reconstruction from
  definition/repetition levels; and, last, Arrow IPC/Feather's own
  FlatBuffers-based schema/`RecordBatch` framework, entirely unstarted.
  None of this is wired into `columns_from_parquet` yet - `parquet_support`
  is a real but currently dormant module (`#[allow(dead_code)]`,
  exercised only by its own tests), and the crate stays a live runtime
  dependency until every behavior this project currently documents and
  tests for Parquet/Arrow IPC (nested types, every compression codec,
  Map non-string-key isolation, named-timezone support, INT96 legacy
  timestamps, Decimal128, dictionary-encoding resolution, and more) is
  matched, verified, and cut over in one deliberate step - not
  incrementally swapped out from under a working build.

  **Phase B (this session): schema tree reconstruction, page headers, the
  RLE/bit-packing hybrid, PLAIN decoding for every physical type,
  dictionary encoding, and `LogicalType`-aware value rendering - still not
  wired into `columns_from_parquet`.** `build_schema`/`schema_from_array_helper`
  turn Phase A's flat, depth-first `SchemaElement` list into a real
  `SchemaNode` tree (verified field-by-field against the `parquet` crate's
  own `schema/types.rs`), and `collect_leaves`/`schema_leaves` walk it into
  the flat `ColumnDescriptor` list (path, max definition/repetition level,
  physical/converted/logical type) every later step keys off - the same
  `build_tree` logic that crate uses internally. Page headers
  (`ParquetPageHeader`/`DataPageHeaderV1`/`DataPageHeaderV2`/
  `DictionaryPageHeader`) are just more Thrift structs read with the exact
  same field-ID-loop-with-skip machinery Phase A already built. The RLE/
  bit-packing hybrid encoding (shared by definition/repetition levels and
  by `RLE_DICTIONARY` indices) was verified directly against
  `util/bit_util.rs`'s own doc comment for one specific, easy-to-get-wrong
  detail: a bit-packed run's values are consumed **LSB-first**, not MSB-
  first the way a naive big-endian reading of "pack N-bit values into
  bytes" would assume. PLAIN decoding covers all seven physical types,
  including INT96's legacy 12-byte (8-byte nanoseconds-of-day + 4-byte
  Julian day) layout, reusing the same Julian-day-2,440,588-is-the-Unix-
  epoch constant this project's own dBase reader already established.
  `LogicalType`/`ConvertedType`-aware rendering covers DECIMAL (both the
  INT32/INT64-backed and BYTE_ARRAY/FIXED_LEN_BYTE_ARRAY-backed forms, via
  a near-duplicate of `avro_support`'s own schoolbook-long-division
  decimal-to-string conversion - deliberately not shared, since `avro`/
  `parquet` are independently togglable features, the same tradeoff made
  throughout this section), DATE, TIME/TIMESTAMP at all three units
  (millis/micros/nanos), and Float16 (see below). Compression codec
  dispatch needed no new algorithmic work at all: Parquet's GZIP codec
  wraps the *full* gzip container (reusing `gzip_decompress` directly),
  while Snappy and Zstd are raw blocks/frames with no extra framing
  (reusing `snappy_support::snappy_decompress`, newly extracted from
  `avro_support` into its own `any(avro, parquet)`-gated module for this
  purpose, and the existing `zstd_support::zstd_decompress`, whose own
  feature gate widened the same way). Scope for this phase is deliberately
  narrow and disclosed up front, matching this project's usual "confident
  common case, explicit gap" discipline: only `max_rep_level == 0`
  columns (no repeated fields, i.e. no arrays) are decoded at all, and
  Data Page V2 and every non-PLAIN/non-dictionary encoding
  (`DELTA_BINARY_PACKED`/`DELTA_LENGTH_BYTE_ARRAY`/`DELTA_BYTE_ARRAY`/
  `BYTE_STREAM_SPLIT`) and non-{Uncompressed,Snappy,Gzip,Zstd} codec
  (LZO/Brotli/LZ4/LZ4_RAW) bail with a clear, disclosed error rather than
  attempting a guess - all explicitly deferred to a later phase, per the
  plan Phase A already laid out.

  **Verification used a genuinely independent oracle, not the Arrow path
  this project's own live reader depends on**: `parquet::record::Row` /
  `Field` - a separate, non-Arrow read API the same crate also exposes -
  so a bug shared between this hand-roll and the Arrow bridge code
  wouldn't get invisibly rubber-stamped by comparing against itself.
  `oracle_field_to_string` remaps several of that API's own rendering
  quirks to match this project's already-established conventions (Decimal/
  Date/Timestamp all need this already; Float16 needed a new one, see
  below) rather than trusting `Field`'s raw `Display` output verbatim -
  the same "don't trust the oracle crate's own formatting, normalize it"
  treatment this project's other cross-verification oracles already get.

  **A real bug was found - in the *test harness*, not the decoder itself -
  via the same real-world-corpus methodology every other hand-roll in this
  project already used.** The two core fixtures
  (`sample.parquet`/`type_detection.parquet`, both single-row-group files)
  passed cleanly on the first attempt, but a further sweep against the
  official `apache/parquet-testing` corpus (`data/`, the same 79-file
  corpus Phase A's own footer test already used) showed several
  multi-row-group files decoding to values that looked wildly,
  structurally wrong - not a formatting difference, but e.g. a `group`
  column's row 0 reading as `"empty-geometries"` when both the oracle
  *and* an independently-run `pyarrow` check agreed the real value was
  `"all"`. Manually hand-tracing the file's own raw Thrift bytes
  byte-by-byte (the same "verify against actual bytes, don't trust a
  first read of the spec" discipline as every other hand-roll's own
  verification) confirmed the *decoder's* own page-header and dictionary-
  page parsing were correct throughout. The actual bug was in
  `decode_column_chunk_matches_the_record_api` itself: its per-row-group
  `mine` vector is indexed from 0 for *that* row group, but the test was
  comparing it against `oracle_rows[row_idx]` - the *global*, whole-file
  row list - so anything past the first row group silently compared
  against the wrong rows. Invisible on both committed fixtures precisely
  *because* they only have one row group each (index 0 is both the local
  and the global row index there), and only surfaced by a real multi-row-
  group file from the wider corpus. Fixed by threading a running
  `global_row` offset (incremented by each row group's own decoded row
  count) through the comparison loop.

  **Two more real, narrower gaps were found and fixed once this test bug
  no longer masked genuine results**: this reader's own printable-text
  heuristic for an un-annotated (no UTF8 `LogicalType`/`ConvertedType`)
  `BYTE_ARRAY`/`FIXED_LEN_BYTE_ARRAY` value was wrong - checked directly
  against `arrow-cast`'s own `DisplayIndex for &GenericBinaryArray` (the
  exact code this project's *own, currently-live* `columns_from_parquet`
  already depends on for this), which always hex-dumps such a value with
  no "does it happen to look like text" heuristic at all. Removed in favor
  of always hex-dumping, matching the behavior this project's live reader
  already has today. Separately, `LogicalType::Float16` had no rendering
  case at all (falling through to a raw 2-byte hex dump instead of a real
  float) - `f16_bytes_to_f64` (a deliberate, disclosed duplicate of
  `cbor_support`'s own already-hand-verified half-precision conversion
  formula, not shared for the same independently-togglable-features reason
  as the decimal conversion above) closes this. Fixing Float16 rendering
  then surfaced a *third*, oracle-side-only issue: `half::f16`'s own
  `Display` impl always shows a decimal point (`Field::Float16(f16::ONE)`
  formats as `"1.0"`, confirmed directly in the `parquet` crate's own
  `record/api.rs` test module), unlike this project's established `f32`/
  `f64` rendering convention (which drops a trailing `.0`, since Rust's own
  float `Display` already does) - normalized in `oracle_field_to_string`
  the same way every other oracle-specific quirk in that function already
  is, not treated as a reason to change this reader's own, already-
  consistent rendering.

  **INT96 timestamps surfaced a genuine oracle limitation, not a bug in
  this reader - confirmed by reading the source on both sides, not just
  picking whichever output looked more familiar.** The `record` API oracle
  renders every INT96 value at fixed millisecond precision -
  `record::api::convert_int96`'s own body is `Field::TimestampMillis(value
  .to_millis())`, discarding real sub-millisecond precision at the point
  of conversion, confirmed directly in the crate's source. But this
  project's own *live* `columns_from_parquet` doesn't go through the
  `record` API at all - it reads through Arrow, whose own INT96-to-Arrow
  conversion (confirmed in `parquet::arrow::array_reader::primitive_array`'s
  `IntoBuffer for Vec<Int96>` impl) targets `Timestamp(Nanosecond, _)` by
  default, not milliseconds. This reader's own nanosecond-precision INT96
  rendering is therefore the behavior actually worth matching; the
  record-API oracle is the one that's lossy here, in a way `oracle_field_
  to_string` can't fix by reformatting (the precision is already gone by
  the time `Field::TimestampMillis` exists). INT96 columns are excluded
  from this specific cross-check with a comment explaining exactly this,
  rather than either silently passing a wrong comparison or forcing this
  reader to (incorrectly) truncate to match a lossy oracle.

  **Final numbers after all of the above**: both committed fixtures still
  match the oracle value-for-value on every in-scope column, and a fresh,
  full sweep of the 79-file real-world corpus (transient, not committed,
  same practice as every other corpus pass in this document) - now
  correctly bucketing failures by *cause* rather than reporting an
  undifferentiated pass/fail count - shows 43 of 63 flat, single-segment-
  path-schema files matching the oracle exactly, 16 skipped for having a
  nested/repeated column (out of scope for this phase, see above), 18
  hitting this reader's own already-disclosed, on-the-roadmap gaps (Data
  Page V2 encoding, `BYTE_STREAM_SPLIT`, or the LZ4/LZ4_RAW codecs - all
  named above as deliberately not started yet), 2 hitting the oracle
  crate's own pre-existing decoding limits (`dict-page-offset-zero.parquet`
  and `nation.dict-malformed.parquet`, both already documented elsewhere in
  this file as known `parquet`/`arrow` crate limits from this project's
  prior real-world Parquet validation pass) - and **zero** genuine,
  unexplained mismatches. The corpus test itself now asserts this last
  count is zero, rather than only logging failures for a human to eyeball,
  so a future regression here fails loudly rather than blending into an
  expected-gaps list.

  **Still deliberately not started, in the same order Phase A laid out**:
  Data Page V2; `DELTA_BINARY_PACKED`/`DELTA_LENGTH_BYTE_ARRAY`/
  `DELTA_BYTE_ARRAY`/`BYTE_STREAM_SPLIT` encodings; the LZO/Brotli/LZ4/
  LZ4_RAW compression codecs; nested Struct/List/Map reconstruction from
  definition/repetition levels (`max_rep_level != 0` columns are still
  explicitly rejected); and, last, Arrow IPC/Feather's own separate
  FlatBuffers-based schema/`RecordBatch` framework, entirely unstarted.
  `parquet_support` remains a real but dormant module
  (`#[allow(dead_code)]`, exercised only by its own tests) and `arrow`/
  `parquet` remain live runtime dependencies until every behavior this
  project currently documents and tests for Parquet/Arrow IPC is matched,
  verified, and cut over in one deliberate step.

**What's deliberately not being hand-rolled**: unlike `arrow`/`parquet`
just above (in progress, not declined), `serde`/`serde_json` are meant to
stay a dependency permanently. They're also the one that's always been
more central than any of the others in this list: `serde_json::Value` is the
literal bridge type seven different format readers (JSON, YAML, TOML,
Avro, MessagePack, CBOR, XML) recurse through via `profile_json_path` -
replacing it means writing and re-verifying a whole JSON value type,
parser, and serializer, not swapping one call site at a time or
hand-rolling a narrower, self-contained parser the way `csv`, `chrono`,
`.xlsx`, `.ods`, `.xls`, `.xlsb`, `serde_norway`, `rust-ini`, `xmltree`,
`zstd`, `npyz`, `rmpv`, `toml`, `ciborium`, `regex`, `dbase`, `dta`,
`apache-avro`, `rusqlite`, and now `sas7bdat` all still were, however
real their own risk.
That's still a real, non-mechanical rewrite - the risk itself is why
it's still deliberately a dependency, the same reasoning that applied to
every other entry in this list right up until it didn't. `serde`/
`serde_json` are the *only* dependency of any kind - direct or
transitive - in the default build (`cargo build` with no `--features`
compiles CSV/TSV/JSON/JSONL support from those two crates plus `std`
alone; every optional format's own additional dependencies are exactly
as documented in the format table at the top of this file, none of them
pulled in unless that specific `--features` flag is passed).

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
- **SAS7BDAT text decoding is limited to UTF-8/US-ASCII and Windows-1252**
  (which, per the WHATWG Encoding Standard `encoding_rs` implements, also
  covers files declaring "ISO-8859-1" - see the Dependency footprint
  section for why that's not genuine Latin-1). SAS can declare roughly 70
  more legacy codepages, several genuinely complex multi-byte/stateful
  schemes (Shift-JIS, EUC-JP/KR, Big5, GB18030, ISO-2022-\*); a file
  declaring one of them is a clear, disclosed error rather than a guess,
  the same dependency-weight tradeoff already declined for dBase's own
  ~20-codepage gap and for SPSS/DuckDB entirely (see below).
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
  mode. Extend the list if a reasonable format is missing one - but if the
  new entry is a `%Y`-anchored format that also has a real-world 2-digit-
  year convention (like `%m/%d/%Y`'s `%m/%d/%y`), add the `%y` form
  *immediately before* it, not anywhere else in the list. `%Y` accepts
  variable-width numeric input while parsing (confirmed directly against
  chrono, not assumed) and will silently misparse a 2-digit year as a
  literal single/double/triple-digit year rather than rejecting it, so
  ordering is the only thing standing between a 2-digit year value and a
  wrong, misleading answer - see the design philosophy section above for
  the worked example.
- **Content-based format sniffing doesn't cover CSV, TSV, TOML, YAML, or
  INI**, and doesn't extend to detecting gzip/zstd compression on an
  extensionless file - see "Content-based format auto-detection" above for
  why both are deliberate, not oversights. The first is the same
  irreducible-ambiguity tradeoff as IPv4-vs-version-string; the second is a
  separate concern (transport encoding, not data format) that was out of
  scope for what this feature was asked to solve.
- **Preamble-row auto-detection (`detect_preamble_rows`) is capped at
  `MAX_PREAMBLE_SCAN` (5) leading rows for both of its signals**, and only
  ever fires on the two specific structural patterns described in the
  design philosophy section above: a padded, mostly-empty banner row (a
  leading run of at-most-one-field-populated rows immediately followed by
  a fully-populated row), or a metadata/row-count line ahead of a *stable*
  multi-row data body (requiring at least 3 corroborating body rows that
  all agree on one field count). A banner that spans more than 5 rows, a
  header that legitimately has an empty/unlabeled column right after the
  banner, or a metadata line ahead of a body with fewer than 3 rows or
  that isn't internally consistent all fall back to `skip_rows = 0` - the
  same old behavior, disclosed nowhere further since nothing auto-fired -
  rather than a wrong guess. `--skip-rows N` is the explicit,
  always-correct escape hatch for any of these.
