# Provenance of the pre-BIFF8 `.xls` fixtures

No tool available here writes BIFF3, BIFF4, or BIFF5 (modern LibreOffice
dropped the Excel 5.0/95 export filter), so these are real files from the
Apache POI project's own `test-data/spreadsheet/` directory (Apache
License 2.0, the same source as `poi_*.xlsb`):

- `poi_biff5_excel95.xls` - POI's `testEXCEL_95.xls` (BIFF5/7, OLE2 `Book`
  stream, three sheets, two empty), copied verbatim.
- `poi_biff3.xls` - POI's `testEXCEL_3.xls` (a bare BIFF3 worksheet, no
  OLE2 container), copied verbatim.
- `edge_xls_biff5_dates_and_codepage.xls` - `testEXCEL_95.xls` with its
  `Book` stream patched in place (same length, via `olefile`): custom
  FORMAT 165 `0.00%` rewritten as `m/d/y`, XF 25 pointed at built-in
  format 14, and the label `Number` rewritten as `Nümber` (cp1252). This
  exercises both BIFF5 date paths (an explicit custom format and an
  implicit built-in one) and 8-bit code-page text, which the original
  file doesn't.
- `edge_xls_biff4_dates.xls` - POI's `testEXCEL_4.xls` (a bare BIFF4
  worksheet) with XF 102's one-byte format index changed from 38 to 18
  (`m/d/yy`), so two numeric cells read as dates.

Every file was checked against `xlrd` (which reads BIFF2-8): each
column's full set of values, and every row count, match.
