#!/usr/bin/env python3
"""Checks sniff-rs's reading of Outlook .msg files against extract_msg.

    python3 tools/check_msg.py [--bin target/release/sniff-rs] FILE.msg...

For each file the profile's single row (subject, date, sender, recipients by
type, body) is compared with what extract_msg reads from the same file.
Needs `pip install extract-msg`.
"""
import argparse
import datetime
import email.utils
import json
import subprocess
import sys

import extract_msg


def clean(s):
    return (s or "").split("\x00")[0]


def row(binary, path):
    out = subprocess.run(
        [binary, path, "-", "--output-format", "json", "--samples", "1"],
        capture_output=True,
        check=True,
    ).stdout
    doc = json.loads(out)
    (cols,) = doc["tables"].values()
    return {c["name"]: (c["sample_values"] or [None])[0] for c in cols}


def addresses(value):
    return sorted(a.lower() for _, a in email.utils.getaddresses([value or ""]) if "@" in a)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("files", nargs="+")
    args = ap.parse_args()
    bad = 0
    for path in args.files:
        ours = row(args.bin, path)
        msg = extract_msg.Message(path)
        want = {"to": [], "cc": [], "bcc": []}
        for r in msg.recipients:
            addr = clean(r.email).lower()
            kind = {1: "to", 2: "cc", 3: "bcc"}.get(getattr(r.type, "value", r.type), "to")
            if "@" in addr:
                want[kind].append(addr)
        problems = []
        if clean(msg.subject) != (ours.get("Subject") or ""):
            problems.append(("subject", clean(msg.subject), ours.get("Subject")))
        for kind, key in (("to", "To"), ("cc", "Cc"), ("bcc", "Bcc")):
            if sorted(set(want[kind])) != sorted(set(addresses(ours.get(key)))):
                problems.append((kind, sorted(set(want[kind])), addresses(ours.get(key))))
        if msg.date:
            got = ours.get("Date")
            if got is None:
                problems.append(("date", msg.date, None))
            else:
                parsed = email.utils.parsedate_to_datetime(got)
                if parsed.astimezone(datetime.timezone.utc) != msg.date.astimezone(datetime.timezone.utc):
                    problems.append(("date", msg.date, got))
        body = clean(msg.body).replace("\r\n", "\n").strip()
        if body != (ours.get("body") or "").replace("\r\n", "\n").strip():
            problems.append(("body", body[:60], (ours.get("body") or "")[:60]))
        print(("ok  " if not problems else "DIFF"), path, problems if problems else "")
        bad += bool(problems)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
