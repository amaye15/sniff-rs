#!/usr/bin/env python3
"""Checks the `uses_column` links of `sniff-rs graph --columns` against sqlglot.

    python3 tools/check_uses_column.py [--bin ./target/release/sniff-rs] [SEEDS]

Writes CSV tables and .sql files of random queries over them; sqlglot says
which column names each file's statements reference, and the graph must
link the file to exactly those that are columns of the tables it reads or
writes (generic names like `id` and `name` have no node). Needs pandas'
sibling tools/check_columns.py next to it.
"""
import json, os, random, subprocess, sys, tempfile
import sqlglot
from sqlglot import exp
sys.path.insert(0, os.path.dirname(__file__))
import check_columns as cc

def queries(tables, rng):
    out = []
    for _ in range(rng.randint(1, 3)):
        names = rng.sample(sorted(tables), rng.randint(1, min(3, len(tables))))
        cols = []
        for t in names:
            cols += [(t, c) for c in rng.sample(sorted(tables[t]), min(len(tables[t]), rng.randint(1, 3)))]
        sel = ", ".join(f"{t}.{c}" if rng.random() < 0.5 else c for t, c in cols)
        sql = f"SELECT {sel} FROM {names[0]}"
        for t in names[1:]:
            sql += f" {rng.choice(['JOIN', 'LEFT JOIN'])} {t} ON {names[0]}.{sorted(tables[names[0]])[0]} = {t}.{sorted(tables[t])[0]}"
        if rng.random() < 0.5:
            t, c = rng.choice(cols)
            sql += f" WHERE {c} IS NOT NULL"
        if rng.random() < 0.3:
            sql = f"-- note: {rng.choice(sorted(tables[names[0]]))} is not used here\n" + sql
        out.append(sql + ";")
    return out

def main():
    args = sys.argv[1:]
    binary = "./target/release/sniff-rs"
    if args[:1] == ["--bin"]:
        binary, args = args[1], args[2:]
    seeds = [int(a) for a in args] or list(range(1, 16))
    bad = 0
    total = 0
    for seed in seeds:
        rng = random.Random(seed)
        with tempfile.TemporaryDirectory() as tmp:
            folder = os.path.join(tmp, "in"); os.makedirs(folder)
            cc.make(folder, rng)
            names = sorted(f[:-4] for f in os.listdir(folder) if f.endswith(".csv"))
            # canonical column names of each table, as the SQL spells them
            spelled = {}
            for n in names:
                header = open(os.path.join(folder, n + ".csv")).readline().strip().split(",")
                spelled[n] = {cc.canon(h): h for h in header}
            # SQL can only use plain identifiers: keep tables whose names are
            simple = {n: {c for c in cs if c.isidentifier() and not cc.generic(c)} for n, cs in spelled.items()}
            simple = {n: cs for n, cs in simple.items() if cs}
            for i in range(rng.randint(2, 5)):
                chosen = {n: simple[n] for n in rng.sample(sorted(simple), min(len(simple), rng.randint(1, 4)))}
                open(os.path.join(folder, f"q{i}.sql"), "w").write("\n".join(queries(chosen, rng)) + "\n")
            out = os.path.join(tmp, "out")
            r = subprocess.run([binary, "graph", folder, out, "--columns", "--no-cache"], capture_output=True, text=True)
            if r.returncode:
                print(seed, "failed:", r.stderr[-200:]); bad += 1; continue
            g = json.load(open(os.path.join(out, "graph.json")))
            got = {(l["source"], l["target"][7:]) for l in g["links"] if l["relation"] == "uses_column"}
            want = set()
            for i in range(10):
                p = os.path.join(folder, f"q{i}.sql")
                if not os.path.exists(p): continue
                sql = open(p).read()
                used_cols, used_tables = set(), set()
                for tree in sqlglot.parse(sql, read="postgres"):
                    used_cols |= {cc.canon(c.name) for c in tree.find_all(exp.Column)}
                    used_tables |= {t.name for t in tree.find_all(exp.Table)}
                have = set()
                for t in used_tables:
                    if t in spelled: have |= set(spelled[t])
                for c in used_cols & have:
                    if not cc.generic(c): want.add((f"q{i}.sql", c))
            total += len(want)
            if got != want:
                bad += 1
                print(f"seed {seed}: missing {sorted(want - got)[:4]} extra {sorted(got - want)[:4]}")
            else:
                print(f"seed {seed}: ok ({len(want)} uses_column links)")
    print("total expected links", total)
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    main()
