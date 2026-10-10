#!/usr/bin/env python3
"""Checks `sniff-rs search`, `neighbors`, `subgraph` and `communities`.

    python3 tools/check_queries.py [--bin target/release/sniff-rs] [--queries 40]

Each query command is compared with an independent implementation on the
graph.json of every graph under tests/fixtures (edge_graph_*, edge_knowledge_graph):

* search: SQLite FTS5's own `bm25()` over a table with the same five fields
  (weights 4, 3, 2, 1, 0.5; `unicode61 remove_diacritics 0`). The ranked
  nodes and every score must agree.
* neighbors: networkx `single_source_shortest_path_length` (nodes and
  distances), and the `via` of each node must be one step nearer and adjacent.
* subgraph: networkx ego graphs of the seeds and the induced multigraph;
  the output must also validate against src/graph.schema.json.
* communities: sizes, internal and boundary link counts from the link list,
  and the modularity from `networkx.community.modularity`.
"""
import argparse
import json
import random
import re
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

import networkx as nx
from networkx.algorithms.community import modularity

WEIGHTS = (4.0, 3.0, 2.0, 1.0, 0.5)


def run(binary, *args):
    r = subprocess.run([binary, *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"{args}: {r.stderr[-400:]}")
    return r.stdout


def graphs(root, binary, tmp):
    out = []
    for d in sorted(Path(root).glob("tests/fixtures/edge_graph_*")) + [Path(root) / "tests/fixtures/edge_knowledge_graph"]:
        if not d.is_dir():
            continue
        target = Path(tmp) / f"{d.name}.json"
        text = run(binary, "graph", str(d), "-", "--no-cache", "--output-format", "json")
        target.write_text(text)
        out.append((d.name, target, json.loads(text)))
    return out


def fields(doc, n):
    lists = [n.get(k) for k in ("top_terms", "column_names", "columns")]
    terms = " ".join(w for v in lists if isinstance(v, list) for w in v)
    kind = " ".join([n["type"], n.get("file_type", ""), n.get("entity_kind", "")])
    comm = doc["graph"]["communities"][n["community"]]["label"]
    return [n["label"], n["id"], terms, kind, comm]


def tokens(text):
    return [t.lower() for t in re.findall(r"[^\W_]+", text)]


def check_search(name, path, doc, binary, nqueries, rng):
    db = sqlite3.connect(":memory:")
    db.execute("create virtual table t using fts5(label,id,terms,kind,community, tokenize='unicode61 remove_diacritics 0')")
    for i, n in enumerate(doc["nodes"]):
        db.execute("insert into t(rowid,label,id,terms,kind,community) values (?,?,?,?,?,?)", (i + 1, *fields(doc, n)))
    vocab = sorted({w for n in doc["nodes"] for f in fields(doc, n) for w in tokens(f) if w.isascii()})
    bad = 0
    for q in range(nqueries):
        words = rng.sample(vocab, min(len(vocab), rng.randint(1, 3)))
        prefix = [rng.random() < 0.3 for _ in words]
        query_cli = " ".join(w[: max(1, len(w) - 2)] + "*" if p and len(w) > 2 else w for w, p in zip(words, prefix))
        used = [(w[: max(1, len(w) - 2)], True) if p and len(w) > 2 else (w, False) for w, p in zip(words, prefix)]
        uniq = []
        for u in used:
            if u not in uniq:
                uniq.append(u)
        match = " OR ".join(f'"{w}"' + ("*" if pre else "") for w, pre in uniq)
        want = db.execute(
            "select rowid, -bm25(t, 4, 3, 2, 1, 0.5) from t where t match ? order by bm25(t,4,3,2,1,0.5), rowid", (match,)
        ).fetchall()
        got = json.loads(run(binary, "search", str(path), query_cli, "--output-format", "json", "--top", "100000"))["results"]
        want_scores = {doc["nodes"][r - 1]["id"]: s for r, s in want}
        got_scores = {h["id"]: h["score"] for h in got}
        ok = set(want_scores) == set(got_scores) and all(
            abs(want_scores[k] - got_scores[k]) <= 1e-9 * max(1.0, abs(want_scores[k])) for k in want_scores
        )
        # Ranked order must be non-increasing by score.
        ok = ok and all(a["score"] >= b["score"] - 1e-12 for a, b in zip(got, got[1:]))
        if not ok:
            bad += 1
            print(f"DIFF search {name} {query_cli!r}: only-fts {sorted(set(want_scores) - set(got_scores))[:3]} only-mine {sorted(set(got_scores) - set(want_scores))[:3]}")
    return bad


def nxgraph(doc):
    g = nx.MultiGraph()
    ids = [n["id"] for n in doc["nodes"]]
    g.add_nodes_from(ids)
    for l in doc["links"]:
        if l["source"] != l["target"]:
            g.add_edge(l["source"], l["target"], relation=l["relation"], weight=l.get("weight", 1.0))
    return g


def check_neighbors(name, path, doc, binary, rng):
    g = nxgraph(doc)
    bad = 0
    for n in rng.sample(doc["nodes"], min(8, len(doc["nodes"]))):
        depth = rng.randint(1, 3)
        got = json.loads(run(binary, "neighbors", str(path), n["id"], "--depth", str(depth), "--output-format", "json"))
        want = nx.single_source_shortest_path_length(g, n["id"], cutoff=depth)
        want.pop(n["id"])
        mine = {x["id"]: x["distance"] for x in got["neighbors"]}
        ok = mine == want and got["count"] == len(want)
        for x in got["neighbors"]:
            via = x["via"]
            if want.get(via, 0) != x["distance"] - 1 or not g.has_edge(via, x["id"]):
                ok = False
            rels = sorted(d["relation"] for d in g.get_edge_data(via, x["id"]).values()) if g.has_edge(via, x["id"]) else []
            if sorted(l["relation"] for l in x["links"]) != rels:
                ok = False
        if not ok:
            bad += 1
            print(f"DIFF neighbors {name} {n['id']} depth {depth}")
    return bad


def check_subgraph(name, path, doc, binary, rng, schema, tmp):
    from jsonschema import Draft7Validator

    g = nxgraph(doc)
    bad = 0
    ncomm = len(doc["graph"]["communities"])
    for trial in range(6):
        nodes = [n["id"] for n in rng.sample(doc["nodes"], rng.randint(0, min(2, len(doc["nodes"]))))]
        comms = rng.sample(range(ncomm), rng.randint(0, min(1, ncomm)))
        if not nodes and not comms:
            nodes = [doc["nodes"][0]["id"]]
        depth = rng.randint(1, 2)
        args = ["subgraph", str(path), "--depth", str(depth)]
        for n in nodes:
            args += ["--node", n]
        for c in comms:
            args += ["--community", str(c)]
        sub = json.loads(run(binary, *args))
        seeds = set(nodes) | {n["id"] for n in doc["nodes"] if n["community"] in comms}
        keep = set()
        for s in seeds:
            keep |= set(nx.single_source_shortest_path_length(g, s, cutoff=depth))
        h = g.subgraph(keep)
        want_edges = sorted((min(a, b), max(a, b), d["relation"]) for a, b, d in h.edges(data=True))
        got_edges = sorted(
            (min(l["source"], l["target"]), max(l["source"], l["target"]), l["relation"])
            for l in sub["links"]
            if l["source"] != l["target"]
        )
        errs = [e.message for e in Draft7Validator(schema).iter_errors(sub)][:2]
        comm_ids = sorted({n["community"] for n in sub["nodes"]})
        ok = (
            {n["id"] for n in sub["nodes"]} == keep
            and want_edges == got_edges
            and not errs
            and comm_ids == list(range(len(comm_ids)))
            and len(sub["graph"]["communities"]) == len(comm_ids)
        )
        # Every other command reads the result.
        out = Path(tmp) / "sub.json"
        out.write_text(json.dumps(sub))
        run(binary, "rank", str(out))
        if not ok:
            bad += 1
            print(f"DIFF subgraph {name} nodes={nodes} comms={comms} depth={depth} {errs}")
    return bad


def check_communities(name, path, doc, binary):
    g = nxgraph(doc)
    got = json.loads(run(binary, "communities", str(path), "--output-format", "json", "--members"))
    by = {}
    for n in doc["nodes"]:
        by.setdefault(n["community"], set()).add(n["id"])
    wg = nx.Graph()
    wg.add_nodes_from(g.nodes)
    for a, b, d in g.edges(data=True):
        w = d.get("weight", 1.0)
        wg.add_edge(a, b, weight=wg[a][b]["weight"] + w if wg.has_edge(a, b) else w)
    q = modularity(wg, list(by.values()), weight="weight") if wg.number_of_edges() else 0.0
    ok = abs(q - got["modularity"]) < 1e-9 and got["communities"] == len(by)
    for c in got["list"]:
        members = by[c["id"]]
        internal = sum(1 for a, b in g.edges() if a in members and b in members)
        boundary = sum(1 for a, b in g.edges() if (a in members) != (b in members))
        if set(c["members"]) != members or c["size"] != len(members) or c["internal_links"] != internal or c["boundary_links"] != boundary:
            ok = False
    sizes = [c["size"] for c in got["list"]]
    ok = ok and sizes == sorted(sizes, reverse=True)
    if not ok:
        print(f"DIFF communities {name}: q {q} vs {got['modularity']}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--queries", type=int, default=40)
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    root = Path(__file__).resolve().parent.parent
    schema = json.loads((root / "src/graph.schema.json").read_text())
    rng = random.Random(args.seed)
    bad = total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for name, path, doc in graphs(root, args.bin, tmp):
            bad += check_search(name, path, doc, args.bin, args.queries, rng)
            bad += check_neighbors(name, path, doc, args.bin, rng)
            bad += check_subgraph(name, path, doc, args.bin, rng, schema, tmp)
            bad += check_communities(name, path, doc, args.bin)
            total += 1
    print("ok" if not bad else f"{bad} differ", f"({total} graphs)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
