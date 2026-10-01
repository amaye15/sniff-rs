# Provenance of the `.xpt` (SAS Transport) fixtures

`xport_nhanes_sshsv1_a.xpt` is `SSHSV1_A.XPT` and `xport_nhanes_paxraw_short.xpt`
is the first 100 observations of `PAXRAW_D.XPT`, both from the US Centers for
Disease Control's National Health and Nutrition Examination Survey
(https://wwwn.cdc.gov/nchs/nhanes/). They are works of the US federal
government and in the public domain. They were chosen because they are real
files written by SAS (a v5 library, SAS 9.3) rather than by a tool built to
match this reader.

`type_detection.xpt` (v5), `sample.xpt` (v8, long labels), `edge_xport_multi_member.xpt`
(two v5 members) and `edge_xport_v8_trailing_blank_rows.xpt` were written with
pyreadstat's `write_xport`; the multi-member file is two single-member files
joined (the second without its three library-header records).

`edge_xport_truncated_numerics_tagged_missing.xpt` and `edge_xport_labelv9.xpt`
are hand-assembled to the published layout, because no available writer
produces numerics shorter than eight bytes, tagged missing values, or a
`LABELV9` record. pyreadstat (ReadStat) reads both - the second in full, the
first for its opening member - with the values the tests expect.

`malformed_garbage.xpt` is text; `malformed_xport_truncated.xpt` is
`type_detection.xpt` cut mid-data.
