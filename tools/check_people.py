#!/usr/bin/env python3
"""Checks `sniff-rs graph --people` against an independent computation.

    python3 tools/check_people.py [--bin ./target/release/sniff-rs] [SEEDS]

Each seed writes mailboxes, address books, calendars and Word files for a
random roster (people with one or two addresses, names written both ways,
two people sharing a name, role mailboxes, authors nobody knows). The people
in each file are read back with Python's mailbox/email, vobject and
icalendar, and the people nodes, involves, authored_by and same_person
links are worked out again with a separate union-find. The graph must agree
exactly. Needs vobject and icalendar.
"""
import datetime, json, mailbox, os, random, re, subprocess, sys, tempfile, zipfile
from email.header import decode_header, make_header
from email.headerregistry import Address
from email.message import EmailMessage
from email.utils import getaddresses
import icalendar, vobject

FIRST = ["Jane", "Jürgen", "María", "Wei", "Aoife", "Dmitri", "Priya", "Tom", "Élodie", "Søren", "Hana", "Luca"]
LAST = ["Smith", "Müller", "García", "Zhang", "Petrov", "Patel", "Brown", "Dubois", "Jensen", "Rossi"]
ROLE = ["noreply", "info", "support", "billing"]

def name_key(name):
    name = name.strip()
    if "," in name:
        last, _, first = name.partition(",")
        if "," not in first and first.strip():
            name = f"{first.strip()} {last.strip()}"
    cleaned = "".join(c.lower() if c.isalnum() or c == "'" else " " for c in name)
    words = cleaned.split()
    return " ".join(words) if len(words) >= 2 and len(cleaned) >= 5 else None

def tidy(name):
    name = " ".join(name.split())
    if "," in name:
        last, _, first = name.partition(",")
        f = first.strip().rstrip(".").lower()
        if last.strip() and first.strip() and "," not in first and f not in ("jr", "sr", "ii", "iii", "iv", "phd", "md", "esq"):
            return f"{first.strip()} {last.strip()}"
    return name

def make(folder, rng):
    roster = []
    used = set()
    for i in range(rng.randint(5, 12)):
        for _ in range(30):
            first, last = rng.choice(FIRST), rng.choice(LAST)
            if rng.random() < 0.2 and roster:        # a namesake with other addresses
                first, last = roster[0]["first"], roster[0]["last"]
            nm = f"{first} {last}"
            break
        local = f"p{i}x"
        emails = [f"{local}@{rng.choice(['acme-corp.com', 'uni.edu.au', 'example.org'])}"]
        if rng.random() < 0.3:
            emails.append(f"{local}.alt@gmail.com")
        roster.append({"first": first, "last": last, "name": nm, "emails": emails})
    # mailboxes
    refs = {}    # file -> list of (email, display name or None)
    for m in range(rng.randint(1, 3)):
        path = os.path.join(folder, f"box{m}.mbox")
        box = mailbox.mbox(path)
        got = []
        for _ in range(rng.randint(2, 6)):
            msg = EmailMessage(); msg["Subject"] = "s"
            frm = rng.choice(roster)
            def addr(p):
                nm = p["name"] if rng.random() < 0.7 else (f"{p['last']}, {p['first']}" if rng.random() < 0.5 else "")
                return Address(display_name=nm, addr_spec=rng.choice(p["emails"]))
            msg["From"] = addr(frm)
            to = [addr(rng.choice(roster)) for _ in range(rng.randint(1, 3))]
            if rng.random() < 0.2: to.append(Address(display_name="", addr_spec=f"{rng.choice(ROLE)}@acme-corp.com"))
            msg["To"] = to
            if rng.random() < 0.3: msg["Cc"] = [addr(rng.choice(roster))]
            msg.set_content("x"); box.add(msg)
        box.flush(); box.close()
        for m_ in mailbox.mbox(path):
            for h in ("From", "Sender", "To", "Cc", "Bcc", "Reply-To"):
                for realname, a in getaddresses(m_.get_all(h, [])):
                    if a:
                        nm = str(make_header(decode_header(realname))).strip() if realname else None
                        got.append((a.lower(), nm or None, "mbox"))
        refs[f"box{m}.mbox"] = got
    # address books
    cards = {}   # file -> list of (emails, name)
    for v in range(rng.randint(0, 2)):
        out, lst = [], []
        for p in rng.sample(roster, rng.randint(1, min(5, len(roster)))):
            c = vobject.vCard(); c.add("n"); c.n.value = vobject.vcard.Name(family=p["last"], given=p["first"])
            c.add("fn"); c.fn.value = p["name"]
            for e in p["emails"]:
                x = c.add("email"); x.value = e
            out.append(c.serialize()); lst.append((list(p["emails"]), p["name"]))
        open(os.path.join(folder, f"book{v}.vcf"), "w").write("".join(out))
        cards[f"book{v}.vcf"] = lst
    # calendars
    cal_refs = {}
    for k in range(rng.randint(0, 2)):
        cal = icalendar.Calendar(); cal.add("prodid", "-//x//"); cal.add("version", "2.0")
        got = []
        for e in range(rng.randint(1, 3)):
            ev = icalendar.Event(); ev.add("summary", "m"); ev.add("dtstart", datetime.datetime(2024, 1, 1 + e, 9)); ev.add("uid", f"{k}{e}@x")
            o = rng.choice(roster); oa = icalendar.vCalAddress("mailto:" + rng.choice(o["emails"]))
            if rng.random() < 0.7: oa.params["cn"] = icalendar.vText(o["name"])
            ev["organizer"] = oa
            for _ in range(rng.randint(0, 3)):
                p = rng.choice(roster); aa = icalendar.vCalAddress("mailto:" + rng.choice(p["emails"]))
                if rng.random() < 0.7: aa.params["cn"] = icalendar.vText(f"{p['last']}, {p['first']}")
                ev.add("attendee", aa)
            cal.add_component(ev)
        open(os.path.join(folder, f"cal{k}.ics"), "wb").write(cal.to_ical())
        for ev in icalendar.Calendar.from_ical(cal.to_ical()).walk("VEVENT"):
            for prop in ("organizer", "attendee"):
                vals = ev.get(prop)
                if vals is None: continue
                for val in (vals if isinstance(vals, list) else [vals]):
                    cn = val.params.get("CN")
                    got.append((str(val).split(":", 1)[1].lower(), str(cn) if cn else None, "ical"))
        cal_refs[f"cal{k}.ics"] = got
    # Word files with an author
    authors = {}
    for d in range(rng.randint(1, 4)):
        p = rng.choice(roster)
        a = rng.choice([p["name"], f"{p['last']}, {p['first']}", "Nobody Known", p["first"]])
        core = f'<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>{a}</dc:creator></cp:coreProperties>'
        with zipfile.ZipFile(os.path.join(folder, f"doc{d}.docx"), "w") as z:
            z.writestr("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>')
            z.writestr("word/document.xml", f'<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>document {d}</w:t></w:r></w:p></w:body></w:document>')
            z.writestr("docProps/core.xml", core)
        authors[f"doc{d}.docx"] = a
    return refs, cards, cal_refs, authors

def expected(refs, cards, cal_refs, authors):
    parent = {}
    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]; x = parent[x]
        return x
    def union(a, b): parent[find(a)] = find(b)
    role = lambda e: re.sub(r"[^a-z0-9]", "", e.split("@")[0].lower()) in ROLE
    files = {}   # key -> set of files ; names ; card
    names = {}
    card_keys = set()
    def see(file, email, name):
        if role(email): return
        find(email)
        files.setdefault(email, set()).add(file)
        if name: names.setdefault(email, set()).add(tidy(name))
    for f, lst in {**refs, **cal_refs}.items():
        for email, name, _ in lst: see(f, email, name)
    for f, lst in cards.items():
        for emails, name in lst:
            for e in emails: see(f, e, name)
            for e in emails[1:]: union(emails[0], e)
            card_keys.add(emails[0])
    ident = {}
    for email in list(parent):
        r = find(email)
        d = ident.setdefault(r, {"emails": set(), "files": set(), "names": set(), "card": False})
        d["emails"].add(email); d["files"] |= files.get(email, set()); d["names"] |= names.get(email, set())
    for c in card_keys: ident[find(c)]["card"] = True
    by_name = {}
    for r, d in ident.items():
        for n in d["names"]:
            k = name_key(n)
            if k: by_name.setdefault(k, set()).add(r)
    authored = {}   # (file, root) -> conf
    for f, a in authors.items():
        k = name_key(a)
        roots = by_name.get(k, set()) if k else set()
        if 0 < len(roots) <= 5:
            for r in roots: authored[(f, r)] = "INFERRED" if len(roots) == 1 else "AMBIGUOUS"
    nodes = {}
    for r, d in ident.items():
        allf = set(d["files"]) | {f for (f, rr) in authored if rr == r}
        if len(allf) >= 2 or d["card"]:
            nodes[min(d["emails"])] = (r, allf)
    root_to_primary = {r: p for p, (r, _) in nodes.items()}
    involves = {(f, root_to_primary[r]) for r, d in ident.items() if r in root_to_primary for f in d["files"]}
    auth = {(f, root_to_primary[r], c) for (f, r), c in authored.items() if r in root_to_primary}
    same = set()
    key_roots = {}
    for r, d in ident.items():
        if r not in root_to_primary: continue
        for n in d["names"]:
            k = name_key(n)
            if k: key_roots.setdefault(k, set()).add(r)
    for k, rs in key_roots.items():
        if 2 <= len(rs) <= 5:
            ps = sorted(root_to_primary[r] for r in rs)
            for i in range(len(ps)):
                for j in range(i + 1, len(ps)): same.add((ps[i], ps[j]))
    return set(nodes), involves, auth, same

def main():
    args = sys.argv[1:]
    binary = "./target/release/sniff-rs"
    if args[:1] == ["--bin"]:
        binary, args = args[1], args[2:]
    seeds = [int(a) for a in args] or list(range(1, 21))
    bad = 0
    for seed in seeds:
        rng = random.Random(seed)
        with tempfile.TemporaryDirectory() as tmp:
            folder = os.path.join(tmp, "in"); os.makedirs(folder)
            refs, cards, cal_refs, authors = make(folder, rng)
            out = os.path.join(tmp, "out")
            r = subprocess.run([binary, "graph", folder, out, "--people", "--no-cache"], capture_output=True, text=True)
            if r.returncode: print(seed, "failed", r.stderr[-300:]); bad += 1; continue
            g = json.load(open(os.path.join(out, "graph.json")))
            want_nodes, want_inv, want_auth, want_same = expected(refs, cards, cal_refs, authors)
            got_nodes = {n["id"][7:] for n in g["nodes"] if n["type"] == "person"}
            got_inv = {(l["source"], l["target"][7:]) for l in g["links"] if l["relation"] == "involves"}
            got_auth = {(l["source"], l["target"][7:], l["confidence"]) for l in g["links"] if l["relation"] == "authored_by"}
            got_same = {tuple(sorted((l["source"][7:], l["target"][7:]))) for l in g["links"] if l["relation"] == "same_person"}
            problems = []
            for what, w, gt in (("nodes", want_nodes, got_nodes), ("involves", want_inv, got_inv), ("authored_by", want_auth, got_auth), ("same_person", want_same, got_same)):
                if w != gt:
                    problems.append(f"{what}: missing {sorted(w - gt)[:3]} extra {sorted(gt - w)[:3]}")
            if problems:
                bad += 1; print(f"seed {seed}:"); [print("   ", p) for p in problems]
            else:
                print(f"seed {seed}: ok ({len(want_nodes)} people, {len(want_inv)} involves, {len(want_auth)} authored_by, {len(want_same)} same_person)")
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    main()
