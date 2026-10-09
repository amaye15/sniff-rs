#!/usr/bin/env python3
"""Writes small Outlook .msg files (an OLE2 container of MAPI properties).

    python3 tools/make_msg.py OUT_DIR

The OLE2 writer is minimal but follows [MS-CFB]: version 3 (512-byte
sectors), a FAT, a mini FAT for streams under 4096 bytes, and a directory
whose sibling trees are balanced and coloured. olefile and extract_msg read
the output (tools/check_msg.py), which is the check that it is real OLE2.
"""
import struct
import sys
from pathlib import Path

SECTOR = 512
MINI = 64
CUTOFF = 4096
FREE, END, FATSECT = 0xFFFFFFFF, 0xFFFFFFFE, 0xFFFFFFFD


def utf16(s):
    return s.encode("utf-16-le")


class Node:
    def __init__(self, name, kind, data=b""):
        self.name, self.kind, self.data = name, kind, data
        self.children = []
        self.start = END
        self.left = self.right = self.child = FREE
        self.red = False
        self.id = None


def cfb_key(n):
    return (len(n.name), n.name.upper())


def link_tree(nodes, depth, deepest, full):
    """Balanced BST over `nodes` (sorted); returns the root. Nodes on the
    deepest level of an incomplete tree are red, which keeps it a valid
    red-black tree."""
    if not nodes:
        return None
    mid = len(nodes) // 2
    root = nodes[mid]
    left = link_tree(nodes[:mid], depth + 1, deepest, full)
    right = link_tree(nodes[mid + 1:], depth + 1, deepest, full)
    root.left = left.id if left else FREE
    root.right = right.id if right else FREE
    root.red = (not full) and depth == deepest and depth > 0
    return root


def write_cfb(root):
    # Collect nodes depth-first and give each a directory id.
    order = [root]
    def walk(n):
        n.children.sort(key=cfb_key)
        for c in n.children:
            order.append(c)
        for c in n.children:
            walk(c)
    walk(root)
    for i, n in enumerate(order):
        n.id = i
    # Sibling trees (children of each storage).
    def tree(n):
        kids = n.children
        if kids:
            count = len(kids)
            height = count.bit_length()
            full = (count + 1) & count == 0
            top = link_tree(kids, 0, height - 1, full)
            n.child = top.id
        for c in kids:
            tree(c)
    tree(root)
    # Streams: small ones in the mini stream, large ones in regular sectors.
    mini = bytearray()
    minifat = []
    big = []
    for n in order:
        if n.kind != 2:
            continue
        if len(n.data) < CUTOFF:
            if not n.data:
                n.start = END
                continue
            n.start = len(minifat)
            sectors = (len(n.data) + MINI - 1) // MINI
            for k in range(sectors):
                minifat.append(len(minifat) + 1 if k < sectors - 1 else END)
            mini += n.data + b"\0" * (sectors * MINI - len(n.data))
        else:
            big.append(n)
    # Lay out sectors: [directory][minifat][mini stream][big streams][FAT].
    sectors = []  # list of bytes objects, each SECTOR long
    def alloc(data):
        start = len(sectors)
        for off in range(0, max(len(data), 1), SECTOR):
            chunk = data[off:off + SECTOR]
            sectors.append(chunk + b"\0" * (SECTOR - len(chunk)))
        return start, (len(data) + SECTOR - 1) // SECTOR
    entries = bytearray()
    for _ in order:
        entries += b"\0" * 128
    dir_sectors = (len(order) * 128 + SECTOR - 1) // SECTOR
    dir_start = len(sectors)
    for _ in range(dir_sectors):
        sectors.append(b"\0" * SECTOR)
    minifat_bytes = b"".join(struct.pack("<I", v) for v in minifat)
    mf_start, mf_n = alloc(minifat_bytes) if minifat_bytes else (END, 0)
    ms_start, ms_n = alloc(bytes(mini)) if mini else (END, 0)
    big_starts = {}
    for n in big:
        start, cnt = alloc(n.data)
        n.start = start
        big_starts[n.id] = (start, cnt)
    root.start = ms_start
    root.data_len = len(mini)
    # FAT chains.
    fat = {}
    def chain(start, count):
        for k in range(count):
            fat[start + k] = start + k + 1 if k < count - 1 else END
    chain(dir_start, dir_sectors)
    if mf_n:
        chain(mf_start, mf_n)
    if ms_n:
        chain(ms_start, ms_n)
    for start, cnt in big_starts.values():
        chain(start, cnt)
    # FAT sectors themselves.
    per = SECTOR // 4
    fat_count = 1
    while True:
        total = len(sectors) + fat_count
        if fat_count * per >= total:
            break
        fat_count += 1
    fat_start = len(sectors)
    for k in range(fat_count):
        fat[fat_start + k] = FATSECT
        sectors.append(b"\0" * SECTOR)
    table = [fat.get(i, FREE) for i in range(fat_count * per)]
    for k in range(fat_count):
        sectors[fat_start + k] = b"".join(
            struct.pack("<I", v) for v in table[k * per:(k + 1) * per]
        )
    # Directory entries.
    for n in order:
        name = n.name.encode("utf-16-le") + b"\0\0"
        typ = {"root": 5, 1: 1, 2: 2}[n.kind if n is not root else "root"]
        size = n.data_len if n is root else (len(n.data) if n.kind == 2 else 0)
        rec = (
            name.ljust(64, b"\0")
            + struct.pack("<H", len(name))
            + bytes([typ, 0 if n.red is False else 0])
        )
        rec = (
            name.ljust(64, b"\0")
            + struct.pack("<H", len(name))
            + bytes([typ, 0 if n.red else 1])
            + struct.pack("<III", n.left, n.right, n.child)
            + b"\0" * 16  # clsid
            + b"\0" * 4  # state bits
            + b"\0" * 16  # create + modify times
            + struct.pack("<I", n.start if n.start != END or n.kind == 2 else END)
            + struct.pack("<Q", size)
        )
        assert len(rec) == 128, len(rec)
        entries[n.id * 128:(n.id + 1) * 128] = rec
    # Unused trailing entries in the last directory sector are free (type 0,
    # sibling links FREE); mark them.
    for i in range(len(order), dir_sectors * SECTOR // 128):
        entries += b"\0" * 128
        entries[i * 128 + 68:i * 128 + 80] = struct.pack("<III", FREE, FREE, FREE)
    for k in range(dir_sectors):
        sectors[dir_start + k] = bytes(entries[k * SECTOR:(k + 1) * SECTOR])
    header = bytearray(512)
    header[0:8] = bytes([0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
    struct.pack_into("<HHHHH", header, 24, 0x003E, 0x0003, 0xFFFE, 9, 6)
    struct.pack_into("<I", header, 40, 0)  # directory sectors (v3: 0)
    struct.pack_into("<I", header, 44, fat_count)
    struct.pack_into("<I", header, 48, dir_start)
    struct.pack_into("<I", header, 56, CUTOFF)
    struct.pack_into("<I", header, 60, mf_start if mf_n else END)
    struct.pack_into("<I", header, 64, mf_n)
    struct.pack_into("<I", header, 68, END)  # first DIFAT sector
    struct.pack_into("<I", header, 72, 0)
    for k in range(109):
        struct.pack_into("<I", header, 76 + 4 * k, fat_start + k if k < fat_count else FREE)
    assert fat_count <= 109
    return bytes(header) + b"".join(sectors)


def prop_stream(entries, header_len):
    """`__properties_version1.0`: a fixed head then 16-byte records."""
    head = bytearray(header_len)
    body = b"".join(
        struct.pack("<II", (pid << 16) | ty, 6) + value.ljust(8, b"\0")
        for pid, ty, value in entries
    )
    return bytes(head) + body


def filetime(unix):
    return struct.pack("<Q", (unix + 11644473600) * 10_000_000)


def string_stream(pid, text, ansi=False):
    if ansi:
        return Node(f"__substg1.0_{pid:04X}001E", 2, text.encode("cp1252") + b"\0")
    return Node(f"__substg1.0_{pid:04X}001F", 2, utf16(text) + b"\0\0")


def make_msg(m):
    """`m`: dict with subject, body, from_name, from_addr, to/cc/bcc as lists
    of (name, addr), date (unix seconds), message_id, ansi (bool)."""
    ansi = m.get("ansi", False)
    root = Node("Root Entry", 1)
    props = []
    if "date" in m:
        props.append((0x0039, 0x0040, filetime(m["date"])))
    root.children.append(Node("__properties_version1.0", 2, prop_stream(props, 32)))
    for pid, key in ((0x0037, "subject"), (0x1000, "body"), (0x0C1A, "from_name"),
                     (0x5D01, "from_addr"), (0x1035, "message_id")):
        if m.get(key) is not None:
            root.children.append(string_stream(pid, m[key], ansi))
    if m.get("from_addr_ex") is not None:
        root.children.append(string_stream(0x0C1E, "EX", ansi))
        root.children.append(string_stream(0x0C1F, m["from_addr_ex"], ansi))
    index = 0
    for kind, code in (("to", 1), ("cc", 2), ("bcc", 3)):
        for name, addr in m.get(kind, []):
            r = Node(f"__recip_version1.0_#{index:08X}", 1)
            r.children.append(Node(
                "__properties_version1.0", 2,
                prop_stream([(0x0C15, 0x0003, struct.pack("<i", code))], 8)))
            r.children.append(string_stream(0x3001, name, ansi))
            if addr is not None:
                r.children.append(string_stream(0x39FE, addr, ansi))
            root.children.append(r)
            index += 1
    return write_cfb(root)


MESSAGES = {
    "edge_msg_basic.msg": dict(
        subject="Quarterly numbers", body="Hi Tom,\r\nThe figures are attached.\r\n\r\nFrom Jane",
        from_name="Jane Smith", from_addr="jane.smith@acme-corp.com",
        to=[("Tom Brown", "tom@acme-corp.com"), ("Pat Lee", "pat@example.org")],
        cc=[("Ann Ng", "ann@example.org")], bcc=[("Audit", "audit@acme-corp.com")],
        date=1767268800, message_id="<q3-numbers@acme-corp.com>"),
    "edge_msg_unicode.msg": dict(
        subject="Réunion — 会議 ✓", body="Bonjour Jürgen,\r\nété 会議\r\n",
        from_name="Müller, Jürgen", from_addr="jmuller@uni.edu.au",
        to=[("Zoë O'Brien", "zoe@example.org"), ("No Address", None)],
        date=1700000000),
    "edge_msg_ansi_exchange.msg": dict(
        subject="Legacy ANSI message", body="café at noon\r\n", ansi=True,
        from_name="Old Sender", from_addr=None, from_addr_ex="/O=ORG/OU=EX/CN=RECIPIENTS/CN=OLDSENDER",
        to=[("Reader One", "reader1@example.org")], date=1262304000),
}

if __name__ == "__main__":
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name, m in MESSAGES.items():
        (out / name).write_bytes(make_msg(m))
        print("wrote", out / name)
