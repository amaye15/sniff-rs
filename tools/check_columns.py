#!/usr/bin/env python3
"""Checks `sniff-rs graph --columns` against pandas on generated CSV folders.

    python3 tools/check_columns.py [--bin ./target/release/sniff-rs] [SEEDS]

For each seed it writes a folder of CSV tables whose column names are one
of a few spellings of a shared vocabulary (snake_case, camelCase, Title
Case) and whose types are mostly consistent per name, runs the graph, and
recomputes from what pandas reads: which column names are shared by two or
more tables (identical schemas count once), which tables have them, and
where a name has two types. The graph must agree on the column nodes,
their has_column links (a table, or the schema node of identical tables),
and every type difference pandas shows must be a type_drift link.
"""
import json, os, random, re, subprocess, sys, tempfile

GENERIC = set("""name names date time datetime timestamp type value values status description notes note comment
comments title label year month day week count total created created_at created_on updated updated_at updated_on
modified modified_at deleted_at text data flag active is_active order rank level size version source""".split())
SURROGATE = {"id","uuid","guid","pk","key","oid","rowid","row_id","row","row_num","row_number","index","idx","level_0"}

def canon(name):
    out, need_sep, prev_lower = [], False, False
    for c in name:
        if c.isascii() and c.isalnum():
            if c.isupper() and prev_lower and need_sep:
                out.append("_")
            out.append(c.lower()); need_sep = True; prev_lower = c.islower() or c.isdigit()
        elif need_sep:
            out.append("_"); need_sep = False; prev_lower = False
    s = "".join(out)
    return s[:-1] if s.endswith("_") else s

def generic(c):
    if len(c) < 2 or c in SURROGATE or c in GENERIC:
        return True
    stem = c.rstrip("0123456789")
    return stem != c and stem.rstrip("_") in ("", "col", "column", "field", "unnamed", "var", "v", "x", "c")

VOCAB = ["customer_id", "order_total", "ship_date", "warehouse_code", "unit_price", "discount_rate", "region_code",
         "supplier_ref", "invoice_no", "sku_code", "carrier_name", "weight_kg", "tax_amount", "currency_code",
         "payment_method", "channel_code", "campaign_ref", "store_code", "latitude_deg", "longitude_deg"]

def spell(base, rng):
    parts = base.split("_")
    return rng.choice(["_".join(parts), parts[0] + "".join(p.title() for p in parts[1:]), " ".join(p.title() for p in parts), "-".join(parts)])

def value(kind, rng, i):
    if kind == "int": return rng.randint(1000, 90000)
    if kind == "float": return round(rng.uniform(1, 500), 2) + 0.5
    return rng.choice(["alpha", "bravo", "delta", "echo", "kilo", "lima", "oscar", "tango"]) + str(i % 5)

def make(folder, rng):
    base_kind = {v: rng.choice(["int", "float", "str"]) for v in VOCAB}
    tables, sets = {}, []
    for t in range(rng.randint(10, 24)):
        for _ in range(20):
            cols = rng.sample(VOCAB, rng.randint(3, 8))
            cs = set(cols)
            # no near-duplicates (the schema grouping has its own tests)
            if all(cs == s or len(cs & s) / len(cs | s) < 0.6 for s in sets):
                break
        sets.append(cs)
        kinds = {c: (base_kind[c] if rng.random() > 0.15 else rng.choice(["int", "float", "str"])) for c in cols}
        tables[f"t{t:02d}"] = [(spell(c, rng), kinds[c]) for c in cols]
        if rng.random() < 0.25:   # an identical twin
            tables[f"t{t:02d}_copy"] = [(n, k) for n, k in tables[f"t{t:02d}"]]
    for name, cols in tables.items():
        rows = [[value(k, rng, i) for _, k in cols] for i in range(rng.randint(12, 20))]
        with open(os.path.join(folder, name + ".csv"), "w") as f:
            f.write(",".join(n for n, _ in cols) + "\n")
            for r in rows: f.write(",".join(map(str, r)) + "\n")
    return tables

def klass(dtype):
    k = dtype.kind
    return {"i": "int", "f": "float"}.get(k, "str")

def main():
    import pandas as pd
    args = sys.argv[1:]
    binary = "./target/release/sniff-rs"
    if args[:1] == ["--bin"]:
        binary, args = args[1], args[2:]
    seeds = [int(a) for a in args] or list(range(1, 21))
    bad = 0
    for seed in seeds:
        rng = random.Random(seed)
        with tempfile.TemporaryDirectory() as tmp:
            folder = os.path.join(tmp, "in"); os.makedirs(folder)
            make(folder, rng)
            out = os.path.join(tmp, "out")
            r = subprocess.run([binary, "graph", folder, out, "--columns", "--no-cache"], capture_output=True, text=True)
            if r.returncode: print(seed, "graph failed:", r.stderr[-300:]); bad += 1; continue
            g = json.load(open(os.path.join(out, "graph.json")))
            # what pandas reads
            info = {}
            for f in sorted(os.listdir(folder)):
                df = pd.read_csv(os.path.join(folder, f))
                info[f] = {canon(c): klass(df[c].dtype) for c in df.columns}
            schema_key = {f: tuple(sorted(cols)) for f, cols in info.items()}
            twins = {}
            for f, k in schema_key.items(): twins.setdefault(k, []).append(f)
            holders = {}      # canon -> list of (unit key, tables, class)
            for k, fs in twins.items():
                for c in k:
                    if generic(c): continue
                    holders.setdefault(c, []).append((tuple(fs), len(fs), info[fs[0]][c]))
            want_nodes = {c: sum(h[1] for h in hs) for c, hs in holders.items() if len(hs) >= 2}
            got_nodes = {n["id"][7:]: n for n in g["nodes"] if n["type"] == "column"}
            problems = []
            for c, n in want_nodes.items():
                if c not in got_nodes: problems.append(f"missing node {c}")
                elif got_nodes[c]["tables"] != n: problems.append(f"{c}: tables {got_nodes[c]['tables']} != {n}")
            for c in got_nodes:
                if c not in want_nodes: problems.append(f"unexpected node {c}")
            # has_column: every holder unit links
            node_files = {n["id"]: n for n in g["nodes"]}
            has = {(l["source"], l["target"]) for l in g["links"] if l["relation"] == "has_column"}
            for c, hs in holders.items():
                if c not in want_nodes: continue
                for fs, cnt, _ in hs:
                    if cnt == 1:
                        if (fs[0], f"column:{c}") not in has: problems.append(f"missing has_column {fs[0]} -> {c}")
                    else:
                        src = [s for (s, t) in has if t == f"column:{c}" and s.startswith("schema:")]
                        if not src: problems.append(f"missing schema has_column for {fs} -> {c}")
            expected_links = sum(len(hs) for c, hs in holders.items() if c in want_nodes)
            actual_links = sum(1 for (s, t) in has if t[7:] in want_nodes)
            if expected_links != actual_links: problems.append(f"has_column links {actual_links} != {expected_links}")
            # type drift: pandas classes that differ must be drift links
            drift = {(l["source"], l["target"]) for l in g["links"] if l["relation"] == "type_drift"}
            for c, hs in holders.items():
                if c not in want_nodes: continue
                classes = {}
                for fs, cnt, k in hs: classes[k] = classes.get(k, 0) + cnt
                if len(classes) > 1:
                    top = max(classes.values())
                    if list(classes.values()).count(top) == 1:
                        majority = max(classes, key=classes.get)
                        for fs, cnt, k in hs:
                            if k != majority and not any(t == f"column:{c}" and (s == fs[0] or s.startswith("schema:")) for (s, t) in drift):
                                problems.append(f"missing type_drift {fs[0]} -> {c} ({k} vs {majority})")
            if problems:
                bad += 1
                print(f"seed {seed}: {len(problems)} problems")
                for p in problems[:6]: print("   ", p)
            else:
                print(f"seed {seed}: ok ({len(want_nodes)} column nodes, {len(info)} tables)")
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    main()
