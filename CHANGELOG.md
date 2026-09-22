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
  sniffing. Encrypted files, LZWDecode, and fonts with no usable mapping
  are clean, disclosed refusals, not guesses.

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
