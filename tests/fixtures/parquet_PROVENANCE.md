# Provenance of `parquet_unknown_logical_type.parquet`

`parquet_unknown_logical_type.parquet` is a real `.parquet` file copied
verbatim from the `apache/parquet-testing` project's own `data/` directory
(`unknown-logical-type.parquet`):

https://github.com/apache/parquet-testing/blob/master/data/unknown-logical-type.parquet

`apache/parquet-testing` is licensed under the Apache License 2.0
(https://github.com/apache/parquet-testing/blob/master/LICENSE.txt). This
file is vendored here because the exact mechanism it exercises - a Parquet
`LogicalType` Thrift union field carrying a variant ID (2555) that isn't
any of the format's own 18 defined variants, past or present - can't be
produced with an ordinary writer tool (pyarrow, DuckDB, and friends only
ever write real, currently-defined logical types), so a real, independently
-produced adversarial file was the only way to verify this reader's
forward-compatible handling of it (see CLAUDE.md's Dependency footprint
section for the full story - the same "vendor a real file when self-
generation is genuinely impossible" call already made for the POI `.xlsb`
and `sas7bdat` fixtures).

# Provenance of `parquet_lz4_hadoop_framed.parquet`, `parquet_lz4_non_hadoop_fallback.parquet`, and `parquet_byte_stream_split.parquet`

All three are real `.parquet` files copied verbatim from the same
`apache/parquet-testing` project's own `data/` directory (same license as
above):

- `parquet_lz4_hadoop_framed.parquet` was `hadoop_lz4_compressed.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/hadoop_lz4_compressed.parquet
- `parquet_lz4_non_hadoop_fallback.parquet` was `non_hadoop_lz4_compressed.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/non_hadoop_lz4_compressed.parquet
- `parquet_byte_stream_split.parquet` was `byte_stream_split.zstd.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/byte_stream_split.zstd.parquet

Vendored for the same reason as `parquet_unknown_logical_type.parquet`
above: each exercises a specific real-world encoding/codec quirk that no
tool available in this project's development environment writes on
request (pyarrow's own writer doesn't expose Hadoop's legacy LZ4 framing,
its backward-compatible bare-block fallback, or `BYTE_STREAM_SPLIT`
encoding as ordinary write options). `parquet_lz4_non_hadoop_fallback
.parquet` is the more interesting of the three: despite its Thrift codec
ID being the deprecated `LZ4` value (the one nominally requiring Hadoop's
own 8-byte-header-per-frame framing), its actual page bytes are a single
*bare*, unframed raw LZ4 block - confirmed by hand-decoding its raw bytes
(see `lz4_decompress`'s own doc comment in `src/lib.rs`) before trusting
that this was really what the file contained, not an assumption. This is
the real, documented backward-compatibility case the reference `parquet`
crate's own `LZ4HadoopCodec` exists to handle (files written by older,
non-conformant `parquet-cpp` versions), and this project's own hand-rolled
`lz4_decompress` reader needed the identical two-tier fallback to read it
correctly.

# Provenance of `parquet_v2_rle_boolean.parquet`, `parquet_v2_concatenated_gzip.parquet`, and `parquet_v2_empty_compressed.parquet`

All three are real `.parquet` files copied verbatim from the same
`apache/parquet-testing` project's own `data/` directory (same license as
above):

- `parquet_v2_rle_boolean.parquet` was `rle_boolean_encoding.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/rle_boolean_encoding.parquet
- `parquet_v2_concatenated_gzip.parquet` was `concatenated_gzip_members.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/concatenated_gzip_members.parquet
- `parquet_v2_empty_compressed.parquet` was `page_v2_empty_compressed.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/page_v2_empty_compressed.parquet

Vendored for the same reason as the files above: each exercises a Data
Page V2-specific mechanism no ordinary writer tool exposes as an option
(pyarrow's own Parquet writer defaults to Data Page V1). `parquet_v2_rle_
boolean.parquet` uses `Encoding::RLE` as a genuine *value* encoding (not
just a level encoding) for a BOOLEAN column - only valid for that one
physical type, confirmed directly against the `parquet` crate's own
`RleValueDecoder::set_data`. `parquet_v2_concatenated_gzip.parquet` is the
file that found a real, general bug in this project's own `gzip_decompress`
(shared by every GZIP-compressed format this project reads, not just
Parquet): a GZIP-compressed V2 page whose bytes are two separate gzip
members concatenated back to back, which - confirmed via RFC 1952 §2.2 and
real `gzip`/`zlib` behavior - a conforming decompressor must decode and
concatenate in full, not just the first member (see `gzip_decompress`'s own
doc comment in `src/lib.rs` for the full fix). `parquet_v2_empty_compressed
.parquet` exercises a V2 page whose non-null value count is zero - the
"decompressed size of zero" case `serialized_reader.rs` documents by citing
the Parquet format's own spec (`apache/parquet-format`'s README, "data
pages" section) directly.

# Provenance of `parquet_delta_binary_packed.parquet`, `parquet_delta_length_byte_array.parquet`, and `parquet_delta_byte_array.parquet`

All three are real `.parquet` files copied verbatim from the same
`apache/parquet-testing` project's own `data/` directory (same license as
above):

- `parquet_delta_binary_packed.parquet` was `delta_binary_packed.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/delta_binary_packed.parquet
- `parquet_delta_length_byte_array.parquet` was `delta_length_byte_array.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/delta_length_byte_array.parquet
- `parquet_delta_byte_array.parquet` was `delta_byte_array.parquet`:
  https://github.com/apache/parquet-testing/blob/master/data/delta_byte_array.parquet

Vendored for the same reason as the files above: pyarrow's own Parquet
writer has no option to force any of the three delta encodings
(`DELTA_BINARY_PACKED`/`DELTA_LENGTH_BYTE_ARRAY`/`DELTA_BYTE_ARRAY`) on
write, so a real file from a writer that does (these three come from the
Parquet reference implementation's own interop test suite) is the only way
to verify this reader's hand-rolled decoder
(`delta_binary_packed_decode_i64` and its two callers in `src/lib.rs`)
against genuine encoded data - including real edge cases a synthetic
fixture might not happen to exercise, such as a miniblock with bit_width
0 (`delta_binary_packed.parquet`'s own `bitwidth0` column name states this
directly) and prefix-compressed strings sharing real, varying-length
common prefixes with their predecessor (`delta_byte_array.parquet`).
`delta_encoding_optional_column.parquet`/`delta_encoding_required_column
.parquet` (also in the same corpus directory) were deliberately *not*
also vendored - they exercise the identical `DELTA_BINARY_PACKED` encoding
these three files already cover, just varying nullability, which this
reader's shared definition-level/null-interleaving logic (already proven
across every other encoding this reader supports) isn't specific to delta
encoding at all.
