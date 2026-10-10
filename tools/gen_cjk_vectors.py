#!/usr/bin/env python3
"""Writes tests/fixtures/cjk_bigram_vectors.jsonl: texts and the terms sniff-rs must find in them.

    python3 tools/gen_cjk_vectors.py [--out tests/fixtures/cjk_bigram_vectors.jsonl] [--n 600]

A run of characters in a script written without spaces (Han, kana, Hangul,
Thai, Lao, Myanmar, Khmer) is cut into overlapping pairs; punctuation, spaces,
digits and other scripts end a run. The script of each character comes from
its Unicode *name* (`unicodedata.name`), not from the ranges the Rust code
uses, so a wrong range shows up as a difference. Latin text in the vectors is
one or two letters, which is never a term, so only the pairs are expected.
"""
import argparse
import json
import random
import unicodedata

PREFIXES = ("CJK UNIFIED", "CJK COMPATIBILITY IDEOGRAPH", "HIRAGANA", "KATAKANA", "HANGUL", "THAI", "LAO ", "MYANMAR", "KHMER")


def spaceless(c):
    try:
        name = unicodedata.name(c)
    except ValueError:
        return False
    if not c.isalpha():
        # Marks and punctuation of those scripts: the Rust side tests
        # `is_alphabetic()` first, and so does this.
        return False
    if name.startswith("HALFWIDTH KATAKANA"):
        return True
    return name.startswith(PREFIXES)


def identifier_chunk(chunk):
    """A whitespace-separated chunk that is an identifier, not prose, is never read for terms."""
    low = chunk.lower()
    return ("@" in chunk or "://" in chunk or low.startswith("www.") or low.startswith("doi:")
            or (any("0" <= c <= "9" for c in chunk) and any(c.isascii() and c.isalpha() for c in chunk)))


def expected(text):
    out = {}
    for chunk in text.split():
        if identifier_chunk(chunk):
            continue
        prev = None
        for c in chunk:
            if spaceless(c):
                if prev is not None:
                    out[prev + c] = out.get(prev + c, 0) + 1
                prev = c
            else:
                prev = None
    return out


POOLS = {
    "han": [chr(c) for c in range(0x4E00, 0x4E00 + 400)] + [chr(c) for c in range(0x3400, 0x3440)],
    "hira": [chr(c) for c in range(0x3041, 0x3094)],
    "kata": [chr(c) for c in range(0x30A1, 0x30FB)],
    "hangul": [chr(c) for c in range(0xAC00, 0xAC00 + 300)],
    "thai": [chr(c) for c in range(0x0E01, 0x0E2F)],
    "lao": [chr(c) for c in range(0x0E81, 0x0EA0) if chr(c).isalpha()],
    "khmer": [chr(c) for c in range(0x1780, 0x17A3)],
    "myanmar": [chr(c) for c in range(0x1000, 0x1021)],
    "ext_b": [chr(c) for c in range(0x20000, 0x20040)],
    "compat": [chr(c) for c in range(0xF900, 0xF930)],
    "far": [chr(c) for c in list(range(0x2A6D0, 0x2A6DE)) + list(range(0x2B740, 0x2B74A)) + list(range(0x30000, 0x30010)) + list(range(0xD7A0, 0xD7A4)) + list(range(0xD7B0, 0xD7B8)) + list(range(0xFAD0, 0xFAD9)) + list(range(0x4DA0, 0x4DB5))],
    "halfwidth": [chr(c) for c in range(0xFF66, 0xFF9E)],
}
SEPARATORS = ["，", "。", " ", "、", "\n", "1", "2024", "!", "-", "·", "AI", "ab", "（", "）", "\t", "ー"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/fixtures/cjk_bigram_vectors.jsonl")
    ap.add_argument("--n", type=int, default=600)
    args = ap.parse_args()
    rng = random.Random(20261010)
    keys = sorted(POOLS)
    with open(args.out, "w") as f:
        for _ in range(args.n):
            parts = []
            for _ in range(rng.randint(1, 8)):
                pool = POOLS[rng.choice(keys)]
                if rng.random() < 0.2:
                    pool = pool + POOLS[rng.choice(keys)]
                parts.append("".join(rng.choice(pool) for _ in range(rng.randint(1, 9))))
                if rng.random() < 0.8:
                    parts.append(rng.choice(SEPARATORS))
            text = "".join(parts)
            f.write(json.dumps({"text": text, "terms": expected(text)}, ensure_ascii=False, sort_keys=True) + "\n")
    print("wrote", args.n, "vectors to", args.out)


if __name__ == "__main__":
    main()
