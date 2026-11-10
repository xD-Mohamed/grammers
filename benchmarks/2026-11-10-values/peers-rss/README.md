# The earlier peers-step RSS rise

Original pair: `7e79532` to `482a4ef`, the binaries retained from that experiment.
Their SHA-256 values match its published configuration. No code was rebuilt for
this comparison. Both use the same local mock, coarse clock and release settings.
The precise clock is a system call on this host; neither side changes its clock.

Each CPU window is now 12 seconds, after 3 seconds of warming, rather than the
original 3-second window. Twelve ordinary pairs and eight tcache-disabled pairs
ran sequentially under the memory watchdog. The investigation was closed at its
20-minute limit, including analysis; actual workload windows took about 11 minutes.

The original VmRSS counter is asynchronous. To inspect the retained executables
without rebuilding them, an identical local preload replaces their steady counter
with the `smaps_rollup` page-table reading. It logs the original value and mapping
classes. Reads remain outside the CPU/allocation windows. The wrapper rejects a
fallback, missing diagnostics, timeout or disagreement with the reported snapshot.
It records preload/source hashes and GLIBC_TUNABLES beside each configuration.

The preload adds its own code/stack pages and changes proc-reading scratch behavior.
These absolute values must not be compared with uninstrumented runs. It is a local
measurement tool only; it is not linked into grammers or deployed to any consumer.

| Metric | Normal allocator, 12 pairs | tcache disabled, 8 pairs |
|---|---:|---:|
| Page-table RSS, before / after KiB | 8,506 / 8,600 | 8,414 / 8,632 |
| Paired RSS change, 95% interval | +1.09% [+0.31, +2.29] | +2.18% [+0.14, +3.00] |
| Paired absolute RSS change KiB | +92 [26, 194] | +184 [12, 252] |
| Anonymous RSS change KiB | -4 [-10, 2] | 0 [-12, 12] |
| Non-anonymous RSS change KiB | +94 [18, 196] | +158 [4, 256] |
| Allocator live heap, before / after B | 483,872 / 480,240 | 419,968 / 420,048 |
| Allocations/RPC, before / after | 19.44 / 18.19 | 19.44 / 18.19 |

Changes are medians of paired differences/ratios. Independent medians need not add.
Raw data: [normal](normal), [tcache disabled](tcache0),
[mapping-class analysis](page-breakdown.json).

**Outcome:** the earlier RSS rise cannot be dismissed as noise. It is reproduced
in non-anonymous resident pages, with no corresponding anonymous-memory increase.
The remainder can include file, shared-memory and special mappings. Changed
executable/library layout and residency are consistent with the result, but the
rollup does not establish that attribution. The ELF text section shrank by 29,888 B;
that fact alone does not characterize every mapping. The exact mapping or function was not identified
within the time box. No claim of a retained per-session heap increase follows.

The existing allocation cuts are retained, with this small process-RSS tradeoff
recorded explicitly. No new runtime change, host tuning or consumer deployment was
made for this investigation.

Validation of the observer: C compilation with `-Wall -Wextra -Werror`, a successful
old-binary smoke run, seven Python regressions covering snapshots and round pairing,
and fresh source review after its requested corrections. See
[the observer source](../tools/rss_snapshot.c) and
[the fail-closed wrapper](../tools/compare-long-rss.py).
