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
