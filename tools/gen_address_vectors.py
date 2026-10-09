#!/usr/bin/env python3
"""Writes address-list header values with the addresses Python's
`email.utils.getaddresses` reads from them, one JSON line each, for the
people_facts test:

    python3 tools/gen_address_vectors.py [N] [SEED] > tests/fixtures/address_vectors.jsonl

Each line: {"raw": ..., "addrs": [[name, address], ...]} (address lower-cased,
name null when empty or the address itself). Values Python rejects (a strict
parse returns nothing for a non-empty value) are dropped.
"""
import json, random, sys
from email.utils import getaddresses

R = random.Random()
FIRST = ["Jane", "Jürgen", "María", "Wei", "Aoife", "Dmitri", "Priya", "Tom", "Élodie", "Søren", "Anne-Marie", "O'Neil"]
LAST = ["Smith", "Müller", "García", "Zhang", "Ní Bhriain", "Petrov", "Patel", "Brown", "Dubois", "Jensen", "van der Berg"]
DOMAINS = ["example.org", "acme-corp.com", "mail.example.co.uk", "uni.edu.au", "Example.COM"]

def addr():
    local = R.choice(["jane", "j.smith", "info", "tom+tag", "a_b", "Wei.Zhang", "x"]) + str(R.randrange(100))
    return f"{local}@{R.choice(DOMAINS)}"

def name():
    return f"{R.choice(FIRST)} {R.choice(LAST)}"

def quote(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'

def one():
    a, n = addr(), name()
    k = R.randrange(10)
    if k == 0: return a
    if k == 1: return f"{n} <{a}>"
    if k == 2:
        swapped = n.split()[-1] + ", " + n.split()[0]
        return f"{quote(swapped)} <{a}>"
    if k == 3: return f"<{a}>"
    if k == 4: return f"{a} ({n})"
    if k == 5: return f"{quote(n)} <{a}>"
    if k == 6:
        boss = n + ' "the boss"'
        return f"{quote(boss)} <{a}>"
    if k == 7: return f"{n}  <{a}>"
    if k == 8: return f"{n} <{a}>".upper() if R.random() < 0.3 else f"{n} <{a}>"
    return f"{n.split()[0]} <{a}>"

def value():
    k = R.randrange(10)
    items = [one() for _ in range(R.randint(1, 4))]
    if k == 0: return "undisclosed-recipients:;"
    if k == 1: return f"Team {R.randrange(9)}: {', '.join(items)};"
    sep = R.choice([", ", ",", " , ", ",\n\t"])
    return sep.join(items)

def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 1500
    R.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 3)
    seen = set()
    while len(seen) < n:
        v = value()
        if v in seen: continue
        got = getaddresses([v])
        out = []
        for realname, address in got:
            if not address: continue
            nm = realname or None
            if nm and nm.lower() == address.lower(): nm = None
            out.append([nm, address.lower()])
        if v.strip() and not out and "undisclosed" not in v: continue
        seen.add(v)
        print(json.dumps({"raw": v, "addrs": out}))

main()
