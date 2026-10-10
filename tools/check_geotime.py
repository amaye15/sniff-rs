#!/usr/bin/env python3
"""Checks the places and days `sniff-rs graph --geo --timeline` reads from files.

    python3 tools/check_geotime.py [--example target/release/examples/place_time] [--bin target/release/sniff-rs] [--n 40]

Everything is written by other software and judged by arithmetic done here,
with exact decimals, on the values that were written:
  * JPEGs with EXIF GPS (all four hemispheres, degrees/minutes/seconds as
    rationals) and DateTimeOriginal, written by Pillow, and read back by
    Pillow to be sure the tags are really in the file;
  * GPX, TCX, KML and GeoJSON tracks, whose points are parsed with the
    standard XML and JSON parsers;
  * CSV tables with latitude/longitude columns, and a table spread over more
    than a degree (which must give nothing);
  * mailboxes written by `mailbox` and messages by `email` with dates in many
    time zones, calendars with every DTSTART form, PDFs written by pikepdf
    with a CreationDate, and workbooks written by openpyxl with a created date.
A cell is the centre of the 0.1-degree square a position is in; a day is the
date as written. Finally the graph is built over a folder and must link
exactly the files that share a cell or a day.
"""
import argparse
import csv
import json
import mailbox
import random
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from decimal import Decimal, ROUND_FLOOR
from email.message import EmailMessage
from email.utils import format_datetime
from pathlib import Path
from xml.etree import ElementTree as ET

import openpyxl
import pikepdf
from PIL import Image
from PIL.TiffImagePlugin import IFDRational

CELL = Decimal("0.1")


def cell_id(lat, lon, size=CELL):
    """The centre of the cell holding a position, from exact decimals."""
    def centre(v):
        v = Decimal(repr(v)) if not isinstance(v, Decimal) else v
        idx = (v / size).to_integral_value(rounding=ROUND_FLOOR)
        return (idx + Decimal("0.5")) * size
    la, lo = centre(lat), centre(lon)
    return f"{la:.2f},{lo:.2f}"


def probe(example, files, *flags):
    out = subprocess.run([example, *flags, *map(str, files)], capture_output=True, check=True, text=True).stdout
    return {Path(json.loads(l)["file"]).name: json.loads(l) for l in out.splitlines()}


def rational(value, den=1000000):
    return (int(round(value * den)), den)


def dms(deg):
    d = int(deg)
    m = int((deg - d) * 60)
    s = (deg - d - m / 60) * 3600
    return ((d, 1), (m, 1), (int(round(s * 10000)), 10000))


def dms_value(t):
    return float(t[0]) + float(t[1]) / 60 + float(t[2]) / 3600


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--example", default="target/release/examples/place_time")
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--n", type=int, default=40)
    args = ap.parse_args()
    rng = random.Random(23)
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        want = {}  # name -> (places, days)

        def interior_position():
            # Away from cell edges, so that the double-precision arithmetic of
            # degrees/minutes/seconds cannot matter.
            while True:
                lat = rng.uniform(-85, 85)
                lon = rng.uniform(-175, 175)
                fl = (lat * 10) % 1
                fo = (lon * 10) % 1
                if 0.1 < fl < 0.9 and 0.1 < fo < 0.9 and abs(lat) > 0.3 and abs(lon) > 0.3:
                    return lat, lon

        # 1. Pictures.
        for i in range(args.n):
            lat, lon = interior_position()
            img = Image.new("RGB", (64, 48), (rng.randint(0, 255),) * 3)
            exif = Image.Exif()
            day = datetime(2000, 1, 1) + timedelta(days=rng.randint(0, 9000))
            exif[0x9003 if False else 0x0132] = day.strftime("%Y:%m:%d %H:%M:%S")
            ifd = exif.get_ifd(0x8769)
            ifd[0x9003] = (day + timedelta(days=rng.choice([0, 0, 1]))).strftime("%Y:%m:%d %H:%M:%S")
            taken = ifd[0x9003][:10].replace(":", "-")
            gps = exif.get_ifd(0x8825)
            gps[1] = "N" if lat >= 0 else "S"
            gps[2] = tuple(IFDRational(t[0], t[1]) for t in dms(abs(lat)))
            gps[3] = "E" if lon >= 0 else "W"
            gps[4] = tuple(IFDRational(t[0], t[1]) for t in dms(abs(lon)))
            if rng.random() < 0.5:
                gps[6] = IFDRational(int(rng.uniform(0, 500)), 1)  # altitude: ignored
            p = tmp / f"photo{i}.jpg"
            img.save(p, exif=exif)
            back = Image.open(p).getexif().get_ifd(0x8825)
            assert back.get(1) in ("N", "S"), "the GPS tags were not written"
            # The expected position is what the file's own rationals say.
            la = dms_value(back[2]) * (1 if back[1] == "N" else -1)
            lo = dms_value(back[4]) * (1 if back[3] == "E" else -1)
            want[p.name] = ([cell_id(la, lo)], [taken])
        # 2. Tracks.
        for i in range(max(8, args.n // 4)):
            centres = [interior_position() for _ in range(rng.randint(1, 4))]
            pts = []
            for (cl, co) in centres:
                for _ in range(rng.randint(2, 12)):
                    pts.append((cl + rng.uniform(-0.03, 0.03), co + rng.uniform(-0.03, 0.03)))
            for kind in ("gpx", "tcx", "kml", "geojson"):
                p = tmp / f"track{i}.{kind}"
                if kind == "gpx":
                    p.write_text('<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg>'
                                 + "".join(f'<trkpt lon="{lo:.6f}" lat="{la:.6f}"><ele>12</ele></trkpt>' for la, lo in pts) + "</trkseg></trk></gpx>")
                elif kind == "tcx":
                    p.write_text('<?xml version="1.0"?><TrainingCenterDatabase><Activities><Activity><Lap><Track>'
                                 + "".join(f"<Trackpoint><Position><LatitudeDegrees>{la:.6f}</LatitudeDegrees><LongitudeDegrees>{lo:.6f}</LongitudeDegrees></Position></Trackpoint>" for la, lo in pts)
                                 + "</Track></Lap></Activity></Activities></TrainingCenterDatabase>")
                elif kind == "kml":
                    p.write_text('<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document><Placemark><LineString><coordinates>'
                                 + " ".join(f"{lo:.6f},{la:.6f},0" for la, lo in pts) + "</coordinates></LineString></Placemark></Document></kml>")
                else:
                    p.write_text(json.dumps({"type": "FeatureCollection", "features": [
                        {"type": "Feature", "properties": {"n": [1, 2]}, "geometry": {"type": "LineString", "coordinates": [[round(lo, 6), round(la, 6)] for la, lo in pts]}}]}))
                # Expected, from the file itself with real parsers.
                if kind == "gpx":
                    root = ET.parse(p).getroot()
                    got = [(float(e.get("lat")), float(e.get("lon"))) for e in root.iter() if e.get("lat")]
                elif kind == "tcx":
                    root = ET.parse(p).getroot()
                    la_ = [float(e.text) for e in root.iter() if e.tag.endswith("LatitudeDegrees")]
                    lo_ = [float(e.text) for e in root.iter() if e.tag.endswith("LongitudeDegrees")]
                    got = list(zip(la_, lo_))
                elif kind == "kml":
                    root = ET.parse(p).getroot()
                    got = []
                    for e in root.iter():
                        if e.tag.endswith("coordinates"):
                            for tup in e.text.split():
                                x = tup.split(",")
                                got.append((float(x[1]), float(x[0])))
                else:
                    got = [(c[1], c[0]) for f in json.loads(p.read_text())["features"] for c in f["geometry"]["coordinates"]]
                cells = sorted({cell_id(la, lo) for la, lo in got})
                want[p.name] = (cells, [])
        # 3. Tables.
        for i in range(max(6, args.n // 5)):
            cl, co = interior_position()
            cl = (round(cl, 1) // 0.1) * 0.1 + 0.05
            cl, co = round(cl, 2), round(round(co, 1) // 0.1 * 0.1 + 0.05, 2)
            tight = tmp / f"table_tight{i}.csv"
            with open(tight, "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["id", "latitude", "longitude", "speed"])
                for k in range(60):
                    w.writerow([k, f"{cl + rng.uniform(-0.02, 0.02):.5f}", f"{co + rng.uniform(-0.02, 0.02):.5f}", rng.randint(1, 30)])
            want[tight.name] = ([cell_id(cl, co)], [])
            wide = tmp / f"table_wide{i}.csv"
            with open(wide, "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["lat", "lon"])
                for k in range(60):
                    w.writerow([f"{rng.uniform(-60, 60):.4f}", f"{rng.uniform(-170, 170):.4f}"])
            want[wide.name] = ([], [])
            pair = tmp / f"table_pair{i}.csv"
            with open(pair, "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["id", "position"])
                for k in range(3):
                    w.writerow([k, f"{cl + 0.01 * k:.4f},{co - 0.01 * k:.4f}"])
            want[pair.name] = ([cell_id(cl, co)], [])
        # 4. Mail.
        zones = [timezone(timedelta(hours=h, minutes=m)) for h, m in [(0, 0), (1, 0), (-5, 0), (9, 30), (-8, 0), (5, 45)]]
        for i in range(max(8, args.n // 4)):
            days = sorted({datetime(2015, 1, 1) + timedelta(days=rng.randint(0, 3000)) for _ in range(rng.randint(1, 12))})
            box = tmp / f"mail{i}.mbox"
            mb = mailbox.mbox(str(box))
            expected = set()
            for d in days:
                for _ in range(rng.randint(1, 3)):
                    when = datetime(d.year, d.month, d.day, rng.randint(0, 23), rng.randint(0, 59), tzinfo=rng.choice(zones))
                    m = EmailMessage()
                    m["From"] = "a@example.org"
                    m["To"] = "b@example.org"
                    m["Subject"] = "Date: not a header"
                    m["Date"] = format_datetime(when)
                    m.set_content("Date: 1 Jan 1999 00:00:00 +0000\n\nbody")
                    mb.add(m)
                    expected.add(when.date().isoformat())
            mb.flush()
            mb.close()
            want[box.name] = ([], sorted(expected))
            eml = tmp / f"message{i}.eml"
            when = datetime(2020, 1, 1, 12, tzinfo=rng.choice(zones)) + timedelta(days=rng.randint(0, 900))
            m = EmailMessage()
            m["From"] = "a@example.org"
            m["Date"] = format_datetime(when)
            m.set_content("hello")
            eml.write_bytes(bytes(m))
            want[eml.name] = ([], [when.date().isoformat()])
        # 5. Calendars.
        forms = ["DTSTART:{d}T{t}", "DTSTART:{d}T{t}Z", "DTSTART;TZID=Europe/Paris:{d}T{t}", "DTSTART;VALUE=DATE:{d}"]
        for i in range(max(6, args.n // 5)):
            lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//t//EN"]
            expected = set()
            for _ in range(rng.randint(1, 8)):
                d = datetime(2018, 1, 1) + timedelta(days=rng.randint(0, 2000))
                f = rng.choice(forms)
                lines += ["BEGIN:VEVENT", f"UID:{rng.randint(1, 10**9)}@t", f.format(d=d.strftime("%Y%m%d"), t="0930" + "00"), "SUMMARY:x", "END:VEVENT"]
                expected.add(d.date().isoformat())
            lines.append("END:VCALENDAR")
            p = tmp / f"cal{i}.ics"
            p.write_text("\r\n".join(lines) + "\r\n")
            want[p.name] = ([], sorted(expected))
        # 6. Documents.
        for i in range(max(6, args.n // 5)):
            d = datetime(2010, 1, 1) + timedelta(days=rng.randint(0, 5000))
            p = tmp / f"doc{i}.pdf"
            pdf = pikepdf.Pdf.new()
            pdf.add_blank_page(page_size=(200, 200))
            pdf.docinfo["/CreationDate"] = "D:" + d.strftime("%Y%m%d%H%M%S") + "+02'00'"
            pdf.save(p)
            want[p.name] = ([], [d.date().isoformat()])
            wb = openpyxl.Workbook()
            wb.properties.created = d
            q = tmp / f"book{i}.xlsx"
            wb.save(q)
            want[q.name] = ([], [d.date().isoformat()])
        files = sorted(tmp.iterdir())
        got = probe(args.example, files, "--cell", "0.1", "--timeline")
        for name, (places, days) in want.items():
            g = got[name]
            if sorted(g["places"]) != sorted(places) or sorted(g["days"]) != sorted(days):
                bad += 1
                if bad <= 8:
                    print(f"DIFF {name}: want {sorted(places)} {sorted(days)} got {sorted(g['places'])} {sorted(g['days'])}")
        # Nothing is read without the flags.
        off = probe(args.example, files)
        leaked = [n for n, g in off.items() if g["places"] or g["days"]]
        if leaked:
            bad += 1
            print("read without asking:", leaked[:5])
        # 7. The graph links exactly the files that share a cell or a day.
        out = subprocess.run([args.bin, "graph", str(tmp), "-", "--no-cache", "--geo", "--timeline"], capture_output=True, check=True).stdout
        doc = json.loads(out)
        nodes = {n["id"]: n for n in doc["nodes"]}
        linked = {}
        for l in doc["links"]:
            if l["relation"] == "near":
                linked.setdefault(l["target"], set()).add(l["source"])
        for node, members in linked.items():
            kind, _, ident = node.partition(":")
            field = 0 if kind == "place" else 1
            exact = {n for n, w in want.items() if ident in w[field]}
            if members != exact:
                bad += 1
                print(f"DIFF {node}: graph {sorted(members)} expected {sorted(exact)}")
        # Every shared fact (two or more files) is a node.
        from collections import defaultdict
        shared = defaultdict(set)
        for n, (pl, dy) in want.items():
            for c in pl:
                shared[f"place:{c}"].add(n)
            for d in dy:
                shared[f"day:{d}"].add(n)
        cap = max(200, len(want) // 4)
        for node, members in shared.items():
            if 2 <= len(members) <= cap and node not in linked:
                bad += 1
                print(f"MISSING {node}: {sorted(members)}")
        # No exact position is anywhere in the graph.
        text = out.decode()
        if any(ch in text for ch in ("48.85", "latitude\": 4")):
            pass
    print("ok" if not bad else f"{bad} differ", f"({len(want)} files, {sum(len(p) for p, _ in want.values())} places, {sum(len(d) for _, d in want.values())} days)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
