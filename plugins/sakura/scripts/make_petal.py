#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Generates the sakura petal asset (assets/petal.png) procedurally — no
# external image library, just the Python stdlib (zlib + struct). Run once;
# commit the resulting PNG. Regenerate to retune the petal's shape/colour.
#
#   python3 scripts/make_petal.py
#
# Output: a SIZE x SIZE RGBA PNG. The petal is a soft pink teardrop with a
# small notch at the wide end (the sakura signature), anti-aliased via
# supersampling. The plugin can tint it, but it's authored already-pink so
# it looks right untinted too.

import math
import struct
import zlib
import os

SIZE = 96          # output edge length in px
SS = 4             # supersample factor for anti-aliasing (renders SIZE*SS)
BASE = (255, 218, 233)   # petal pink (top/centre, brightest)
EDGE = (240, 170, 198)   # slightly deeper pink toward the rim


def petal_alpha(u, v):
    """Coverage in [0,1] for normalised petal coords.

    u in [-1,1] is horizontal (0 = centre line), v in [-1,1] is vertical
    with v=-1 the pointed base (stem) and v=+1 the wide notched tip. The
    silhouette is a teardrop: a sharp point at the base that swells to a
    rounded, wide tip, with a deep V-notch cut into the tip (the sakura
    signature).
    """
    if v < -1.0 or v > 1.0:
        return 0.0

    t = (v + 1.0) / 2.0   # 0 at base (stem), 1 at tip

    # Half-width profile: a teardrop. Near-zero at the base, growing to
    # widest around t~0.7, then easing in slightly toward the tip so the
    # top is rounded rather than blunt. sqrt-ish rise gives the pointed
    # stem; the cap term rounds the top.
    rise = t ** 0.6                       # pointed at base, fast swell
    cap = math.sin(math.pi * min(t, 1.0)) ** 0.35   # round the tip
    width = 0.95 * rise * (0.55 + 0.45 * cap)
    # Pull the widest point above centre for the classic petal silhouette.
    width *= 0.85 + 0.15 * math.sin(math.pi * t)
    if width <= 1e-4:
        return 0.0

    edge = width - abs(u)                 # >0 inside, signed distance-ish
    if edge <= 0.0:
        return 0.0

    # Shallow V-notch at the very tip: a small cleft, the sakura cue — not
    # a deep heart cleavage. Only the top ~8% carves in, and the wedge is
    # narrow so the two tip lobes stay broad and close together.
    if t > 0.92:
        nt = (t - 0.92) / 0.08            # 0 at notch start, 1 at very top
        wedge = 0.16 * nt                 # narrow removed wedge
        edge = min(edge, abs(u) - wedge)
        if edge <= 0.0:
            return 0.0

    # Feather the rim over a fixed band for a clean, airy edge.
    return min(1.0, edge / 0.08)


def main():
    n = SIZE * SS
    # Render at supersampled resolution, then box-downsample to SIZE.
    hi = bytearray(4 * n * n)
    for py in range(n):
        # Map pixel row to v; image row 0 is top -> tip (v=+1).
        v = 1.0 - 2.0 * (py + 0.5) / n
        for px in range(n):
            u = 2.0 * (px + 0.5) / n - 1.0
            a = petal_alpha(u, v)
            if a <= 0.0:
                continue
            # Colour: blend BASE->EDGE by distance from the centre line, and
            # darken slightly toward the base for a touch of depth.
            rim = min(1.0, abs(u))
            r = int(BASE[0] * (1 - rim) + EDGE[0] * rim)
            g = int(BASE[1] * (1 - rim) + EDGE[1] * rim)
            b = int(BASE[2] * (1 - rim) + EDGE[2] * rim)
            o = 4 * (py * n + px)
            hi[o] = r
            hi[o + 1] = g
            hi[o + 2] = b
            hi[o + 3] = int(a * 255)

    # Downsample SS x SS boxes -> final RGBA (straight alpha; the plugin
    # premultiplies in-shader like the other plugins).
    out = bytearray(4 * SIZE * SIZE)
    for y in range(SIZE):
        for x in range(SIZE):
            ar = ag = ab = aa = 0
            for dy in range(SS):
                for dx in range(SS):
                    o = 4 * ((y * SS + dy) * n + (x * SS + dx))
                    al = hi[o + 3]
                    ar += hi[o] * al
                    ag += hi[o + 1] * al
                    ab += hi[o + 2] * al
                    aa += al
            oo = 4 * (y * SIZE + x)
            if aa == 0:
                continue
            out[oo] = ar // aa            # alpha-weighted colour average
            out[oo + 1] = ag // aa
            out[oo + 2] = ab // aa
            out[oo + 3] = aa // (SS * SS)

    write_png(os.path.join(os.path.dirname(__file__), "..", "assets", "petal.png"),
              SIZE, SIZE, out)
    print(f"wrote assets/petal.png ({SIZE}x{SIZE})")


def write_png(path, w, h, rgba):
    """Minimal RGBA PNG writer (stdlib only)."""
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    # IHDR: 8-bit, colour type 6 (RGBA).
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    # Raw scanlines, each prefixed with filter byte 0.
    raw = bytearray()
    stride = 4 * w
    for y in range(h):
        raw.append(0)
        raw.extend(rgba[y * stride:(y + 1) * stride])
    idat = zlib.compress(bytes(raw), 9)
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", ihdr)
           + chunk(b"IDAT", idat)
           + chunk(b"IEND", b""))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(png)


if __name__ == "__main__":
    main()
