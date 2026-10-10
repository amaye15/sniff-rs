#!/usr/bin/env python3
"""Checks the `version_of` links `sniff-rs graph` reads from file names.

    python3 tools/check_versions.py [--bin target/release/sniff-rs] [--folders 60]

Each folder holds file series made by a script that knows the true order:
numbered (`_v2`, `-V3`, ` v1.2`, `_ver4`, `version 5`), dated
(`2024-01-31`, `20240131`, `2024_02`) and marked (`draft`, plain, `final`),
plus names that only look like versions (`part_1`, `chapter2`, `data_2`) and
a series split over two folders. Modified times are scrambled, so the order
has to come from the names. The graph must link each file to the one before
it in its series, newer to older, and nothing else.
"""
import argparse
import json
import os
import random
import subprocess
import sys
import tempfile
from pathlib import Path

BASES = ["budget", "report", "survey results", "inventory", "roadmap", "minutes", "pricing", "forecast"]
EXTS = ["docx", "csv", "xlsx", "txt", "md", "json"]


def numbered(rng, base):
    n = rng.randint(2, 6)
    style = rng.choice(["_v{}", "-V{}", " v{}", "_ver{}", " version {}", "_rev{}", ".v{}"])
    return [f"{base}{style.format(i)}" for i in range(1, n + 1)]


def dotted(rng, base):
    return [f"{base}_v1.{i}" for i in range(0, rng.randint(2, 5))] + [f"{base}_v1.10"]


def dated(rng, base):
    y = rng.randint(2019, 2025)
    days = sorted(rng.sample(range(1, 28), rng.randint(2, 5)))
    fmt = rng.choice(["{y}-{m:02d}-{d:02d}", "{y}{m:02d}{d:02d}", "{y}_{m:02d}_{d:02d}"])
    return [f"{base}_{fmt.format(y=y, m=rng.randint(1, 12) if False else 3, d=d)}" for d in days]


def monthly(rng, base):
    y = rng.randint(2019, 2025)
    months = sorted(rng.sample(range(1, 13), rng.randint(2, 6)))
    sep = rng.choice(["-", "_"])
    return [f"{base}_{y}{sep}{m:02d}" for m in months]


def marked(rng, base):
    return [f"{base}_draft", base, f"{base}_final"]


SCHEMES = [numbered, dotted, dated, monthly, marked]


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
            (d / "archive").mkdir(parents=True)
            want = set()
            bases = rng.sample(BASES, rng.randint(2, 4))
            files = []
            for base in bases:
                ext = rng.choice(EXTS)
                names = rng.choice(SCHEMES)(rng, base)
                paths = [f"{n}.{ext}" for n in names]
                if rng.random() < 0.3 and len(paths) > 2:
                    # A second folder holds its own series; versions across folders are not linked.
                    paths = [("archive/" + p if i % 2 else p) for i, p in enumerate(paths)]
                    groups = [[p for p in paths if p.startswith("archive/")], [p for p in paths if not p.startswith("archive/")]]
                else:
                    groups = [paths]
                for g in groups:
                    for a, b in zip(g, g[1:]):
                        want.add((b, a))  # newer -> older
                files += paths
            # Names that look like versions but are not.
            files += ["part_1.csv", "part_2.csv", "chapter2.txt", "data_2.csv", "data_3.csv", "figure 4.png"]
            for f in files:
                p = d / f
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text("id,v\n1,a\n" if f.endswith(".csv") else "text\n")
                t = rng.randint(1_600_000_000, 1_700_000_000)
                os.utime(p, (t, t))
            out = subprocess.run([args.bin, "graph", str(d), "-", "--no-cache"], capture_output=True, check=True).stdout
            doc = json.loads(out)
            got = {(l["source"], l["target"]) for l in doc["links"] if l["relation"] == "version_of"}
            total += len(want)
            if got != want:
                bad += 1
                print(f"DIFF seed {seed}: missing {sorted(want - got)} extra {sorted(got - want)}")
    print("ok" if not bad else f"{bad} differ", f"({args.folders} folders, {total} version links)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
