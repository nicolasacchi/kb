#!/usr/bin/env python3
"""Synthetic omp-like kitty Unicode-placeholder stream (no private content).

Mirrors what omp (PI_FORCE_IMAGE_PROTOCOL=kitty, inside tmux) was observed to emit (spike T8):
  [DCS-tmux( APC a=d,d=A,q=2 )]
  [DCS-tmux( APC a=t,f=100,q=2,i=ID,m=1 ; <b64 4096> )] then continuation chunks 'q=2,m=1;...' ... last 'q=2,m=0;...'
  [DCS-tmux( APC a=p,U=1,q=2,i=ID,p=ID,c=COLS,r=ROWS )]
  ROWS lines:  SGR 38;2;R;G;B  SGR 58:2::R:G:B  (U+10EEEE rowDiacritic colDiacritic)*COLS  SGR 39;59  CR CR LF ESC[2K
DCS-tmux wrapping: ESC P tmux; <inner with every ESC doubled> ESC \\   (omit with --no-wrap).
Usage: synth_omp_kitty_stream.py [--cols 90] [--rows 19] [--id 0x66CF38] [--px 360x152] [--no-wrap]
       [--resize-replay] (append a=d + a=d,d=I + full retransmit, as omp did after resize-window)
Writes raw bytes to stdout. Stdlib only.
"""
import sys, zlib, struct, base64, argparse
DIAC = [773,781,782,784,786,829,830,831,838,842,843,844,848,849,850,855,859,867,868,869,870,871,872,873,874,875,876,877,878,879,1155,1156,1157,1158,1159,1426,1427,1428,1429,1431,1432,1433,1436,1437,1438,1439,1440,1441,1448,1449,1451,1452,1455,1476,1552,1553,1554,1555,1556,1557,1558,1559,1623,1624,1625,1626,1627,1629,1630,1750,1751,1752,1753,1754,1755,1756,1759,1760,1761,1762,1764,1767,1768,1771,1772,1840,1842,1843,1845,1846,1850,1853,1855,1856,1857,1859,1861,1863,1865,1866,2027,2028,2029,2030,2031,2032,2033,2035,2070,2071,2072,2073,2075,2076,2077,2078,2079,2080,2081,2082,2083,2085,2086,2087,2089,2090,2091,2092,2093,2385,2387,2388,3970,3971,3974,3975,4957,4958,4959,6109,6458,6679,6773,6774,6775,6776,6777,6778,6779,6780,7019,7021,7022,7023,7024,7025,7026,7027,7376,7377,7378,7386,7387,7392,7616,7617,7619,7620,7621,7622,7623,7624,7625,7627,7628,7633,7634,7635,7636,7637,7638,7639,7640,7641,7642,7643,7644,7645,7646,7647,7648,7649,7650,7651,7652,7653,7654,7678,8400,8401,8404,8405,8406,8407,8411,8412,8417,8423,8425,8432,11503,11504,11505,11744,11745,11746,11747,11748,11749,11750,11751,11752,11753,11754,11755,11756,11757,11758,11759,11760,11761,11762,11763,11764,11765,11766,11767,11768,11769,11770,11771,11772,11773,11774,11775,42607,42620,42621,42736,42737,43232,43233,43234,43235,43236,43237,43238,43239,43240,43241,43242,43243,43244,43245,43246,43247,43248,43249,43696,43698,43699,43703,43704,43710,43711,43713,65056,65057,65058,65059,65060,65061,65062,68111,68152,119173,119174,119175,119176,119177,119210,119211,119212,119213,119362,119363,119364]
def png(w, h):
    rows = bytearray()
    for y in range(h):
        rows.append(0)
        for x in range(w):
            rows += bytes(((x * 255) // max(w - 1, 1), (y * 255) // max(h - 1, 1), 128 + 100 * (((x // 20) + (y // 20)) & 1), 255))
    def ch(t, d): return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + ch(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + ch(b"IDAT", zlib.compress(bytes(rows), 6)) + ch(b"IEND", b"")
def apc(ctrl, payload=b""): return b"\x1b_G" + ctrl + (b";" + payload if payload else b"") + b"\x1b\\"
def wrap(b, on): return (b"\x1bPtmux;" + b.replace(b"\x1b", b"\x1b\x1b") + b"\x1b\\") if on else b
def transmit(i, pngb, w):
    b64 = base64.b64encode(pngb); out = b""
    chunks = [b64[k:k + 4096] for k in range(0, len(b64), 4096)]
    for n, c in enumerate(chunks):
        more = b"1" if n < len(chunks) - 1 else b"0"
        ctrl = (b"a=t,f=100,q=2,i=%d,m=" % i if n == 0 else b"q=2,m=") + more
        out += wrap(apc(ctrl, c), w)
    return out
def rows(i, cols, nrows):
    rgb = ((i >> 16) & 255, (i >> 8) & 255, i & 255)
    sgr = (b"\x1b[38;2;%d;%d;%dm" % rgb) + (b"\x1b[58:2::%d:%d:%dm" % rgb)
    out = []
    for r in range(nrows):
        line = sgr + b"".join(("\U0010EEEE" + chr(DIAC[r]) + chr(DIAC[c])).encode() for c in range(cols)) + b"\x1b[39;59m"
        out.append(line)
    return b"\r\r\n\x1b[2K".join(out) + b"\r\r\n"
def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--cols", type=int, default=90); ap.add_argument("--rows", type=int, default=19)
    ap.add_argument("--id", type=lambda x: int(x, 0), default=0x66CF38); ap.add_argument("--px", default="360x152")
    ap.add_argument("--no-wrap", action="store_true"); ap.add_argument("--resize-replay", action="store_true")
    a = ap.parse_args(); w = not a.no_wrap; pw, ph = map(int, a.px.split("x"))
    assert a.rows <= len(DIAC) and a.cols <= len(DIAC) and 0 < a.id < (1 << 24)
    pb = png(pw, ph); o = sys.stdout.buffer
    def render():
        o.write(wrap(apc(b"a=d,d=A,q=2"), w)); o.write(transmit(a.id, pb, w))
        o.write(wrap(apc(b"a=p,U=1,q=2,i=%d,p=%d,c=%d,r=%d" % (a.id, a.id, a.cols, a.rows)), w)); o.write(rows(a.id, a.cols, a.rows))
    render()
    if a.resize_replay:
        o.write(wrap(apc(b"a=d,d=A,q=2"), w)); o.write(wrap(apc(b"a=d,d=I,i=%d,q=2" % a.id), w))
        o.write(transmit(a.id, pb, w)); o.write(wrap(apc(b"a=p,U=1,q=2,i=%d,p=%d,c=%d,r=%d" % (a.id, a.id, a.cols, a.rows)), w)); o.write(rows(a.id, a.cols, a.rows))
if __name__ == "__main__": main()
