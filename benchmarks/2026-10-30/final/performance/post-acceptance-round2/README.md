# Historical second acceptance-review measurements

Measured [archived source](source/source.json): SHA-256 `f39d322de0885831e90e74ee4c232b0b079042314bfe0df2b3b743273b413c00`.
[Build receipt](build.json) and [build output](build.log) record a fresh
default-feature release executable from this source with Rust/Cargo 1.99.1.
The exact retained `7774ed6` baseline executable is reused, not rebuilt.
Historical validation commands are retained in [their receipt](../../validation/acceptance-r2-commands.json).
These results precede the [QR/account-kind correction](../post-qr-correction/README.md);
statements about this source describe that historical behavior.

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
| Plain RPC/s | -1.17% [-6.08%, +1.73%] |
| Plain client CPU/RPC | +1.19% [-1.43%, +6.43%] |
| Plain sampled p99 | +0.06% [-2.47%, +7.04%] |
| Gzip RPC/s | -0.49% [-3.79%, +2.75%] |
| Gzip client CPU/RPC | +0.48% [-2.58%, +4.12%] |
| Gzip sampled p99 | +1.59% [+0.58%, +11.51%] |
| Overload completed | -5.88% [-11.06%, -1.54%] |
| Busy-pool p99 | +135.31% [+95.11%, +141.29%] |
| Quiet-pool p99 | -45.37% [-50.67%, -35.08%] |
| Overload whole-process max RSS | +0.45% [-0.39%, +1.43%] |

The intervals above describe this run; they do not erase the historical significant losses or explain them. The busy/quiet tail
tradeoff remains material; no general optimization or causal share is inferred
by subtracting percentages from earlier runs. Each of the 24 overload executions accounts
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

Saturation includes the 8,192-byte replay-ID allocation. Heap values match the
previous post-Opus2 snapshot; this does not measure UpdateStream peer-cache work,
exhausted-channel tombstones, SRP jobs or the 256-byte key copy per authorized DC.
Those costs are outside this loopback fixture. Tombstones grow with distinct
exhausted channels for the receiver lifetime and clear on usable access/success.

Dedicated heap-run whole-process RSS is 11,256 to 14,412 KiB
(+28.04%). It includes the responder and differs from client
heap and overload RSS. A separate scenario heap repeat is also retained; neither
is a confidence experiment. Historical +28.15% heap-run RSS remains a real earlier
observation, distinct from the current overload RSS comparison.

[Allocation baseline](allocations/baseline.log) and
[candidate](allocations/candidate.log) each report 2.00 allocations/RPC in both
tracked windows per payload. Bytes allocated/RPC in those windows:

| Payload | Baseline windows | Candidate windows |
|---|---|---|
| Plain | 1800 / 1801 | 1803 / 1801 |
| Gzip | 1804 / 1799 | 1807 / 1792 |

These are local windows, not precise universal allocation/tail estimates.

## Fresh scheduled-arrival and runtime controls

[Scenario receipt](scenarios/receipt.json), [configuration](scenarios/configuration.json)
and [parsed samples](scenarios/samples.json) cover 42 successful named-test
executions from these exact binaries. Warm 20k/s, cold 20k/s and 1 MiB replies are
now rerun on the corrected source. Each scenario has only one pair; no confidence
or general speedup follows. Every offered call is accounted for below.

| Scenario | Binary | Offered | Completed | Errors | Harness drops | Group0 / group1 p99 ms | Whole-process max RSS KiB |
|---|---|---:|---:|---:|---:|---:|---:|
| open-warm | baseline | 30,000 | 30,000 | 0 | 0 | 2.502 / 2.463 | 11,260 |
| open-warm | candidate | 30,000 | 30,000 | 0 | 0 | 2.494 / 2.549 | 11,256 |
| open-overload | baseline | 180,000 | 120,000 | 0 | 60,000 | 16.901 / 18.094 | 11,320 |
| open-overload | candidate | 180,000 | 109,825 | 0 | 70,175 | 43.500 / 11.311 | 11,264 |
| open-cold | baseline | 30,000 | 30,000 | 0 | 0 | 34.093 / 53.010 | 11,264 |
| open-cold | candidate | 30,000 | 29,462 | 0 | 538 | 66.000 / 75.975 | 11,260 |
| open-large | baseline | 150 | 74 | 76 | 0 | 1.303 / 1.571 | 15,032 |
| open-large | candidate | 150 | 150 | 0 | 0 | 9.082 / 8.449 | 90,280 |
| open-large-burst | baseline | 3,000 | 1,500 | 1,500 | 0 | 115.282 / 121.346 | 87,264 |
| open-large-burst | candidate | 3,000 | 1,953 | 0 | 1,047 | 1622.168 / 1322.145 | 417,916 |

The 1 MiB file payload occupies 1,048,592 TL reply bytes. Baseline rejects some
large replies; its successful-call latency is therefore not an equivalent
comparison. Candidate completes every low-rate large reply without errors.
The burst saturates admission and causes explicit harness drops, not an
unlimited-capacity claim. Cold setup uses mock preloaded auth keys, not a full
DH handshake or account authorization.

Runtime/control medians over three executions per binary/configuration:

| Configuration | Baseline RPC/s | Candidate RPC/s |
|---|---:|---:|
| current-1cpu | 72,233 | 78,410 |
| multi-1cpu | 77,998 | 70,838 |
| multi-2cpu | 89,412 | 93,219 |
| blocking-1cpu | 65,298 | 67,207 |
| blocking-2cpu | 89,640 | 92,061 |

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
| 1,000 | 7.899251 | 0.000160 | 0.456585 | 0.002815 |
| 10,000 | 749.071890 | 0.000190 | 48.463835 | 0.024045 |

Tiny cached totals are sensitive to timer resolution/loop overhead; do not infer
precise nanosecond operation latency. The actual missing-hash pass comparison
uses the historical post-Opus2 source, plus the identical harness:
[receipt](retry-pass/receipt.json), [harness-only patch](retry-pass/benchmark-only.patch),
[control manifest](retry-pass/control-source-inputs.json), [samples](retry-pass/samples.json).

| Channels | Previous initial pass ms | Current initial pass ms | Previous retry pass ms | Current retry pass ms |
|---|---:|---:|---:|---:|
| 1,000 | 12.465999 | 0.161171 | 12.752646 | 0.178795 |
| 10,000 | 4206.253409 | 1.934851 | 4155.781292 | 1.962182 |

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

For the SQLite receiver control, export checkpoint 5f0cb05 and apply
`performance/post-acceptance-round1/source/source.patch`, then
`receiver/benchmark-only.patch`. Verify `receiver/control-source-inputs.json`
before building with its receipt's all-feature integration-test command. Use
that receipt's exact test name, CPU and three-pair procedure as above.

The gate binds current source, validations, executable receipts and artifact
membership. Final exact-tree clone/review/publication receipts are external under
[PUBLICATION.md](../../../../../PUBLICATION.md). No reviewer build or independent
benchmark execution is claimed; the parent executed the retained commands.

## SQLite receiver component

Three alternating release pairs on CPU 0 compare the immediately preceding
reviewed source with this correction, using identical added integration harnesses.
[Receipt](receiver/receipt.json), [control manifest](receiver/control-source-inputs.json),
[harness-only patch](receiver/benchmark-only.patch) and [samples](receiver/samples.json)
retain their identities. Each run uses SQLite in memory with 1,000 and 100,000
peer rows, timing 256 negative self lookups separately from 256 Common-update
deliveries after an idle-boundary yield. Setup and insertion are excluded.

| Peers | Old 256 self lookups ms | New 256 self lookups ms | Old 256 deliveries ms | New 256 deliveries ms |
|---|---:|---:|---:|---:|
| 1,000 | 9.162 | 9.174 | 9.584 | 0.760 |
| 100,000 | 858.003 | 845.548 | 856.254 | 0.732 |

The negative lookup control still scans the table; the correction avoids that
lookup during ordinary live delivery. These component totals are not disk
latency, network throughput, complete catch-up or production capacity estimates.
An unknown account kind uses limit 100, including unread cached bots, which can
increase catch-up page count relative to known-bot limit 100,000.

## Absolute large-reply RSS

- open-large: candidate **90,280 KiB**; baseline **15,032 KiB** whole-process maximum RSS; baseline errors 76/150, candidate errors 0/150.
- open-large-burst: candidate **417,916 KiB**; baseline **87,264 KiB** whole-process maximum RSS; baseline errors 1,500/3,000, candidate errors 0/3,000.

The local responder is included. Baseline rejects the valid large replies, so
these are not equivalent-successful-work memory or latency comparisons. The
absolute candidate footprint remains relevant for many bots sharing one process.
Historical acceptance-round1 values were 90,504/15,052 KiB at 100 calls/s and
353,152/71,652 KiB at 2,000/s; current observations do not erase those costs.
