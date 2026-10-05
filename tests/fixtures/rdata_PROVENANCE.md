# R data fixtures

`edge_rdata_pyreadr_*` are real files written by R, vendored from the test data of
[pyreadr](https://github.com/ofajardo/pyreadr) (`test_data/basic`, MIT licence), which makes them from
`basic_dataset.R`: a data frame with doubles (`Inf`, `NaN`, `NA`), integers, characters, factors, logicals and
POSIXct columns, matrices, tables, dates, and `save()` files with several objects under gzip, bzip2 and xz
(`two*.RData`), serialization versions 2 and 3, ALTREP compact sequences, wrapper and deferred-string objects.
Every file was read by this tool and by pyreadr and compared cell by cell.

`edge_rdata_ggplot2_mpg.rda` and `edge_rdata_ggplot2_msleep.rda` (ggplot2, MIT) and
`edge_rdata_dplyr_starwars.rda` (dplyr, MIT, list columns) are the packages' own data files.

`edge_rdata_written_{xdr,native,ascii}.rds` were written by a small independent serializer (the three wire
formats of one data frame: `X` big-endian XDR, `B` native binary, `A` decimal text); the XDR one is read by
pyreadr, and the three agree here. R itself isn't available where they were made, so the text and native
binary wires are checked against that serializer and R's documented format, not against R.
