# sniff-rs

Profile a data file and produce a data dictionary — one row per column:
what type the data actually is, what type it *should* be, missing %,
sample values, and why.

Reads CSV, TSV, JSON, JSON Lines, Parquet, Arrow IPC/Feather, Avro, Excel,
SQLite, MessagePack, TOML, YAML, CBOR, INI, XML, fixed-width text, NumPy,
Common/Combined access logs, RFC 3164/5424 syslog, dBase, Stata, SAS7BDAT,
SAS Transport (.xpt), SPSS, ORC, BSON, plist, JSON5/JSONC, HAR, GeoJSON, MBOX, vCard, iCalendar,
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

`sniff-rs graph <DIR>` links every file in a folder, of every data type.
Each link carries a confidence (extracted / inferred / ambiguous), a score
and its evidence, and Louvain communities group what belongs together.

```bash
sniff-rs graph ./data/                    # ./data.graph/graph.json + GRAPH_REPORT.md
sniff-rs graph ./data/ --obsidian         # plus an Obsidian vault
sniff-rs graph ./data/ --export graphml,sqlite,html
```

**What it links**

| Link | What it says |
|---|---|
| `joins`, `shares_key`, `has_schema` | tables whose columns line up by name or by value; declared foreign keys (SQLite, SQL dumps, `--db`) |
| `mentions`, `references`, `similar_to`, `same_name`, `duplicate_of` | identifiers (emails, DOIs, ISBNs, IBANs, UUIDs, ...) found in content, one file naming another, similar wording, one document saved twice |
| `imports`, `reads`, `writes`, `derived_from` | code to data: Python, JS/TS, R, Rust, Go, Java, SQL, dbt and notebook cells; which script reads which file or table, and what a query builds from what |
| `version_of`, `exported_from` | `report_v2` after `report_v1`; a CSV that is an export of a database table |
| `metadata`, `looks_like` | the same author, camera or artist in the files' own properties (EXIF, XMP, ID3, Office); near-identical pictures |
| `has_column`, `same_column`, `type_drift`, `uses_column` | with `--columns`: a node per column that tables share |
| `involves`, `authored_by`, `same_person`, `member_of` | with `--people`: mail, contacts, calendars and document authors |
| `changes_with` | with `--git`: files that change in the same commits |
| `near` | with `--geo` / `--timeline`: files from the same place or day |

Reads mail (`.mbox`, `.eml`, `.msg`), contact cards and calendars, Word,
PowerPoint, OpenDocument, RTF and EPUB text, PDFs, workbooks (formulas
that read other sheets), SQL dumps and live PostgreSQL/MySQL schemas
(`--db`). `--geo` and `--timeline` keep a place as a grid cell (about
11 km) and a time as a day, never an exact position or time, and are off
by default; so are `--people`, `--git` and `--columns`.

**Add what you know.** Write links as JSON and hand them in, or put rules
in `.sniff-rs.toml` (custom identifier patterns, aliases, links to add or
reject, entities to ignore):

```bash
sniff-rs graph ./data/ --links extra-links.json
sniff-rs graph merge a/graph.json b/graph.json -o merged/
sniff-rs graph ./data/ --unlinked-report   # unlinked.json: what nothing links to, and why
```

**Ask it.** Every query takes a directory or a `graph.json`:

```bash
sniff-rs search ./data.graph "invoice 2024"      # BM25, like SQLite FTS5
sniff-rs neighbors ./data.graph crm/customers.csv --depth 2
sniff-rs subgraph ./data.graph --node crm/customers.csv --depth 2 -
sniff-rs communities ./data.graph --members
sniff-rs explain ./data.graph crm/customers.csv
sniff-rs path ./data.graph crm/customers.csv crm/orders.json
sniff-rs rank ./data.graph --sort importance
sniff-rs diff old.graph/graph.json new.graph/graph.json
```

`graph.json` is networkx node-link JSON (the shape graphify writes) and
has a published JSON Schema: `sniff-rs graph --schema`. Exports:
GraphML, GEXF, DOT, Cypher, JSON-LD, Mermaid, a self-contained HTML
viewer, and a SQLite database.

The graph and vault contain the identifiers found in your files (card
numbers only as their last four digits), so keep them as private as the
input. `sniff-rs graph --help` lists every option.

**Scale.** Measured on a laptop (10 cores), default build:

| Input | Cold | Warm cache | Peak memory |
|---|---|---|---|
| 50,000 files, 1 million links | 18 s | 12 s | 1.0 GB |
| 100,000 tables in 1,000 SQLite files | 11 s | 10 s | 0.9 GB |
| A question on the 50,000-file `graph.json` (317 MB) | | 2-3 s | 0.5 GB |

A table-level input (one database with 10,000 tables) answers `explain`,
`path` and `rank` in 1.5 s. A column name held by more than 512 tables is
a convention, not a join: it pairs only with the table that owns it, and
a note says so.

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

**Shell completions** (bash, zsh, fish, PowerShell) are built from the
help texts, so they follow every flag. The release archives hold them in
`completions/`; or print them:

```bash
source <(sniff-rs completions bash)                    # ~/.bashrc
sniff-rs completions zsh > "${fpath[1]}/_sniff-rs"     # then restart zsh
sniff-rs completions fish > ~/.config/fish/completions/sniff-rs.fish
sniff-rs completions powershell | Out-String | Invoke-Expression
```

## Quick start

```bash
sniff-rs data.csv
sniff-rs events.jsonl out.md --samples 5
sniff-rs warehouse.db - --output-format json | jq .
sniff-rs data.csv.gz                  # or .tar.gz, .zip, .xz, .bz2, ...
sniff-rs bundle.zip                   # an archive of several files: one combined dictionary
sniff-rs legacy.csv --encoding windows-1252   # or shift_jis, gbk, euc-kr, big5, ... (unnamed non-UTF-8 reads as windows-1252, with a note)
sniff-rs ./data/ --output-dir ./dictionaries/
sniff-rs diff old.json new.json
```

`sniff-rs --help` documents every flag. `CHANGELOG.md` tracks releases.
