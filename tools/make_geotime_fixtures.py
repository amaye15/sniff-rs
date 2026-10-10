#!/usr/bin/env python3
"""Writes tests/fixtures/edge_graph_geotime: files about the same places and days.

    python3 tools/make_geotime_fixtures.py [--out tests/fixtures/edge_graph_geotime]

A route around the Louvre as GPX, KML and GeoJSON and a table of GPS fixes
(all one 0.1-degree cell); two photos by the Eiffel Tower (another cell) and
one in Rome, taken on 2024-06-01; a mailbox and a calendar and a PDF dated
2024-06-01..03; and a file about nothing. Needs Pillow, pikepdf.
"""
import argparse
import csv
import json
import mailbox
from datetime import datetime, timezone, timedelta
from email.message import EmailMessage
from email.utils import format_datetime
from pathlib import Path

import pikepdf
from PIL import Image
from PIL.TiffImagePlugin import IFDRational

ROUTE = [(48.8606 + 0.0004 * i, 2.3376 - 0.0005 * i) for i in range(12)]


def photo(path, lat, lon, when):
    img = Image.new("RGB", (32, 24), (90, 120, 150))
    exif = Image.Exif()
    exif.get_ifd(0x8769)[0x9003] = when
    gps = exif.get_ifd(0x8825)

    def dms(v):
        v = abs(v)
        d = int(v)
        m = int((v - d) * 60)
        s = (v - d - m / 60) * 3600
        return (IFDRational(d, 1), IFDRational(m, 1), IFDRational(int(round(s * 1000)), 1000))

    gps[1], gps[2] = ("N" if lat >= 0 else "S"), dms(lat)
    gps[3], gps[4] = ("E" if lon >= 0 else "W"), dms(lon)
    img.save(path, exif=exif)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/fixtures/edge_graph_geotime")
    out = Path(ap.parse_args().out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "route.gpx").write_text(
        '<?xml version="1.0"?>\n<gpx version="1.1" creator="fixture" xmlns="http://www.topografix.com/GPX/1/1"><trk><name>walk</name><trkseg>\n'
        + "\n".join(f'<trkpt lat="{la:.6f}" lon="{lo:.6f}"><ele>35</ele></trkpt>' for la, lo in ROUTE)
        + "\n</trkseg></trk></gpx>\n")
    (out / "route.kml").write_text(
        '<?xml version="1.0"?>\n<kml xmlns="http://www.opengis.net/kml/2.2"><Document><Placemark><name>walk</name><LineString><coordinates>\n'
        + "\n".join(f"{lo:.6f},{la:.6f},0" for la, lo in ROUTE)
        + "\n</coordinates></LineString></Placemark></Document></kml>\n")
    (out / "route.geojson").write_text(json.dumps({
        "type": "FeatureCollection",
        "features": [{"type": "Feature", "properties": {"name": "walk"},
                      "geometry": {"type": "LineString", "coordinates": [[round(lo, 6), round(la, 6)] for la, lo in ROUTE]}}]}, indent=1) + "\n")
    with open(out / "fixes.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["time", "latitude", "longitude", "speed"])
        for i, (la, lo) in enumerate(ROUTE):
            w.writerow([f"2024-06-02T10:{i:02d}:00", f"{la:.5f}", f"{lo:.5f}", 4 + i % 3])
    photo(out / "eiffel_1.jpg", 48.8584, 2.2945, "2024:06:01 09:15:00")
    photo(out / "eiffel_2.jpg", 48.8530, 2.2900, "2024:06:01 17:40:00")
    photo(out / "rome.jpg", 41.9028, 12.4964, "2024:06:01 12:00:00")
    box = mailbox.mbox(str(out / "inbox.mbox"))
    for when in [datetime(2024, 6, 1, 8, 30, tzinfo=timezone.utc), datetime(2024, 6, 1, 23, 10, tzinfo=timezone(timedelta(hours=-5))), datetime(2024, 6, 2, 7, 0, tzinfo=timezone(timedelta(hours=2)))]:
        m = EmailMessage()
        m["From"], m["To"], m["Subject"] = "a@example.org", "b@example.org", "plans"
        m["Date"] = format_datetime(when)
        m.set_content("See you there.")
        box.add(m)
    box.flush()
    box.close()
    (out / "trip.ics").write_text("\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//fixture//EN",
        "BEGIN:VEVENT", "UID:1@fixture", "DTSTART;TZID=Europe/Paris:20240601T100000", "SUMMARY:Tower", "END:VEVENT",
        "BEGIN:VEVENT", "UID:2@fixture", "DTSTART;VALUE=DATE:20240603", "SUMMARY:Flight home", "END:VEVENT",
        "END:VCALENDAR", ""]))
    pdf = pikepdf.Pdf.new()
    pdf.add_blank_page(page_size=(200, 200))
    pdf.docinfo["/CreationDate"] = "D:20240602101500+02'00'"
    pdf.save(out / "itinerary.pdf")
    (out / "lonely.txt").write_text("A note about nothing in particular.\n")
    for f in sorted(out.iterdir()):
        print(f.name, f.stat().st_size)


if __name__ == "__main__":
    main()
