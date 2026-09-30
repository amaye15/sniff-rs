# Provenance of `sas7bdat_people_nonascii.sas7bdat`

`sas7bdat_people_nonascii.sas7bdat` is a real `.sas7bdat` file copied
verbatim from the `sas7bdat` crate's own `tests/fixtures/` directory
(`people_nonascii.sas7bdat`):

https://github.com/tkragholm/sas7bdat-parser-rs

The `sas7bdat` crate is licensed under the MIT license (per its own
`Cargo.toml`: `license = "MIT"`). This file is vendored here because no
tool available in this project's development environment can *write* a
genuine `.sas7bdat` file - it's a proprietary binary format with no
public writer tool (see CLAUDE.md's Known limitations for the fuller
story - `pyreadstat`, the usual option, only writes `.dta`/`.sav`/
`.xport`, not sas7bdat itself). This was previously a real, standing gap:
before this fixture, `columns_from_sas7bdat`/`sas7bdat_support` had no
committed non-malformed fixture at all, only `malformed_garbage.sas7bdat`
- this file closes that gap with a genuine SAS7BDAT file exercising real
non-ASCII text content, the same "vendor a real file when self-generation
is genuinely impossible" call already made for the POI `.xlsb` fixtures
(see `poi_xlsb_PROVENANCE.md`).

`sas7bdat_pandas_windows1251.sas7bdat` is `datetime.sas7bdat` copied
verbatim from pandas' own test data (`pandas/tests/io/sas/data/`,
https://github.com/pandas-dev/pandas, BSD-3-Clause). It declares the
WINDOWS-1251 text encoding, which this reader used to refuse; it now
reads through the shared single-byte code page tables.

`sas7bdat_pandas_airline.sas7bdat` and `sas7bdat_pandas_cars.sas7bdat` are
`airline.sas7bdat` and `cars.sas7bdat` from the same pandas directory
(BSD-3-Clause). Both carry per-column labels (`level of output`,
`miles per gallon`, ...), which the reader now surfaces as each column's
`description`; the expected labels were cross-checked against pyreadstat.
