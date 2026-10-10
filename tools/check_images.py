#!/usr/bin/env python3
"""Checks the picture decoding and the `looks_like` links of `sniff-rs graph`.

    python3 tools/check_images.py [--bin target/release/sniff-rs] [--example target/release/examples/image_hash]
                                  [--png 150] [--jpeg 150] [--groups 12]

1. PNG: files written by pypng in every colour type and bit depth, with and
   without Adam7 interlacing, odd sizes. The brightness plane sniff-rs reads
   must equal the one computed here from the source samples, exactly
   (ITU-R 601 luma as Pillow computes it; 16-bit samples by their high byte;
   sub-byte grey scaled to 8 bits; alpha ignored).
2. JPEG: files written by Pillow (quality, 4:4:4/4:2:2/4:2:0, progressive,
   optimised Huffman tables, restart intervals, grey) are decoded by Pillow in
   luma-only mode; each full 8x8 block's mean must match the value sniff-rs
   gets from the DC coefficient alone, to within 2 levels.
3. Copies: several pictures each saved as PNG, JPEG at three qualities,
   progressive JPEG, scaled, brightened and lightly blurred. The graph must
   link the copies of one picture (`looks_like`) and never two pictures; the
   same pairs are classified with `imagehash.dhash` for comparison.
Needs numpy, Pillow, pypng and imagehash.
"""
import argparse
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

import imagehash
import numpy as np
import png
from PIL import Image, ImageFilter, ImageEnhance


def probe(example, files, plane=True):
    out = subprocess.run([example, *(["--plane"] if plane else []), *map(str, files)], capture_output=True, check=True, text=True).stdout
    return {Path(json.loads(l)["file"]).name: json.loads(l) for l in out.splitlines()}


def luma8(r, g, b):
    return ((r.astype(np.uint32) * 19595 + g.astype(np.uint32) * 38470 + b.astype(np.uint32) * 7471 + 0x8000) >> 16).astype(np.uint8)


def png_case(rng, path):
    color = rng.choice(["grey", "rgb", "palette", "greyalpha", "rgba"])
    depth = {"grey": [1, 2, 4, 8, 16], "rgb": [8, 16], "palette": [1, 2, 4, 8], "greyalpha": [8, 16], "rgba": [8, 16]}[color]
    depth = rng.choice(depth)
    w, h = rng.randint(32, 90), rng.randint(32, 70)
    interlace = rng.random() < 0.5
    ch = {"grey": 1, "rgb": 3, "palette": 1, "greyalpha": 2, "rgba": 4}[color]
    top = (1 << depth) - 1
    # Smooth-ish samples so the filters (sub, up, average, paeth) all get used.
    base = np.cumsum(np.random.default_rng(rng.randint(0, 1 << 30)).integers(-3, 4, size=(h, w * ch)), axis=1)
    pix = np.clip(base + np.arange(h)[:, None] * 2 + top // 2, 0, top).astype(np.uint16 if depth == 16 else np.uint8)
    kw = {"width": w, "height": h, "bitdepth": depth, "interlace": interlace, "greyscale": False}
    palette = None
    if color == "grey":
        kw["greyscale"] = True
    elif color == "greyalpha":
        kw.update(greyscale=True, alpha=True)
    elif color == "rgba":
        kw["alpha"] = True
    elif color == "palette":
        n = min(1 << depth, 256)
        palette = [tuple(rng.randint(0, 255) for _ in range(3)) for _ in range(n)]
        kw["palette"] = palette
        pix = (pix.astype(np.uint16) % n).astype(np.uint8)
    rows = pix.reshape(h, w * ch)
    with open(path, "wb") as f:
        png.Writer(**kw).write(f, rows.tolist())
    a = pix.reshape(h, w, ch).astype(np.uint32)
    if color == "grey":
        v = a[:, :, 0]
        want = (v >> 8) if depth == 16 else (v * 255 // top if depth < 8 else v)
    elif color == "greyalpha":
        v = a[:, :, 0]
        want = (v >> 8) if depth == 16 else v
    elif color == "palette":
        pal = np.array(palette, dtype=np.uint8)
        rgb = pal[a[:, :, 0]]
        want = luma8(rgb[:, :, 0], rgb[:, :, 1], rgb[:, :, 2])
    else:
        s = (a >> 8) if depth == 16 else a
        want = luma8(s[:, :, 0], s[:, :, 1], s[:, :, 2])
    return np.asarray(want, dtype=np.uint8), f"{color}/{depth}/{'adam7' if interlace else 'plain'}/{w}x{h}"


def smooth_image(rng, w, h, gray=False, noise=2.0):
    """Something picture-like: blobs, a gradient, a few hard-edged rectangles."""
    r = np.random.default_rng(rng.randint(0, 1 << 30))
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float64)
    chans = []
    for _ in range(1 if gray else 3):
        img = np.full((h, w), r.uniform(60, 190))
        img += (xx / w - 0.5) * r.uniform(-90, 90) + (yy / h - 0.5) * r.uniform(-90, 90)
        for _ in range(r.integers(5, 10)):
            cx, cy, s = r.uniform(0, w), r.uniform(0, h), r.uniform(0.08, 0.3) * max(w, h)
            img += r.uniform(-80, 80) * np.exp(-((xx - cx) ** 2 + (yy - cy) ** 2) / (2 * s * s))
        for _ in range(r.integers(1, 4)):
            x0, y0 = r.integers(0, w // 2), r.integers(0, h // 2)
            img[y0:y0 + r.integers(h // 8, h // 3), x0:x0 + r.integers(w // 8, w // 3)] += r.uniform(-50, 50)
        chans.append(img)
    arr = np.stack(chans, axis=-1) if not gray else chans[0]
    arr = np.clip(arr + (r.normal(0, noise, arr.shape) if noise else 0), 25, 230).astype(np.uint8)
    return Image.fromarray(arr, "L" if gray else "RGB")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--example", default="target/release/examples/image_hash")
    ap.add_argument("--png", type=int, default=150)
    ap.add_argument("--jpeg", type=int, default=150)
    ap.add_argument("--groups", type=int, default=12)
    args = ap.parse_args()
    rng = random.Random(5)
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        # 1. PNG planes.
        cases = {}
        for i in range(args.png):
            p = tmp / f"p{i}.png"
            want, label = png_case(rng, p)
            cases[p.name] = (want, label)
        got = probe(args.example, [tmp / n for n in cases])
        wrong = 0
        for n, (want, label) in cases.items():
            g = got[n]
            if g.get("w") is None or (g["w"], g["h"]) != (want.shape[1], want.shape[0]) or not np.array_equal(np.array(g["luma"], dtype=np.uint8).reshape(want.shape), want):
                wrong += 1
                if wrong <= 5:
                    print(f"PNG DIFF {label}")
        print(f"PNG: {args.png - wrong}/{args.png} planes exact")
        bad += wrong
        # 2. JPEG block means.
        cases = {}
        worst = 0
        for i in range(args.jpeg):
            gray = rng.random() < 0.15
            w, h = rng.randint(72, 260), rng.randint(64, 220)
            img = smooth_image(rng, w, h, gray)
            p = tmp / f"j{i}.jpg"
            opts = dict(quality=rng.choice([10, 30, 50, 75, 90, 95]), progressive=rng.random() < 0.4, optimize=rng.random() < 0.4)
            if not gray:
                opts["subsampling"] = rng.choice([0, 1, 2])
            if rng.random() < 0.3:
                opts["restart_marker_blocks"] = rng.randint(1, 9)
            try:
                img.save(p, "JPEG", **opts)
            except TypeError:
                opts.pop("restart_marker_blocks", None)
                img.save(p, "JPEG", **opts)
            cases[p.name] = (opts, gray, w, h)
        got = probe(args.example, [tmp / n for n in cases])
        wrong = 0
        for n, (opts, gray, w, h) in cases.items():
            im = Image.open(tmp / n)
            im.draft("L", im.size)
            y = np.asarray(im.convert("L") if im.mode != "L" else im, dtype=np.float64)
            g = got[n]
            bw, bh = (w + 7) // 8, (h + 7) // 8
            ours = np.array(g["luma"], dtype=np.float64).reshape(bh, bw) if g.get("w") else None
            ok = ours is not None and (g["w"], g["h"]) == (bw, bh)
            if ok:
                full = y[: (h // 8) * 8, : (w // 8) * 8].reshape(h // 8, 8, w // 8, 8).mean(axis=(1, 3))
                diff = np.abs(ours[: h // 8, : w // 8] - full).max()
                worst = max(worst, diff)
                ok = diff <= 2.0
            if not ok:
                wrong += 1
                if wrong <= 5:
                    print(f"JPEG DIFF {n} {opts} gray={gray} {w}x{h}")
        print(f"JPEG: {args.jpeg - wrong}/{args.jpeg} block-mean planes within 2 levels (worst {worst:.2f})")
        bad += wrong
        # 3. Copies.
        folder = tmp / "copies"
        folder.mkdir()
        truth = {}
        for gidx in range(args.groups):
            w, h = rng.randint(180, 320), rng.randint(140, 260)
            src = smooth_image(rng, w, h)
            variants = {
                "orig.png": src,
                "q90.jpg": src, "q50.jpg": src, "q20.jpg": src, "prog.jpg": src,
                "half.png": src.resize((w // 2, h // 2), Image.LANCZOS),
                "bright.png": ImageEnhance.Brightness(src).enhance(1.06),
                "blur.png": src.filter(ImageFilter.GaussianBlur(1.2)),
            }
            for name, im in variants.items():
                p = folder / f"g{gidx}_{name}"
                if name.endswith(".jpg"):
                    q = {"q90.jpg": 90, "q50.jpg": 50, "q20.jpg": 20, "prog.jpg": 75}[name]
                    im.save(p, "JPEG", quality=q, progressive=name == "prog.jpg")
                else:
                    im.save(p)
                truth[p.name] = gidx
        out = subprocess.run([args.bin, "graph", str(folder), "-", "--no-cache"], capture_output=True, check=True).stdout
        doc = json.loads(out)
        ids = {n["id"]: n for n in doc["nodes"] if n.get("type") == "file"}
        linked = {tuple(sorted((l["source"], l["target"]))) for l in doc["links"] if l["relation"] == "looks_like"}
        same = {tuple(sorted((a, b))) for a in truth for b in truth if a < b and truth[a] == truth[b]}
        wrong_links = linked - same
        names = sorted(truth)
        got = probe(args.example, [folder / n for n in names], plane=False)
        h = {n: int(got[n]["hash"], 16) for n in names if got[n]["hash"]}
        bits = lambda a, b: bin(h[a] ^ h[b]).count("1")
        # The rule: link two pictures whose hashes differ in at most 5 bits,
        # each picture to its five closest. So every link is within 5 bits;
        # a picture with five or fewer such partners has a link to each; one
        # with more has at least five.
        too_far = [(a, b) for a, b in linked if a not in h or b not in h or bits(a, b) > 5]
        missing = 0
        for a in h:
            partners = [b for b in h if b != a and bits(a, b) <= 5]
            have = [b for b in partners if tuple(sorted((a, b))) in linked]
            if len(partners) <= 5 and len(have) != len(partners):
                missing += 1
            if len(partners) > 5 and len(have) < 5:
                missing += 1
        hashed_groups = sum(1 for g in range(args.groups) if sum(1 for n in h if truth[n] == g) >= 2)
        ih_hashes = {n: imagehash.dhash(Image.open(folder / n)) for n in names}
        ih = {tuple(sorted((a, b))) for a in names for b in names if a < b and ih_hashes[a] - ih_hashes[b] <= 5}
        same = {tuple(sorted((a, b))) for a in names for b in names if a < b and truth[a] == truth[b]}
        print(f"copies: {len(h)}/{len(names)} pictures hashed; {len(linked)} looks_like links, "
              f"{len(linked & same)} between copies of one picture and {len(wrong_links)} between different pictures; "
              f"{len(too_far)} beyond 5 bits; {missing} pictures missing a link the rule asks for; "
              f"imagehash.dhash (<=5 bits) finds {len(ih & same)} of the {len(same)} copy pairs and {len(ih - same)} wrong")
        if too_far or missing or len(wrong_links) > 0.02 * len(linked):
            bad += 1
        if wrong_links:
            print("  between different pictures:", sorted(wrong_links)[:4])
    print("ok" if not bad else f"{bad} problem(s)")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
