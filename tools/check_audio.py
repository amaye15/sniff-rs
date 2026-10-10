#!/usr/bin/env python3
"""Checks the artist, composer and album that `sniff-rs graph` reads from audio tags.

    python3 tools/check_audio.py [--example target/release/examples/file_meta] [--files 60]

Silent clips made with ffmpeg (MP3, M4A, FLAC, Ogg Vorbis, Ogg Opus) get random
tags, some with accents, CJK and emoji, and decoys (title, genre, year,
comments). Tags are written two ways: with mutagen, and by hand for the ID3
shapes mutagen does not write (v2.2, an unsynchronised v2.3 tag, every text
encoding, several NUL-separated values, an ID3v1 trailer alone). What sniff-rs
reads must equal what mutagen reads back from the same file. Needs ffmpeg and
mutagen.
"""
import argparse
import json
import random
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import mutagen
from mutagen import id3 as mid3
from mutagen.flac import FLAC
from mutagen.mp4 import MP4
from mutagen.oggopus import OggOpus
from mutagen.oggvorbis import OggVorbis

WORDS = ["Blue", "Night", "Echo", "Garden", "Signal", "Harbor", "Ember", "Nova", "Static", "Lumen", "Orchid", "Vector"]
UNI = ["Björk", "Sigur Rós", "坂本龍一", "宇多田ヒカル", "Мумий Тролль", "Ёлка", "Café Tacvba", "😀 Band", "Ólafur Arnalds", "김광석", "Beyoncé", "Mötley Crüe"]


def name(rng):
    if rng.random() < 0.35:
        return rng.choice(UNI)
    return " ".join(rng.sample(WORDS, rng.randint(1, 3)))


def clip(path, kind):
    args = {
        "mp3": ["-c:a", "libmp3lame"],
        "m4a": ["-c:a", "aac"],
        "flac": ["-c:a", "flac"],
        "ogg": ["-c:a", "vorbis", "-strict", "-2"],
        "opus": ["-c:a", "libopus"],
    }[kind]
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "0.3", *args, "-map_metadata", "-1", str(path)], check=True)


# ---- hand-written ID3 ----

def syncsafe(n):
    return bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F])


def encode(text_list, enc):
    if enc == 0:
        return b"\x00".join(t.encode("latin-1", "replace") for t in text_list)
    if enc == 1:
        return b"\x00\x00".join(b"\xff\xfe" + t.encode("utf-16-le") for t in text_list)
    if enc == 2:
        return b"\x00\x00".join(t.encode("utf-16-be") for t in text_list)
    return b"\x00".join(t.encode("utf-8") for t in text_list)


def latin_ok(s):
    try:
        s.encode("latin-1")
        return True
    except UnicodeEncodeError:
        return False


def id3_tag(version, frames, rng, unsync=False):
    """frames: list of (id4, texts). Returns the bytes of an ID3 tag."""
    names = {"TPE1": "TP1", "TPE2": "TP2", "TCOM": "TCM", "TALB": "TAL", "TIT2": "TT2", "TCON": "TCO", "TYER": "TYE"}
    body = b""
    for fid, texts in frames:
        enc = rng.choice([0, 1, 2, 3] if version == 4 else [0, 1] if version in (2, 3) else [0, 1])
        if enc == 0 and not all(latin_ok(t) for t in texts):
            enc = 1
        payload = bytes([enc]) + encode(texts if version == 4 else texts[:1], enc)
        if version == 2:
            body += names[fid].encode() + len(payload).to_bytes(3, "big") + payload
        else:
            size = syncsafe(len(payload)) if version == 4 else struct.pack(">I", len(payload))
            body += fid.encode() + size + b"\x00\x00" + payload
    body += b"\x00" * rng.randint(0, 20)
    flags = 0
    if unsync and version == 3:
        out = bytearray()
        for i, b in enumerate(body):
            out.append(b)
            if b == 0xFF and (i + 1 == len(body) or body[i + 1] >= 0xE0 or body[i + 1] == 0):
                out.append(0)
        body = bytes(out)
        flags = 0x80
    return b"ID3" + bytes([version, 0, flags]) + syncsafe(len(body)) + body


def id3v1(artist, album):
    f = lambda s: s.encode("latin-1", "replace")[:30].ljust(30, b"\x00")
    return b"TAG" + f("t") + f(artist) + f(album) + b"2020" + b"\x00" * 30 + b"\x00"


def expected_from_mutagen(path):
    f = mutagen.File(str(path))
    artist, album_artist, composer, album = [], [], [], []

    def add(lst, vals):
        for v in vals:
            v = str(v).strip()
            if v and v not in lst:
                lst.append(v)

    t = f.tags
    if isinstance(f, mutagen.mp4.MP4):
        add(artist, t.get("\xa9ART", [])[:1])
        add(album_artist, t.get("aART", [])[:1])
        add(composer, t.get("\xa9wrt", [])[:1])
        add(album, t.get("\xa9alb", [])[:1])
    elif isinstance(f, (FLAC, OggVorbis, OggOpus)):
        add(artist, t.get("artist", []) + t.get("performer", []))
        add(album_artist, t.get("albumartist", []))
        add(composer, t.get("composer", []))
        add(album, t.get("album", []))
    else:
        # MP3 (or any file with ID3).
        if t is None:
            t = mid3.ID3(str(path))
        for fid, lst in (("TPE1", artist), ("TPE2", album_artist), ("TCOM", composer), ("TALB", album)):
            if fid in t:
                add(lst, [x for v in t[fid].text for x in str(v).split("\x00")])
    out = []

    def push(k, v):
        if [k, v] not in out:
            out.append([k, v])

    for a in artist + album_artist:
        push("artist", a)
    for c in composer:
        push("composer", c)
    lead = (album_artist or artist or [None])[0]
    for al in album:
        push("album", f"{al} - {lead}" if lead else al)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--example", default="target/release/examples/file_meta")
    ap.add_argument("--files", type=int, default=60)
    args = ap.parse_args()
    rng = random.Random(11)
    bad = 0
    total = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        base = {}
        for kind in ("mp3", "m4a", "flac", "ogg", "opus"):
            base[kind] = tmp / f"base.{kind}"
            clip(base[kind], kind)
        files = []
        for i in range(args.files):
            kind = ["mp3", "m4a", "flac", "ogg", "opus", "mp3", "mp3", "mp3"][i % 8]
            path = tmp / f"t{i}.{kind}"
            path.write_bytes(base[kind].read_bytes())
            tags = {"artist": name(rng), "album_artist": name(rng) if rng.random() < 0.5 else None,
                    "composer": name(rng) if rng.random() < 0.5 else None, "album": name(rng) if rng.random() < 0.8 else None}
            if kind == "mp3":
                mode = rng.choice(["mutagen3", "mutagen4", "hand2", "hand3", "hand3_unsync", "hand4", "v1only", "hand4_v1"])
                if mode.startswith("mutagen"):
                    t = mid3.ID3()
                    ver = 3 if mode == "mutagen3" else 4
                    for fid, key in (("TPE1", "artist"), ("TPE2", "album_artist"), ("TCOM", "composer"), ("TALB", "album")):
                        if tags[key]:
                            enc = rng.choice([mid3.Encoding.LATIN1, mid3.Encoding.UTF16, mid3.Encoding.UTF16BE, mid3.Encoding.UTF8]) if ver == 4 else rng.choice([mid3.Encoding.LATIN1, mid3.Encoding.UTF16])
                            txt = [tags[key]] if ver == 3 or rng.random() < 0.7 else [tags[key], name(rng)]
                            try:
                                if enc == mid3.Encoding.LATIN1:
                                    tags[key].encode("latin-1")
                            except UnicodeEncodeError:
                                enc = mid3.Encoding.UTF16
                            t.add(getattr(mid3, fid)(encoding=enc, text=txt))
                    t.add(mid3.TIT2(encoding=3, text=["decoy title"]))
                    t.add(mid3.TCON(encoding=3, text=["Jazz"]))
                    t.save(str(path), v2_version=ver, v1=rng.choice([0, 2]))
                elif mode == "v1only":
                    data = path.read_bytes() + id3v1(tags["artist"] if latin_ok(tags["artist"]) else "x", tags["album"] if tags["album"] and latin_ok(tags["album"]) else "y")
                    path.write_bytes(data)
                else:
                    ver = {"hand2": 2, "hand3": 3, "hand3_unsync": 3, "hand4": 4, "hand4_v1": 4}[mode]
                    frames = [("TIT2", ["decoy"]), ("TCON", ["Rock"])]
                    for fid, key in (("TPE1", "artist"), ("TPE2", "album_artist"), ("TCOM", "composer"), ("TALB", "album")):
                        if tags[key]:
                            texts = [tags[key]] if ver != 4 or rng.random() < 0.6 else [tags[key], name(rng)]
                            frames.append((fid, texts))
                    rng.shuffle(frames)
                    data = id3_tag(ver, frames, rng, unsync=(mode == "hand3_unsync")) + path.read_bytes()
                    if mode == "hand4_v1":
                        data += id3v1("legacy", "legacy album")
                    path.write_bytes(data)
            elif kind == "m4a":
                f = MP4(str(path))
                f["\xa9ART"] = [tags["artist"]]
                if tags["album_artist"]:
                    f["aART"] = [tags["album_artist"]]
                if tags["composer"]:
                    f["\xa9wrt"] = [tags["composer"]]
                if tags["album"]:
                    f["\xa9alb"] = [tags["album"]]
                f["\xa9nam"] = ["decoy"]
                f.save()
            else:
                f = {"flac": FLAC, "ogg": OggVorbis, "opus": OggOpus}[kind](str(path))
                f["artist"] = [tags["artist"]] + ([name(rng)] if rng.random() < 0.3 else [])
                if tags["album_artist"]:
                    f["albumartist"] = [tags["album_artist"]]
                if tags["composer"]:
                    f["composer"] = [tags["composer"]]
                if tags["album"]:
                    f["album"] = [tags["album"]]
                f["title"] = ["decoy"]
                f.save()
            files.append(path)
        out = subprocess.run([args.example, *map(str, files)], capture_output=True, check=True, text=True).stdout
        got = {Path(json.loads(l)["file"]).name: json.loads(l)["facts"] for l in out.splitlines()}
        for p in files:
            want = expected_from_mutagen(p)
            have = got[p.name]
            total += len(want)
            if sorted(map(tuple, want)) != sorted(map(tuple, have)):
                bad += 1
                print(f"DIFF {p.name}: mutagen {want} sniff-rs {have}")
    print("ok" if not bad else f"{bad} differ", f"({args.files} files, {total} facts)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
