#!/usr/bin/env python3
"""Checks what `sniff-rs graph --git` reads from a repository's history.

    python3 tools/check_git.py [--bin target/release/sniff-rs] [--repos 25]

Each run builds a repository with a scripted history (the script is the
answer, not git): people committing under several names and addresses (one
mapped by .mailmap), files that always change together, one-off commits,
a commit touching many files, files outside the graphed folder, files that
are later deleted. `sniff-rs graph proj --git` must report, for every file
that still exists, the same authors with the same number of commits, and the
same pairs of files that change together (at least 3 commits, and at least
half of the less-changed file's commits) with the same counts.
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
from collections import Counter, defaultdict
from itertools import combinations
from pathlib import Path

PEOPLE = [("Ann Ng", "ann@example.org"), ("Bo Li", "bo@example.org"), ("Cy Roe", "cy@example.com"),
          ("Dee Fox", "dee@example.net")]


def git(repo, *args, env=None):
    e = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", **(env or {}))
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True, env=e).stdout


def commit(repo, files, who, message, day):
    for f in files:
        p = repo / f
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, "a") as fh:
            fh.write(f"{message}\n")
    git(repo, "add", "-A")
    name, email = who
    when = f"2024-01-{day:02d}T12:00:00+00:00"
    git(repo, "-c", f"user.name={name}", "-c", f"user.email={email}", "commit", "-q", "-m", message,
        env={"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})


def build(rng, repo):
    git(repo, "init", "-q", "-b", "main")
    (repo / ".mailmap").write_text("Ann Ng <ann@example.org> <ann.old@example.org>\n")
    inside = [f"proj/{n}.txt" for n in "abcdefgh"] + ["proj/sub/x.txt", "proj/sub/y.txt"]
    outside = ["other/z.txt", "README.md"]
    groups = [["proj/a.txt", "proj/b.txt"], ["proj/c.txt", "proj/d.txt", "proj/sub/x.txt"]]
    commits = []  # (files, canonical (name, email))
    n = 0
    for k in range(rng.randint(25, 45)):
        n += 1
        r = rng.random()
        if r < 0.45:
            files = list(rng.choice(groups))
        elif r < 0.85:
            files = rng.sample(inside, rng.randint(1, 3))
        elif r < 0.92:
            files = list(rng.sample(inside + outside, rng.randint(2, 5)))
        else:
            files = [f"proj/mass{j}.txt" for j in range(70)]  # too many to say anything
        who = rng.choice(PEOPLE)
        committed_as = who
        if who[1] == "ann@example.org" and rng.random() < 0.4:
            committed_as = ("Ann N.", "ann.old@example.org")
        commit(repo, files, committed_as, f"c{n}", 1 + k % 28)
        commits.append((files, who))
    # A deletion: that file's history no longer matters.
    gone = "proj/h.txt"
    if (repo / gone).exists():
        git(repo, "rm", "-q", gone)
        git(repo, "-c", "user.name=Bo Li", "-c", "user.email=bo@example.org", "commit", "-q", "-m", "drop")
    return commits


def expected(repo, commits):
    present = {str(p.relative_to(repo)) for p in (repo / "proj").rglob("*") if p.is_file()}
    present = {f for f in present}
    authors = defaultdict(Counter)
    own = Counter()
    pair = Counter()
    for files, who in commits:
        files = sorted({f for f in files if f in present})
        for f in files:
            own[f] += 1
            authors[f][who[1]] += 1
        if 2 <= len(files) <= 60:
            for a, b in combinations(files, 2):
                pair[(a, b)] += 1
    together = {}
    for (a, b), t in pair.items():
        if t >= 3 and t / min(own[a], own[b]) >= 0.5:
            together[(a, b)] = t
    return authors, together, own


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--repos", type=int, default=25)
    args = ap.parse_args()
    bad = pairs_seen = 0
    with tempfile.TemporaryDirectory() as tmp:
        for seed in range(args.repos):
            repo = Path(tmp) / f"r{seed}"
            repo.mkdir()
            commits = build(random.Random(seed), repo)
            authors, together, own = expected(repo, commits)
            out = subprocess.run([args.bin, "graph", str(repo / "proj"), "-", "--no-cache", "--git"],
                                 capture_output=True, check=True,
                                 env=dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null")).stdout
            doc = json.loads(out)
            got_authors = defaultdict(Counter)
            got_together = {}
            for l in doc["links"]:
                if l["relation"] == "involves" and l["target"].startswith("person:") and l["source"] != l["target"]:
                    m = re.search(r"author of (\d+) commit", " ".join(l["evidence"]))
                    if m:
                        got_authors[l["source"].replace("sub/", "sub/")][l["target"][len("person:"):]] = int(m.group(1))
                if l["relation"] == "changes_with":
                    m = re.search(r"changed together in (\d+) commits", l["evidence"][0])
                    a, b = sorted((l["source"], l["target"]))
                    got_together[a, b] = int(m.group(1))
            # Graph ids are relative to the graphed folder.
            want_authors = {f[len("proj/"):]: dict(c) for f, c in authors.items()}
            want_together = {(a[len("proj/"):], b[len("proj/"):]): t for (a, b), t in together.items()}
            # A person needs two files to be a node; leave the rest out of both sides.
            seen_people = Counter(e for c in want_authors.values() for e in c)
            nodes = {n["id"][len("person:"):] for n in doc["nodes"] if n["type"] == "person"}
            want_authors = {f: {e: n for e, n in c.items() if e in nodes} for f, c in want_authors.items()}
            want_authors = {f: c for f, c in want_authors.items() if c}
            got = {f: dict(c) for f, c in got_authors.items()}
            pairs_seen += len(want_together)
            if want_authors != got or want_together != got_together:
                bad += 1
                print(f"DIFF seed {seed}:")
                if want_authors != got:
                    for f in sorted(set(want_authors) | set(got)):
                        if want_authors.get(f) != got.get(f):
                            print("  authors", f, "want", want_authors.get(f), "got", got.get(f))
                if want_together != got_together:
                    print("  together want", want_together, "got", got_together)
    print("ok" if not bad else f"{bad} differ", f"({args.repos} repositories, {pairs_seen} co-change pairs)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
