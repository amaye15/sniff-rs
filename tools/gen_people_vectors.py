#!/usr/bin/env python3
"""Writes mailboxes, address books and calendars with the people Python's
mailbox/email, vobject and icalendar libraries read from them, one JSON
line each, for the people_facts tests:

    python3 tools/gen_people_vectors.py [N] [SEED] > tests/fixtures/people_vectors.jsonl

Each line: {"format": "mbox"|"vcard"|"ical", "text": ..., "people":
[[email, name|null, role, count, group|0], ...]}, sorted.
"""
import json, mailbox, os, random, sys, tempfile, datetime
from email.header import decode_header, make_header
from email.message import EmailMessage
from email.headerregistry import Address
from email.utils import getaddresses
import vobject, icalendar

R = random.Random()
FIRST = ["Jane", "Jürgen", "María", "Wei", "Aoife", "Dmitri", "Priya", "Tom", "Élodie", "Søren"]
LAST = ["Smith", "Müller", "García", "Zhang", "Ní Bhriain", "Petrov", "Patel", "Brown", "Dubois"]
DOMAINS = ["example.org", "acme-corp.com", "uni.edu.au", "mail.example.co.uk"]

def person():
    first, last = R.choice(FIRST), R.choice(LAST)
    local = (first[0] + last.split()[-1]).lower().replace("é", "e").replace("ü", "u").replace("ø", "o")
    local = "".join(c for c in local if c.isascii() and c.isalnum()) or "user"
    return f"{first} {last}", f"{local}{R.randrange(30)}@{R.choice(DOMAINS)}"

POOL = [person() for _ in range(14)]

def mbox_vector():
    msgs = []
    for _ in range(R.randint(1, 5)):
        m = EmailMessage()
        m["Subject"] = "hello"
        fields = {"From": 1, "To": R.randint(0, 3), "Cc": R.randint(0, 2), "Bcc": R.randint(0, 1), "Reply-To": R.randint(0, 1), "Sender": R.randint(0, 1)}
        for h, n in fields.items():
            if n:
                people = [R.choice(POOL) for _ in range(n)]
                m[h] = [Address(display_name=nm if R.random() < 0.85 else "", addr_spec=a) for nm, a in people]
        m.set_content("body")
        msgs.append(m)
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "x.mbox")
        box = mailbox.mbox(path)
        for m in msgs:
            box.add(m)
        box.flush(); box.close()
        text = open(path, encoding="utf-8", errors="surrogateescape").read()
        counts = {}
        for msg in mailbox.mbox(path):
            seen = {}
            for h, role in [("From", "from"), ("Sender", "sender"), ("To", "to"), ("Cc", "cc"), ("Bcc", "bcc"), ("Reply-To", "reply-to")]:
                for realname, addr in getaddresses(msg.get_all(h, [])):
                    if not addr:
                        continue
                    name = str(make_header(decode_header(realname))).strip() if realname else None
                    name = name or None
                    if name and name.lower() == addr.lower():
                        name = None
                    k = (addr.lower(), role)
                    # once per address and role; the first display name wins
                    if k not in seen or (seen[k] is None and name):
                        seen[k] = name
            for (addr, role), name in seen.items():
                key = (addr, name.lower() if name else "", role)
                c = counts.setdefault(key, [addr, name, role, 0, 0]); c[3] += 1
    return {"format": "mbox", "text": text, "people": sorted(counts.values(), key=lambda x: (x[0], x[1] or "", x[2]))}

def vcard_vector():
    out, counts = [], {}
    for gi in range(1, R.randint(2, 5)):
        nm, addr = R.choice(POOL)
        c = vobject.vCard()
        first, last = nm.split(" ", 1)
        c.add("n"); c.n.value = vobject.vcard.Name(family=last, given=first)
        use_fn = R.random() < 0.8
        if use_fn:
            c.add("fn"); c.fn.value = nm
        else:
            c.add("fn"); c.fn.value = ""
        emails = [addr] + ([R.choice(POOL)[1]] if R.random() < 0.4 else [])
        grouped = R.random() < 0.3
        for e in emails:
            em = c.add("email"); em.value = e; em.type_param = R.choice(["WORK", "HOME", "INTERNET"])
            if grouped: em.group = "item1"
        org = R.choice(["Acme", "Globex Ltd", None])
        if org:
            c.add("org"); c.org.value = [org]
        out.append(c.serialize())
        expected_name = nm if use_fn else f"{first} {last}"
        for e in emails:
            key = (gi, e)
            counts[key] = [e.lower(), expected_name, "card", 1, gi]
    return {"format": "vcard", "text": "".join(out), "people": sorted(counts.values(), key=lambda x: (x[4], x[0]))}

def ical_vector():
    cal = icalendar.Calendar()
    cal.add("prodid", "-//x//"); cal.add("version", "2.0")
    counts = {}
    for _ in range(R.randint(1, 4)):
        ev = icalendar.Event()
        ev.add("summary", "meeting"); ev.add("dtstart", datetime.datetime(2024, 1, 1, 9, 0))
        ev.add("uid", f"{R.randrange(10**9)}@x")
        nm, addr = R.choice(POOL)
        org = icalendar.vCalAddress(f"mailto:{addr}")
        if R.random() < 0.7:
            org.params["cn"] = icalendar.vText(nm)
        ev["organizer"] = org
        seen = {(addr.lower(), "organizer")}
        org_name = nm if "cn" in org.params else None
        counts.setdefault((addr.lower(), "organizer", org_name), [addr.lower(), org_name, "organizer", 0, 0])[3] += 1
        for _ in range(R.randint(0, 3)):
            nm2, a2 = R.choice(POOL)
            at = icalendar.vCalAddress(f"MAILTO:{a2}")
            has_cn = R.random() < 0.7
            if has_cn:
                at.params["cn"] = icalendar.vText(f"{nm2.split()[-1]}, {nm2.split()[0]}")
            ev.add("attendee", at)
            if (a2.lower(), "attendee") in seen:
                continue
            seen.add((a2.lower(), "attendee"))
            at_name = f"{nm2.split()[-1]}, {nm2.split()[0]}" if has_cn else None
            c = counts.setdefault((a2.lower(), "attendee", at_name), [a2.lower(), at_name, "attendee", 0, 0])
            c[3] += 1
        cal.add_component(ev)
    text = cal.to_ical().decode("utf-8")
    # oracle: read it back with the library
    back = icalendar.Calendar.from_ical(text)
    counts2 = {}
    for ev in back.walk("VEVENT"):
        seen = set()
        for prop, role in (("organizer", "organizer"), ("attendee", "attendee")):
            vals = ev.get(prop)
            if vals is None: continue
            for v in (vals if isinstance(vals, list) else [vals]):
                email = str(v).split(":", 1)[1].lower()
                if (email, role) in seen: continue
                seen.add((email, role))
                cn = str(v.params.get("CN")) if v.params.get("CN") else None
                c = counts2.setdefault((email, role, cn), [email, cn, role, 0, 0]); c[3] += 1
    return {"format": "ical", "text": text, "people": sorted(counts2.values(), key=lambda x: (x[0], x[2]))}

def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 300
    R.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 8)
    for i in range(n):
        print(json.dumps([mbox_vector, vcard_vector, ical_vector][i % 3](), ensure_ascii=False))

main()
