"""Glyphspin mark: a torus drawn as a lattice of braille-style dots, the app's own glyph language.

Writes assets/glyphspin.ico (16-256 px, PNG-compressed entries) and assets/logo.png (512 px).
Pure Python (zlib + struct), no dependencies:  python tools/brand.py
macOS app icon (used by packaging/macos/bundle.sh):  python tools/brand.py --icns Glyphspin.icns
"""
import math
import os
import struct
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BG = (11, 14, 15)
ACCENT = (79, 199, 189)
TOP = (182, 233, 228)  # accent mixed 45% toward white, the brightest band


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def render(size, backdrop=True):
    """RGBA bytes of the mark at `size` px."""
    n = max(5, min(22, round(size / 3.4)))         # dots across
    pitch = size / (n + 1)
    dots = []
    for j in range(n + 2):
        for i in range(n):
            x = (i - (n - 1) / 2) / ((n - 1) / 2)    # -1..1
            y = (j - (n + 1) / 2) / ((n - 1) / 2) * 0.9
            # a torus seen from above at ~50 degrees: an elliptical ring, the hole shifted up a little
            e_out = math.hypot(x / 0.98, y / 0.62)
            e_in = math.hypot(x / 0.46, (y + 0.06) / 0.25)
            if e_out > 1.0 or e_in < 1.0:
                continue
            # lit from the upper left; the near (lower) half brighter, like the depth bands
            ang = math.atan2(y, x)
            light = 0.5 + 0.5 * math.cos(ang - 2.3) * -1
            near = 0.5 + 0.5 * max(-1.0, min(1.0, y / 0.62))
            b = max(0.0, min(1.0, 0.25 + 0.45 * near + 0.3 * light))
            cx = size / 2 + x * pitch * (n - 1) / 2
            cy = size / 2 + y * pitch * (n - 1) / 2 / 0.9
            r = pitch * (0.22 + 0.16 * b)
            dots.append((cx, cy, r, b))
    px = bytearray(size * size * 4)
    radius = size * 0.2  # rounded-square backdrop
    for py in range(size):
        for pxl in range(size):
            fx, fy = pxl + 0.5, py + 0.5
            if backdrop:
                dx = max(radius - fx, 0, fx - (size - radius))
                dy = max(radius - fy, 0, fy - (size - radius))
                a_bg = max(0.0, min(1.0, radius - math.hypot(dx, dy) + 0.5))
            else:
                a_bg = 0.0
            col = BG
            a_dot = 0.0
            for (cx, cy, r, b) in dots:
                if abs(fx - cx) > r + 1 or abs(fy - cy) > r + 1:
                    continue
                a = max(0.0, min(1.0, r - math.hypot(fx - cx, fy - cy) + 0.5))
                if a > a_dot:
                    a_dot = a
                    col = mix(mix(BG, ACCENT, 0.35 + 0.65 * b), TOP, max(0.0, b - 0.7) / 0.3)
            if backdrop:
                c = mix(BG, col, a_dot)
                alpha = a_bg
            else:
                c = col
                alpha = a_dot
            o = (py * size + pxl) * 4
            px[o:o + 4] = bytes((round(c[0]), round(c[1]), round(c[2]), round(alpha * 255)))
    return bytes(px)


def png(size, rgba):
    raw = b''.join(b'\x00' + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))

    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))


def render_mac(size):
    """The mark on the macOS icon grid: the rounded square at ~80 % of the canvas, centred, transparent margin."""
    inner = round(size * 0.805)
    src = render(inner)
    off = (size - inner) // 2
    px = bytearray(size * size * 4)
    for y in range(inner):
        o = ((y + off) * size + off) * 4
        px[o:o + inner * 4] = src[y * inner * 4:(y + 1) * inner * 4]
    return bytes(px)


def icns(path):
    """An .icns of PNG entries (macOS 10.7+ reads PNG data in these slots)."""
    slots = [(b'ic11', 32), (b'ic12', 64), (b'ic07', 128), (b'ic13', 256), (b'ic08', 256), (b'ic14', 512),
             (b'ic09', 512), (b'ic10', 1024)]
    cache = {}
    body = b''
    for kind, size in slots:
        if size not in cache:
            cache[size] = png(size, render_mac(size))
        body += kind + struct.pack('>I', 8 + len(cache[size])) + cache[size]
    with open(path, 'wb') as f:
        f.write(b'icns' + struct.pack('>I', 8 + len(body)) + body)
    print('wrote', path)


def main():
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [png(s, render(s)) for s in sizes]
    head = struct.pack('<HHH', 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries = b''
    for s, data in zip(sizes, images):
        entries += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(os.path.join(ROOT, 'assets', 'glyphspin.ico'), 'wb') as f:
        f.write(head + entries + b''.join(images))
    with open(os.path.join(ROOT, 'assets', 'logo.png'), 'wb') as f:
        f.write(png(512, render(512)))
    print('wrote assets/glyphspin.ico', sizes, 'and assets/logo.png')


if __name__ == '__main__':
    import sys
    if len(sys.argv) == 3 and sys.argv[1] == '--icns':
        icns(sys.argv[2])
    else:
        main()
