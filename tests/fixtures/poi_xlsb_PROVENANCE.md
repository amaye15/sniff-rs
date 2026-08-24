# Provenance of `poi_*.xlsb` fixtures

`poi_simple.xlsb`, `poi_date.xlsb`, `poi_sample.xlsb`, and `poi_various.xlsb`
are real `.xlsb` files copied verbatim from the Apache POI project's own
`test-data/spreadsheet/` directory (`Simple.xlsb`, `date.xlsb`,
`sample.xlsb`, `testVarious.xlsb` respectively):

https://github.com/apache/poi/tree/trunk/test-data/spreadsheet

Apache POI is licensed under the Apache License 2.0
(https://github.com/apache/poi/blob/trunk/legal/LICENSE). These files are
vendored here because no tool available in this project's development
environment can *write* a genuine `.xlsb` file (see CLAUDE.md's
Dependency footprint section for the full story - openpyxl/xlsxwriter
only write `.xlsx`, and LibreOffice's own `.xlsb` export filter doesn't
work at all), so a real, independently-produced file was the only way to
verify the hand-rolled `.xlsb` reader (`xlsx_support::columns_from_xlsb`
in `src/lib.rs`) against genuine data rather than a synthetic guess.
