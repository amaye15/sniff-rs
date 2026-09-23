# sniff-rs

Profile a data file and produce a data dictionary — one row per column:
what type the data actually is, what type it *should* be, missing %,
sample values, and why.

Reads CSV, TSV, JSON, JSON Lines, Parquet, Arrow IPC/Feather, Avro, Excel,
SQLite, MessagePack, TOML, YAML, CBOR, INI, XML, fixed-width text, NumPy,
Common/Combined access logs, RFC 3164/5424 syslog, dBase, Stata, SAS7BDAT,
SPSS, ORC, BSON, plist, JSON5/JSONC, HAR, GeoJSON, MBOX, vCard, iCalendar,
Jupyter notebooks, and PDF page text — any of them gzip- or
zstd-compressed — plus Delta Lake and Iceberg tables.
Writes Markdown, rich JSON, JSON-Schema, or a runnable SQL script.
`sniff-rs diff` compares two dictionaries and flags schema drift.

Zero runtime dependencies: every reader is hand-rolled pure `std`.

## Relationships and graph queries

Rich JSON output carries a top-level `relationships` array: join
candidates detected across tables (SQLite, Excel, INI, multi-table
formats, or `--combine` directories), each tagged `extracted` (measured
in the data: matching names, a `users.id` ← `orders.user_id` shape, or
shared samples) or `inferred` (similar names, or a shared UUID/Email
domain), with per-edge evidence. Three subcommands query the graph
without re-reading any file - each takes a dictionary or a raw file:

```bash
sniff-rs explain warehouse.db users.id     # one column: profile + edges
sniff-rs path warehouse.db orders users    # shortest join chain
sniff-rs rank warehouse.db                 # god tables + communities
```

`diff` additionally reports relationship drift: joins that appeared,
vanished, or changed confidence between snapshots (a lost join is
breaking, like a removed column).

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
sniff-rs data.csv.gz
sniff-rs ./data/ --output-dir ./dictionaries/
sniff-rs diff old.json new.json
```

`sniff-rs --help` documents every flag. `CHANGELOG.md` tracks releases.
