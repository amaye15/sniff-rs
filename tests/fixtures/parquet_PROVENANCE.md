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
