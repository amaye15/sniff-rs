#!/usr/bin/env python3
"""Checks which sheets of a workbook sniff-rs says its formulas read.

    python3 tools/check_formula_refs.py [--bin target/release/sniff-rs] [--books 60]

Random workbooks are written with openpyxl: sheets with awkward names
(spaces, quotes, hyphens, accents, CJK), and formulas that read other sheets
plainly, quoted, as ranges, in function arguments, as 3-D ranges, next to
string literals that only look like references, and next to #REF! errors.
openpyxl's formula tokenizer finds the sheet each reference names (the
answer); `sniff-rs graph` must report the same sheet-to-sheet links, with the
same number of formula cells. Needs `pip install openpyxl`.
"""
import argparse
import json
import random
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

import openpyxl
from openpyxl.formula import Tokenizer
from openpyxl.formula.tokenizer import Token

NAMES = ["Data", "Summary Sheet", "It's here", "Q1-2024", "naïve", "会議", "a.b", "Sheet (1)",
         "Ünï cödé", "O'Brien's", "Totals", "x_y", "2024", "R1C1", "Sheet-2"]
PLAIN = re.compile(r"^[A-Za-z_][A-Za-z0-9_.]*$")


def ref_name(name):
    # Excel quotes a name unless it is a plain identifier (and is not a cell reference).
    if PLAIN.match(name) and not re.match(r"^[A-Za-z]{1,3}[0-9]+$", name) and not re.match(r"^R[0-9]*C[0-9]*$", name, re.I):
        return name
    return "'" + name.replace("'", "''") + "'"


def formulas_for(rng, sheets, me):
    others = [s for s in sheets if s != me]
    out = []
    for _ in range(rng.randint(1, 6)):
        a, b = rng.choice(others), rng.choice(others)
        kind = rng.randint(0, 9)
        if kind == 0:
            out.append(f"=SUM({ref_name(a)}!B2:B5)")
        elif kind == 1:
            out.append(f"={ref_name(a)}!A1")
        elif kind == 2:
            out.append(f"=IF(A1>1,{ref_name(a)}!B1,{ref_name(b)}!C3)")
        elif kind == 3:
            out.append(f"=VLOOKUP(A1,{ref_name(a)}!$A$1:$B$5,2,0)")
        elif kind == 4:
            out.append(f'="{a}!A1 is text"&{ref_name(b)}!A2')
        elif kind == 5:
            out.append(f"=A1+B1")
        elif kind == 6:
            out.append(f"={ref_name(me)}!A1+{ref_name(a)}!A2")
        elif kind == 7:
            out.append(f"=SUM({ref_name(a)}!A1,{ref_name(a)}!A2,{ref_name(b)}!A3)")
        elif kind == 8:
            out.append(f'=IF(1,"it""s {b}!A1",{ref_name(a)}!A4)')
        else:
            out.append(f"=IFERROR({ref_name(a)}!B2/{ref_name(b)}!B3,0)")
    return out


def oracle(wb):
    """(from, to) -> formula cells that read `to`, by openpyxl's tokenizer."""
    counts = Counter()
    for ws in wb.worksheets:
        for row in ws.iter_rows():
            for cell in row:
                v = cell.value
                if cell.data_type != "f" or not isinstance(v, str):
                    continue
                seen = set()
                for t in Tokenizer(v).items:
                    if t.type == Token.OPERAND and t.subtype == Token.RANGE and "!" in t.value:
                        sheet = t.value.rsplit("!", 1)[0]
                        if sheet.endswith("#REF"):
                            continue
                        for part in ([sheet] if sheet.startswith("'") else sheet.split(":")):
                            if part.startswith("'") and part.endswith("'"):
                                part = part[1:-1].replace("''", "'")
                            if part and part != ws.title:
                                seen.add(part)
                for part in seen:
                    counts[(ws.title, part)] += 1
    return counts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--books", type=int, default=60)
    args = ap.parse_args()
    bad = total = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for seed in range(args.books):
            rng = random.Random(seed)
            names = rng.sample(NAMES, k=rng.randint(3, 7))
            wb = openpyxl.Workbook()
            wb.remove(wb.active)
            for name in names:
                ws = wb.create_sheet(name)
                for r in range(1, 6):
                    ws.append([r, r * 10, f"v{r}"])
            for name in names:
                ws = wb[name]
                for k, f in enumerate(formulas_for(rng, names, name)):
                    ws.cell(row=7 + k, column=1, value=f)
            d = tmp / f"b{seed}"
            d.mkdir()
            wb.save(d / "book.xlsx")
            want = oracle(openpyxl.load_workbook(d / "book.xlsx"))
            out = subprocess.run([args.bin, "graph", str(d), "-", "--no-cache"], capture_output=True, check=True).stdout
            doc = json.loads(out)
            got = Counter()
            for l in doc["links"]:
                if l["relation"] != "references":
                    continue
                m = re.match(r'^(\d+) formula cells? in sheet "(.*)" read sheet "(.*)"$', l["evidence"][0], re.S)
                if m:
                    got[(m.group(2), m.group(3))] += int(m.group(1))
            total += sum(want.values())
            if got != want:
                bad += 1
                print(f"DIFF seed {seed}: missing {dict(want - got)} extra {dict(got - want)}")
    print("ok" if not bad else f"{bad} differ", f"({args.books} workbooks, {total} sheet references)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
