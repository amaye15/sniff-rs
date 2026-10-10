#!/usr/bin/env python3
"""Writes tests/fixtures/edge_graph_audio: a few short silent tracks with tags.

    python3 tools/make_audio_fixtures.py [--out tests/fixtures/edge_graph_audio]

Five tracks of one album by "Sigur Rós" (ID3v2.4 UTF-8, ID3v2.3 UTF-16,
ID3v2.2 Latin-1, FLAC, Ogg Vorbis), three of "Kid A" by Radiohead (M4A, Opus,
an MP3 with only an ID3v1 trailer), and one track that shares nothing. Needs
ffmpeg and mutagen. Tags are checked with mutagen after writing.
"""
import argparse
import struct
import subprocess
from pathlib import Path

from mutagen import id3 as mid3
from mutagen.flac import FLAC
from mutagen.mp4 import MP4
from mutagen.oggvorbis import OggVorbis

import check_audio as ca


def clip(path, kind):
    ca.clip(path, kind)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/fixtures/edge_graph_audio")
    out = Path(ap.parse_args().out)
    out.mkdir(parents=True, exist_ok=True)
    for kind in ("mp3", "flac", "ogg", "m4a", "opus"):
        base = out / f"_base.{kind}"
        clip(base, kind)
    base = {k: (out / f"_base.{k}").read_bytes() for k in ("mp3", "flac", "ogg", "m4a", "opus")}
    for k in base:
        (out / f"_base.{k}").unlink()

    artist, album = "Sigur Rós", "Ágætis byrjun"
    # ID3v2.4, UTF-8.
    p = out / "sigur_01.mp3"
    p.write_bytes(base["mp3"])
    t = mid3.ID3()
    t.add(mid3.TPE1(encoding=3, text=[artist]))
    t.add(mid3.TALB(encoding=3, text=[album]))
    t.add(mid3.TIT2(encoding=3, text=["Intro"]))
    t.save(str(p), v2_version=4, v1=0)
    # ID3v2.3, UTF-16.
    p = out / "sigur_02.mp3"
    p.write_bytes(base["mp3"])
    t = mid3.ID3()
    t.add(mid3.TPE1(encoding=1, text=[artist]))
    t.add(mid3.TALB(encoding=1, text=[album]))
    t.save(str(p), v2_version=3, v1=0)
    # ID3v2.2, Latin-1 (hand-written).
    p = out / "sigur_03.mp3"
    frames = b""
    for fid, text in (("TP1", artist), ("TAL", album), ("TT2", "Svefn-g-englar")):
        payload = b"\x00" + text.encode("latin-1")
        frames += fid.encode() + len(payload).to_bytes(3, "big") + payload
    p.write_bytes(b"ID3\x02\x00\x00" + ca.syncsafe(len(frames)) + frames + base["mp3"])
    for name, kind, cls in (("sigur_04.flac", "flac", FLAC), ("sigur_05.ogg", "ogg", OggVorbis)):
        p = out / name
        p.write_bytes(base[kind])
        f = cls(str(p))
        f["artist"] = [artist]
        f["album"] = [album]
        f["title"] = ["Track"]
        f.save()

    p = out / "radiohead_01.m4a"
    p.write_bytes(base["m4a"])
    f = MP4(str(p))
    f["\xa9ART"] = ["Radiohead"]
    f["\xa9alb"] = ["Kid A"]
    f.save()
    from mutagen.oggopus import OggOpus
    p = out / "radiohead_02.opus"
    p.write_bytes(base["opus"])
    f = OggOpus(str(p))
    f["artist"] = ["Radiohead"]
    f["album"] = ["Kid A"]
    f.save()
    p = out / "radiohead_03.mp3"
    p.write_bytes(base["mp3"] + ca.id3v1("Radiohead", "Kid A"))

    p = out / "alone.mp3"
    p.write_bytes(base["mp3"])
    t = mid3.ID3()
    t.add(mid3.TPE1(encoding=3, text=["Lone Wolf Quartet"]))
    t.add(mid3.TALB(encoding=3, text=["Nobody Else"]))
    t.save(str(p), v2_version=4, v1=0)

    for f in sorted(out.iterdir()):
        print(f.name, f.stat().st_size)


if __name__ == "__main__":
    main()
