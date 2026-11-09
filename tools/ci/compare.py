"""Compare two offline_cases.sh output folders (Windows vs macOS):  python compare.py WIN_DIR MAC_DIR
For each case: cell-grid hash and differing halfs, BMP pixels that differ and the largest channel difference."""
import os
import struct
import sys


def bmp(path):
    with open(path, 'rb') as f:
        b = f.read()
    w, h = struct.unpack('<ii', b[18:26])
    return abs(w), abs(h), b[54:]


def halfs(path):
    with open(path, 'rb') as f:
        b = f.read()
    return struct.unpack('<%dH' % (len(b) // 2), b)


def f16(h):
    return struct.unpack('<e', struct.pack('<H', h))[0]


def main(a, b):
    names = sorted(n[:-4] for n in os.listdir(a) if n.endswith('.bmp'))
    print('| case | grid / cells | halfs differing (max abs) | pixels differing | max channel diff |')
    print('|---|---|---|---|---|')
    for n in names:
        pa, pb = os.path.join(a, n + '.bmp'), os.path.join(b, n + '.bmp')
        if not os.path.exists(pb):
            print(f'| {n} | missing on the second side | | | |')
            continue
        wa, ha, da = bmp(pa)
        wb, hb, db = bmp(pb)
        ca, cb = os.path.join(a, n + '.cells'), os.path.join(b, n + '.cells')
        cells = 'n/a'
        hd = ''
        if os.path.exists(ca) and os.path.exists(cb):
            xa, xb = halfs(ca), halfs(cb)
            if len(xa) != len(xb):
                cells = f'different grids ({len(xa)} vs {len(xb)} halfs)'
            else:
                diff = [(f16(p), f16(q)) for p, q in zip(xa, xb) if p != q]
                cells = 'identical' if not diff else f'{len(xa)} halfs'
                hd = '0' if not diff else f'{len(diff)} ({max(abs(p - q) for p, q in diff):.4g})'
        if (wa, ha) != (wb, hb):
            print(f'| {n} | {cells} | {hd} | sizes differ {wa}x{ha} vs {wb}x{hb} | |')
            continue
        row = (wa * 3 + 3) & ~3
        npx = 0
        mx = 0
        for y in range(ha):
            ra = da[y * row:y * row + wa * 3]
            rb = db[y * row:y * row + wa * 3]
            if ra == rb:
                continue
            for x in range(wa):
                d = max(abs(ra[x * 3 + k] - rb[x * 3 + k]) for k in range(3))
                if d:
                    npx += 1
                    mx = max(mx, d)
        print(f'| {n} | {cells} | {hd} | {npx} of {wa * ha} ({100.0 * npx / (wa * ha):.3f} %) | {mx} |')


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
