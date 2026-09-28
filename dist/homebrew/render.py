#!/usr/bin/env python3
"""Render sniff-rs.rb for one release.

    render.py VERSION SHA256SUMS

Replaces the template's version, every `v0.1.0` in download URLs, and each
`UPDATE_ME_FROM_SHA256SUMS_TXT` with the checksum of the archive named on
the URL line just above it. Fails if any archive's checksum is missing.
"""
import pathlib
import re
import sys

version, sums_path = sys.argv[1], sys.argv[2]
sums = {}
for line in pathlib.Path(sums_path).read_text().splitlines():
    digest, name = line.split()
    sums[name.lstrip("*")] = digest

template = pathlib.Path(__file__).with_name("sniff-rs.rb").read_text()
out, last_asset = [], None
for line in template.splitlines():
    line = line.replace('version "0.1.0"', f'version "{version}"')
    line = line.replace("/v0.1.0/", f"/v{version}/")
    m = re.search(r"/(sniff-rs-[^/\"]+\.tar\.gz)\"", line)
    if m:
        last_asset = m.group(1)
    if "UPDATE_ME_FROM_SHA256SUMS_TXT" in line:
        if last_asset not in sums:
            sys.exit(f"no checksum for {last_asset}")
        line = line.replace("UPDATE_ME_FROM_SHA256SUMS_TXT", sums[last_asset])
    out.append(line)
print("\n".join(out))
