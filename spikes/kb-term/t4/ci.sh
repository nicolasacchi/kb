#!/usr/bin/env bash
# T4 CI: prove the stand-in runs and its byte stream is self-consistent.
# Runs from the repo root; outputs to spikes/kb-term/t4/out/.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/out"
rm -rf "$out"; mkdir -p "$out"
py="python3 -I"

echo "== 1. --smoke"
$py "$here/mirror_standin.py" --smoke --out "$out/smoke.stream" --report "$out/smoke-report.json"

echo "== 2. interactive mode under a pty at default sizes (seed 3000), rate 0"
$py - "$here/mirror_standin.py" "$out" <<'PY'
import os, pty, sys, json, re, struct, fcntl, termios, time
script, out = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.dirname(script))
import importlib.util
spec = importlib.util.spec_from_file_location("ms", script); ms = importlib.util.module_from_spec(spec); spec.loader.exec_module(ms)
pid, fd = pty.fork()
if pid == 0:
    os.execvp("python3", ["python3", "-I", script, "--rate", "0", "--live", "400", "--rebuild-every", "100"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
buf = b""
while True:
    try:
        d = os.read(fd, 65536)
    except OSError:
        break
    if not d: break
    buf += d
os.waitpid(pid, 0)
text = buf.decode("utf-8", "replace")
open(os.path.join(out, "pty.stream"), "w", encoding="utf-8", newline="").write(text)
# the header is printed before the stream; strip it
start = text.index(ms.ED3)
body = text[start:]
v = ms.verify_stream(body, 3400, 100)
res = {"bytes": len(buf), "ed3": body.count(ms.ED3), "rebuilds": body.count(ms.SYNC_ON),
       "summary_present": "kb-term T4 stand-in finished" in text, "verify": v}
# a last-segment plain dump, as a perfect terminal would hold it
last = body.split(ms.ED3)[-1]
plain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", last)
plain = plain.split("=== kb-term T4")[0].replace("\r\n", "\n")
open(os.path.join(out, "perfect-dump.txt"), "w").write(plain)
# a broken terminal that ignored ED3: the whole stream's lines are retained
allplain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", body).split("=== kb-term T4")[0].replace("\r\n", "\n")
open(os.path.join(out, "ed3-ignored-dump.txt"), "w").write(allplain)
json.dump(res, open(os.path.join(out, "pty-report.json"), "w"), indent=2)
print(json.dumps(res, indent=2))
assert not v["problems"], v["problems"]
assert res["summary_present"] and res["ed3"] == 5 and res["rebuilds"] == 4
PY

echo "== 3. --check-dump: perfect terminal must PASS, ED3-ignoring terminal must FAIL with duplicates"
$py "$here/mirror_standin.py" --check-dump "$out/perfect-dump.txt" | tee "$out/check-perfect.txt"
if $py "$here/mirror_standin.py" --check-dump "$out/ed3-ignored-dump.txt" | tee "$out/check-ed3-ignored.txt"; then
  echo "checker failed to flag duplicates"; exit 1
fi
grep -q 'duplicates             : [1-9]' "$out/check-ed3-ignored.txt"
echo "T4 CI OK"
