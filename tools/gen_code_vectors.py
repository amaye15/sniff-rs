#!/usr/bin/env python3
"""Writes the imports of Python files, as `ast` reads them, one JSON line per
file, for the code_facts differential test:

    python3 tools/gen_code_vectors.py DIR... > vectors.jsonl
    SNIFF_CODE_VECTORS=vectors.jsonl cargo +nightly test python_imports_match_ast -- --ignored

Each line: {"path": ..., "imports": [[module, [names], level], ...]}.
Files `ast` cannot parse are skipped.
"""
import ast, json, os, sys, warnings

warnings.simplefilter("ignore")

def imports(tree):
    out = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                out.append([a.name, [], 0])
        elif isinstance(node, ast.ImportFrom):
            out.append([node.module or "", [a.name for a in node.names], node.level])
    return out

for root in sys.argv[1:]:
    for dirpath, _, files in os.walk(root):
        for f in sorted(files):
            if not f.endswith(".py"):
                continue
            p = os.path.join(dirpath, f)
            try:
                src = open(p, encoding="utf-8").read()
                tree = ast.parse(src)
            except Exception:
                continue
            print(json.dumps({"path": p, "imports": imports(tree)}))
