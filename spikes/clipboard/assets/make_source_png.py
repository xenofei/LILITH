"""Writes source.png deterministically: 256x256 RGBA, stdlib only.
R = x, G = y, B = 255 - (x+y)/2 (a colour gradient so a swapped channel is obvious);
alpha 255 everywhere except a half-transparent disc (alpha 96) centred at (128,128) r=56 and a
fully transparent 32x32 top-right corner (alpha 0).
Usage: python -I make_source_png.py <out.png>"""
import struct, sys, zlib

W = H = 256

def pixel(x, y):
    r, g, b = x, y, 255 - (x + y) // 2
    a = 255
    if (x - 128) ** 2 + (y - 128) ** 2 <= 56 * 56:
        a = 96
    if x >= W - 32 and y < 32:
        a = 0
    return bytes((r, g, b, a))

def row(y):
    px = [pixel(x, y) for x in range(W)]
    out = bytearray(b"")  # PNG filter 1 (Sub): keeps the gradient compressible
    for x in range(W):
        left = px[x - 1] if x else bytes(4)
        out += bytes((px[x][k] - left[k]) & 255 for k in range(4))
    return bytes(out)

raw = b"".join(row(y) for y in range(H))

def chunk(tag, data):
    c = struct.pack(">I", len(data)) + tag + data
    return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(raw, 9))
       + chunk(b"IEND", b""))
open(sys.argv[1], "wb").write(png)
