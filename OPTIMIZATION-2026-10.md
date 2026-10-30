# October 2026 optimization implementation and review ledger

This continues the retained joint research from baseline
`7774ed666317d981e1dfea0d12311cbd38acb43e`. The reviewed implementation checkpoint
is `8f109a67458acf35f278451ee6d32c0acaab80e3`; round-one corrections are recorded
in [REVIEW-ROUND1.md](REVIEW-ROUND1.md). The verified remote is
`https://github.com/xD-Mohamed/grammers.git`. No consumer repository or Telegram
account is involved.

## Evidence and review provenance

Earlier joint research survives only as the supplied retained summary. The
report-reader repair cannot restore missing legacy prose. New implementation
and review reports are available in full, including native model receipts:
[implementation](benchmarks/2026-10-30/round1/astra-implementation.md),
[review](benchmarks/2026-10-30/round1/claude-review.md), and
[provider evidence](benchmarks/2026-10-30/round1/provenance.json).
The runner accepted native `gpt-6-astra` and `claude-opus-5-5` (alias `opus`).
Both requested `xhigh`; neither receipt independently confirms native effort.
Claude's first implementation review requested changes and did not run builds.
The orchestrator checkpointed round-one corrections as `5b16ac4`. The second
review requested further changes, tracked in [REVIEW-ROUND2.md](REVIEW-ROUND2.md).
The orchestrator checkpointed those corrections as `5f0cb05`; the latest
corrections are in [the acceptance ledger](benchmarks/2026-10-30/final/acceptance-review.md).
Exact-tree acceptance/publication receipts are external under [PUBLICATION.md](PUBLICATION.md).

## Scope matrix

| Retained finding | Implementation / disposition | Verification and remaining questions |
|---|---|---|
| P0 factorization termination | Add `try_factorize`, reject primes/non-semiprimes, bound all rho loops/restarts; authentication uses checked path. Legacy tuple-returning `factorize` remains with documented panic on failure. | Small/composite/prime/u64 boundary corpus; existing three large PQ vectors; child-process regression killed/reaped after 10 seconds. |
| P0 DH input validation | Bound encrypted answer before slicing, validate hash and padding, safe 2048-bit prime/subgroup before modular arithmetic, bound public values and checked time subtraction. Harden shared `check_p_and_g` against invalid generators and entropy errors. | Existing exact successful handshake vector; short ciphertext, valid-hash invalid-DH fixtures. The published Telegram prime skips only primality; all generators retain subgroup checks. Unknown primes use the existing primality backend in a bounded blocking job. |
| P0 outbound accounting | Reserve all encrypted/header/container/padding bytes under `0xff000`; automatic wrapper included; all requests, ACKs and salt requests use capacity checks. | Exact request limit with compression on/off and update wrapper; retained ACK IDs/deadline across full packets; batches max 8192. Conservative uncompressed limit is now 1,044,372 bytes, minus automatic wrapper. |
| P0 inbound limits | Remove outbound-derived inbound body cap; require constructor-sized aligned bodies within the enclosing slice. | Real TL `rpc_result(upload.file)` with 1 MiB bytes: 1,048,604-byte body. Transport receive cap unchanged. |
| P0 gzip resources | Configurable 64 MiB/member, 128 MiB cumulative/packet, depth 32, work 4096 defaults. Charge failed decode work too; iterative body unwrapping releases owned parents. | Exact-limit CRC/footer, over-limit expansion, trailing data/recovery, deep wrappers, sibling cumulative budget, hidden nested simple containers. These are resource policies, not protocol size guarantees. |
| Generated TL recursion | **Deferred, known availability risk.** MTProto/gzip budgets do not bound recursive generated TL values such as `RichText::Bold` inside a cached webpage. | A valid authenticated peer can deliver a deeply nested value within the packet cap, including content influenced by remote users. Recursive decoding can overflow the process stack; checked runners cannot contain that abort. A coordinated generated-decoder depth budget and safe partial-value cleanup/drop strategy require separate implementation and compatibility testing. Neither the packet cap nor wrapper depth is claimed to solve this. Opus finding 3 explicitly permits a recorded deferral; final acceptance must account for this remaining risk. |
| P0 authenticated envelopes | Check session, padding, positive odd IDs, nonnegative sequence, container count (1024), complete inner envelope layout/order; bounded recent-ID set suppresses replays and re-ACKs duplicates. Gzip bodies reuse their one authenticated envelope. | Cache adds up to 8 KiB of retained IDs/session, allocated on use. Timestamp rejection remains conditional work: a reused key does not prove local clock synchronization; bad-time service messages must remain usable. |
| P0 input-triggered panics | Wrong session, unsupported service replies, malformed plain lengths, raw `Sender` requests, short RPC replies and responses to serialized-but-unsent requests become errors. | Sender permit-release/cancellation tests; malformed container and length tests; protocol errors scoped to the connection or addressed invocation. |
| P0 delivery/retry certainty | Share original connection error through `Arc`; distinguish not-sent from sent/partially-written packets. Default retry accepts only not-sent connection failures. Typed transport inspection preserves cached-key 404 fallback through the new wrappers. Keep custom retry policies explicit. | Partial/complete/no-write scenarios, original OS error inspection and shared identity, bounded definitive protocol rejections; local transport-404 fixture must observe a fresh plaintext ReqPqMulti. No automatic replay after ambiguous delivery. |
| P1 service handling | Plain phase has no automatic keepalive. ACK-only packets get an idle deadline on the retained sender timer; one salt-prefetch does not block later user RPCs. At most one retained keepalive record; a sent ping is replaced at the next interval even if its pong is delayed. At most 65,536 pending ACK IDs, finite salt/protocol rejection retries. Optional valid-packet liveness deadline complements initialization timeout. Config persistence is outside the network initialization deadline. | Paused-time service/keepalive/liveness tests; timer cancellation, ping retirement, ACK backlog/wire compatibility/deferral; silent connections. Salt expiry and the storage/deadline boundary remain review targets. |
| P1 pool admission/fairness | Per-DC single-flight setup tasks outside control runner; failures shared with waiters; cancellation/shutdown owns tasks; generation identity rejects stale completions. Serialize config/key merges under a cold-path async mutex. Optional request-byte admission travels with SDK permits. Cooperative runner work budgets. | Same-DC shared setup failure while another DC responds; caller-only 200-waiter cancellation, partial cancellation, disconnect and stale-generation regressions; byte limit and release tests. Cross-pool fairness still depends on caller/runtime scheduling. |
| P1 updates and client input | Out-of-band home failure generation wakes `UpdatesReceiver` even when updates queue is full. Catch-up alternates account/channels and rotates channel pages. Deduplicate saved channels, preserve older position, widen counters. Strict loading/account/channel recovery rejects invalid state without mutation; normal stream loading repairs invalid saved rows and missing common state with warnings. Invalid mentions remain ordinary text links; empty/incomplete albums fail locally; download offsets/upload part counts checked. | Queue-overflow notification, saved-state/counter and catch-up tests, malformed HTML/Markdown, media input regressions. Legacy low-level message-box methods retain preconditions; the normal updates receiver uses repairing `load` for saved state and checked response-application paths. |
| P2 micro-optimizations | Published-prime cache, duplicate future removal and selective timer reset adopted. Existing shared-body/gzip/ACK/routing A/B controls rerun. New compression, packing, lookup, serializer and allocator designs are **not evaluated**, not rejected by evidence. | The [correction matrix](REVIEW-ROUND1.md) distinguishes evaluated components and unevaluated proposals; paired RPC, runtime, open-loop and saturated-heap data bound the measured scope. |
| P3 upstream intake | Inspect canonical Codeberg through `1e31e73cd9797790905bbea82cd75adcbe6f1242`; adapt `ad7b4725` accounting and `1e31e73` sender validation, preserve deferred ACKs and separate inbound cap. | No upstream merge. Schema 232, dependency/version changes, sans-I/O/transport rewrites and media API changes remain isolated intake candidates, not prerequisites for these fixes. |

## Primary sources and upstream decisions

- [Telegram authentication requirements](https://core.telegram.org/mtproto/auth_key):
  safe prime and generator validation precede DH exponentiation; public values
  also need the recommended 1984-bit safety margin.
- [Encrypted-message security checks](https://core.telegram.org/mtproto/security_guidelines):
  authenticated padding, session, ID parity and duplicate suppression are
  separate from AES/msg-key verification. A client must allow time-correction
  messages when its clock is uncertain.
- [Service messages](https://core.telegram.org/mtproto/service_messages): simple
  containers cannot nest; at most 1024 inner messages and 8192 IDs per ACK.
- [File download limits](https://core.telegram.org/api/files): a file part may
  contain 1 MiB. Outbound reset limits cannot constrain that response body.
- [Upstream accounting change](https://codeberg.org/Lonami/grammers/commit/ad7b4725f7ad62a93ddd251035405b628e1bed1e):
  its encrypted-payload ceiling is an upstream observation, not a measurement
  against Telegram here. Its take-before-fallible-ACK-serialization and inbound
  assertion were deliberately not copied.
- [Upstream request validation](https://codeberg.org/Lonami/grammers/commit/1e31e73cd9797790905bbea82cd75adcbe6f1242):
  adapted to the fork's existing typed invalid-input error categories and
  unchanged `Mtp::push` signature. New default validation/service hooks keep
  third-party `Mtp` implementations source-compatible.

The other 15 newer upstream commits cover layer 232, version/dependency bumps,
media TTL behavior, sans-I/O extraction, configurable transports, DequeBuffer
relocation/fast path, MsgId conversion, Debug/logging/docs and public upload_media.
They were inventoried; those architecture/schema changes were not silently
merged into this fork's layer 230 API. Existing slice extension already delegates
to vector/slice paths; a second rewrite has no demonstrated gain here.

Exact intake inventory (newest first): `1e31e73` sender validation, `ad7b472`
buffer accounting, `58f7bd1` public upload_media/clone, `4ca2263` buffer docs,
`d654b04` buffer relocation, `84cfb99` remove crypto buffer use, `dad5581`
unused dependency removal, `98b7b88` Debug, `07b51ab` logging typo, `c8d6979`
MsgId conversion, `4101f5e` configurable transport, `e564eef` slice extension,
`a984cac` sans-I/O extraction, `dfffb71` layer 232, `15b7afa` version,
`5b80079` dependencies, `6ce4c54` TTL semantics.

The [canonical Git source audit](benchmarks/2026-10-30/final/upstream/README.md)
now verifies the official remote tip, parent/tree/diff hashes and the challenged
patches, and explains each deferred intake item from its actual source.

## Historical measurements at the reviewed checkpoint

These measurements preceded the corrections. Current results and reproducible
commands are in [REVIEW-ROUND1.md](REVIEW-ROUND1.md) and
[round1/README.md](benchmarks/2026-10-30/round1/README.md).

Host: Linux x86_64, AMD Ryzen 9 7950X3D; rustc 1.99.1
(`58cf8f9c3`, 2026-10-13), cargo 1.99.1, locked workspace dependencies, default
release optimization (no `target-cpu=native`). Cargo was not on the initial PATH;
commands require the installed Cargo toolchain on PATH.

Before production edits, built the existing loopback harness with only the new
1/64 latency sampling instrumentation. Preserved that executable as
`<artifacts>/baseline-loopback`. Workload: 64 pools, 16 callers,
4-second windows, real framing/AES-IGE/serialization, plain and gzip replies,
alternating allocator instrumentation; responder uses four threads. No Telegram
endpoint or credential. Linux's reported thread "user" field is actually total
thread CPU; that original harness did not measure kernel time separately. Round-one measurements use `getrusage(RUSAGE_THREAD)` to report user/system time; cycle counts remain unavailable.

Initial baseline (untracked rounds 0/2/4; median of the three round metrics):

| Reply | RPC/s | Thread CPU us/RPC | p50 / p95 / p99 us | Allocations/RPC |
|---|---:|---:|---:|---:|
| Plain, 1592 B | 60,830 | 15.82 | 231.4 / 444.2 / 764.3 | 2.00 |
| Gzip, 528 B | 62,294 | 15.53 | 211.2 / 448.9 / 832.2 | 2.00 |

Initial heap census, 400 pools: 2,103 B/pool before connection, 7,911 B after
one/nine RPCs. Whole-process high-water RSS 12,884 KiB; CPU 48.70 s user +
61.96 s system over 63.22 s, including responder and heap census. These are not
per-client RSS or CPU values.

No `perf` executable was available. End-to-end loopback and queue benchmarks
can test particular scheduling costs; they cannot attribute all CPU to Tokio
or substantiate a runtime rewrite. The original measurements did not evaluate a runtime replacement. The round-one runtime and blocking-socket controls address that gap with explicit scope limits.

The first paired candidate is retained under
`benchmarks/2026-10-30/first-candidate/`, including unfavorable measurements.
It used a transient additional service timer. Three alternating pairs measured
plain throughput 60,346 -> 49,015 RPC/s and gzip 62,640 -> 58,580 RPC/s
(medians of nine untracked windows per reply type). The final candidate reuses
one pinned timer for service, keepalive and liveness deadlines, adds the ACK
backlog bound and single outstanding ping (subsequently corrected to refresh
sent pings), and keeps config writes outside the
initialization timeout. Comparing these iterations is not an isolated estimate
of the timer change's contribution; final correctness checks also have costs.

Final alternating A/B comparison (three executions/version, nine untracked
windows/reply type; medians of each window's metric, **not** a pooled latency
distribution):

| Reply/version | RPC/s | Client-thread CPU us/RPC | p50 / p95 / p99 us | Allocations/RPC |
|---|---:|---:|---:|---:|
| Plain baseline | 68,504 | 14.47 | 209.3 / 386.3 / 507.3 | 2.00 |
| Plain final | 66,743 | 14.82 | 208.2 / 398.6 / 494.9 | 2.00 |
| Gzip baseline | 70,210 | 14.08 | 193.3 / 388.6 / 502.5 | 2.00 |
| Gzip final | 69,374 | 14.32 | 197.2 / 392.0 / 501.2 | 2.00 |

The final candidate is **2.57%/1.19% slower** on plain/gzip throughput, with
2.42%/1.70% more client CPU/RPC. Plain p95 increased 3.18%; p99 medians are
similar. Keep the correctness/resource/fairness changes for their stated
behavior, subject to independent review of this cost; this is not a claimed
throughput optimization. Host variability is visible in the raw samples and
in the difference between the initial and paired baselines. No statistical
significance or universal latency improvement is established.

Whole-process medians for the paired runs: baseline 38.02 s user + 55.61 s
system and 11,312 KiB peak RSS; final 37.76 s + 54.71 s and 11,316 KiB. These
include the responder and fixed-duration runs completing different amounts of
work, so the lower total CPU is not evidence of cheaper RPCs. Reply bodies stay
1592 B plain / 528 B gzip; this does not measure total TCP bandwidth.

Full raw logs, sample arrays, executable hashes, and reproduction instructions
are in [benchmarks/2026-10-30](benchmarks/2026-10-30/README.md). The final
executable SHA-256 is
`b6412b42b85fe4afb29b10475843b83df81440b283fa8b0bba2c08be0d50768a`.

Separate 400-session heap census (gzip replies):

| Stage | Baseline B/session | Final B/session |
|---|---:|---:|
| Created, disconnected | 2,103 | 2,135 |
| Connected, after one RPC | 7,911 | 8,615 |
| After nine RPCs | 7,911 | 8,711 |

The nine-call increase is 800 B/session (10.1%). This is **not** the saturated
replay-history cost: the final census holds a 128-byte ID allocation then;
unit tests verify growth to 1024 IDs / 8192 bytes. That adds another 8064 bytes
if other capacities remain constant; this is an allocation-size derivation,
not a measured saturated-session census. Before any traffic, history remains
unallocated. Whole-process peak RSS was 11,316/11,328 KiB in the two heap runs.

The default gzip-limit fixture expanded 65,250 compressed bytes to 67,108,864
bytes in **46.69 ms**, accepted output exactly at the 64 MiB limit, and rejected
the same stream with a one-byte-smaller limit. Process peak RSS was **67,680
KiB**, including input construction and both decodes. This is one synthetic
resource-limit check, not an application memory guarantee; the 128 MiB budget
is per packet, not a global cap across pools. An initial wrong exact-test name
matched zero tests; its log is retained and excluded from measurements.

The unchanged queue component benchmark measured median round trips of
**602.8/576.5 ns** (Tokio unbounded/lazy queue) on a current-thread runtime and
**17,489.9/16,933.2 ns** on two workers. Both implementations use Tokio; the
cross-thread gap demonstrates this workload's scheduling/synchronization cost,
not a runtime-wide diagnosis or evidence that replacing Tokio helps.

## Validation at the reviewed checkpoint

The counts below belong to `8f109a6`. The correction rerun passed 590 tests
with 39 ignored, including fresh minimal/optional builds; see
[current validation](benchmarks/2026-10-30/round1/validation/README.md).

- `cargo test --locked --release --workspace --all-features`: **565 distinct
  tests/doctests passed, 36 ignored**. The factorization deadline test also runs
  itself once in a child process; summing result lines counts that rerun twice.
- Workspace all-target/all-feature check and strict Clippy passed. The first
  check received SIGKILL in generated TL types; retry with one build job,
  `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_INCREMENTAL=0` passed. The failure log is
  retained alongside the successful check.
- Warning-free all-feature Rustdoc and formatting check passed.
- Independent release session builds: memory only **37**, string import **59**,
  native strings **44**, file import **79** passed (one ignored each). Minimal
  client library: **39** passed. No feature or dependency changes accompanied
  the subsequent transport-404 accessor fix; final workspace tests cover it.
- Real localhost transport tests cover a 1 MiB download reply, a silent DC
  alongside a healthy cold DC, execute-then-drop-response uncertainty, and a
  rejected stored key starting a fresh plaintext handshake.

Commands, counts and full logs are in
[validation receipts](benchmarks/2026-10-30/validation/README.md). No hosted
Windows/Linux CI, live authorization, Telegram load test or consumer deployment
was observed. The report does not claim those passed.

## Compatibility and remaining review work

The maximum accepted uncompressed request is slightly smaller; an exact old
limit now returns `InvalidInput` instead of producing an over-limit packet.
`InvocationError` and `ReadError` gain variants: exhaustive matches need arms
for shared failures, delivery-aware connection errors and protocol rejection.
Use `InvocationError::root_cause`, `category` and standard `Error::source` for
inspection; `transport_error` exposes framing errors through either wrapper.
Retry policies must explicitly account for uncertain delivery.
Legacy raw Vec/shared APIs, constructors, unlimited low-level admission defaults,
layer 230, session formats and the public media representation remain intact.

Before publication the independent reviewer must check actual diff semantics,
new API/error cases, cancellation races, performance costs, default budgets and
the retained legacy message-box preconditions. Save native provider receipts
and full review artifacts through the Duo runner; do not infer acceptance from
this document. Resolve blocking findings and re-run affected checks.

The orchestrator checkpointed the initial implementation as `8f109a6` and the
first correction as `5b16ac4`. These are historical source identities, not a
claim about the checkout's current HEAD or publication status. Local
execution-location details are excluded from public artifacts.

Remaining-work ledger for the review/completion continuation:

1. Continue independent Opus review against the full cumulative candidate plus
   the second Opus correction recorded in REVIEW-OPUS-ROUND2.md and raw
   measurements. Persist its complete report and both native runner receipts
   with the Duo run; requested `xhigh` must not be described as independently
   native-confirmed effort. Acceptance is recorded outside this tree under PUBLICATION.md.
2. Prioritize delivery-aware error/fresh-key compatibility, per-DC setup
   cancellation/generation races and storage timing, ACK/salt semantics,
   envelope/replay validation, gzip budgets, and checked message-box recovery.
   Timestamp rejection with an uncertain clock is explicitly deferred; legacy
   low-level message-box APIs still require their documented valid states.
3. Challenge the default resource budgets, saturated replay-cache heap and
   overload/fairness measurements. The correction matrix names unevaluated P2
   proposals explicitly; runtime controls do not justify replacing Tokio.
   No claim of all possible optimizations is made.
4. Resolve blocking review findings, run affected verification and receive a
   substantive reciprocal review. If two rounds leave blockers, record them
   for correction/review continuation; do not mark accepted or publish.
5. Any later publication must follow the owner's applicable authorization and
   review gate, verify the intended fork/branch, use a normal push, and compare
   the remote SHA. No review ledger alone constitutes publication approval.


### Post-Opus correction scope

[REVIEW-OPUS-CORRECTION.md](REVIEW-OPUS-CORRECTION.md) supersedes earlier
acceptance claims for the correction that incorrectly rejected zero QTS.
The three historical checkpoints `8f109a6`, `5b16ac4`, `5f0cb05` retain their
original subjects because historical receipts pin those objects. Any eventual
final commit should use a conventional subject, after the applicable review gate.
At that correction checkpoint, independent acceptance and publication were
pending. Later publication follows the owner's applicable instructions and gates.

The receive-arm-only cooperative-budget alternative (finding 13) is deferred:
the current charge covers ready read/write/service iterations as well as the
request arm, and removing those charges can change starvation behavior. The
existing remove-all control establishes a tradeoff, not optimal placement or
historical throughput causation. No scheduling policy change or new performance
claim is made from that unmeasured proposal.
