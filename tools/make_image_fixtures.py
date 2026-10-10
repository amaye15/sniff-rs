#!/usr/bin/env python3
"""Writes tests/fixtures/edge_graph_images: copies of one picture and some that are not.

    python3 tools/make_image_fixtures.py [--out tests/fixtures/edge_graph_images]

scene.png and its copies (a JPEG at quality 75, a progressive JPEG, a half-size
PNG, an Adam7-interlaced PNG written by pypng, a byte-identical copy) must be
linked by `looks_like`; other.png (a different picture), icon.png (too small)
and flat.png (no detail) must not. Needs numpy, Pillow and pypng.
"""
import argparse
import random
import shutil
from pathlib import Path

import imagehash
import numpy as np
import png
from PIL import Image

import check_images as ci


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/fixtures/edge_graph_images")
    out = Path(ap.parse_args().out)
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(404)
    # A picture with enough detail for a hash that says something: about half
    # its 64 bits set, and a second picture far from it.
    for _ in range(500):
        scene = ci.smooth_image(rng, 192, 144, noise=0.0)
        other = ci.smooth_image(rng, 192, 144, noise=0.0)
        a, b = imagehash.dhash(scene), imagehash.dhash(other)
        ones = sum(bin(int(x)).count("1") for x in a.hash.flatten())
        if 24 <= ones <= 40 and a - b >= 20:
            break
    scene.save(out / "scene.png", optimize=True)
    scene.save(out / "scene_q75.jpg", "JPEG", quality=75)
    scene.save(out / "scene_progressive.jpg", "JPEG", quality=60, progressive=True)
    scene.resize((96, 72), Image.LANCZOS).save(out / "scene_half.png", optimize=True)
    rows = np.asarray(scene).reshape(144, 192 * 3).tolist()
    with open(out / "scene_adam7.png", "wb") as f:
        png.Writer(width=192, height=144, bitdepth=8, greyscale=False, interlace=True).write(f, rows)
    shutil.copyfile(out / "scene.png", out / "scene_copy.png")
    other.save(out / "other.png", optimize=True)
    scene.resize((24, 18), Image.LANCZOS).save(out / "icon.png")
    Image.new("RGB", (160, 120), (200, 200, 200)).save(out / "flat.png")
    for f in sorted(out.iterdir()):
        print(f.name, f.stat().st_size)


if __name__ == "__main__":
    main()
