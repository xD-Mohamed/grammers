# Current measurements after acceptance-review corrections

Measured [historical source](source/source.json): SHA-256 `8bf94b609247c59b2316e550a5954c4eeb2d12d2aa7c51ef56aa26d9c80458f6`.
[Build receipt](build.json) and [build output](build.log) record a fresh
default-feature release executable from this source with Rust/Cargo 1.99.1.
The exact retained `7774ed6` baseline executable is reused, not rebuilt.
That historical source passed 673 release workspace tests/doctests, 43 ignored.

Twelve alternating A/B–B/A pairs use the unchanged encrypted local loopback
harness, a 1,500 ms window, client CPU 0 and responder CPUs 2,3. No concurrent
build or test suite ran during measurements. Other activity on this shared host
is uncontrolled. No Telegram traffic or production capacity claim is involved.

Changes are medians of paired percentage changes, not ratios of overall medians.
Brackets are paired-bootstrap 95% intervals without multiple-comparison adjustment.
[Paired samples](paired/samples.json), [paired summary](paired/summary.json),
[overload samples](overload/samples.json) and [overload summary](overload/summary.json)
retain all executions and other measured percentiles/CPU components.

| Metric | Paired change [95% interval] |
|---|---|
| Plain RPC/s | -1.75% [-5.97%, +2.98%] |
| Plain client CPU/RPC | +1.67% [-2.83%, +5.39%] |
| Plain sampled p99 | +0.84% [-4.75%, +24.24%] |
| Gzip RPC/s | -1.45% [-3.33%, +0.98%] |
| Gzip client CPU/RPC | +1.59% [-0.74%, +3.15%] |
| Gzip sampled p99 | +2.03% [-2.73%, +5.91%] |
| Overload completed | -5.83% [-11.23%, +2.42%] |
| Busy-pool p99 | +149.63% [+103.90%, +189.59%] |
| Quiet-pool p99 | -38.35% [-49.11%, -27.22%] |
| Overload whole-process max RSS | -0.61% [-1.01%, -0.19%] |

Throughput and total CPU/RPC intervals cross zero in this run. That does not
erase the historical significant losses or explain them. The busy/quiet tail
tradeoff remains material; no general optimization or causal share is inferred
by subtracting percentages from earlier runs. All 24 overload executions account
for 180,000 offered calls, zero errors and peak admission 1024. Latency starts at
intended arrival, excluding harness-dropped calls. Historical scheduling controls
support the fairness tradeoff, not a complete attribution of throughput/CPU loss.

## Client heap, process RSS and allocations

The dedicated 400-session [baseline](heap/baseline.log) and
[candidate](heap/candidate.log) census has one run per binary:

| Stage | Baseline B/session | Candidate B/session |
|---|---:|---:|
| pool created, not connected | 2,103 | 2,143 |
| connected after one RPC | 7,911 | 7,919 |
| after nine RPCs per session | 7,911 | 8,015 |
| after 1100 RPCs per session (replay cache saturated) | 7,911 | 16,079 |

Saturation includes the 8192-byte replay-ID allocation. Heap values match the
previous post-Opus2 snapshot; this does not measure UpdateStream peer-cache work,
exhausted-channel tombstones, SRP jobs or the 256-byte key copy per authorized DC.
Those costs are outside this loopback fixture. Tombstones grow with distinct
exhausted channels for the receiver lifetime and clear on usable access/success.

Dedicated heap-run whole-process RSS is 11,336 to 14,432 KiB
(+27.31%). It includes the responder and differs from client
heap and overload RSS. A separate scenario heap repeat is also retained; neither
is a confidence experiment. Historical +28.15% heap-run RSS remains a real earlier
observation, not superseded by the small overload RSS decrease.

[Allocation baseline](allocations/baseline.log) and
[candidate](allocations/candidate.log) each report 2.00 allocations/RPC in both
tracked windows per payload. Bytes allocated/RPC in those windows:

| Payload | Baseline windows | Candidate windows |
|---|---|---|
| Plain | 1792 / 1794 | 1796 / 1797 |
| Gzip | 1788 / 1788 | 1798 / 1792 |

These are local windows, not precise universal allocation/tail estimates.

## Fresh scheduled-arrival and runtime controls

[Scenario receipt](scenarios/receipt.json), [configuration](scenarios/configuration.json)
and [parsed samples](scenarios/samples.json) cover 42 successful named-test
executions from these exact binaries. Warm 20k/s, cold 20k/s and 1 MiB replies are
now rerun on the corrected source. Each scenario has only one pair; no confidence
or general speedup follows. Every offered call is accounted for below.

| Scenario | Binary | Offered | Completed | Errors | Harness drops | Group 0 / group 1 p99 ms |
|---|---|---:|---:|---:|---:|---:|
| open-warm | baseline | 30,000 | 30,000 | 0 | 0 | 2.563 / 2.520 |
| open-warm | candidate | 30,000 | 30,000 | 0 | 0 | 2.459 / 2.511 |
| open-overload | baseline | 180,000 | 119,310 | 0 | 60,690 | 17.616 / 18.887 |
| open-overload | candidate | 180,000 | 116,712 | 0 | 63,288 | 41.351 / 11.251 |
| open-cold | baseline | 30,000 | 29,487 | 0 | 513 | 56.098 / 83.104 |
| open-cold | candidate | 30,000 | 29,796 | 0 | 204 | 49.754 / 59.685 |
| open-large | baseline | 150 | 74 | 76 | 0 | 1.957 / 2.094 |
| open-large | candidate | 150 | 150 | 0 | 0 | 10.498 / 9.121 |
| open-large-burst | baseline | 3,000 | 1,500 | 1,500 | 0 | 104.197 / 109.132 |
| open-large-burst | candidate | 3,000 | 2,042 | 0 | 958 | 1555.526 / 1144.223 |

The 1 MiB file payload occupies 1,048,592 TL reply bytes. Baseline rejects some
large replies; its successful-call latency is therefore not an equivalent
comparison. Candidate completes every low-rate large reply without errors.
The burst saturates admission and causes explicit harness drops, not an
unlimited-capacity claim. Cold setup uses mock preloaded auth keys, not a full
DH handshake or account authorization.

Runtime/control medians over three executions per binary/configuration:

| Configuration | Baseline RPC/s | Candidate RPC/s |
|---|---:|---:|
| current-1cpu | 78,583 | 77,231 |
| multi-1cpu | 75,505 | 72,425 |
| multi-2cpu | 90,320 | 90,232 |
| blocking-1cpu | 66,445 | 65,010 |
| blocking-2cpu | 91,419 | 90,136 |

These are socket/thread controls, not a stack profile. The blocking control
bypasses SenderPool with 16 blocking workers and 64 connections; it is not a drop-in
runtime comparison. Zero main-thread CPU fields under the multi-worker runtime
and zero cycle fields mean unavailable attribution, not zero cost. Worker CPU
lines and whole-process counters remain in the logs. `perf` was unavailable;
these controls neither establish Tokio as a bottleneck nor justify replacing it.

## Deadline and missing-hash retry components

Three alternating release pairs on CPU 0 measure message-box components, not
complete catch-up. [Deadline receipt](deadlines/receipt.json),
[control-only patch](deadlines/benchmark-only.patch),
[control manifest](deadlines/baseline-source-inputs.json) and
[samples](deadlines/samples.json) compare the archived first-Opus correction with
the current source. Only the identical ignored benchmark is appended to the
control; all other Rust/Cargo inputs match its archived manifest.

| Channels | Previous 32 cached checks ms | Current 32 cached checks ms | Previous expired sweep ms | Current expired sweep ms |
|---|---:|---:|---:|---:|
| 1,000 | 8.229232 | 0.000191 | 0.448692 | 0.002796 |
| 10,000 | 746.761072 | 0.000191 | 48.580843 | 0.030588 |

Tiny cached totals are sensitive to timer resolution/loop overhead; do not infer
precise nanosecond operation latency. The actual missing-hash pass comparison
uses the immediately preceding reviewed source, plus the identical harness:
[receipt](retry-pass/receipt.json), [harness-only patch](retry-pass/benchmark-only.patch),
[control manifest](retry-pass/control-source-inputs.json), [samples](retry-pass/samples.json).

| Channels | Previous initial pass ms | Current initial pass ms | Previous retry pass ms | Current retry pass ms |
|---|---:|---:|---:|---:|
| 1,000 | 14.961985 | 0.155754 | 15.155497 | 0.164118 |
| 10,000 | 4388.785569 | 1.920123 | 4319.198016 | 1.882612 |

Each pass includes selection, deferral and the receiver-style deadline check
for every channel. It excludes terminal exhausted-entry removal, successful
recovery, network/storage and overall catch-up. Ready-only queueing removes
repeated deferred-prefix and minimum-deadline scans. Terminal sorted-vector
removal retains a quadratic worst case and is not represented by these results.
The old unmeasured 8-second estimate is not used as evidence or a universal bound.

## Reproduction

Build default-feature loopback with the exact command in build.json and retain
the executable. Run `scripts/compare-loopback-r1.py --runs 12` and
`scripts/compare-overload.py --pairs 12`,
supplying `--baseline`, `--candidate` and `--output`. Then run
`scripts/run-loopback-scenarios.py` with those explicit paths. Heap/allocation
configuration files preserve their exact environments and named test commands.

For component controls, export checkpoint 5f0cb05, apply the archived
`performance/post-opus/source/source.patch` (deadlines) or
`performance/post-opus2/source/source.patch` (retry passes), then the corresponding
harness-only patch. Verify every control input against its manifest before
building with the receipt's Cargo command. Run each retained binary on CPU 0 with
`--exact` plus its receipt's test name, `--ignored --nocapture --test-threads=1`,
alternating three pairs. Current and control executable hashes are retained.

The gate binds current source, validations, executable receipts and artifact
membership. Final exact-tree clone/review/publication receipts are external under
[PUBLICATION.md](../../../../../PUBLICATION.md). No reviewer build or independent
benchmark execution is claimed; the parent executed the retained commands.

The 1 MiB candidate whole-process maximum RSS was **90,504 KiB** against baseline
**15,052 KiB** at 100 offered calls/s; at 2,000/s it was **353,152 KiB** against
**71,652 KiB**. These figures include the responder. The baseline failed about half
its calls, so neither memory ratio nor latency compares equivalent successful
work. The absolute candidate footprint remains relevant for shared processes.
