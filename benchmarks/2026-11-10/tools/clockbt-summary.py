import sys, subprocess, collections, re
path, binary = sys.argv[1], sys.argv[2]
lines = open(path).read().split("\n")
i = lines.index("MAPS")
stacks = [l.split() for l in lines[:i] if l.strip()]
base = None
bname = binary.split("/")[-1]
for l in lines[i+1:]:
    m = re.match(r'^([0-9a-f]+)-([0-9a-f]+) (\S+) (\S+) \S+ \S+\s+(\S.*)$', l)
    if m and m.group(5).endswith(bname) and m.group(4) == "00000000":
        base = int(m.group(1), 16); break
ranges = []
for l in lines[i+1:]:
    m = re.match(r'^([0-9a-f]+)-([0-9a-f]+) (\S+) (\S+) \S+ \S+\s+(\S.*)$', l)
    if m and m.group(5).endswith(bname):
        ranges.append((int(m.group(1),16), int(m.group(2),16)))
def inbin(a): return any(lo <= a < hi for lo, hi in ranges)
addrs = sorted({int(a,16) for s in stacks for a in s[1:] if inbin(int(a,16))})
# return addresses: subtract 1 to land inside the call instruction
out = subprocess.run(["addr2line", "-f", "-C", "-e", binary] + [hex(a - base - 1) for a in addrs], capture_output=True, text=True).stdout.split("\n")
name = {a: out[2*k] for k, a in enumerate(addrs)}
def clean(n):
    n = re.sub(r'::h[0-9a-f]{16}$', '', n)
    n = re.sub(r'<[^<>]*>', '<>', n)
    return n
tids = collections.Counter(s[0] for s in stacks)
print("samples per thread:", dict(tids))
main_tid = tids.most_common(1)[0][0]
skip = ("std::", "core::", "clock_gettime", "tokio::time::instant", "<>::", "alloc::")
agg = collections.Counter(); agg3 = collections.Counter()
for s in stacks:
    if s[0] != main_tid: continue
    fr = [clean(name[int(a,16)]) for a in s[1:] if inbin(int(a,16))]
    fr = [f for f in fr if f != "??"]
    interesting = [f for f in fr if not f.startswith(("std::time", "std::sys", "tokio::time::instant::Instant::now", "tokio::time::clock"))]
    agg[interesting[0] if interesting else "?"] += 1
    agg3[" <- ".join(interesting[:4])] += 1
n = sum(agg.values())
print("main thread samples:", n)
for k, v in agg.most_common(14): print(f"{100*v/n:5.1f}%  {k[:150]}")
print("--- with callers")
for k, v in agg3.most_common(16): print(f"{100*v/n:5.1f}%  {k[:330]}")
