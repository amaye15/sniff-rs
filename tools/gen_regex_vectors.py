#!/usr/bin/env python3
"""Generates tests/fixtures/regex_vectors.jsonl: random patterns in the
subset sniff-rs's `regex_lite` reads, random texts (some built to match),
and the matches Python's `re` (ASCII mode, leftmost-first) finds. The Rust
test `regex_lite_tests::matches_python_re` requires every line to agree.

    python3 tools/gen_regex_vectors.py > tests/fixtures/regex_vectors.jsonl
"""
import json, random, re, sys

random.seed(20261010)
ALPHA = list("abcABC012_ -.@/:")
META = set(r"\.^$|?*+()[]{}")


def lit(c):
    return ("\\" + c if c in META or c in "-" else c)


class N:  # a node with rust/python spelling, nullable flag, and a sampler
    def __init__(self, rust, py, nullable, sample, zero=False):
        self.rust, self.py, self.nullable, self.sample, self.zero = rust, py, nullable, sample, zero


def atom(depth):
    r = random.random()
    if r < 0.35:
        c = random.choice(ALPHA)
        return N(lit(c), re.escape(c) if c != "_" else "_", False, lambda c=c: c)
    if r < 0.50:
        kind = random.choice([r"\d", r"\w", r"\s", r"\D", r"\W", r"\S"])
        pool = {r"\d": "0123456789", r"\w": "abcABC012_", r"\s": " \t\n", r"\D": "abc -.", r"\W": " -.@/:", r"\S": "abc012_-"}[kind]
        return N(kind, kind, False, lambda p=pool: random.choice(p))
    if r < 0.65:
        neg = random.random() < 0.3
        parts = []
        samples = []
        for _ in range(random.randint(1, 3)):
            t = random.random()
            if t < 0.4:
                a, b = sorted(random.sample("abcABC012", 2))
                parts.append(f"{a}-{b}")
                samples.append((a, b))
            elif t < 0.6:
                parts.append(random.choice([r"\d", r"\w", r"\s"]))
                samples.append(None)
            else:
                c = random.choice("abc012_.-@")
                parts.append(lit(c) if c in META or c == "-" else c)
                samples.append((c, c))
        body = "".join(parts)
        if neg:
            return N(f"[^{body}]", f"[^{body}]", False, lambda: random.choice("xyz!#~"))
        def samp(samples=samples):
            s = random.choice(samples)
            if s is None:
                return random.choice("a0 _")
            return chr(random.randint(ord(s[0]), ord(s[1])))
        return N(f"[{body}]", f"[{body}]", False, samp)
    if r < 0.72:
        return N(".", ".", False, lambda: random.choice("abc-@"))
    if r < 0.82 and depth < 3:
        branches = [seq(depth + 1, allow_empty=False) for _ in range(random.randint(1, 3))]
        group = random.choice(["(", "(?:"])
        rust = group + "|".join(b.rust for b in branches) + ")"
        py = "(?:" + "|".join(b.py for b in branches) + ")"
        nullable = any(b.nullable for b in branches)
        return N(rust, py, nullable, lambda bs=branches: random.choice(bs).sample())
    if r < 0.90:
        a = random.choice([r"\b", r"\B"])
        return N(a, a, True, lambda: "", zero=True)
    return N("^", "^", True, lambda: "", zero=True) if random.random() < 0.5 else N("$", r"\Z", True, lambda: "", zero=True)


def quantified(a):
    if a.zero or a.nullable or random.random() < 0.45:
        return a
    q = random.choice(["*", "+", "?", "{2}", "{1,3}", "{2,}", "{0,2}"])
    lazy = random.random() < 0.3
    mn, mx = {"*": (0, 4), "+": (1, 4), "?": (0, 1), "{2}": (2, 2), "{1,3}": (1, 3), "{2,}": (2, 4), "{0,2}": (0, 2)}[q]
    nullable = mn == 0
    def samp(a=a, mn=mn, mx=mx):
        return "".join(a.sample() for _ in range(random.randint(mn, mx)))
    return N(a.rust + q + ("?" if lazy else ""), a.py + q + ("?" if lazy else ""), nullable, samp)


def seq(depth, allow_empty=True):
    items = [quantified(atom(depth)) for _ in range(random.randint(1, 4))]
    # A quantified `{n}` can't directly follow a `{`-looking literal; fine.
    nullable = all(i.nullable for i in items)
    return N("".join(i.rust for i in items), "".join(i.py for i in items), nullable, lambda items=items: "".join(i.sample() for i in items))


def text_for(node):
    pieces = []
    for _ in range(random.randint(1, 4)):
        if random.random() < 0.6:
            pieces.append(node.sample())
        else:
            pieces.append("".join(random.choice(ALPHA + ["\n"]) for _ in range(random.randint(0, 8))))
        pieces.append("".join(random.choice(ALPHA + ["\n"]) for _ in range(random.randint(0, 4))))
    return "".join(pieces)


count = 0
while count < 3000:
    node = seq(0)
    if node.nullable:
        continue
    fold = random.random() < 0.15
    rust = ("(?i)" if fold else "") + node.rust
    flags = re.ASCII | (re.IGNORECASE if fold else 0)
    try:
        rx = re.compile(node.py, flags)
    except re.error:
        continue
    for _ in range(2):
        text = text_for(node)
        if fold:
            text = "".join(c.upper() if random.random() < 0.4 else c for c in text)
        spans = [[m.start(), m.end()] for m in rx.finditer(text) if m.end() > m.start()]
        print(json.dumps([rust, text, spans], ensure_ascii=False))
        count += 1
