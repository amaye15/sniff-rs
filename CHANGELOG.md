# Changelog

All notable changes to sniff-rs are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Fixed
- Inline SQL loaded into a real PostgreSQL or MySQL server (found by
  loading every fixture into both): dates and times are written as ISO
  literals using the column's own detected format (`15/01/2024`,
  `Jan 15, 2024`, RFC 2822, `10:00 PM` used to be embedded as-is and were
  rejected or misread); identifiers past 63 bytes are shortened with a
  hash suffix instead of being silently truncated into duplicate columns
  (PostgreSQL) or rejected (MySQL), and trailing whitespace in a name is
  trimmed; `--load-into mysql` sets `ANSI_QUOTES`/`NO_BACKSLASH_ESCAPES`,
  uses `DATETIME(6)`/`TIME(6)`, and accepts a `mysql://` URI (the classic
  client can't read one); `--load-into` stops at the first failing
  statement (`psql` used to run on and exit 0). A non-finite float
  (`Infinity`, `NaN`) still can't be stored in a MySQL `DOUBLE` and
  fails that load.
- dBase files with a backlink after the field descriptors but not marked
  Visual FoxPro (FoxPro 2 writes these) no longer fail on phantom fields.
- Near-identical tables (same column names) now link as a star on the
  first copy instead of every copy to every other: 1,000 same-schema
  tables wrote 499,500 `duplicate_schema` relationships and JSON output
  grew quadratically (81 s at 4,000 tables, now under half a second).
- ZIP-based inputs (`.xlsx`, `.ods`, `.xlsb`, `.npz`) read Zip64
  archives, e.g. an `.npz` with more than 65,535 arrays.
- gzip/zstd input with no telling extension (an extensionless file, or
  data piped to `-`) is recognized by its magic bytes and decompressed,
  instead of reaching the readers still compressed.
- An empty `/FlateDecode` stream in a PDF no longer fails the file.

### Added
- Directory mode `--load-into postgres:...`/`mysql:...` (or a connection
  URI) creates one database per file, named from its path
  (`sub/types.csv` becomes `<prefix>_sub_types_csv`); an existing
  database is an error, never reused.
- PDF right-to-left (Hebrew/Arabic) lines read in logical order, and
  vertical text reads down its columns (stacked glyphs and `Identity-V`
  fonts).
- `.xls` reads Excel 5.0/95 (BIFF5/7) workbooks and bare BIFF3/BIFF4
  worksheets, not just BIFF8.
- Directory mode profiles files in parallel (`--jobs N`, default: CPU
  cores); output and progress lines stay in walk order, identical to a
  sequential run.
- PDF: filled-in AcroForm field values become a `<file>_form` table and
  comment/note annotation text an `annotations` column, including in
  encrypted files.
- `--output-format sql --sql-mode inline` and `--load-into` work on PDF
  (pages and form) and Jupyter notebooks.
- ORC Struct/List/Map/Union columns are decoded and flattened into
  sub-columns instead of placeholder notes, in profiling and inline SQL.
- `--output-format sql` (inline and staging) and `--load-into` work on
  Delta Lake and Iceberg tables.
- Delta deletion vectors (inline and file-backed) are applied instead of
  refusing the table.
- Delta and Iceberg flatten nested struct/list/map columns into
  sub-columns; Iceberg applies equality deletes and follows renamed
  columns by field id.
- MBOX decodes MIME: RFC 2047 header words, base64/quoted-printable
  bodies in their declared charset, the plain-text part of multipart
  messages, and an `attachments` column of file names.
- Stata `strL` long strings resolve to their text (releases 117-119)
  instead of a placeholder.
- dBase memo fields read from their `.dbt`/`.fpt` file (dBase III/IV,
  FoxPro, Visual FoxPro) instead of refusing the file.
- dBase and SAS7BDAT decode legacy single-byte code pages (DOS, Windows,
  ISO-8859, KOI8, Mac) instead of refusing them; double-byte East Asian
  encodings still refuse.
- JSON5 reads the full 1.0.0 grammar: hex, signed, and leading/trailing-dot
  numbers, `Infinity`/`NaN`, `\x`/`\v`/`\0` escapes, Unicode identifier
  keys, and Unicode whitespace.
- YAML aliases (`*name`) and merge keys (`<<`) resolve, with an
  expansion cap against alias bombs; they used to be a parse error.
- `sniff-rs diff` accepts `-` for one side (stdin), with `--format` for
  that side when its format can't be sniffed.
- `rank` gains three graph measures per table: `reference_rank`
  (weighted PageRank along references, 1.0 = average - high for the
  tables everything ultimately points at), `area` (a Louvain modularity
  subject area inside a community, with a new top-level `areas` array
  and a "Subject areas" Markdown section when a community splits), and
  `articulation` (every join path between some other pair of tables runs
  through it). `rank` also computes components once instead of per row.
- A reference that runs to both a hub and a table that itself references
  that hub (`trip.station_id` to `station` and to `status.station_id`)
  now reads `shared_reference` for the derived hop.
- Relationship tiers: `declared` (SQLite `REFERENCES`/`FOREIGN KEY`
  constraints, surfaced per column as `references` and turned into edges
  with probability 1), `discovered` (inclusion dependencies measured on a
  new bounded per-column `value_sketch`), and `probable` (a
  Fellegi-Sunter model over name, type and value comparisons, fitted on
  834 declared foreign keys). Edges gain a `probability` field; bridges
  below 0.5 are dropped.
- Composite foreign keys stay whole: each column of a multi-column
  `FOREIGN KEY` carries the full key in its `references` entry
  (`"composite": [["o", "order_id"], ["l", "line_no"]]`), and its edge's
  evidence says to join on every pair. `explain` also lists a column's
  declared self-references (`staff.manager -> staff.id`), which have no
  edge since the graph links tables; its JSON gains `self_references`.
- PDF page-text reader (`--features pdf`, in `full`): one record per page
  (`page_number`, `text`). Hand-rolled, pure `std` - xref tables/streams
  with `/Prev` chains (plus bare-trailer files via index rebuild), object
  streams, FlateDecode (+ASCII85/ASCIIHex/RunLength, stacked),
  WinAnsi/MacRoman/Differences/ToUnicode font decoding, `%PDF-` content
  sniffing. LZWDecode is a clean refusal; text no font mapping covers
  reads as U+FFFD, disclosed in the column's notes, never guessed.
- PDF decryption with an empty user password, across every Standard
  Security Handler revision (RC4, AES-128, AES-256 `/R` 5 and 6);
  a real user password is a clean refusal.
- PDF text from Form XObjects (`/Do`), and from a FlateDecode stream
  truncated mid-block (everything before the cut is kept).
- PDF fonts: each font's implicit built-in encoding - an embedded Type 1
  or CFF program's own encoding vector, or StandardEncoding for a
  nonsymbolic font - so symbolic embedded fonts decode instead of
  refusing; glyph names resolve through Adobe's full Glyph List plus
  lcdf-typetools' TeX extensions and the AGL spec's `_` ligature rule.
- PDF `text` column notes disclose lossy decoding: fonts and codes that
  read as U+FFFD (with the first reason), and Form XObjects skipped
  because their content couldn't be parsed.
- PDF marked-content `/ActualText` (inline or from `/Resources
  /Properties`) replaces the text of the glyphs it covers: Chrome's
  ligatures; InDesign's small caps, soft hyphens, and tabs. ActualText
  holding U+FFFD is ignored, so the tab-leader dots it covers stay.

- `sniff-rs graph <DIR>`: a knowledge graph across every file and data
  type. Files (and tables, and identifiers two or more files share) are
  nodes; links are joins measured by value overlap, shared schemas,
  shared identifiers (emails, domains, URLs, DOIs, ISBNs, UUIDs, IPs,
  VINs, IBANs, course codes, reference numbers, masked card numbers),
  file references, TF-IDF-similar wording, and shared name stems - each
  EXTRACTED / INFERRED / AMBIGUOUS with evidence. Louvain communities.
  Writes graphify-style `graph.json` and `GRAPH_REPORT.md`, and with
  `--obsidian` an Obsidian vault. Word and PowerPoint text, plain text,
  HTML, and source files are read for links; images and other binaries
  are nodes too.
- `explain`/`path`/`rank` accept a directory or a `graph.json` and then
  query the knowledge graph's nodes.

### Fixed
- Directory walks skip folders `sniff-rs graph` generated, so a graph
  or vault inside its own input is never profiled as data.
- PDF: a font selected inside `q ... Q` no longer leaks past `Q`; Form
  XObject fonts no longer collide across pages; Identity-H codes decode at
  their real two-byte width; `/Differences [255 /a /b]` degrades that
  font instead of panicking; an unknown glyph name only costs the codes a
  page actually shows; `propersubset`/`propersuperset` map to U+2282/3.
- PDF text expands `ﬁ`-style presentation-form ligatures to letters
  (Unicode's own NFKC mapping).
- PDF: TeX/Symbol extensible-delimiter pieces (`bracketlefttp`, ...) read
  as their Unicode 3.2 characters (U+239B-U+23AD and kin) instead of
  U+FFFD.
- PDF: the `"` operator's string is no longer dropped.
- PDF: unembedded standard Symbol and ZapfDingbats fonts use their
  published built-in encodings (AcroForm checkboxes read as ✔), and
  ZapfDingbats glyph names (`a20`) resolve in a ZapfDingbats font.

### Changed
- Plain `--output-format sql` (inline by default) falls back to staging
  when a column is an array of objects, instead of failing; an explicit
  `--sql-mode inline` or `--load-into` still errors.
- Relationship probabilities refitted after correcting the benchmark:
  Spider leaves 15 of its `baseball_1` (Lahman) keys undeclared, which had
  pushed an exact key name owned by a table named for it, outside that
  table's first column (`category.business_id -> business.business_id`),
  down to 0.46 - under the threshold. Those links now score about 0.81
  and are kept (Spider F1 0.811 -> 0.815 on its own labels, 0.813 ->
  0.828 with baseball corrected; the three real databases unchanged).
- Relationship `confidence` is now `declared`/`discovered`/`probable`,
  replacing `extracted`/`inferred`. Column JSON gains `references` and
  `value_sketch` (appended last).
- PDF: word and line breaks come from where glyphs are drawn - text,
  line and transformation matrices, Form `/Matrix`, `TJ` offsets, and
  glyph widths (`/Widths`, `/W`, or Core 14 metrics) - instead of which
  operator drew them. Files matching PDFium word for word: 218 to 446 of
  607. Runs of space glyphs collapse to one; text is otherwise unchanged.
- PDF: a font or code with no Unicode mapping now reads as U+FFFD with a
  disclosure note instead of failing the whole file (18 real files that
  used to be refused now read; structural errors still refuse).
- Default build is now every format plus SIMD (`default = ["full"]`).
  A plain build requires a nightly toolchain; `--no-default-features`
  gives a minimal stable-compatible build (CSV/TSV/JSON/JSONL,
  fixed-width, gzip, no SIMD).
- Zero runtime dependencies: every format reader is hand-rolled pure
  `std`. Former third-party crates remain only as dev-only
  cross-verification oracles.

## [0.1.0]
- Initial release: CSV/TSV/JSON/JSONL plus 25+ optional formats, Markdown /
  rich JSON / JSON-Schema / SQL output, `diff` subcommand, directory batch
  mode, Delta Lake and Iceberg table support.
