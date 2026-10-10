#!/usr/bin/env python3
"""Checks `sniff-rs graph --unlinked-report`.

    python3 tools/check_unlinked.py [--bin target/release/sniff-rs] [--folders 30]

Each folder holds text documents on a few topics (so most are linked by
shared words), documents that mix a topic with unrelated words (they share a
few words with their topic but not enough for a link), a document in no
topic, and binary files. The report must list exactly the files that have no
link but structural ones in graph.json (worked out here, from the graph);
every candidate must be a real file below the similarity bar; and the loop
the report is for must close: a links file made by taking each file's best
candidate, passed back with `--links`, leaves no such file unlinked.
"""
import argparse
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

TOPICS = {
    "kitchen": "flour butter sugar oven bake whisk batter dough recipe simmer sauce roast season".split(),
    "garden": "soil seeds compost prune blossom harvest orchard greenhouse trellis mulch seedling watering".split(),
    "harbor": "vessel anchor cargo tide dock pier lighthouse captain voyage mooring ferry shipping".split(),
    "orchestra": "violin cello conductor symphony rehearsal concerto melody tempo baton score ensemble overture".split(),
}
FILLER = "report meeting notes project budget review schedule update summary draft plan status team week".split()
NOISE = "zephyr quartz lantern marble velvet cobalt saffron tundra glacier ember willow falcon".split()
STRUCTURAL = {"contains", "in_folder", "has_column", "shares_key", "type_drift"}


def pseudo(rng):
    """A made-up word with vowels, so it counts as a word but nobody else has it."""
    return "".join(rng.choice("bcdfghklmnprstvz") + rng.choice("aeiou") for _ in range(4))


def doc(rng, words, n=120):
    return " ".join(rng.choice(words) for _ in range(n)) + "\n"


def graph(binary, folder, out, *extra):
    subprocess.run([binary, "graph", str(folder), str(out), "--no-cache", "--unlinked-report", *extra], capture_output=True, check=True)
    return json.loads((out / "graph.json").read_text()), json.loads((out / "unlinked.json").read_text())


def lonely_ids(g):
    files = {n["id"] for n in g["nodes"] if n.get("type") == "file"}
    linked = set()
    for l in g["links"]:
        if l["relation"] in STRUCTURAL or l["source"] == l["target"]:
            continue
        linked.add(l["source"])
        linked.add(l["target"])
    return {f for f in files if f not in linked}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--folders", type=int, default=30)
    args = ap.parse_args()
    bad = total_lonely = with_candidates = 0
    with tempfile.TemporaryDirectory() as tmp:
        for seed in range(args.folders):
            rng = random.Random(seed)
            root = Path(tmp) / f"in{seed}"
            root.mkdir()
            topics = rng.sample(sorted(TOPICS), rng.randint(2, 3))
            for t in topics:
                for k in range(rng.randint(3, 5)):
                    (root / f"{t}_{k}.txt").write_text(doc(rng, TOPICS[t] + FILLER[:3]))
            for k in range(rng.randint(1, 3)):
                # Two words of one topic, said often, among words nobody else uses.
                t = rng.choice(topics)
                own = [pseudo(rng) for _ in range(60)]
                text = " ".join(own) + " " + " ".join(rng.sample(TOPICS[t], 2) * 30) + "\n"
                (root / f"mixed_{k}.txt").write_text(text)
            (root / "outsider.txt").write_text(doc(rng, NOISE))
            (root / "photo.png").write_bytes(bytes(rng.getrandbits(8) for _ in range(300)))
            out = Path(tmp) / f"out{seed}"
            g, u = graph(args.bin, root, out)
            want = lonely_ids(g)
            have = {f["id"] for f in u["files"]}
            total_lonely += len(want)
            if want != have:
                bad += 1
                print(f"DIFF seed {seed}: report lacks {sorted(want - have)} and has extra {sorted(have - want)}")
                continue
            files = {n["id"] for n in g["nodes"] if n.get("type") == "file"}
            links = []
            for f in u["files"]:
                for c in f["candidates"]:
                    assert c["id"] in files and c["id"] != f["id"], (seed, f["id"], c)
                    assert 0.05 <= c["cosine"] < 0.3, (seed, f["id"], c)
                    assert len(c["shared_terms"]) >= 1
                if f["candidates"]:
                    with_candidates += 1
                    t = dict(f["link_template"])
                    t.update(target=f["candidates"][0]["id"], label="shares vocabulary", evidence="agent: shared words", by="check_unlinked")
                    links.append(t)
            lf = Path(tmp) / f"links{seed}.json"
            lf.write_text(json.dumps({"links": links}))
            g2, u2 = graph(args.bin, root, Path(tmp) / f"out2_{seed}", "--links", str(lf))
            left = {f["id"] for f in u2["files"]}
            expect_left = have - {l["source"] for l in links}
            if left != expect_left:
                bad += 1
                print(f"DIFF seed {seed}: after --links still unlinked {sorted(left)}, expected {sorted(expect_left)}")
    print("ok" if not bad else f"{bad} differ", f"({args.folders} folders, {total_lonely} unlinked files, {with_candidates} with candidates)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
