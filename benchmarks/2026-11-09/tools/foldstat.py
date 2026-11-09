#!/usr/bin/env python3
"""Summarise folded stacks from mkprof.py: leaf categories, self time, inclusive time."""
import collections
import re
import sys

path = sys.argv[1]
thread = sys.argv[2] if len(sys.argv) > 2 else ''
top = int(sys.argv[3]) if len(sys.argv) > 3 else 40
SKIP = ('pprof', 'backtrace', 'perf_signal_handler', '__restore_rt', 'sigaction', '<unknown>', '__sigaction', 'trace_unsynchronized', '_Unwind', 'uw_', 'unwind', 'libunwind', 'execute_cfa', 'dl_iterate', 'find_fde')

def strip(name):
    name = re.sub(r'::h[0-9a-f]{16}$', '', name)
    out, depth = [], 0
    for ch in name:
        if ch == '<':
            depth += 1
        elif ch == '>':
            depth = max(0, depth - 1)
        elif depth == 0:
            out.append(ch)
    return ''.join(out)[:120] or name[:120]

total = 0
selfs, incl, cats, lines = (collections.Counter() for _ in range(4))
for row in open(path):
    stack, count = row.rsplit(' ', 1)
    count = int(count)
    frames = stack.split(';')
    if thread and thread not in frames[0]:
        continue
    frames = frames[1:]
    while frames and any(s in frames[-1] for s in SKIP):
        frames.pop()
    if not frames:
        continue
    total += count
    leaf = frames[-1]
    name = leaf.split('@')[0]
    selfs[strip(name)] += count
    lines[strip(name) + ' @ ' + (leaf.split('@')[1] if '@' in leaf else '?')] += count
    low = name
    loc = leaf.split('@')[1] if '@' in leaf else ''
    if any(m in low for m in ('malloc', 'cfree', 'realloc', '_int_', 'calloc', '__rust_alloc', '__rust_dealloc', '__libc_free', 'tcache')) or low == 'free':
        cat = 'allocator'
    elif low.startswith('clock_gettime') or low.startswith('__clock_gettime'):
        cat = 'clock_gettime'
    elif low.startswith('getrandom'):
        cat = 'getrandom'
    elif low in ('__send', 'send', '__libc_send'):
        cat = 'send syscall'
    elif low in ('recv', '__recv', '__libc_recv'):
        cat = 'recv syscall'
    elif low.startswith('epoll_wait'):
        cat = 'epoll_wait'
    elif low in ('syscall', '__write', 'write', '__read', 'read', 'writev', 'futex') or 'futex' in low:
        cat = 'other syscalls'
    elif any(m in low for m in ('memcpy', 'memmove', 'memset', 'memcmp')):
        cat = 'memcpy/memset'
    elif any(m in loc for m in ('aes', 'sha', 'block_api', 'x86.rs', 'ni.rs')) or '_mm_' in low or 'sha256' in low.lower() or 'aes' in low.lower():
        cat = 'crypto'
    else:
        cat = 'other user code'
    cats[cat] += count
    seen = set()
    for frame in frames:
        key = strip(frame.split('@')[0])
        if key not in seen:
            seen.add(key)
            incl[key] += count

def show(title, counter, n):
    print(f'--- {title}')
    for name, count in counter.most_common(n):
        print(f'{count * 100 / total:6.2f}% {name}')

print(f'samples: {total} (thread filter {thread!r})')
show('leaf categories', cats, 20)
show('self', selfs, top)
show('self by line', lines, top)
show('inclusive', incl, top + 30)
