#!/usr/bin/env python3
"""Checks that sniff-rs reads a .eml file the way it reads the same message in an mbox.

    python3 tools/check_eml.py [--bin target/release/sniff-rs] [--seeds 60]

Random messages are built with Python's `email` package (plain, multipart with
attachments, RFC 2047 subjects, 8-bit and quoted-printable bodies, CRLF or LF
line ends, body lines that read as an envelope) and written both as `.eml` and
through Python's `mailbox.mbox` (which quotes such lines). The columns must
agree, apart from the envelope ones.
"""
import argparse
import json
import mailbox
import random
import subprocess
import sys
import tempfile
from email.message import EmailMessage
from email.policy import SMTP, default
from pathlib import Path

WORDS = "alpha beta gamma delta münchen café 会議 données report budget Q3 invoice".split()
PEOPLE = [("Jane Smith", "jane@acme-corp.com"), ("Tom Brown", "tom@acme-corp.com"),
          ("Müller, Jürgen", "jm@uni.edu.au"), ("", "bare@example.org"), ("Pat O'Lee", "pat@example.net")]


def sentence(rng, n=6):
    return " ".join(rng.choice(WORDS) for _ in range(n))


def build(rng):
    m = EmailMessage(policy=default)
    people = rng.sample(PEOPLE, k=rng.randint(2, 4))
    m["From"] = f'"{people[0][0]}" <{people[0][1]}>' if people[0][0] else people[0][1]
    m["To"] = ", ".join(f'"{n}" <{a}>' if n else a for n, a in people[1:])
    if rng.random() < 0.4:
        m["Cc"] = people[0][1]
    m["Subject"] = sentence(rng, rng.randint(2, 6))
    m["Date"] = "Tue, 05 Mar 2024 10:00:00 +0000"
    m["Message-ID"] = f"<{rng.randint(1, 10**9)}@example.org>"
    body = "\n".join(sentence(rng, rng.randint(3, 9)) for _ in range(rng.randint(1, 5)))
    if rng.random() < 0.5:
        body += "\n\nFrom the desk of someone\n"
    m.set_content(body, subtype="plain", cte=rng.choice(["quoted-printable", "base64", "8bit"]))
    if rng.random() < 0.4:
        m.add_attachment(b"data" * 10, maintype="application", subtype="octet-stream",
                         filename=f"{rng.choice(WORDS)}.bin".replace("é", "e"))
    return m


def profile(binary, path):
    out = subprocess.run([binary, str(path), "-", "--output-format", "json", "--samples", "1"],
                         capture_output=True, check=True).stdout
    (cols,) = json.loads(out)["tables"].values()
    return {c["name"]: (c["sample_values"] or [None])[0] for c in cols
            if not c["name"].startswith("envelope_")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--seeds", type=int, default=60)
    args = ap.parse_args()
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for seed in range(args.seeds):
            rng = random.Random(seed)
            m = build(rng)
            raw = m.as_bytes(policy=SMTP if seed % 2 else default)  # CRLF on odd seeds
            eml = tmp / f"m{seed}.eml"
            eml.write_bytes(raw)
            box = mailbox.mbox(str(tmp / f"m{seed}.mbox"))
            box.add(m)
            box.flush()
            box.close()
            want = profile(args.bin, tmp / f"m{seed}.mbox")
            got = profile(args.bin, eml)
            if want != got:
                bad += 1
                diff = {k: (want.get(k), got.get(k)) for k in set(want) | set(got) if want.get(k) != got.get(k)}
                print("DIFF seed", seed, {k: (str(a)[:50], str(b)[:50]) for k, (a, b) in diff.items()})
    print("ok" if not bad else f"{bad} seed(s) differ", f"({args.seeds} messages)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
