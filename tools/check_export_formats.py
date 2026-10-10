#!/usr/bin/env python3
"""Checks the graph exports `sniff-rs graph --export gexf,jsonld,mermaid,sqlite`.

    python3 tools/check_export_formats.py [--bin target/release/sniff-rs] [--writer target/release/examples/sqlite_write]
                                          [--mermaid DIR] [--fuzz 40]

1. The SQLite writer: random tables (every integer width from one byte to
   eight, floats, empty / short / page-sized / huge text, nulls, rowids that
   need nine-byte varints, up to 60,000 rows so the b-tree has interior
   levels, rows of 100 KB so they spill into overflow pages). Each database
   must pass SQLite's own `PRAGMA integrity_check` and read back, through
   Python's `sqlite3`, to exactly what was written.
2. The exports of real graphs, each read by an independent program and
   compared with graph.json: SQLite by `sqlite3`; GEXF by NetworkX; JSON-LD by
   rdflib (the statements and the `Link` resources); Mermaid by the real
   Mermaid parser run under Node (`--mermaid` is a directory with `mermaid`
   and `jsdom` installed and `parse.mjs`), including folders whose file
   names hold quotes, brackets, `|`, `#`, `<`, `;` and backslashes.
"""
import argparse
import json
import math
import os
import random
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

import networkx as nx
import rdflib

INTS = [0, 1, -1, 2, -2, 127, -128, 128, -129, 32767, -32768, 32768, 8388607, -8388608, 8388608,
        2147483647, -2147483648, 2147483648, 140737488355327, -140737488355328, 140737488355328,
        2**62, -(2**62), 2**63 - 1, -(2**63)]
TEXTS = ["", "a", "hello world", "héllo wörld ✓ 日本語 🙂", "x" * 100, "y" * 4060, "z" * 4061, "w" * 4062, "v" * 5000, "u" * 12000]


def value(rng, big=False):
    r = rng.random()
    if r < 0.12:
        return None
    if r < 0.40:
        return rng.choice(INTS) if rng.random() < 0.5 else rng.randint(-10**6, 10**6)
    if r < 0.55:
        return rng.choice([0.0, 1.5, -2.25, 3.141592653589793, 1e-300, 1e300, rng.uniform(-1e6, 1e6)])
    if big and rng.random() < 0.05:
        return "".join(rng.choice("abcdefghij é日") for _ in range(rng.choice([100_000, 70_000, 9000])))
    t = rng.choice(TEXTS)
    return t + str(rng.randint(0, 99)) if rng.random() < 0.4 else t


def writer_case(rng, writer, tmp, idx):
    tables = []
    for t in range(rng.randint(1, 4)):
        ncols = rng.randint(1, 6)
        cols = [f"c{i}" for i in range(ncols)]
        alias = rng.random() < 0.5
        nrows = rng.choice([0, 1, 5, 40, 200, 3000, 20000, 60000]) if idx % 7 == 0 else rng.choice([0, 1, 5, 40, 200, 1500])
        rows = []
        rid = rng.choice([1, 1, 1, 2**40, 2**56])
        for _ in range(nrows):
            rid += rng.choice([1, 1, 1, 2, 300, 70000])
            vals = [value(rng, big=nrows < 50) for _ in cols]
            if alias:
                vals[0] = None
            rows.append([rid, vals])
        sql = "CREATE TABLE t%d (%s)" % (t, ", ".join(("c0 INTEGER PRIMARY KEY" if alias and i == 0 else c) for i, c in enumerate(cols)))
        tables.append({"name": f"t{t}", "sql": sql, "rows": rows, "alias": alias})
    spec = {"tables": [{k: v for k, v in t.items() if k != "alias"} for t in tables],
            "views": [["v0", "CREATE VIEW v0 AS SELECT * FROM t0"]]}
    path = tmp / f"w{idx}.sqlite"
    subprocess.run([writer, str(path)], input=json.dumps(spec), text=True, check=True)
    con = sqlite3.connect(path)
    ok = con.execute("pragma integrity_check").fetchall() == [("ok",)]
    for t in tables:
        got = con.execute(f"select rowid, * from {t['name']} order by rowid").fetchall()
        want = []
        for rid, vals in t["rows"]:
            vals = list(vals)
            if t["alias"]:
                vals[0] = rid
            want.append((rid, *vals))
        if len(got) != len(want):
            ok = False
        else:
            for g, w in zip(got, want):
                if g != w and not all(a == b or (isinstance(a, float) and isinstance(b, float) and a == b) for a, b in zip(g, w)):
                    ok = False
                    break
    ok = ok and con.execute("select count(*) from v0").fetchone()[0] == len(tables[0]["rows"])
    con.close()
    return ok, sum(len(t["rows"]) for t in tables), path.stat().st_size


def graph_with_exports(binary, folder, tmp, name, *flags):
    out = tmp / name
    subprocess.run([binary, "graph", str(folder), str(out), "--no-cache", "--export", "gexf,jsonld,mermaid,sqlite", *flags],
                   capture_output=True, check=True)
    return out, json.loads((out / "graph.json").read_text())


def check_sqlite(out, g):
    con = sqlite3.connect(out / "graph.sqlite")
    ok = con.execute("pragma integrity_check").fetchall() == [("ok",)]
    nodes = con.execute("select id, node_id, label, type, file_type, source_file, community, degree, attrs from nodes order by id").fetchall()
    ok &= len(nodes) == len(g["nodes"])
    index = {}
    for row, n in zip(nodes, g["nodes"]):
        index[n["id"]] = row[0]
        ok &= (row[1], row[2], row[3], row[4], row[5], row[6]) == (n["id"], n["label"], n["type"], n["file_type"], n.get("source_file"), n["community"])
        ok &= row[7] == n["degree"]
    links = con.execute("select id, source, target, relation, confidence, score, weight, directed, provenance, by_who, label from links order by id").fetchall()
    ok &= len(links) == len(g["links"])
    for row, l in zip(links, g["links"]):
        ok &= (row[1], row[2]) == (index[l["source"]], index[l["target"]])
        ok &= (row[3], row[4], row[5], row[6]) == (l["relation"], l["confidence"], l["confidence_score"], l["weight"])
        ok &= bool(row[7]) == bool(l.get("directed", False))
        ok &= row[8] == l.get("provenance", "extracted")
        ok &= row[9] == l.get("by") and row[10] == l.get("label")
        ev = [r[0] for r in con.execute("select text from evidence where link = ? order by seq", (row[0],))]
        ok &= ev == l["evidence"]
    comms = con.execute("select id, label, size from communities order by id").fetchall()
    ok &= [(c[1], c[2]) for c in comms] == [(c["label"], c["size"]) for c in g["graph"]["communities"]]
    ok &= con.execute("select count(*) from link_names").fetchone()[0] == len(g["links"])
    meta = dict(con.execute("select key, value from meta"))
    ok &= meta["nodes"] == str(len(g["nodes"])) and meta["links"] == str(len(g["links"]))
    return bool(ok)


def check_gexf(out, g):
    """Read with ElementTree (every graph) and, when no link is directed, with
    NetworkX too - which refuses a graph that mixes directed and undirected
    edges, as GEXF itself allows."""
    from xml.etree import ElementTree as ET
    ns = {"g": "http://www.gexf.net/1.2draft"}
    root = ET.parse(out / "graph.gexf").getroot()
    ok = root.get("version") == "1.2"
    titles = {a.get("id"): a.get("title") for c in root.findall(".//g:attributes", ns) if c.get("class") == "node" for a in c}
    nodes = root.findall(".//g:nodes/g:node", ns)
    ok &= len(nodes) == len(g["nodes"])
    by_xml = {}
    for x, n in zip(nodes, g["nodes"]):
        vals = {titles[v.get("for")]: v.get("value") for v in x.findall(".//g:attvalue", ns)}
        ok &= x.get("label") == n["label"] and vals["node_id"] == n["id"] and vals["type"] == n["type"]
        ok &= int(vals["community"]) == n["community"] and vals["file_type"] == n["file_type"]
        by_xml[x.get("id")] = n["id"]
    edges = root.findall(".//g:edges/g:edge", ns)
    ok &= len(edges) == len(g["links"])
    for x, l in zip(edges, g["links"]):
        ok &= (by_xml[x.get("source")], by_xml[x.get("target")]) == (l["source"], l["target"])
        ok &= x.get("type") == ("directed" if l.get("directed") else "undirected")
        ok &= x.get("label") == l["relation"] and abs(float(x.get("weight")) - l["weight"]) < 1e-9
        vals = {v.get("for"): v.get("value") for v in x.findall(".//g:attvalue", ns)}
        ok &= vals["3"] == "; ".join(l["evidence"])
    if not any(l.get("directed") for l in g["links"]):
        G = nx.read_gexf(out / "graph.gexf")
        ok &= G.number_of_nodes() == len(g["nodes"]) and G.number_of_edges() == len(g["links"])
        ok &= {d["node_id"] for _, d in G.nodes(data=True)} == {n["id"] for n in g["nodes"]}
    return bool(ok)


def check_jsonld(out, g):
    rg = rdflib.Graph()
    rg.parse(out / "graph.jsonld", format="json-ld")
    V = "urn:sniff-rs:vocab:"
    ok = True
    types = {s: o for s, p, o in rg.triples((None, rdflib.RDF.type, None))}
    node_iris = {}
    for n in g["nodes"]:
        iri = None
        for s in rg.subjects(rdflib.URIRef(V + "nodeId"), rdflib.Literal(n["id"])):
            iri = s
        ok &= iri is not None
        if iri is None:
            continue
        node_iris[n["id"]] = iri
        ok &= str(rg.value(iri, rdflib.URIRef("http://www.w3.org/2000/01/rdf-schema#label"))) == n["label"]
    ok &= sum(1 for s, o in types.items() if str(o) == V + "Link") == len(g["links"])
    links = {}
    for s in rg.subjects(rdflib.RDF.type, rdflib.URIRef(V + "Link")):
        src = rg.value(s, rdflib.URIRef(V + "linkSource"))
        tgt = rg.value(s, rdflib.URIRef(V + "linkTarget"))
        rel = str(rg.value(s, rdflib.URIRef(V + "relation")))
        ev = sorted(str(o) for o in rg.objects(s, rdflib.URIRef(V + "evidence")))
        links.setdefault((str(src), str(tgt), rel), []).append(ev)
        # The plain statement exists too.
        ok &= (src, rdflib.URIRef("urn:sniff-rs:rel:" + rel), tgt) in rg
    for l in g["links"]:
        key = (str(node_iris[l["source"]]), str(node_iris[l["target"]]), l["relation"])
        want = sorted(set(l["evidence"]))
        ok &= key in links and any(e == want for e in links[key])
    return bool(ok)


def check_mermaid(mm, out, g):
    text = (out / "graph.mmd").read_text()
    r = subprocess.run(["node", str(Path(mm) / "parse.mjs"), str(out / "graph.mmd")], capture_output=True, text=True, cwd=mm)
    ok = r.returncode == 0 and r.stdout.startswith("OK")
    if not ok:
        print("  mermaid:", r.stdout.strip()[:200])
    nodes = [l for l in text.splitlines() if l.startswith("  n") and '["' in l]
    edges = [l for l in text.splitlines() if "|\"" in l and l.startswith("  n")]
    if len(g["nodes"]) <= 120:
        ok &= len(nodes) == len(g["nodes"])
        ok &= len(edges) == sum(1 for l in g["links"] if l["source"] != l["target"])
    else:
        ok &= len(nodes) == 120
    return bool(ok)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--writer", default="target/release/examples/sqlite_write")
    ap.add_argument("--mermaid")
    ap.add_argument("--fuzz", type=int, default=40)
    args = ap.parse_args()
    bad = 0
    rng = random.Random(77)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        rows = size = 0
        for i in range(args.fuzz):
            ok, n, sz = writer_case(rng, args.writer, tmp, i)
            rows += n
            size = max(size, sz)
            if not ok:
                bad += 1
                print(f"WRITER DIFF case {i}")
        print(f"sqlite writer: {args.fuzz} databases, {rows} rows, largest {size // 1024} KiB, integrity_check and read-back {'ok' if not bad else 'FAILED'}")
        fixtures = Path("tests/fixtures")
        folders = [fixtures / n for n in ("edge_graph_lineage", "edge_graph_sources", "edge_graph_people", "edge_knowledge_graph", "edge_graph_code", "edge_graph_columns", "edge_graph_audio", "edge_graph_images", "edge_graph_geotime")]
        # Hostile names, and a bigger folder that needs interior b-tree pages.
        hostile = tmp / "hostile"
        hostile.mkdir()
        names = ['we"ird.txt', "br[ack]ets (and) {braces}.txt", "pipe|bar#hash.txt", "less<than>greater.txt", "semi;colon&amp.txt",
                 "back\\slash`tick`.txt", "%% comment.txt", "emoji 🙂 日本語.txt", "x" * 150 + ".txt", "classDef.txt", "end.txt", "graph TD.txt"]
        for n in names:
            (hostile / n).write_text("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima\n" * 3)
        big = tmp / "big"
        big.mkdir()
        for i in range(1500):
            (big / f"doc_{i:04d}.txt").write_text(f"contact user{i % 40}@example.org about project{i % 25} and ticket{i % 300}\n" * 2)
        folders += [hostile, big]
        for k, folder in enumerate(folders):
            if not folder.exists():
                continue
            out, g = graph_with_exports(args.bin, folder, tmp, f"o{k}", "--columns", "--people", "--folders")
            res = {"sqlite": check_sqlite(out, g), "gexf": check_gexf(out, g), "jsonld": check_jsonld(out, g)}
            if args.mermaid:
                res["mermaid"] = check_mermaid(args.mermaid, out, g)
            fails = [n for n, v in res.items() if not v]
            if fails:
                bad += 1
            print(f"{folder.name}: {len(g['nodes'])} nodes, {len(g['links'])} links: " + ("ok" if not fails else "FAILED " + ",".join(fails)))
    print("ok" if not bad else f"{bad} problem(s)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
