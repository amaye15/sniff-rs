# sniff-rs

Profile a data file and produce a data dictionary — one row per column:
what type the data actually is, what type it *should* be, missing %,
sample values, and why.

Reads CSV, TSV, JSON, JSON Lines, Parquet, Arrow IPC/Feather, Avro, Excel,
SQLite, MessagePack, TOML, YAML, CBOR, INI, XML, fixed-width text, NumPy,
Common/Combined access logs, RFC 3164/5424 syslog, dBase, Stata, SAS7BDAT,
SPSS, ORC, BSON, plist, JSON5/JSONC, HAR, GeoJSON, MBOX, vCard, iCalendar,
Jupyter notebooks, and PDF page text — any of them inside a gzip, zstd,
bzip2, xz, brotli, LZ4, zip or tar wrapper (`data.tar.gz` works), in UTF-8,
UTF-16/32, a single-byte code page, or Shift-JIS/EUC/GBK/Big5 (`--encoding`) — plus Delta Lake and
Iceberg tables.
Writes Markdown, rich JSON, JSON-Schema, or a runnable SQL script; nested
data (arrays of objects, maps) becomes child tables keyed to the parent row.
`sniff-rs diff` compares two dictionaries (or two raw files) and flags
schema drift. `sniff-rs graph` builds a knowledge graph across every file
in a folder.

Zero runtime dependencies: every reader is hand-rolled pure `std`.

## Relationships and graph queries

Rich JSON output carries a top-level `relationships` array: join
candidates detected across tables (SQLite, Excel, INI, multi-table
formats, or `--combine` directories), each in one of three tiers:
`declared` (the schema states it - SQLite `REFERENCES`/`FOREIGN KEY`),
`discovered` (the values show it: one column's values sit inside
another's unique values), or `probable` (names and types make it more
likely than not). Every edge carries a calibrated `probability` and its
evidence; weaker candidates are dropped. Three subcommands query the graph
without re-reading any file - each takes a dictionary, a raw file, a
directory, or a `graph.json` (see below):

```bash
sniff-rs explain warehouse.db users.id     # one column: profile + edges
sniff-rs path warehouse.db orders users    # shortest join chain
sniff-rs rank warehouse.db                 # god tables + communities
```

`diff` additionally reports relationship drift: joins that appeared,
vanished, or changed tier or probability between snapshots (a lost join is
breaking, like a removed column).

## Knowledge graph

`sniff-rs graph <DIR>` links every file in a folder, of every data type:
table joins, shared identifiers found in content (emails, DOIs, ISBNs,
IBANs, UUIDs, reference numbers, ...), one file naming another, similar
wording, shared schemas, and the same document saved twice. Each link
carries a confidence (extracted / inferred / ambiguous) and its evidence;
Louvain communities group what belongs together.

```bash
sniff-rs graph ./data/                    # ./data.graph/graph.json + GRAPH_REPORT.md
sniff-rs graph ./data/ --obsidian         # plus an Obsidian vault
sniff-rs explain data.graph/graph.json crm/customers.csv
sniff-rs path data.graph/graph.json crm/customers.csv crm/orders.json
sniff-rs rank ./data/                     # graph the folder in memory
```

`graph.json` is networkx node-link JSON (the shape graphify writes). The
graph and vault contain the identifiers found in your files, so keep them
as private as the input. `sniff-rs graph --help` lists every option.

## Install

Pick one row. All binaries are the full build (every format, SIMD on).

| Method | Command | Platforms |
|---|---|---|
| Prebuilt binary (macOS/Linux) | `curl -fsSL https://raw.githubusercontent.com/amaye15/sniff-rs/main/install.sh \| bash` | macOS arm64/x86_64, Linux x86_64/arm64 |
| Prebuilt binary (Windows) | `powershell -c "irm https://raw.githubusercontent.com/amaye15/sniff-rs/main/install.ps1 \| iex"` | Windows x86_64 (native PowerShell, no git-bash needed) |
| Homebrew | `brew tap amaye15/sniff-rs && brew install sniff-rs` | macOS, Linux |
| cargo-binstall | `cargo binstall sniff-rs` | macOS, Linux, Windows |
| From source (nightly required) | `cargo +nightly install --locked --git https://github.com/amaye15/sniff-rs` | anywhere Rust runs |
| Minimal stable build | `cargo install --locked --no-default-features --git https://github.com/amaye15/sniff-rs` | anywhere stable Rust runs |

The default build needs a **nightly toolchain** (`std::simd`, still
unstable). If you only have stable Rust, use the minimal-build row above
(CSV/TSV/JSON/JSONL, fixed-width, gzip) or any prebuilt binary.

Verify any install:

```bash
sniff-rs --version
sniff-rs --list-formats          # every format this binary can read
```

## Quick start

```bash
sniff-rs data.csv
sniff-rs events.jsonl out.md --samples 5
sniff-rs warehouse.db - --output-format json | jq .
sniff-rs data.csv.gz                  # or .tar.gz, .zip, .xz, .bz2, ...
sniff-rs bundle.zip                   # an archive of several files: one combined dictionary
sniff-rs legacy.csv --encoding windows-1252   # or shift_jis, gbk, euc-kr, big5, ...
sniff-rs ./data/ --output-dir ./dictionaries/
sniff-rs diff old.json new.json
```

`sniff-rs --help` documents every flag. `CHANGELOG.md` tracks releases.
