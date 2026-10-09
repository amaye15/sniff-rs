#!/usr/bin/env python3
"""Checks `sniff-rs graph --db` against the servers' own catalogs.

    python3 tools/check_live_db.py [--bin target/release/sniff-rs] [--seeds 30]

For each seed a random schema (tables with awkward names, composite primary
keys, composite and self-referencing foreign keys) is created in a real
PostgreSQL and a real MySQL server (the ones tools/gen_ddl_vectors.py uses),
`sniff-rs graph <empty folder> --db <server>/<database>` reads it back, and
the table nodes (names, column names in order) and the declared joins between
tables must match what the server's catalog says.
"""
import argparse
import json
import re
import subprocess
import sys
import tempfile
import random
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import gen_ddl_vectors as g  # noqa: E402


def graph(binary, folder, uri):
    out = subprocess.run([binary, "graph", str(folder), "-", "--no-cache", "--db", uri],
                         capture_output=True, check=True).stdout
    doc = json.loads(out)
    cols = {}
    for n in doc["nodes"]:
        if n["type"] == "table" and n["source_file"].startswith(("postgres-", "mysql-")):
            cols[n["label"]] = n["column_names"]
    joins = set()
    for l in doc["links"]:
        if l["relation"] == "joins" and l["confidence"] == "EXTRACTED" and l["evidence"] and l["evidence"][0].startswith("declared foreign key"):
            a, b = l["source"].split("#", 1)[1], l["target"].split("#", 1)[1]
            joins.add((a, b))
    return cols, joins


def expected(tables):
    cols = {t["name"]: t["columns"] for t in tables}
    joins = set()
    for t in tables:
        for fk in t["fks"]:
            if fk["table"] != t["name"]:
                joins.add((t["name"], fk["table"]))
    return cols, joins


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--seeds", type=int, default=30)
    ap.add_argument("--pg-port", type=int, default=54329)
    ap.add_argument("--my-port", type=int, default=33069)
    args = ap.parse_args()
    bad = checked = 0
    with tempfile.TemporaryDirectory() as folder:
        for seed in range(args.seeds):
            for dialect in ("postgres", "mysql"):
                rng = random.Random(seed)
                tables = g.make_schema(rng, dialect)
                db = f"live{seed}"
                if dialect == "postgres":
                    g.psql(args.pg_port, "postgres", f'DROP DATABASE IF EXISTS "{db}"; CREATE DATABASE "{db}";')
                    g.psql(args.pg_port, db, "\n".join(g.ddl(tables, "postgres")))
                    cat = json.loads(g.psql(args.pg_port, db, g.PG_CATALOG, tuples=True))
                    uri = f"postgresql://postgres@127.0.0.1:{args.pg_port}/{db}"
                    want = expected(cat)
                else:
                    g.my(args.my_port, f"DROP DATABASE IF EXISTS `{db}`; CREATE DATABASE `{db}`;")
                    g.my(args.my_port, "SET FOREIGN_KEY_CHECKS=0;\n" + "\n".join(g.ddl(tables, "mysql")), db)
                    cols = g.my(args.my_port, "select table_name, column_name from information_schema.columns "
                                f"where table_schema='{db}' order by table_name, ordinal_position")
                    keys = g.my(args.my_port, "select table_name, constraint_name, column_name, ifnull(referenced_table_name,'') "
                                "from information_schema.key_column_usage "
                                f"where table_schema='{db}' order by table_name, constraint_name, ordinal_position")
                    cat = {}
                    for line in cols.splitlines():
                        t, c = line.split("\t")
                        cat.setdefault(t, {"name": t, "columns": [], "fks": []})["columns"].append(c)
                    seen = set()
                    for line in keys.splitlines():
                        t, cn, c, rt = line.split("\t")
                        if cn != "PRIMARY" and rt and (t, cn) not in seen:
                            seen.add((t, cn))
                            cat[t]["fks"].append({"table": rt})
                    uri = f"mysql://root@127.0.0.1:{args.my_port}/{db}"
                    want = expected(list(cat.values()))
                got = graph(args.bin, folder, uri)
                checked += 1
                if got != want:
                    bad += 1
                    print(f"DIFF {dialect} seed {seed}")
                    if got[0] != want[0]:
                        for t in sorted(set(got[0]) | set(want[0])):
                            if got[0].get(t) != want[0].get(t):
                                print("  table", t, "want", want[0].get(t), "got", got[0].get(t))
                    if got[1] != want[1]:
                        print("  joins want-got", want[1] - got[1], "got-want", got[1] - want[1])
                if dialect == "postgres":
                    g.psql(args.pg_port, "postgres", f'DROP DATABASE "{db}";')
                else:
                    g.my(args.my_port, f"DROP DATABASE `{db}`")
    print("ok" if not bad else f"{bad} differ", f"({checked} live databases)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
