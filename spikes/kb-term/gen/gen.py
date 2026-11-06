import os, sys, time, hashlib, base64, random, json, argparse
ap = argparse.ArgumentParser()
ap.add_argument('--kind', required=True)   # text | apc | lines
ap.add_argument('--total', type=float, required=True)  # bytes
ap.add_argument('--rate', type=float, default=0)       # bytes/s, 0 = unthrottled
ap.add_argument('--tag', required=True)
ap.add_argument('--shaout', required=True)
ap.add_argument('--go', required=True)
a = ap.parse_args()
START = b'@@KBT6-START-' + a.tag.encode() + b'@@'
END = b'@@KBT6-END-' + a.tag.encode() + b'@@'
rng = random.Random(1234)
def wr(b):
    mv = memoryview(b)
    while mv:
        n = os.write(1, mv); mv = mv[n:]
while not os.path.exists(a.go): time.sleep(0.05)
h = hashlib.sha256(); sent = 0
t0 = time.monotonic()
def out(b):
    global sent
    h.update(b); wr(b); sent += len(b)
    if a.rate:
        tgt = t0 + sent / a.rate
        d = tgt - time.monotonic()
        if d > 0: time.sleep(d)
wr(START)
if a.kind == 'text':
    tbl = bytes((65 + (i % 26)) if i % 3 else (48 + (i % 10)) for i in range(256))
    raw = rng.randbytes(1 << 20).translate(tbl)
    base = b''.join(raw[i:i+78] + b'\r\n' for i in range(0, len(raw), 78))
    i = 0
    while sent < a.total:
        out(b'CHUNK%08d\r\n' % i + base); i += 1
elif a.kind == 'apc':
    pool = [base64.b64encode(rng.randbytes(3072)) for _ in range(64)]
    big_osc = base64.b64encode(rng.randbytes(1 << 20))
    big_apc = base64.b64encode(rng.randbytes(6 << 20))
    seq = 0; buf = []; blen = 0
    while sent + blen < a.total:
        if seq % 2000 == 1999:
            c = b'\x1b_Ga=t,f=100,i=%d,m=0;' % (seq + 1) + big_apc + b'\x1b\\'
        elif seq % 100 == 99:
            c = b'\x1b]1337;File=inline=1;size=%d:' % len(big_osc) + big_osc + b'\x07'
        else:
            c = b'\x1b_Ga=t,f=100,i=%d,m=%d;' % (seq // 100 + 1, seq % 100 != 98) + pool[seq % 64] + b'\x1b\\'
        buf.append(c); blen += len(c); seq += 1
        if blen >= (1 << 20):
            out(b''.join(buf)); buf = []; blen = 0
    if buf: out(b''.join(buf))
elif a.kind == 'lines':
    texts = [(b'payload-%02d ' % k) + bytes(rng.choice(b'abcdefghij klmnopqrstuvwxyz0123456789') for _ in range(70)) for k in range(40)]
    seq = 0
    while sent < a.total:
        b = []
        for _ in range(2000):
            b.append(b'\x1b[%dmL%09d ' % (31 + seq % 7, seq) + texts[seq % 40] + b'\x1b[0m\r\n'); seq += 1
        out(b''.join(b))
else:
    raise SystemExit('kind')
wr(END)
if a.kind == 'lines': wr(b'\r\n')
json.dump({'bytes': sent, 'sha256': h.hexdigest(), 'elapsed': time.monotonic() - t0}, open(a.shaout, 'w'))
time.sleep(10**6)
