#!/usr/bin/env python3
"""Writes the file reads and writes Python code makes, as `ast` sees them,
one JSON line per file, for the code_facts call-engine test:

    python3 tools/gen_code_access_vectors.py DIR... > vectors.jsonl
    SNIFF_CODE_ACCESS_VECTORS=vectors.jsonl cargo +nightly test python_accesses_match_ast -- --ignored

Each line: {"path": ..., "accesses": [[path, write], ...]}. The call tables
below are the Python tables of `code_facts::classify`; a name in one and not
the other shows up as a difference.
"""
import ast, json, os, re, sys, warnings

warnings.simplefilter("ignore")

READ = set("""read_csv read_table read_excel read_json read_parquet read_feather read_pickle read_fwf read_hdf
read_stata read_sas read_spss read_orc read_xml read_html loadtxt genfromtxt load load_workbook read_file read_text
read_bytes imread from_csv open_workbook fromfile read_sql_table read_gbq read_netcdf open_dataset read_raster""".split())
WRITE = set("""to_csv to_excel to_json to_parquet to_feather to_pickle to_hdf to_stata to_html to_latex to_markdown to_xml
to_file to_orc savefig save savez savez_compressed savetxt dump imwrite write_text write_bytes write_table write_csv
tofile to_netcdf to_zarr""".split())
NEUTRAL = {"path", "purepath", "posixpath", "windowspath"}
NON_PATH_KW = set("""encoding mode errors newline sep delimiter compression engine sheet_name orient lineterminator
quotechar na_rep float_format date_format decimal key dtype format index_label header comment na_values quote
fileencoding enc colnames dec name label title xlabel ylabel usecols parse_dates storage_options protocol mime""".split())

EXT1 = {"r", "c", "h", "m"}

def known_ext1(e):
    return e.lower() in EXT1 or False

def path_like(s):
    t = s.strip()
    if not t or len(t) > 300 or "://" in t:
        return False
    if any(ord(c) < 32 or c in '<>|"' for c in t):
        return False
    if any(i != 1 for i, c in enumerate(t) if c == ":"):
        return False
    last = re.split(r"[/\\]", t)[-1]
    if "." not in last:
        return False
    stem, ext = last.rsplit(".", 1)
    if not ext or len(ext) > 10 or not ext.isalnum() or not any(c.isalpha() for c in ext):
        return False
    if len(ext) == 1 and ext.lower() not in ("r",):
        return False
    return bool(stem) or len(t) > len(last)

def join_parts(parts):
    out = ""
    for p in parts:
        if not p:
            continue
        if not out or p.startswith("/"):
            out = p
        else:
            if not out.endswith("/"):
                out += "/"
            out += p
    return out or None

def kind_of(name, chain):
    n = name.lower(); c = chain.lower()
    if n == "open": return "open"
    if n in READ: return "read"
    if n in WRITE: return "write"
    if n in NEUTRAL: return "neutral"
    if n == "join" and "path" in c: return "neutral"
    return "none"

def chain_of(func):
    parts = []
    while isinstance(func, ast.Attribute):
        parts.append(func.attr); func = func.value
    if isinstance(func, ast.Name):
        parts.append(func.id)
    return ".".join(reversed(parts)), (parts[0] if parts else "")

def fstring_text(j):
    out = ""
    for v in j.values:
        out += v.value if isinstance(v, ast.Constant) else "{}"
    return out

def collect(node, out):
    """String constants that belong to the enclosing call's own frame."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        out.append(node.value)
    elif isinstance(node, ast.JoinedStr):
        out.append(fstring_text(node))
    elif isinstance(node, (ast.BinOp,)):
        collect(node.left, out); collect(node.right, out)
    elif isinstance(node, ast.BoolOp):
        for v in node.values: collect(v, out)
    elif isinstance(node, ast.IfExp):
        collect(node.body, out); collect(node.orelse, out)
    elif isinstance(node, (ast.UnaryOp,)):
        collect(node.operand, out)
    elif isinstance(node, ast.Starred):
        collect(node.value, out)
    elif isinstance(node, ast.Call):
        collect_func(node.func, out)
    elif isinstance(node, ast.Attribute):
        collect(node.value, out)

def collect_func(func, out):
    if isinstance(func, ast.Attribute):
        collect(func.value, out)

def analyze(tree):
    acc = set()
    def pend(node):
        """Paths this expression hands to the call around it."""
        if isinstance(node, ast.Call):
            return call(node)
        res = []
        for ch in ast.iter_child_nodes(node):
            res += pend(ch)
        return res
    def call(c):
        chain, _ = chain_of(c.func)
        name = c.func.attr if isinstance(c.func, ast.Attribute) else (c.func.id if isinstance(c.func, ast.Name) else "")
        kind = kind_of(name, chain) if name else "none"
        pending = []
        for ch in ast.iter_child_nodes(c):
            pending += pend(ch)
        strings = []   # (index, keyword, text)
        for i, a in enumerate(c.args):
            t = []; collect(a, t)
            strings += [(i, None, x) for x in t]
        for kw in c.keywords:
            t = []; collect(kw.value, t)
            strings += [(-1, kw.arg, x) for x in t]
        # strings inside the callee expression (`"a".format(x)`) belong to the outer frame in a token view.
        first = None
        for (i, k, t) in strings:
            if path_like(t) and (k is None or k.lower() not in NON_PATH_KW):
                first = t; break
        if kind in ("read", "write", "open"):
            write = kind == "write"
            if kind == "open":
                mode = None
                # `Path(...).open(mode)` takes the mode first; `open(file, mode)` second.
                on_path = (isinstance(c.func, ast.Attribute) and isinstance(c.func.value, ast.Call)
                           and kind_of(*(lambda f: (f.attr if isinstance(f, ast.Attribute) else getattr(f, "id", ""), chain_of(f)[0]))(c.func.value.func)) == "neutral")
                pos = 0 if on_path else 1
                for (i, k, t) in strings:
                    if k == "mode": mode = t; break
                if mode is None:
                    for (i, k, t) in strings:
                        if i == pos and k is None: mode = t; break
                write = bool(mode) and any(ch in mode for ch in "waxX+")
            for p in pending: acc.add((p, write))
            if first is not None: acc.add((first, write))
            return []
        if kind == "neutral":
            cand = [t for (i, k, t) in strings if k is None or k.lower() not in NON_PATH_KW]
            joined = join_parts(cand)
            if len(cand) >= 2 and joined is not None and path_like(joined):
                return pending + [joined]
            return pending + ([first] if first is not None else [])
        return pending
    pend(tree)
    return acc


import random

R = random.Random()
PATHS = ["sales.csv", "data/orders.csv", "../raw/events.json", "out/report.xlsx", "model.pkl", "my file.csv", "C:\\data\\x.csv", "cache.parquet", "notes.txt", "t\u00e9st.html"]
MODES = ["r", "rb", "w", "wb", "a", "x", "r+", "rt", "w+", "ab"]
ENC = ["utf-8", "ascii", "latin-1", "utf8"]

def p():
    return R.choice(PATHS)

def q(s):
    return R.choice(['"', "'"]) + s.replace("\\", "\\\\") + R.choice(['"']) if False else repr(s)

def stmt():
    k = R.randrange(34)
    path = p()
    if k == 0: return f"df = pd.read_csv({q(path)})"
    if k == 1: return f"df.to_csv({q(path)}, index=False)"
    if k == 2: return f"with open({q(path)}, {q(R.choice(MODES))}) as f:\n    data = f.read()"
    if k == 3: return f"f = open({q(path)}, encoding={q(R.choice(ENC))})"
    if k == 4: return f"f = open(file={q(path)}, mode={q(R.choice(MODES))})"
    if k == 5: return f"df = pd.read_csv(os.path.join(DIR, {q(path)}))"
    if k == 6: return f"text = Path({q(path)}).read_text()"
    if k == 7: return f"Path({q(path)}).write_text(text)"
    if k == 8: return f"df = pd.read_excel(Path('data') / {q(path)}, sheet_name={q('Sheet1.x')})"
    if k == 9: return f"json.dump({{{q('file')}: {q(path)}}}, open({q(path)}, 'w'))"
    if k == 10: return f"np.save({q(path)}, arr)"
    if k == 11: return f"arr = np.load({q(path)})"
    if k == 12: return f"plt.savefig(f'{{OUT}}/{path}')"
    if k == 13: return f"# df = pd.read_csv({q(path)})"
    if k == 14: return f'"""doc: pd.read_csv({q(path)})"""'
    if k == 15: return f"df = pd.read_csv(\n    {q('data/')}\n    {q(path)},\n    sep={q(';')},\n)"
    if k == 16: return f"x = [pd.read_csv(f) for f in [{q(path)}, {q(p())}]]"
    if k == 17: return f"df.to_json(path_or_buf={q(path)})"
    if k == 18: return f"Path({q(path)})"
    if k == 19: return f"out = Path({q(path)}).open({q(R.choice(MODES))})"
    if k == 20: return f"data = json.load(open({q(path)}))"
    if k == 21: return f"pickle.dump(obj, open({q(path)}, 'wb'))"
    if k == 22: return f"df = pd.read_csv(({q('prefix_')} + name + {q('.csv')}))"
    if k == 23: return f"df = pd.read_csv({q(path)}.format(i))"
    if k == 24: return f"with open({q(path)}) as a, open({q(p())}, 'w') as b:\n    b.write(a.read())"
    if k == 25: return f"x = d[{q(path)}]"
    if k == 26: return f"df = pd.read_csv({q(path)} if flag else {q(p())})"
    if k == 27: return f"print({q(path)})"
    if k == 28: return f"df = pd.read_csv({q('http://example.com/a.csv')})"
    if k == 29: return f"os.path.join(DIR, {q(path)})"
    if k == 30: return f"df.to_parquet(fname={q(path)}, compression={q('snappy')}, engine={q('pyarrow')})"
    if k == 31: return f"wb = openpyxl.load_workbook({q(path)})"
    if k == 32: return f"f = open({q(path)}, {q('w')}, encoding={q('ascii')})"
    return f"lines = open({q(path)}).readlines()"

def synth(n):
    seen = set()
    for _ in range(n):
        body = "\n".join(stmt() for _ in range(R.randint(1, 5)))
        if body in seen:
            continue
        seen.add(body)
        try:
            tree = ast.parse(body)
        except Exception:
            continue
        a = analyze(tree)
        print(json.dumps({"src": body, "accesses": sorted([list(x) for x in a])}))

if len(sys.argv) > 1 and sys.argv[1] == "--synthetic":
    R.seed(int(sys.argv[3]) if len(sys.argv) > 3 else 5)
    synth(int(sys.argv[2]))
    sys.exit(0)

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
            a = analyze(tree)
            if True:
                print(json.dumps({"path": p, "accesses": sorted([list(x) for x in a])}))
