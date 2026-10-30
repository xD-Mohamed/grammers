# Second-review correction ledger

The independent review inspected checkpoint
`5b16ac41ae9d6b0cdd0bf42df50d6064561bc19e` and requested changes. This ledger
records the correction continuation; it does not assert review acceptance or
publication. Source/build hashes identify the measured correction separately
from its eventual integration commit. The [full review](benchmarks/2026-10-30/round2/claude-review.md),
[preceding implementation report](benchmarks/2026-10-30/round2/astra-round1-report.md)
and [provider receipts](benchmarks/2026-10-30/round2/provenance.json) are retained.
Native identities are accepted `gpt-6-astra` and `claude-opus-5-5`; `xhigh`
remains requested effort, not independently reported native effort.

## Finding disposition

| Finding | Action | Evidence / limits |
|---|---|---|
| 1 | Accepted. Add recursive JSON exceptions for round-one and round-two artifacts; add all 18 previously ignored round-one JSON files to the index. | A clean `git checkout-index` export resolves 54 local document links and contains all 30 round-one/two JSON files. Round-one source hashes match checkpoint 5b16ac4; round-two hashes match the clean export. Artifact checks include Git membership, not just filesystem existence. |
| 2 | Accepted. Close the failed connection's queue, drain unframed calls with the same underlying ReadError Arc and `may_have_been_sent: false`, and release each permit before notifying its caller. | A real half-closed loopback socket with 512 queued RPCs verifies source identity, certainty, AutoSleep's decision, route closure and permit release. Sent/partially written calls retain their existing uncertainty. |
| 3 | Accepted. Remove the full response pre-scan. Existing request lookup and bad-message arms return a typed protocol error for Serialized requests. | All four response/error variants retain unsent metadata until teardown. Repeated overload comparisons below include a same-source control that restores only the pre-scan. |
| 4 | Accepted runtime defect. Both authentication step2 and step3 execute in the shared, four-job blocking pool. | Current-thread progress test executes real step2 on a balanced 64-bit semiprime. A separate full 1,250,000-step rho-budget control has a process-enforced deadline and release measurements below. See the explicit distinction between that control and publicly accepted PQ inputs. |
| 5 | Accepted. Clear GetState readiness only after the RPC, storage write and in-memory set_state succeed. | Cancel next after GetState reaches the queue, then fail storage, then retry successfully. Neither cancellation nor failure loses readiness. |
| 6 | Accepted operational concern. Execute each requested public test-DC probe once. Keep child sequence > parent as a warning pending representative container evidence. | Both probes pass, with no account login. Their logs do not establish an observed inbound container. ID ordering, nonnegative sequence, session, padding, length and resource validation remain strict. |
| 7 | Accepted. Apply the cached self-user guard on home-generation recovery, after cancel-safe storage lookup. | Logged-out home failure delivers a non-pts update without requesting GetState; the logged-in generation regression explicitly caches a self user. |
| 8 | Accepted. Register closure wakers for new waiters only; a dirty flag triggers a rescan on cancellation. Clear the stored runner waker when setup retires. | A 20,000-arrival test records 20,000 closure polls; an idle pass adds none, a closure triggers one rescan. Existing 200-caller cancellation/partial-cancellation tests still cover real runner teardown. Release old/new scan controls below. |
| 9 | Accepted. Determine account readiness from the Common entry independently of saved channels; set_state preserves channel entries/deadlines. | `{pts:0,qts:5,channels:[...]}` triggers GetState for a logged-in session and preserves channels in storage and pending channel recovery. |
| 10 | Accepted. After the fourth salt-prefetch rejection, disable further optional prefetch for this connection instead of failing unrelated RPCs. | User RPC still frames with the current salt; a later bad_server_salt updates it and reaches the appropriate request. |
| 11 | Accepted documentation overstatement. Narrow the original mutation claim to its actual single rpc_result seed. | Container/gzip/service behavior has separate example and resource-limit regressions; no claim of a multi-shape mutation corpus or coverage-guided fuzzing. |
| 12 | Accepted stale metadata. Identify round-one source as checkpoint 5b16ac4 and make its diff command use explicit commits. Remove transient “this turn/uncommitted” assertions from historical reports. | Recomputed `git diff 8f109a6 5b16ac4` code hash matches the original measurement manifest. This continuation must not invent a future commit SHA or perform the prohibited commit. |

## Public test-DC verification

The existing `test_no_updates_initialization_on_public_test_dc` and
`test_invoke_encrypted_method` each ran once against Telegram's public test DC,
using their existing read-only GetNearestDc/initialization flow and no account
login. Both passed (1.21 s and 1.14 s). Each child process had an external
40-second deadline and a two-second kill grace. No proxy test, account session,
stress workload or repeated live retry was run.

[Live metadata](benchmarks/2026-10-30/round2/live-tests.json) identifies the
binary and commands; [live-source.patch](benchmarks/2026-10-30/round2/live-source.patch)
preserves its exact production diff from 5b16ac4. These probes ran before the
child-sequence rule was relaxed; the subsequent change only warns for that one
ordering violation. This small sample does not establish production container
compatibility. It supersedes the round-one decision to reject child seq > parent
despite the documented protocol rule. Other envelope validation is unchanged.

## Factorization work and cancellation

Started factorization/DH jobs cannot be forcibly cancelled by an async timeout.
The shared semaphore retains each job's slot until its blocking closure returns;
queued jobs remain cancellable, and network initialization can time out while a
bounded CPU job finishes. No unbounded blocking pool expansion is introduced.

The full-budget cost control calls the actual internal rho loop with a prime
which exhausts all 1,250,000 polynomial steps. Public `try_factorize` rejects
that prime in its preliminary primality check, so it is **not** presented as a
hostile handshake PQ that reaches the default budget. The current-thread test
instead uses the real step2 path with a balanced 64-bit semiprime and checks
runtime progress. This distinction avoids inventing a default-budget-exhausting
public fixture. The full-budget test is also invoked inside the killed/reaped
factorization subprocess regression. Wall-clock maxima remain host-dependent.

Five release full-budget controls measured **14.871 ms median, 15.081 ms maximum**
(range 14.519–15.081 ms), pinned to CPU 0. This measures the full rho loop, not
an end-to-end handshake or a universal worst-case wall-clock bound. The review's
rough hundreds-of-milliseconds estimate was not observed on this host. Raw
[samples](benchmarks/2026-10-30/round2/factorization-budget.log) and
[commands/binary hashes](benchmarks/2026-10-30/round2/component-commands.json)
retain the distinction between this control and the early public prime check.

## Cold-waiter bookkeeping

Three alternating release pairs per size, same allocations/grouping, open
receivers, no network. Medians of arrival-plus-poll time:

| Waiters | Previous poll-all expression | Closure-wake/new-suffix path |
|---:|---:|---:|
| 1024 | 3.277 ms | 0.149 ms |
| 20,000 | 1324.423 ms | 3.876 ms |

The deterministic test separately counts 20,000 closure polls for 20,000 arrivals
and zero additional polls on an idle pass. One closure wake triggers a rescan;
the new path does not traverse the old prefix on ordinary arrivals.
These [component results](benchmarks/2026-10-30/round2/cold-waiters.log) do not
measure handshake speed or a worst-case cancellation storm. Cancellation still
uses a full rescan when notified, and the existing caller-only/partial-cancel
runner tests exercise cooperative teardown.

## Repeated overload and isolated response-scan control

Twelve alternating release pairs compare historical baseline `7774ed6` with the
corrected source. A separate twelve pairs restore only the removed full response
pre-scan in that same corrected source. Harness, release settings and affinity
are identical: 64 pools, two DCs, 1024 in-flight cap, 180,000 scheduled arrivals
over 1.5 seconds. Latency includes time since intended arrival, and refused
arrivals are counted as harness drops. See [reproduction and limitations](benchmarks/2026-10-30/round2/README.md).

Baseline → corrected, execution medians; percentages are median **paired** ratios
and therefore need not equal ratios of the displayed medians:

| Metric | Baseline | Corrected | Paired change [95% bootstrap interval] |
|---|---:|---:|---:|
| Completed RPCs | 110,194.5 | 99,126.5 | −8.21% [−14.80%, −2.20%] |
| Harness drops | 69,805.5 | 80,873.5 | +12.28% [+3.35%, +24.25%] |
| Busy pool p50 | 13.140 ms | 22.869 ms | +71.10% [+54.65%, +87.15%] |
| Busy pool p95 | 17.906 ms | 44.477 ms | +137.18% [+129.03%, +176.83%] |
| Busy pool p99 | 20.599 ms | 50.572 ms | +135.52% [+119.04%, +184.11%] |
| Quiet pools p50 | 14.828 ms | 7.999 ms | −46.87% [−49.40%, −42.78%] |
| Quiet pools p95 | 19.247 ms | 11.591 ms | −42.67% [−48.44%, −30.64%] |
| Quiet pools p99 | 22.163 ms | 14.353 ms | −38.17% [−44.08%, −18.40%] |
| Whole-process peak RSS | 11,450 KiB | 11,396 KiB | −0.80% [−1.19%, +0.60%] |

Every execution reached the 1024 cap and recorded zero RPC errors. Drops occur
in the harness before invocation; they are not successes and do not enter the
latency distribution. The throughput and busy-pool tail regressions persist;
the quiet-pool tail improves. These repeated data bound the regression on this
shared host, rather than attributing it to the pre-scan from one noisy pair.
They do not establish a universal bound, improved sustainable capacity or a
whole-system memory reduction. The separate saturated-session replay cost
remains as recorded in round one.

Same-source pre-scan → removed-scan control:

| Metric | Pre-scan median | Removed median | Paired change [95% bootstrap interval] |
|---|---:|---:|---:|
| Completed RPCs | 97,419 | 95,863.5 | +4.53% [−8.01%, +7.37%] |
| Busy pool p99 | 53.299 ms | 54.517 ms | −3.50% [−9.77%, +18.40%] |
| Quiet pools p99 | 15.085 ms | 15.551 ms | −1.04% [−24.30%, +25.06%] |

This isolated comparison does **not** resolve a speed or tail improvement from
removing the redundant scan. It cannot support the scan as the established cause
of the broad regression. The change is retained because validation belongs at
the existing lookup and duplicating that traversal is unnecessary. Different
fairness scheduling and retained protocol checks are plausible contributors to
the busy/quiet tradeoff, but are not isolated by this experiment. Further causal
profiling is a remaining performance investigation, not a claimed result.

Complete samples, command/binary provenance and bootstrap summaries:
[baseline comparison](benchmarks/2026-10-30/round2/overload/summary.json),
[scan control](benchmarks/2026-10-30/round2/scan-effect/summary.json).
Intervals use execution pairs as sampling units (10,000 fixed-seed bootstrap
draws); they are not corrected for multiple comparisons or shared-host drift.

## Validation

**599 release workspace tests/doctests passed, 41 ignored**; strict Clippy,
all-target/all-feature check, warning-free Rustdoc and rustfmt passed. Minimal
client passed 39 tests; session memory/import/string/file-import configurations
passed 40/62/47/82, one ignored each. Complete commands and logs are in
[validation](benchmarks/2026-10-30/round2/validation/README.md). Two requested live
test-DC probes and named offline component benchmarks are recorded separately;
these totals do not imply that all ignored tests ran or hosted CI passed.

## Remaining review gate

Two review rounds requested changes. The correction diff, exact source/binary
hashes, complete logs and this disposition ledger are the durable handoff for
the next independent correction review. No push is appropriate before blocking
findings are resolved and accepted. Reconfirm queue/error certainty, wake races,
account-state recovery and the repeated overload evidence. The retained replay
cache still costs 8 KiB at saturation; the reviewer explicitly accepted keeping
it. Unevaluated P2 proposals and deferred upstream/schema work remain listed in
the main scope matrix; no universal optimization or runtime-replacement claim
is made.
