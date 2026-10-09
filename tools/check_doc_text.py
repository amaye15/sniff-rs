#!/usr/bin/env python3
"""Checks the text the knowledge graph reads from RTF, ODT, DOCX and EPUB documents.

    python3 tools/check_doc_text.py [--docs 40] [--soffice /path/to/soffice]

Random flat-ODF documents (headings, paragraphs with accents, CJK, Hebrew,
emoji, tabs, runs of spaces, lists, tables, hyperlinks) are written, then
LibreOffice converts each to ODT, RTF, DOCX and EPUB, and to plain text
(that text is the truth). `cargo run --example doc_text` reads each artifact;
its words must be exactly the truth's words (plus hyperlink targets, which
the graph keeps as lines of their own).

Needs LibreOffice (soffice) and `cargo build --release --example doc_text`.
"""
import argparse
import collections
import random
import re
import subprocess
import sys
import tempfile
from pathlib import Path

POOL = ("alpha beta gamma budget invoice café naïve Müller Söhne données 会議 日本語 שלום עולם "
        "مرحبا 😀 🚀 résumé Ångström ñandú Zoë O'Brien “quoted” ‘single’ — – … € £ ¥ © ® ™ "
        "x² H₂O 50% 3.14 INV-2024-00123 jane@acme-corp.com").split()
NS = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
      'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
      'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
      'xmlns:xlink="http://www.w3.org/1999/xlink"')


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def words(rng, n):
    return " ".join(rng.choice(POOL) for _ in range(n))


def inline(rng):
    out = esc(words(rng, rng.randint(2, 8)))
    if rng.random() < 0.3:
        out += "<text:tab/>" + esc(words(rng, 2))
    if rng.random() < 0.2:
        out += "<text:s text:c=\"%d\"/>" % rng.randint(2, 6) + esc(words(rng, 1))
    if rng.random() < 0.2:
        out += "<text:line-break/>" + esc(words(rng, 2))
    if rng.random() < 0.25:
        out += ' <text:a xlink:href="https://example.org/%d">%s</text:a>' % (rng.randint(1, 999), esc(words(rng, 2)))
    return out


def fodt(rng):
    body = []
    for _ in range(rng.randint(3, 10)):
        kind = rng.random()
        if kind < 0.15:
            body.append('<text:h text:outline-level="%d">%s</text:h>' % (rng.randint(1, 3), esc(words(rng, 3))))
        elif kind < 0.3:
            items = "".join("<text:list-item><text:p>%s</text:p></text:list-item>" % inline(rng) for _ in range(rng.randint(1, 4)))
            body.append("<text:list>%s</text:list>" % items)
        elif kind < 0.45:
            cols = rng.randint(2, 3)
            rows = "".join(
                "<table:table-row>" + "".join("<table:table-cell><text:p>%s</text:p></table:table-cell>" % esc(words(rng, 2)) for _ in range(cols)) + "</table:table-row>"
                for _ in range(rng.randint(1, 3)))
            body.append('<table:table table:name="T%d"><table:table-column table:number-columns-repeated="%d"/>%s</table:table>' % (rng.randint(1, 9999), cols, rows))
        else:
            body.append("<text:p>%s</text:p>" % inline(rng))
    return ('<?xml version="1.0" encoding="UTF-8"?><office:document %s office:version="1.3" '
            'office:mimetype="application/vnd.oasis.opendocument.text"><office:body><office:text>%s'
            '</office:text></office:body></office:document>') % (NS, "".join(body))


def tokens(text):
    return collections.Counter(re.findall(r"\w+", text.replace(" ", " "), flags=re.UNICODE))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--docs", type=int, default=40)
    ap.add_argument("--soffice", default="/Applications/LibreOffice.app/Contents/MacOS/soffice")
    ap.add_argument("--bin", default="target/release/examples/doc_text")
    args = ap.parse_args()
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for n in range(args.docs):
            (tmp / f"d{n}.fodt").write_text(fodt(random.Random(n)), encoding="utf-8")
        for fmt in ("odt", "rtf", "docx", "epub", "txt:Text"):
            subprocess.run([args.soffice, "--headless", "--convert-to", fmt, "--outdir", str(tmp), *map(str, sorted(tmp.glob("d*.fodt")))],
                           capture_output=True, check=True)
        for ext in ("odt", "rtf", "docx", "epub"):
            files = [tmp / f"d{n}.{ext}" for n in range(args.docs)]
            out = subprocess.run([args.bin, *map(str, files)], capture_output=True, text=True, check=True).stdout
            chunks = out.split("\x01END\n")[:-1]
            assert len(chunks) == len(files), (ext, len(chunks))
            for n, chunk in enumerate(chunks):
                truth = (tmp / f"d{n}.txt").read_text(encoding="utf-8-sig")
                lines = [l for l in chunk.splitlines() if not l.startswith("https://example.org/")]
                got, want = tokens("\n".join(lines)), tokens(truth)
                if got != want:
                    bad += 1
                    print(f"DIFF d{n}.{ext}: missing {dict(want - got)} extra {dict(got - want)}")
    print("ok" if not bad else f"{bad} differ", f"({args.docs} documents x 4 formats)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
