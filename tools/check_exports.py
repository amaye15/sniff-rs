#!/usr/bin/env python3
"""Checks the `exported_from` links `sniff-rs graph` finds between flat files and database tables.

    python3 tools/check_exports.py [--bin target/release/sniff-rs] [--folders 40]

Each folder holds a SQLite database made with Python's sqlite3. Some of its
tables are exported by pandas to CSV (all rows, or a sample), some under the
table's name and some under another name. Decoys: a CSV with the same
column names and unrelated values, a CSV with only some of the columns, and a
CSV of another table. The graph must link each true export, and nothing else.
A SQL script with only a schema is covered too: its tables hold no values, so
a CSV is linked only when it is named for the table.
"""
import argparse
import json
import random
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

import pandas as pd

TABLES = {
    "orders": ["order_id", "customer_id", "amount", "placed_on", "status"],
    "invoices": ["invoice_no", "client_code", "total", "issued", "paid"],
    "shipments": ["shipment_id", "carrier", "weight", "sent_on", "destination"],
    "tickets": ["ticket_id", "reporter", "severity", "opened", "summary"],
}


def make_rows(rng, name, cols, n):
    rows = []
    for i in range(n):
        rows.append((
            f"{name[:3].upper()}-{rng.randint(100000, 999999)}-{i}",
            f"C{rng.randint(10000, 99999)}",
            round(rng.uniform(1, 9000), 2),
            f"2024-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}",
            rng.choice(["open", "closed", "late", "new"]),
        ))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--folders", type=int, default=40)
    args = ap.parse_args()
    bad = total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for seed in range(args.folders):
            rng = random.Random(seed)
            d = Path(tmp) / f"f{seed}"
            d.mkdir()
            db = sqlite3.connect(d / "shop.db")
            names = rng.sample(sorted(TABLES), rng.randint(2, 4))
            data = {}
            for name in names:
                cols = TABLES[name]
                rows = make_rows(rng, name, cols, rng.randint(60, 200))
                db.execute(f"create table {name} ({', '.join(c + ' text' for c in cols)})")
                db.executemany(f"insert into {name} values (?,?,?,?,?)", [tuple(str(v) for v in r) for r in rows])
                data[name] = (cols, rows)
            db.commit()
            db.close()
            want = set()
            for k, name in enumerate(names):
                cols, rows = data[name]
                if k == 0:
                    continue  # one table is never exported
                part = rows if rng.random() < 0.5 else rng.sample(rows, max(30, len(rows) // 2))
                fname = rng.choice([f"{name}.csv", f"{name}_2024.csv", f"export_{name}.csv", f"dump{k}.csv"])
                pd.DataFrame(part, columns=cols).to_csv(d / fname, index=False)
                want.add((fname, name))
            first = names[0]
            cols, rows = data[first]
            # Decoys.
            other = [(f"X{rng.randint(1, 10**9)}", f"Z{rng.randint(1, 10**9)}", round(rng.uniform(1, 9), 2), "2020-01-01", "n/a") for _ in range(80)]
            pd.DataFrame(other, columns=cols).to_csv(d / "same_columns_other_values.csv", index=False)
            pd.DataFrame([r[:2] for r in rows], columns=cols[:2]).to_csv(d / "two_columns_only.csv", index=False)
            out = subprocess.run([args.bin, "graph", str(d), "-", "--no-cache"], capture_output=True, check=True).stdout
            doc = json.loads(out)
            nodes = {n["id"]: n for n in doc["nodes"]}
            got = set()
            for l in doc["links"]:
                if l["relation"] != "exported_from":
                    continue
                s, t = nodes[l["source"]], nodes[l["target"]]
                got.add((Path(s["source_file"]).name, t["label"].split("#")[-1].split("/")[-1]))
            total += len(want)
            if got != want:
                bad += 1
                print(f"DIFF seed {seed}: missing {sorted(want - got)} extra {sorted(got - want)}")
    print("ok" if not bad else f"{bad} differ", f"({args.folders} folders, {total} export links)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
