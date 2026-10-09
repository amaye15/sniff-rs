#!/usr/bin/env python3
"""Writes the imports and file reads/writes of JavaScript, Rust, Java and Go
source files, as tree-sitter parses them, one JSON line per file, for the
code_facts tests:

    python3 tools/gen_code_ts_vectors.py DIR... > vectors.jsonl
    SNIFF_CODE_TS_VECTORS=vectors.jsonl cargo +nightly test tree_sitter -- --ignored

Each line: {"path", "lang", "imports": [[module, dynamic]...],
"accesses": [[path, write]...]}. The call tables are those of
`code_facts::classify`. Files with a parse error, or a path literal holding
a backslash, are skipped.
"""
import json, os, re, sys
from tree_sitter import Language, Parser
import tree_sitter_javascript as tsjs, tree_sitter_rust as tsrs, tree_sitter_java as tsjava, tree_sitter_go as tsgo

LANGS = {
    "javascript": (Language(tsjs.language()), {".js", ".mjs", ".cjs", ".jsx"}),
    "rust": (Language(tsrs.language()), {".rs"}),
    "java": (Language(tsjava.language()), {".java"}),
    "go": (Language(tsgo.language()), {".go"}),
}
EXT = {"csv","tsv","psv","json","jsonl","ndjson","parquet","pqt","xlsx","xls","xlsb","ods","txt","dat","pkl","pickle","npy","npz","h5","hdf5","hdf","feather","arrow","db","sqlite","sqlite3","png","jpg","jpeg","gif","svg","pdf","html","htm","xml","yaml","yml","toml","ini","md","log","sav","dta","rds","rdata","rda","orc","avro","zip","gz","bz2","xz","sql","py","ipynb","js","ts","r","geojson","shp","gpkg","mat","nc","fits","bin","pt","pth","ckpt","onnx","parq","tif","tiff","bmp","webp","mp3","mp4","wav","docx","pptx","rtf","tex","bib","fasta","fa","fastq","vcf","bed","gff","sam","bam","xpt","sas7bdat","zst","tar","mbox","eml","ics","har","plist","msgpack","cbor","bson","env","cfg","conf"}

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
    if len(ext) == 1 and ext.lower() not in EXT:
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

def kind_of(lang, name, chain):
    n = name.lower(); c = chain.lower()
    if lang == "java":
        c = ".".join(c.split(".")[-2:])
    if lang == "javascript":
        if n in ("readfile","readfilesync","createreadstream","readjson","readjsonsync","readfilepromise","loadjson","readcsv"): return "read"
        if n in ("writefile","writefilesync","appendfile","appendfilesync","createwritestream","outputfile","outputfilesync","writejson","writejsonsync"): return "write"
        if n in ("csv","tsv","json","text","xml","html","image") and c.startswith("d3."): return "read"
        if n == "fetch": return "read"
        if n in ("require","import"): return "import"
        if n in ("join","resolve") and c.startswith("path"): return "neutral"
    elif lang == "rust":
        if n in ("open","read_to_string","read","include_str","include_bytes","read_csv","from_path") and not (n == "from_path" and "writer" in c): return "read"
        if n == "from_path": return "write"
        if n in ("create","write","write_csv","to_path"): return "write"
        if (n == "new" and (c.startswith("path::") or c.startswith("pathbuf::"))) or (n == "from" and "pathbuf" in c) or n == "join": return "neutral"
    elif lang == "go":
        if n in ("open","readfile","openfile"): return "read"
        if n in ("create","writefile"): return "write"
        if n == "join" and (c.startswith("filepath.") or c.startswith("path.")): return "neutral"
    elif lang == "java":
        if n in ("filereader","fileinputstream","readalllines","readallbytes","readstring","lines","newbufferedreader","newinputstream"): return "read"
        if n in ("filewriter","fileoutputstream","printwriter","writestring","newbufferedwriter","newoutputstream"): return "write"
        if n == "write" and c.startswith("files"): return "write"
        if n == "file": return "neutral"
        if n == "get" and c.startswith("paths"): return "neutral"
        if n == "of" and c.startswith("path"): return "neutral"
    return "none"

class Skip(Exception):
    pass

STRINGS = {"string", "template_string", "string_literal", "raw_string_literal", "interpreted_string_literal", "text_block", "string_fragment"}

def literal(node):
    t = node.text.decode("utf-8", "replace")
    if node.type == "raw_string_literal" and t.startswith("r"):
        t = t.lstrip("r").strip("#")
    t = t[1:-1] if len(t) >= 2 else t
    if "\\" in t:
        raise Skip()
    return t

def collect(lang, node, out):
    if node.type in STRINGS:
        out.append(literal(node))
    elif node.type in ("binary_expression",):
        op = node.child_by_field_name("operator")
        if op is not None and op.text == b"+":
            collect(lang, node.child_by_field_name("left"), out); collect(lang, node.child_by_field_name("right"), out)
    elif node.type == "parenthesized_expression":
        for ch in node.named_children: collect(lang, ch, out)
    elif node.type == "ternary_expression":
        for f in ("consequence", "alternative"):
            c = node.child_by_field_name(f)
            if c is not None: collect(lang, c, out)
    elif node.type == "await_expression":
        for ch in node.named_children: collect(lang, ch, out)

def ident_chain(node):
    """Dotted name of identifier / member chains, stopping at anything else."""
    t = node.type
    if t in ("identifier", "property_identifier", "field_identifier", "type_identifier", "this", "scoped_type_identifier"):
        return node.text.decode()
    if t in ("member_expression", "field_expression", "selector_expression"):
        obj = node.child_by_field_name("object") or node.child_by_field_name("value") or node.child_by_field_name("operand")
        prop = node.child_by_field_name("property") or node.child_by_field_name("field")
        base = ident_chain(obj) if obj is not None else ""
        p = prop.text.decode() if prop is not None else ""
        return (base + "." + p) if base else p
    if t in ("scoped_identifier", "scoped_type_identifier"):
        return node.text.decode().replace(" ", "")
    return ""

def analyze(lang, root):
    acc = set(); imports = set()
    def call_parts(node):
        """(name, chain, argument nodes, extra strings) for a call-like node, or None."""
        t = node.type
        if lang == "javascript":
            if t == "call_expression":
                f = node.child_by_field_name("function"); a = node.child_by_field_name("arguments")
                if f is None or a is None: return None
                if f.type == "import": name, chain = "import", "import"
                else:
                    chain = ident_chain(f)
                    name = chain.split(".")[-1]
                    if f.type in ("member_expression",):
                        prop = f.child_by_field_name("property"); name = prop.text.decode() if prop else name
                extra = []
                if f.type == "member_expression":
                    o = f.child_by_field_name("object")
                    if o is not None and o.type in STRINGS | {"template_string"}: collect(lang, o, extra)
                return name, chain, list(a.named_children), extra
            if t == "new_expression":
                c = node.child_by_field_name("constructor"); a = node.child_by_field_name("arguments")
                if c is None or a is None: return None
                chain = ident_chain(c); return chain.split(".")[-1], chain, list(a.named_children), []
        elif lang == "java":
            if t == "method_invocation":
                n = node.child_by_field_name("name"); o = node.child_by_field_name("object"); a = node.child_by_field_name("arguments")
                if n is None or a is None: return None
                chain = (ident_chain(o) + "." if o is not None and ident_chain(o) else "") + n.text.decode()
                extra = []
                if o is not None and o.type in STRINGS: collect(lang, o, extra)
                return n.text.decode(), chain, list(a.named_children), extra
            if t == "object_creation_expression":
                ty = node.child_by_field_name("type"); a = node.child_by_field_name("arguments")
                if ty is None or a is None: return None
                chain = ty.text.decode().replace(" ", ""); return chain.split(".")[-1], chain, list(a.named_children), []
        elif lang == "rust":
            if t == "call_expression":
                f = node.child_by_field_name("function"); a = node.child_by_field_name("arguments")
                if f is None or a is None: return None
                if f.type == "generic_function": f = f.child_by_field_name("function") or f
                chain = ident_chain(f)
                name = re.split(r"::|\.", chain)[-1] if chain else ""
                if f.type == "scoped_identifier":
                    chain = "::".join(f.text.decode().replace(" ", "").split("::"))
                return name, chain, list(a.named_children), []
            if t == "macro_invocation":
                m = node.child_by_field_name("macro")
                tt = [c for c in node.children if c.type == "token_tree"]
                if m is None or not tt: return None
                name = m.text.decode().split("::")[-1]
                return name, name, [c for c in tt[0].children if c.type in STRINGS], []
        elif lang == "go":
            if t == "call_expression":
                f = node.child_by_field_name("function"); a = node.child_by_field_name("arguments")
                if f is None or a is None: return None
                chain = ident_chain(f); name = chain.split(".")[-1] if chain else ""
                return name, chain, list(a.named_children), []
        return None
    def pend(node):
        if (p := call_parts(node)) is not None:
            return call(node, *p)
        if lang == "rust" and node.type == "token_tree":
            # A macro called inside another macro or an attribute: `#[doc = include_str!("x.md")]`.
            kids = node.children
            for k in range(len(kids) - 2):
                if kids[k].type == "identifier" and kids[k + 1].type == "!" and kids[k + 2].type == "token_tree":
                    name = kids[k].text.decode()
                    kind = kind_of(lang, name, name)
                    if kind in ("read", "write"):
                        strings = [literal(c) for c in kids[k + 2].children if c.type in STRINGS]
                        first = next((t for t in strings if path_like(t)), None)
                        if first is not None:
                            acc.add((first, kind == "write"))
        res = []
        for ch in node.named_children:
            res += pend(ch)
        return res
    def call(node, name, chain, args, extra):
        kind = kind_of(lang, name, chain) if name else "none"
        pending = []
        for ch in node.named_children:
            pending += pend(ch)
        strings = list(extra)
        for a in args:
            collect(lang, a, strings)
        first = next((t for t in strings if path_like(t)), None)
        if kind == "import":
            if strings and "${" not in strings[0]:
                imports.add((strings[0], True))
            return []
        if kind in ("read", "write"):
            for p in pending: acc.add((p, kind == "write"))
            if first is not None: acc.add((first, kind == "write"))
            return []
        if kind == "neutral":
            joined = join_parts(strings)
            if len(strings) >= 2 and joined is not None and path_like(joined):
                return pending + [joined]
            return pending + ([first] if first is not None else [])
        return pending
    pend(root)
    # statement imports
    def walk(node):
        t = node.type
        if lang == "javascript":
            if t == "import_statement":
                s = node.child_by_field_name("source")
                if s is not None: imports.add((literal(s), False))
            elif t == "export_statement":
                s = node.child_by_field_name("source")
                if s is not None: imports.add((literal(s), False))
        elif lang == "rust" and t == "mod_item":
            if node.child_by_field_name("body") is None:
                imports.add((node.child_by_field_name("name").text.decode().removeprefix("r#"), False))
        elif lang == "rust" and t == "token_tree":
            # `mod x;` written inside a macro (cfg_if!) is a module declaration too.
            kids = node.children
            for k in range(len(kids) - 2):
                if kids[k].type == "mod" and kids[k + 1].type == "identifier" and kids[k + 2].type == ";":
                    imports.add((kids[k + 1].text.decode().removeprefix("r#"), False))
        elif lang == "java" and t == "import_declaration":
            kids = [c for c in node.named_children if c.type in ("scoped_identifier", "identifier")]
            wildcard = any(c.type == "asterisk" for c in node.named_children)
            if kids and not wildcard:
                imports.add((kids[0].text.decode().replace(" ", ""), False))
        for ch in node.children:
            walk(ch)
    walk(root)
    return acc, imports

def main():
    parsers = {k: Parser(v[0]) for k, v in LANGS.items()}
    for root in sys.argv[1:]:
        for dirpath, dirs, files in os.walk(root):
            dirs[:] = [d for d in dirs if d != "target"]
            for f in sorted(files):
                ext = os.path.splitext(f)[1]
                lang = next((k for k, v in LANGS.items() if ext in v[1]), None)
                if lang is None: continue
                p = os.path.join(dirpath, f)
                try:
                    data = open(p, "rb").read()
                    if len(data) > 1_500_000: continue
                    data.decode("utf-8")
                    tree = parsers[lang].parse(data)
                    if tree.root_node.has_error: continue
                    acc, imps = analyze(lang, tree.root_node)
                except Skip:
                    continue
                except Exception:
                    continue
                print(json.dumps({"path": p, "lang": lang, "imports": sorted([list(i) for i in imps]), "accesses": sorted([list(a) for a in acc])}))

main()
