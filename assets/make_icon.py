#!/usr/bin/env python3
"""Renders icon_art.txt (32x32 ASCII art) into icon.ico (16/32/48/256),
icon_256.png, icon_256.rgba and icon_32.rgba. Pure stdlib. Run from assets/:  python make_icon.py
Legend: . clear   # background   C mic body   W highlight   S shade   w sound waves"""
import struct, zlib, os
PAL = {'.': (0,0,0,0), '#': (27,31,59,255), 'C': (79,209,255,255),
       'W': (225,248,255,255), 'S': (43,163,214,255), 'w': (120,140,230,255)}
here = os.path.dirname(os.path.abspath(__file__))
art = [l for l in open(os.path.join(here, 'icon_art.txt')).read().split('\n') if l]
assert len(art) == 32 and all(len(l) == 32 for l in art)
base = [[PAL[c] for c in row] for row in art]

def resize(px, n):
    s = len(px)
    out = []
    for y in range(n):
        row = []
        for x in range(n):
            if n >= s and n % s == 0:            # integer upscale: crisp pixels
                row.append(px[y*s//n][x*s//n]); continue
            fx, fy = (x+.5)*s/n-.5, (y+.5)*s/n-.5   # bilinear / area-ish
            x0, y0 = max(int(fx//1),0), max(int(fy//1),0)
            x1, y1 = min(x0+1,s-1), min(y0+1,s-1)
            tx, ty = min(max(fx-x0,0),1), min(max(fy-y0,0),1)
            if n < s:                              # downscale: box average
                k = s//n if s % n == 0 else 2
                cells = [px[min(y*s//n+j,s-1)][min(x*s//n+i,s-1)] for j in range(k) for i in range(k)]
                a = sum(c[3] for c in cells)
                if a == 0: row.append((0,0,0,0)); continue
                row.append(tuple(round(sum(c[ch]*c[3] for c in cells)/a) for ch in range(3)) + (round(a/len(cells)),))
                continue
            cs = [(px[y0][x0],(1-tx)*(1-ty)),(px[y0][x1],tx*(1-ty)),(px[y1][x0],(1-tx)*ty),(px[y1][x1],tx*ty)]
            a = sum(c[3]*w for c,w in cs)
            if a == 0: row.append((0,0,0,0)); continue
            row.append(tuple(round(sum(c[ch]*c[3]*w for c,w in cs)/a) for ch in range(3)) + (round(a),))
        out.append(row)
    return out

def rgba(px): return bytes(v for row in px for p in row for v in p)
def png(px):
    n = len(px)
    raw = b''.join(b'\x00' + bytes(v for p in row for v in p) for row in px)
    def ch(t, d): c = struct.pack('>I', len(d)) + t + d; return c + struct.pack('>I', zlib.crc32(t+d) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + ch(b'IHDR', struct.pack('>IIBBBBB', n, n, 8, 6, 0, 0, 0)) + ch(b'IDAT', zlib.compress(raw, 9)) + ch(b'IEND', b'')

sizes = [16, 32, 48, 256]
imgs = {n: resize(base, n) for n in sizes}
pngs = {n: png(imgs[n]) for n in sizes}
ico = struct.pack('<HHH', 0, 1, len(sizes)); off = 6 + 16*len(sizes); body = b''
for n in sizes:
    ico += struct.pack('<BBBBHHII', n % 256, n % 256, 0, 0, 1, 32, len(pngs[n]), off + len(body)); body += pngs[n]
open(os.path.join(here, 'icon.ico'), 'wb').write(ico + body)
open(os.path.join(here, 'icon_256.png'), 'wb').write(pngs[256])
open(os.path.join(here, 'icon_256.rgba'), 'wb').write(rgba(imgs[256]))
open(os.path.join(here, 'icon_32.rgba'), 'wb').write(rgba(imgs[32]))
print('ok')
