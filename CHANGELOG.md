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
  sniffing. LZWDecode and fonts with no usable mapping are clean,
  disclosed refusals, not guesses.
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
- PDF `text` column notes disclose lossy decoding: codes that read as
  U+FFFD, and Form XObjects skipped because their text couldn't be decoded.

### Fixed
- PDF: a font selected inside `q ... Q` no longer leaks past `Q`; Form
  XObject fonts no longer collide across pages; Identity-H codes decode at
  their real two-byte width; `/Differences [255 /a /b]` is a clean
  refusal instead of a panic; an unknown glyph name only refuses when a
  page actually shows it; `propersubset`/`propersuperset` map to U+2282/3.
- PDF text expands `ﬁ`-style presentation-form ligatures to letters
  (Unicode's own NFKC mapping).

### Changed
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
