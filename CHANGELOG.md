# Changelog

All notable changes to sniff-rs are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added
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

### Fixed
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

### Changed
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
