#!/usr/bin/env python3
"""Generate the TUI About screen's half-block app icon.

Reads the desktop app icon's 1024px master (`desktop/assets/icon/
shelbi-1024.png`, the only source of the artwork), crops it to the tile
(dropping the macOS padding and the soft shadow, which only waste cells in a
terminal), box-filters it down to SIZE x SIZE pixels with premultiplied alpha,
and writes `crates/shelbi-tui/src/overlay/about_icon.rs`: one RGBA value per
pixel. The TUI draws two pixels per cell with `▀` (top in the foreground,
bottom in the background), so SIZE pixels square is SIZE columns by SIZE / 2
rows, and blends the alpha over the overlay's fill as it draws.

Standard library only (a small PNG reader), so it runs anywhere:

    python3 scripts/tui-about-icon.py

Rerun it whenever the master changes (see desktop/assets/icon/README.md).
`--preview out.png` also writes the table scaled up, for checking by eye.
"""

import argparse
import os
import struct
import sys
import zlib

SIZE = 20
# Alpha (0-255) a pixel needs to count as the tile when cropping: high enough
# to skip the drop shadow, low enough to keep the tile's anti-aliased rim.
TILE_ALPHA = 160

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MASTER = os.path.join(ROOT, "desktop", "assets", "icon", "shelbi-1024.png")
OUT = os.path.join(ROOT, "crates", "shelbi-tui", "src", "overlay", "about_icon.rs")


def read_png(path):
    """Decode an 8-bit RGBA, non-interlaced PNG to rows of (r, g, b, a)."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"{path}: not a PNG")
    pos, idat, width, height = 8, b"", 0, 0
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if (depth, color, interlace) != (8, 6, 0):
                sys.exit(f"{path}: want 8-bit RGBA, non-interlaced")
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * 4
    rows, prev, at = [], bytearray(stride), 0
    for _ in range(height):
        kind = raw[at]
        line = bytearray(raw[at + 1 : at + 1 + stride])
        at += 1 + stride
        for i in range(stride):
            a = line[i - 4] if i >= 4 else 0
            b = prev[i]
            c = prev[i - 4] if i >= 4 else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        rows.append(line)
        prev = line
    return width, height, rows


def tile_box(width, height, rows):
    """The tile's bounding box, squared up around its center."""
    xs, ys = [], []
    for y in range(height):
        row = rows[y]
        for x in range(width):
            if row[x * 4 + 3] >= TILE_ALPHA:
                xs.append(x)
                ys.append(y)
    left, right, top, bottom = min(xs), max(xs) + 1, min(ys), max(ys) + 1
    side = max(right - left, bottom - top)
    cx, cy = (left + right) / 2, (top + bottom) / 2
    return cx - side / 2, cy - side / 2, side


def downsample(rows, x0, y0, side, size):
    """Area-average the `side`-square at (x0, y0) to `size` x `size`, in
    premultiplied alpha so transparent pixels don't darken the rim."""
    step = side / size
    out = []
    for py in range(size):
        line = []
        for px in range(size):
            fx0, fy0 = x0 + px * step, y0 + py * step
            fx1, fy1 = fx0 + step, fy0 + step
            acc = [0.0, 0.0, 0.0, 0.0]
            total = 0.0
            for y in range(int(fy0), int(-(-fy1 // 1))):
                wy = min(fy1, y + 1) - max(fy0, y)
                if wy <= 0:
                    continue
                row = rows[y]
                for x in range(int(fx0), int(-(-fx1 // 1))):
                    wx = min(fx1, x + 1) - max(fx0, x)
                    if wx <= 0:
                        continue
                    w = wx * wy
                    r, g, b, a = row[x * 4 : x * 4 + 4]
                    al = a / 255
                    acc[0] += r * al * w
                    acc[1] += g * al * w
                    acc[2] += b * al * w
                    acc[3] += al * w
                    total += w
            alpha = acc[3] / total
            if alpha <= 0:
                line.append((0, 0, 0, 0))
                continue
            line.append(
                tuple(round(acc[i] / acc[3]) for i in range(3)) + (round(alpha * 255),)
            )
        out.append(line)
    return out


def write_rust(pixels, size):
    lines = [
        "//! The app icon as half-block pixel art, for the About screen: generated",
        "//! by `scripts/tui-about-icon.py` from the desktop app icon's master",
        "//! (`desktop/assets/icon/shelbi-1024.png`), cropped to the tile and",
        f"//! box-filtered to {size} x {size}. Regenerate it, never edit it by hand.",
        "",
        "/// The icon's width and height, in pixels (cells are one pixel wide and",
        "/// two tall).",
        f"pub const SIZE: usize = {size};",
        "",
        "/// Row-major `[r, g, b, a]`, straight (not premultiplied) alpha.",
        "#[rustfmt::skip]",
        f"pub const PIXELS: [[u8; 4]; SIZE * SIZE] = [",
    ]
    for row in pixels:
        cells = " ".join(f"[{r},{g},{b},{a}]," for r, g, b, a in row)
        lines.append(f"    {cells}")
    lines.append("];")
    open(OUT, "w").write("\n".join(lines) + "\n")


def write_preview(path, pixels, scale=16, bg=(0x29, 0x29, 0x29)):
    """The table scaled up over `bg` (the dark theme's overlay fill)."""
    size = len(pixels)
    raw = bytearray()
    for row in pixels:
        line = bytearray([0])
        for r, g, b, a in row:
            al = a / 255
            px = bytes(round(c * al + k * (1 - al)) for c, k in zip((r, g, b), bg))
            line += px * scale
        raw += bytes(line) * scale
    w = h = size * scale

    def chunk(kind, body):
        crc = zlib.crc32(kind + body) & 0xFFFFFFFF
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", crc)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    open(path, "wb").write(png)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--size", type=int, default=SIZE)
    parser.add_argument("--preview", help="also write the table scaled up to this PNG")
    parser.add_argument("--no-write", action="store_true", help="don't write the Rust table")
    args = parser.parse_args()
    if args.size % 2:
        sys.exit("--size must be even: a cell holds two pixels")
    width, height, rows = read_png(MASTER)
    x0, y0, side = tile_box(width, height, rows)
    pixels = downsample(rows, x0, y0, side, args.size)
    if not args.no_write:
        write_rust(pixels, args.size)
    if args.preview:
        write_preview(args.preview, pixels)


if __name__ == "__main__":
    main()
