#!/usr/bin/env python3
"""Makes the synthetic inputs behind the scale numbers in CLAUDE.md "Phase 10".

    python3 tools/gen_scale.py files  OUT_DIR 50000
    python3 tools/gen_scale.py tables OUT_DIR 1000 100    # 1,000 SQLite files of 100 tables
    python3 tools/gen_scale.py tables OUT_DIR 1 10000     # one SQLite file of 10,000 tables

`files`: small CSV, JSONL, Markdown and text files in 200 folders; they share
20,000 customer e-mails, so about one link in the graph per mention.
`tables`: each table has an `id` and three of twelve shared column names, so
a few key names are held by thousands of tables.

Then, for example:

    /usr/bin/time -l sniff-rs graph OUT_DIR OUT_DIR.graph
    /usr/bin/time -l sniff-rs rank OUT_DIR/db00000.sqlite

The checks are byte equality with the previous build's graph.json and the
unit tests; there is no oracle, because the point is time and memory.
"""
import json
import os
import random
import sqlite3
import sys


def make_files(root, n):
    rng = random.Random(7)
    words = [f"w{i}" for i in range(4000)]
    cust = [f"c{i:07d}@shop{i % 300}.example.com" for i in range(20000)]
    ndir = 200
    for d in range(ndir):
        os.makedirs(f"{root}/d{d:03d}", exist_ok=True)
    for i in range(n):
        d = f"{root}/d{i % ndir:03d}"
        k = i % 5
        if k == 0:
            with open(f"{d}/orders_{i}.csv", "w") as f:
                f.write("order_id,customer_id,amount,note\n")
                for r in range(30):
                    f.write(f"{i * 100 + r},{rng.randrange(20000)},{rng.random() * 100:.2f},{' '.join(rng.sample(words, 3))}\n")
        elif k == 1:
            with open(f"{d}/customers_{i}.csv", "w") as f:
                f.write("customer_id,email,region\n")
                for r in range(30):
                    c = rng.randrange(20000)
                    f.write(f"{c},{cust[c]},r{c % 12}\n")
        elif k == 2:
            with open(f"{d}/events_{i}.jsonl", "w") as f:
                for r in range(20):
                    f.write(json.dumps({"id": i * 50 + r, "user": cust[rng.randrange(20000)], "tags": rng.sample(words, 2)}) + "\n")
        elif k == 3:
            with open(f"{d}/notes_{i}.md", "w") as f:
                f.write("# Notes\n" + " ".join(rng.choices(words, k=120)) + f"\nContact {cust[rng.randrange(20000)]}\n")
        else:
            with open(f"{d}/report_{i}.txt", "w") as f:
                f.write(" ".join(rng.choices(words, k=200)) + "\n")


def make_tables(root, nfiles, per):
    rng = random.Random(11)
    cols = ["customer_id", "order_id", "product_id", "region", "amount", "qty", "sku", "email", "created", "status", "store_id", "note"]
    for f in range(nfiles):
        db = sqlite3.connect(f"{root}/db{f:05d}.sqlite")
        for t in range(per):
            cs = ["id"] + rng.sample(cols, 3)
            ddl = ", ".join(c + (" integer" if c.endswith("id") or c == "qty" else " text") for c in cs)
            db.execute(f"create table t{f}_{t} ({ddl})")
            rows = [
                tuple(i if c == "id" else (rng.randrange(5000) if c.endswith("_id") or c == "qty" else f"{c}{rng.randrange(5000)}") for c in cs)
                for i in range(6)
            ]
            db.executemany(f"insert into t{f}_{t} values ({','.join('?' * len(cs))})", rows)
        db.commit()
        db.close()


def main():
    if len(sys.argv) < 4 or sys.argv[1] not in ("files", "tables"):
        sys.exit(__doc__)
    root = sys.argv[2]
    os.makedirs(root, exist_ok=True)
    if sys.argv[1] == "files":
        make_files(root, int(sys.argv[3]))
    else:
        if len(sys.argv) < 5:
            sys.exit(__doc__)
        make_tables(root, int(sys.argv[3]), int(sys.argv[4]))


if __name__ == "__main__":
    main()
