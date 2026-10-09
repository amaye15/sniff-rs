#!/usr/bin/env python3
"""Checks `same_column` links of `sniff-rs graph --columns` on planted renames.

    python3 tools/check_renames.py [--bin ./target/release/sniff-rs] [SEEDS]

Each seed writes parent tables with a unique code column and child tables
whose column of a different name holds codes drawn from one parent (a
foreign key under another name), plus columns of unrelated words. The
graph must link exactly the planted pairs of column nodes with
`same_column`, and nothing else.
"""
import json, os, random, subprocess, sys, tempfile

NAMES = ["sku_code", "product_ref", "store_code", "outlet_ref", "vendor_code", "supplier_ref", "route_code",
         "lane_ref", "asset_code", "device_ref", "badge_code", "member_ref"]

def main():
    args = sys.argv[1:]
    binary = "./target/release/sniff-rs"
    if args[:1] == ["--bin"]:
        binary, args = args[1], args[2:]
    seeds = [int(a) for a in args] or list(range(1, 21))
    bad = planted_total = 0
    for seed in seeds:
        rng = random.Random(seed)
        pool = NAMES[:]
        rng.shuffle(pool)
        with tempfile.TemporaryDirectory() as tmp:
            folder = os.path.join(tmp, "in"); os.makedirs(folder)
            planted = set()
            for p in range(rng.randint(1, 3)):
                key, fk = pool.pop(), pool.pop()
                codes = [f"{key[:2].upper()}{rng.randrange(16**6):06x}" for _ in range(rng.randint(24, 40))]
                codes = sorted(set(codes))
                with open(os.path.join(folder, f"parent{p}.csv"), "w") as f:
                    f.write(f"{key},label{p}\n" + "\n".join(f"{c},{rng.choice(['red','green','blue'])}" for c in codes) + "\n")
                with open(os.path.join(folder, f"child{p}.csv"), "w") as f:
                    f.write(f"line{p},{fk},qty{p}\n" + "\n".join(f"{i},{rng.choice(codes)},{rng.randint(1, 9)}" for i in range(rng.randint(40, 60))) + "\n")
                planted.add(frozenset((key, fk)))
            out = os.path.join(tmp, "out")
            r = subprocess.run([binary, "graph", folder, out, "--columns", "--no-cache"], capture_output=True, text=True)
            if r.returncode:
                print(seed, "failed", r.stderr[-200:]); bad += 1; continue
            g = json.load(open(os.path.join(out, "graph.json")))
            got = {frozenset((l["source"][7:], l["target"][7:])) for l in g["links"] if l["relation"] == "same_column"}
            planted_total += len(planted)
            if got != planted:
                bad += 1
                print(f"seed {seed}: missing {[sorted(x) for x in planted - got]} extra {[sorted(x) for x in got - planted]}")
            else:
                print(f"seed {seed}: ok ({len(planted)} renames)")
    print("planted", planted_total)
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    main()
