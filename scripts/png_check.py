"""Check a frame PNG written by pcbcraft-render-server --snapshot: size, not uniform, and enough
distinct colours overall and in a top band (the ribbon) and a centre box (the canvas).
Usage: python png_check.py <png> <width> <height> [min_colours]. Stdlib only (8-bit RGBA/RGB, no interlace)."""
import struct
import sys
import zlib


def decode(path):
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos, idat, header = 8, [], None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
    w, h, depth, color, _, _, interlace = header
    channels = {2: 3, 6: 4}.get(color)
    if depth != 8 or channels is None or interlace:
        raise SystemExit(f"{path}: unsupported PNG (depth {depth}, colour type {color}, interlace {interlace})")
    raw = zlib.decompress(b"".join(idat))
    stride = w * channels
    rows, prev = [], bytearray(stride)
    for y in range(h):
        base = y * (stride + 1)
        filt, line = raw[base], bytearray(raw[base + 1:base + 1 + stride])
        for i in range(stride):
            left = line[i - channels] if i >= channels else 0
            up = prev[i]
            upleft = prev[i - channels] if i >= channels else 0
            if filt == 1:
                line[i] = (line[i] + left) & 255
            elif filt == 2:
                line[i] = (line[i] + up) & 255
            elif filt == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif filt == 4:
                p = left + up - upleft
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - upleft)
                pred = left if pa <= pb and pa <= pc else (up if pb <= pc else upleft)
                line[i] = (line[i] + pred) & 255
        rows.append(line)
        prev = line
    return w, h, channels, rows


def colours(rows, channels, x0, x1, y0, y1):
    seen = set()
    for y in range(y0, y1):
        row = rows[y]
        for x in range(x0, x1):
            seen.add(bytes(row[x * channels:x * channels + 3]))
    return len(seen)


def main():
    path, w, h = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    minimum = int(sys.argv[4]) if len(sys.argv) > 4 else 100
    gw, gh, channels, rows = decode(path)
    if (gw, gh) != (w, h):
        raise SystemExit(f"{path}: {gw}x{gh}, expected {w}x{h}")
    total = colours(rows, channels, 0, gw, 0, gh)
    top = colours(rows, channels, 0, gw, 0, gh // 4)
    centre = colours(rows, channels, gw // 4, 3 * gw // 4, gh // 3, 5 * gh // 6)
    print(f"{path}: {gw}x{gh}, distinct colours total {total}, top band {top}, centre box {centre}")
    if total < minimum or top < 10 or centre < 10:
        raise SystemExit(f"{path}: looks empty or uniform (needs total >= {minimum}, top >= 10, centre >= 10)")


main()
