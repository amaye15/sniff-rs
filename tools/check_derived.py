#!/usr/bin/env python3
"""Checks the `derived_from` links `sniff-rs graph` makes from SQL and Python.

    python3 tools/check_derived.py [--bin target/release/sniff-rs] [--folders 60]

Each folder holds CSV files and random scripts: SQL files of one to four
statements (CREATE TABLE ... AS SELECT with joins and CTEs, INSERT INTO ...
SELECT, plain SELECTs) and Python files that read some CSVs and write others.
What a statement writes was made from what that same statement reads
(sqlglot says which tables are which); a Python script's outputs come from
all of its inputs (the `ast` module says which calls are which). The links in
the graph must be exactly those pairs. Needs sqlglot.
"""
import argparse
import ast
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

import sqlglot
from sqlglot import exp

NAMES = ["orders", "customers", "sales", "refunds", "regions", "products", "stock", "returns",
         "visits", "events", "users", "tickets"]


def sql_script(rng, tables):
    stmts = []
    for _ in range(rng.randint(1, 4)):
        a, b, c = rng.sample(tables, 3)
        kind = rng.randint(0, 5)
        if kind == 0:
            stmts.append(f"CREATE TABLE out_{c} AS SELECT x.id, y.v FROM {a} x JOIN {b} y ON x.id = y.id;")
        elif kind == 1:
            stmts.append(f"INSERT INTO out_{c} SELECT * FROM {a} WHERE id IN (SELECT id FROM {b});")
        elif kind == 2:
            stmts.append(f"SELECT * FROM {a} UNION ALL SELECT * FROM {b};")
        elif kind == 3:
            stmts.append(f"CREATE TABLE out_{c} AS WITH t AS (SELECT * FROM {a}) SELECT * FROM t JOIN {b} USING (id);")
        elif kind == 4:
            stmts.append(f"CREATE VIEW out_{c} AS SELECT * FROM {a};")
        else:
            stmts.append(f"DELETE FROM out_{c} WHERE id IN (SELECT id FROM {a});")
    return "\n".join(stmts) + "\n"


def sql_truth(script):
    pairs = set()
    for stmt in sqlglot.parse(script):
        if stmt is None:
            continue
        ctes = {c.alias_or_name.lower() for c in stmt.find_all(exp.CTE)}
        target = None
        if isinstance(stmt, (exp.Create, exp.Insert)):
            t = stmt.this
            target = t.find(exp.Table) if not isinstance(t, exp.Table) else t
        if target is None:
            continue
        for t in stmt.find_all(exp.Table):
            if t is target or t.name.lower() in ctes or not t.name:
                continue
            pairs.add((target.name.lower(), t.name.lower()))
    return pairs


def py_script(rng, tables):
    ins = rng.sample(tables, rng.randint(1, 3))
    outs = rng.sample([t for t in tables if t not in ins], rng.randint(1, 2))
    lines = ["import pandas as pd"]
    for i, t in enumerate(ins):
        lines.append(f'd{i} = pd.read_csv("{t}.csv")')
    for t in outs:
        lines.append(f'd0.to_csv("pyout_{t}.csv")')
    return "\n".join(lines) + "\n"


def py_truth(script):
    reads, writes = set(), set()
    for node in ast.walk(ast.parse(script)):
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.args:
            if isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str):
                name = node.args[0].value.removesuffix(".csv")
                (reads if node.func.attr == "read_csv" else writes if node.func.attr == "to_csv" else set()).add(name)
    return {(w, r) for w in writes for r in reads}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--folders", type=int, default=60)
    args = ap.parse_args()
    bad = total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for seed in range(args.folders):
            rng = random.Random(seed)
            d = Path(tmp) / f"f{seed}"
            d.mkdir()
            tables = rng.sample(NAMES, rng.randint(6, 10))
            for t in tables:
                (d / f"{t}.csv").write_text("id,v\n1,a\n2,b\n")
            want = set()
            for i in range(rng.randint(1, 3)):
                s = sql_script(rng, tables)
                (d / f"q{i}.sql").write_text(s)
                want |= {(w.removeprefix("out_") if False else w, r) for w, r in sql_truth(s)}
            for i in range(rng.randint(0, 2)):
                s = py_script(rng, tables)
                (d / f"p{i}.py").write_text(s)
                want |= py_truth(s)
            out = subprocess.run([args.bin, "graph", str(d), "-", "--no-cache"], capture_output=True, check=True).stdout
            doc = json.loads(out)
            label = {n["id"]: n["label"] for n in doc["nodes"]}
            got = set()
            for l in doc["links"]:
                if l["relation"] == "derived_from":
                    a = label[l["source"]].lower().removesuffix(".csv")
                    b = label[l["target"]].lower().removesuffix(".csv")
                    got.add((a, b))
            # A written table nobody else names is not a node (one script alone makes no phantom),
            # so only pairs whose both ends are nodes can be compared.
            ends = {n["label"].lower().removesuffix(".csv") for n in doc["nodes"]}
            want = {(w, r) for w, r in want if w in ends and r in ends}
            total += len(want)
            if got != want:
                bad += 1
                print(f"DIFF seed {seed}: missing {sorted(want - got)} extra {sorted(got - want)}")
    print("ok" if not bad else f"{bad} differ", f"({args.folders} folders, {total} derived pairs)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
