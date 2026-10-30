# Corrections after the first independent review

Reviewed checkpoint: `8f109a67458acf35f278451ee6d32c0acaab80e3`, against
`7774ed666317d981e1dfea0d12311cbd38acb43e`. This document covers the subsequent
correction source subsequently checkpointed by the orchestrator as
`5b16ac41ae9d6b0cdd0bf42df50d6064561bc19e`. The second review requested changes;
see [REVIEW-ROUND2.md](REVIEW-ROUND2.md). This historical report does not assert acceptance.

The [complete review](benchmarks/2026-10-30/round1/claude-review.md) and
[provider receipts](benchmarks/2026-10-30/round1/provenance.json) preserve the new
artifacts. The report was read in two pages (14,171 Unicode characters).
Receipts accept native `gpt-6-astra` and `claude-opus-5-5`; `xhigh` is requested,
not independently reported native effort. The older missing prose remains missing.

## Disposition of all numbered findings

| Finding | Decision and implementation | Regression / evidence |
|---|---|---|
| 1 | Accepted. Only account/channel difference results enter the forced branch. Socket pts for a pending key is dropped; non-pts delivery retains the previous saved date/seq. | pts 100 -> socket 101 -> difference 101 delivers once, with getDifference still requesting 100 before recovery. |
| 2 | Accepted. A missing peer on messageEmpty is valid; a known non-channel peer remains invalid. | Peerless socket batch preserves its non-pts sibling, and account difference completes. |
| 3 | Accepted. `retain_mut(poll_closed)` removes closed cold waiters incrementally and releases their SDK permits. | 200 caller-only cancellations observe peer EOF within 1 second before Quit; partial cancellation admits another DC while one waiter remains. |
| 4 | Accepted. Cache primality only for Telegram's published constant, preserve generator checks, and execute handshake DH validation/exponentiation in at most four blocking jobs. | Full primality versus cached constant release A/B; existing good handshake vector; current-thread progress test. Started jobs retain capacity until completion after caller cancellation. |
| 5 | Accepted. Updating time offset resets the outgoing message-ID floor. | Authenticated code-17 notification corrects a clock one hour fast; next message ID follows corrected time. |
| 6 | Accepted evidence gap. Add current-thread/multi-thread runs, per-thread `getrusage` user/system accounting, and a blocking-socket client control. | Runtime controls below. The blocking client excludes SDK routing/admission, so its whole delta is not a Tokio-only effect. |
| 7 | Accepted evidence gap. Separate evaluated existing proposals from proposals not evaluated; do not equate absence of evidence with rejection. | P2 matrix below, independent component logs and paired RPC data. |
| 8 | Accepted evidence gap. Add independent scheduled arrivals, capacity 1024, two DCs, shared busy and quiet pools, cold startup and 1 MiB replies. | Open-loop logs record intended-arrival latency, completed/error/drop counts and observed peak in-flight work. |
| 9 | Accepted. Replace a Sent keepalive at the next interval even without its pong; preserve a partially written ping. | Sent-ping replacement and existing bounded backlog/pong retirement tests. |
| 10 | Accepted. Rewrite initialization failures to definite-not-sent for callers never framed by that setup. | Original ReadError Arc identity is preserved while certainty changes; shared-failure tests remain. |
| 11 | Accepted. Liveness deadline uses `Instant::checked_add`. | Polling a real sender step with `Duration::MAX` does not panic. |
| 12 | Accepted. Ping interval is `min(60 seconds, liveness / 2)`. | Paused-time 4-second liveness sends a ping after 2 seconds. |
| 13 | Accepted. `request_fits` precedes all service mutation. | Invalid push leaves salt request, ACK IDs and buffer untouched. |
| 14 | Accepted. Salt requests only enter an empty buffer; finalization does not append an ACK to a salt-only packet. | Final packet ID remains the salt request ID and pending ACK remains queued. |
| 15 | Accepted. Time-offset arithmetic uses i64, with saturating addition for caller configuration. | Extreme authenticated server timestamp and code-17 regressions. |
| 16 | Accepted. Discard orphan saved qts/date/seq; do not create Secondary from socket updates before Common exists; getDifference handles absence explicitly. | Saved secondary-only state and live qts 1/3 tests; independently valid channel state remains preserved. |
| 17 | Accepted. Definitive invalid/private/inaccessible-channel errors end only that channel's difference. Flood and authorization failures still propagate. | Real `UpdatesReceiver::next` driven by an in-process RPC responder proceeds from CHANNEL_INVALID to the next channel. |
| 18 | Accepted. Missing channel hash immediately continues recovery for other keys. | Same next-driven test skips channel 1, handles channel 2's failure, and receives channel 3's update. |
| 19 | Accepted. A new home-failure generation rearms GetState when the box is empty. Failed GetState reaches the caller and retains readiness for their next call, avoiding an internal reconnect loop. | Next-driven GetState recovery test plus existing full-queue generation notification test. |
| 20 | Accepted. Empty reply album returns InvalidInput before peer lookup. | Empty Message fixture invokes `reply_album(vec![])` without network work. |
| 21 | Accepted. Keep PreFailed sticky and restore the request after storage, RPC or CDN errors. | Repeated failing next calls remain errors; request offset remains unchanged even if a pending next future is cancelled. |
| 22 | Accepted. A single body exceeding the entire byte budget is permanent InvalidInput. | Separate permanent oversize and temporary exhaustion tests. |
| 23 | Accepted. AutoSleep whitelists connection-related I/O kinds. | Both direct Io and definite-not-sent Read reject invalid input/data, permission, unsupported and overload kinds; refused connections remain retryable. |
| 24 | Accepted. Validated dh_gen_retry maps to DhGenRetry. | Valid nonce-hash-2 fixture reaches the distinct variant. |
| 25 | Accepted. Invalid container metadata/trailing data reports InvalidEnvelope; plain misalignment is distinct from excessive length; oversized receive data is Protocol. | Authenticated container accept/reject tests and typed error-category test. |
| 26 | Accepted. Rho restarts share 1,250,000 polynomial steps, GCD batches cap at 128, and prime squares have an exact integer-root path. | Nonzero budget exhaustion, balanced 64-bit semiprime, 4294967291 squared and three-prime composite. The review's estimated failure probability was not independently measured. |
| 27 | Accepted. `run_sender` returns `(dc_id, result)` and is spawned directly. | Footprint assertion measures the actual spawned shape; 1100-RPC/session census below. |
| 28 | Accepted. Reset the retained timer only for an earlier deadline or an elapsed timer. | Service deadline cancellation test; isolated timer A/B below distinguishes this change from the whole correctness patch. |
| 29 | Accepted. Build malformed fixtures on the published valid prime. | Padding >=16, bad hash, both nonces, long g_a, time subtraction overflow and low/high public-value bounds each reach their intended gate; valid control succeeds. |
| 30 | Accepted. Child deadline requires an explicit completion marker with nocapture. | Zero matching tests can no longer pass; child is killed/reaped on deadline; nonzero budget and large-number fixtures included. |
| 31 | Accepted. Exercise authenticated container validation through process_plaintext. | Equal/lower child seq accepted; higher/negative seq and nonpositive/even/not-older IDs rejected before delivery. |
| 32 | Accepted. Add caller-only, partial cancellation, disconnect and stale completion cases. | Socket EOF and admission checked before Quit; old task identity cannot remove the replacement waiter. |
| 33 | Accepted. Drive next with an RPC responder; force storage to yield during concurrent config merge. | Real next recovery tests, a ready-control-backlog cooperative-yield test and deterministic concurrent IPv4/IPv6 merge test; cross-pool fairness measured by scheduled arrivals. |
| 34 | Accepted. Add deterministic seeded mutation/property corpora without I/O. | 4096 mutations of one small rpc_result seed, plus transport offsets checked over 512 input sizes. The mutation corpus does not cover container, gzip or service seeds; those have separate example regressions. This is finite coverage, not continuous coverage-guided fuzzing. |
| 35 | Accepted. Identify reviewed checkpoint separately from baseline; revalidate minimal client after corrections. | Current validation logs and reproducible source manifest. |
| 36 | Accepted. Record new behavior in AGENTS and exhaustive-enum migration in CHANGELOG. | Public variants and configuration fields explicitly listed. |
| 37 | Accepted. Replace causal claims from three noisy pairs with 22 CPU-pinned pairs and paired bootstrap intervals. | Raw executions, per-pair arrays, configuration and deterministic analysis script. |
| 38 | Accepted. Extend the census past 1024 replies and lead with saturated heap. | 400 sessions, 1100 RPCs each, both revisions. |
| 39 | Accepted. Record source checkpoint, exact build/diff commands and hashes. New complete harness is identical in both revisions. | The undocumented legacy hash input cannot be recovered; source.json explicitly limits that provenance rather than inventing a command. |
| 40 | Accepted. Sanitize host paths and private task IDs in public artifacts. | New provider receipts retain model evidence without private runner identifiers; original numerical samples remain intact. |

## Protocol disagreement: container sequence validation

The review additionally suggests logging a child seqno greater than its container
instead of rejecting it until captured production traffic establishes the rule.
At this checkpoint that relaxation was not adopted. Telegram's primary specification explicitly
requires a container's ID to exceed its children and its sequence number to be
at least theirs: [message ID and sequence rules](https://core.telegram.org/mtproto/description#message-sequence-number-msg-seqno).
Authentication does not make a malformed envelope valid. Fixtures now test this
through the authenticated packet entry point; a violation remains a typed error
scoped to its connection. No captured Telegram traffic or live compatibility
claim was made for this checkpoint. Round two subsequently ran the two public
test-DC probes and made child-sequence ordering warn-only; see its ledger for
the operational decision. Structural/ID/nonnegative-sequence checks remain strict.

## P2 proposal matrix

| Area / concrete proposal | Evaluation status | Disposition |
|---|---|---|
| DH constant-prime cache | Direct release A/B against the same safe-prime backend, five samples | Adopted; cryptographic subgroup checks retained, unknown primes still checked. |
| Sender future wrapper removal | Actual spawned future assertion and 400-session saturated census | Adopted to remove duplicate by-value state. |
| Retained ACK/service timer reset | Isolated same-source A/B with only reset condition changed | Adopted; see measured interval, not a claim that every RPC gets faster. |
| Gzip decoder reuse | Existing legacy/new decoder A/B rerun with bounded output path | Already present at baseline; retain only measured scope, no new compression algorithm adopted. |
| ACK serialization | Existing typed/reference versus direct encoder A/B | Already present; preserve exact wire tests. |
| Shared request serialization/storage | Existing Vec clone versus shared-body A/B | Already present; API remains compatible. |
| Warm route and lock path | Existing queued/direct route A/B at one/four runtime workers | Already present; no new lock-free route representation. |
| New compression threshold/algorithm | Not evaluated in this correction | No policy change; no evidence-based rejection claimed. |
| Packing window/forced batching | Not evaluated in this correction | Existing immediately available packing retained; no unmeasured latency delay added. |
| Sent-request hash/index rewrite | Not evaluated in this correction | No representation change; replay-cache correctness is separate. |
| New TL serializer or socket-buffer allocator | Not evaluated in this correction | Existing paths retained. Saturated heap measured, but that is not an A/B of every allocator design. |
| Runtime replacement | Current/multi-thread plus blocking-client controls | These controls bound specific costs and do not establish an application-wide replacement benefit. |

## Measurement interpretation and remaining gates

The original three-pair results remain historical data in
[OPTIMIZATION-2026-10.md](OPTIMIZATION-2026-10.md). They did not independently
resolve runtime, syscall, allocation or fairness effects. New results are stored
under [round1](benchmarks/2026-10-30/round1/), with
[reproduction commands and source hashes](benchmarks/2026-10-30/round1/README.md).

### Final paired RPC measurements

22 alternating pairs, identical release harness, 1500 ms/reply window,
client CPU 0 and responders CPUs 2,3. Values are medians of per-execution metrics;
95% bootstrap intervals resample paired execution ratios (10,000 draws).

| Metric | Plain baseline -> corrected | Gzip baseline -> corrected |
|---|---:|---:|
| RPC/s | 70,171.5 -> 68,398 | 72,929 -> 69,466.5 |
| Paired throughput change, 95% interval | -3.50% [-6.42%, +0.003%] | -4.11% [-8.22%, +1.43%] |
| Client user us/RPC | 6.235 -> 6.535 | 6.175 -> 6.435 |
| Client system us/RPC | 7.975 -> 8.155 | 7.595 -> 7.695 |
| Client total CPU us/RPC | 14.155 -> 14.455 | 13.565 -> 14.240 |
| Paired total CPU change, 95% interval | +3.43% [-0.14%, +6.31%] | +3.63% [-0.17%, +9.05%] |
| p50 / p95 / p99 us | 210.3 / 371.25 / 468.75 -> 210.0 / 379.9 / 495.0 | 189.35 / 367.0 / 467.15 -> 196.2 / 391.0 / 505.45 |

Whole-process median peak RSS was 11,352 -> 11,358 KiB in these executions.
The [complete metric intervals](benchmarks/2026-10-30/round1/paired/summary.json)
include latency and split CPU. Throughput and total CPU intervals include zero;
the point estimates lean slower, so there is no measured steady-state speedup.
Gzip user CPU's paired interval is positive (+0.42% to +14.08%); these many
secondary comparisons are unadjusted and cannot isolate a particular change.
Independent medians of user/system values need not sum to the median total.

### Published-prime validation

The [release component A/B](benchmarks/2026-10-30/round1/prime.log) measured
median **137,154.6 us -> 0.263 us** for checking the same published prime
(five full checks versus five batches of 10,000 cached checks). This isolates
the skipped primality test, not network connection latency or the complete DH
handshake. Generator/subgroup validation remains in both paths. Unknown primes
still use the backend, off the runtime thread; a semaphore limits started jobs
to four and cancellation cannot preempt a started CPU calculation.

### Isolated service-timer comparison

22 additional alternating pairs compare the corrected source against the same
source with only the old timer reset predicate restored. Baseline in this table
means that timer control, not `7774ed6`.

| Reply | Control -> selective reset RPC/s | Paired change, 95% interval | Total CPU us/RPC |
|---|---:|---:|---:|
| Plain | 67,375 -> 65,770.5 | -0.64% [-6.19%, +1.71%] | 14.680 -> 14.905 |
| Gzip | 68,094.5 -> 71,487.5 | +2.42% [-0.72%, +7.94%] | 14.435 -> 13.910 |

[Raw pairs and complete intervals](benchmarks/2026-10-30/round1/timer-paired/summary.json)
do not establish the timer as the cause of the earlier slowdown. The selective
reset is retained because moving a deadline later does not require re-registering
the existing earlier wake; the regression checks elapsed/rearmed behavior and
prevents premature ACK flushing. No measured throughput gain is claimed for it.

### Saturated session heap and component controls

**Measured saturated client heap: 7,911 -> 15,887 B/session (+100.8%)**, after
1100 RPCs each across 400 sessions. This is the appropriate headline, not the
earlier nine-RPC percentage. The replay-ID allocation grows from 128 to 8192
bytes between the nine-call and saturated census; other retained capacity is
unchanged in those two corrected snapshots.

| Census point | Baseline B/session | Corrected B/session |
|---|---:|---:|
| Created, disconnected | 2103 | 2143 |
| Connected, one reply | 7911 | 7727 |
| Nine replies | 7911 | 7823 |
| 1100 replies | 7911 | 15887 |

Whole-process peak RSS was 11,432 -> 14,168 KiB. The actual spawned connection
future is **1096 bytes for an 824-byte Sender**, below the duplicate-state
threshold. Relative to the reviewed checkpoint's nine-call 8711 B/session,
the corrected nine-call census is 888 bytes smaller; that is a net correction
comparison, not an isolated A/B of every field's contribution. The replay cache
is retained for correctness and its memory cost remains a review consideration.

Rerun existing component A/B controls (all release, CPUs 0,1, alternating paths;
these optimizations already existed at baseline):

| Proposal / workload | Reference -> retained path |
|---|---:|
| Gzip decode, 512 B plain / 289 B packed | 1546.4 -> 723.5 ns |
| Gzip decode, 2048 / 302 B | 1720.3 -> 925.9 ns |
| Gzip decode, 8192 / 359 B | 2579.0 -> 1700.7 ns |
| Gzip decode, 65536 / 591 B | 14309.5 -> 12328.9 ns |
| Gzip decode, random 2048 / 2071 B | 1112.5 -> 400.7 ns |
| ACK typed allocation versus direct serialization | 109.2 -> 38.8 ns |
| Vec versus shared-body clone, 152 / 1024 / 8192 B | 15.7 / 20.5 / 92.0 -> 14.1 / 13.7 / 14.1 ns |
| Queued versus direct warm route, one thread | 743.6 -> 667.2 ns |
| Queued versus direct warm route, four workers | 20046.2 -> 20090.7 ns |

Four-worker route timing shows no benefit in this sample; shared-body clone is
not a complete serializer benchmark. These results support keeping existing
paths within their measured scope, not attributing new gains to this patch.
Raw values and exact commands are in
[components](benchmarks/2026-10-30/round1/components/commands.json).
The exact 64 MiB gzip fixture decoded 65,250 compressed bytes in 40.692 ms,
process peak RSS 67,640 KiB, and rejected a one-byte-smaller output budget.

Separate allocator-enabled RPC windows remained **2.00 allocations/RPC** for
both versions and both reply types. Observed bytes allocated/RPC were
1851–1854 -> 1835–1842 plain and 1839–1845 -> 1826–1840 gzip; these include
amortized latency sample storage and are not evidence of a representation
saving. Plain/gzip reply bodies remain 1592/528 B; total TCP wire bytes were
not measured. Bounds and checksum validation remain enabled.

### Validation

The final source passed **590 release workspace tests/doctests, 39 ignored**;
all-target/all-feature check, strict Clippy, warning-free Rustdoc and rustfmt.
Minimal client passed 39 tests. Session memory/import/string/file-import
configurations passed 39/61/46/81, with one ignored each. Commands and full logs
are in [validation](benchmarks/2026-10-30/round1/validation/README.md).
No live integration test or hosted build result is claimed.

### Runtime and socket controls

Three executions per configuration, median plain RPC/s, the same 64 connections,
16 concurrent callers, crypto/framing/replies and four responder workers:

| Client arrangement | Baseline | Corrected |
|---|---:|---:|
| Tokio current-thread, CPU 0 | 71,132 | 68,325 |
| Tokio two workers, CPU 0 | 71,474 | 70,589 |
| Tokio two workers, CPUs 0,1 | 87,562 | 84,955 |
| Blocking sockets, CPU 0 | 61,608 | 60,481 |
| Blocking sockets, CPUs 0,1 | 86,939 | 87,888 |

The blocking control is slower on one CPU and only 3.45% ahead of corrected
Tokio on two CPUs in these three medians; it also omits SDK routing, admission
and async request state. It does not isolate a runtime replacement. Tokio gains
throughput with another CPU here. A claim that Tokio alone is the bottleneck,
or that replacing it would improve this library, is unsupported by these controls.

The corrected current-thread client measured 6.81 us user + 7.69 us system/RPC
in these runtime cases. Blocking measured 5.853 + 10.523 us on one CPU, and
5.008 + 13.089 us on two CPUs: kernel work remains substantial without Tokio on
the client. Multi-worker totals (including startup/shutdown) were median
0.663 s user + 0.842 s system on one CPU and 1.144 s + 1.246 s on two CPUs.
Those totals are not window-matched per-RPC values. Each worker's actual split
is preserved in [raw logs and summary](benchmarks/2026-10-30/round1/scenarios/runtime-summary.json).
Whole-process system time includes the responder and cannot all be assigned
to the client runtime.

Separate `strace -f -c` runs confirm active socket and scheduler paths. Current
Tokio recorded 12,308 sendto / 12,500 recvfrom / 5774 epoll_wait / 3945 futex
calls; two-worker Tokio 23,930 / 24,123 / 9868 / 12,161; blocking control
36,276 / 36,343 / 2135 / 5304. Counts include responders and different numbers
of completed RPCs under tracing, so they are diagnostic rather than rates.
Trace time is heavily perturbed; futex/epoll/recv blocking durations are **not
kernel CPU attribution**. Both SDK runs also contain 192 child waits and 193
successful execs (including the test process), absent from the blocking control:
`ConnectionParams::default` calls `os_info::get`, whose locked Linux backend
probes `lsb_release` on pool creation. This startup work is outside RPC windows
but inside whole-process CPU/RSS. Caching OS discovery was not evaluated here.
The original 59% whole-process system-time figure therefore mixes socket work,
server scheduling and initialization; it cannot diagnose Tokio. `perf` is
unavailable, so instruction/stack attribution remains unmeasured.

### Scheduled arrivals, multiple DCs and large replies

Each row below is one baseline/corrected pair, **not a confidence interval**.
64 pools, two DCs, shared busy pool versus rotating quiet pools, 1024 maximum
in-flight tasks. Latency starts at intended arrival time. Drops below are the
harness rejecting arrivals at its capacity, not lost SDK requests. Errors and
timeouts are excluded from successful latency and counted explicitly.

| Scenario | Completed baseline -> corrected | Errors | Harness drops | Peak in-flight | Busy / quiet p99 ms, baseline -> corrected |
|---|---:|---:|---:|---:|---|
| Warm 20k/s; 30k scheduled | 30,000 -> 30,000 | 0 -> 0 | 0 -> 0 | 141 -> 41 | 5.02 / 5.54 -> 2.52 / 2.57 |
| Cold 20k/s; 30k scheduled | 29,749 -> 29,602 | 0 -> 0 | 251 -> 398 | 1024 -> 1024 | 47.53 / 85.18 -> 58.66 / 70.90 |
| Overload 120k/s; 180k scheduled | 87,880 -> 76,691 | 0 -> 0 | 92,120 -> 103,309 | 1024 -> 1024 | 28.21 / 29.75 -> 80.79 / 24.93 |
| Cold mixed 1 MiB/gzip, 100/s; 150 scheduled | 74 -> 150 | 76 -> 0 | 0 -> 0 | 2 -> 2 | Not comparable: baseline rejects large replies |
| Cold mixed 1 MiB/gzip, 2k/s; 3000 scheduled | 1500 -> 1912 | 1500 -> 0 | 0 -> 1088 | 306 -> 1024 | Not comparable: baseline rejects large replies |

The overload result exposes a tradeoff: the corrected build completed fewer
requests and the busy pool's tail worsened while quiet pools' tail improved.
Cold startup still reaches harness capacity. These results do not establish
universal fairness or throughput gains and should guide subsequent profiling.
The high-rate large-reply case is deliberately overloaded: corrected busy/quiet
p99 was 1636/1342 ms, with 1088 rejected arrivals. At 100/s, all 150 corrected
responses succeeded with p99 9.54/9.23 ms. Baseline successful-only latency would
hide its rejection of every 1 MiB response, so it is not compared as a speedup.

Whole-process peak RSS for the large burst was 77,776 -> **412,008 KiB**;
the low-rate large case was 15,036 -> 90,332 KiB. The process includes both
responders, retained grown connection buffers and concurrent response bodies.
Per-packet decode budgets and outbound byte admission are not a global inbound
memory cap across pools. ACK-only, salt-prefetch and exact gzip-limit behavior
are exercised by targeted protocol tests; these RPC scenarios are not claimed
to exercise every service state or worst-case decompression shape.

Closed-loop throughput and CPU/RPC are coupled under CPU saturation. Their
agreement is not independent causal evidence. Closed-loop sampled percentiles
have coordinated omission; only the scheduled-arrival scenarios include waiting
from intended arrival time. Those scenarios report rejected work and errors
separately from successful latency, retain bounded in-flight work, and use
millisecond-granularity Tokio timers. RSS includes the local responder; allocation
census excludes responder threads. No traffic reaches Telegram.

Independent acceptance remains required; round two requested further changes.
The historical upstream
and compatibility dispositions remain in the main ledger; no claim of all
possible optimizations is made.
