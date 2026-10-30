# October 2026 optimization implementation and review ledger

This continues the retained joint research, starting at
`7774ed666317d981e1dfea0d12311cbd38acb43e`. The implementation worktree is
`duo/1cb9a1ed/impl`, with `origin` verified as
`https://github.com/xD-Mohamed/grammers.git`. No consumer repository, account,
Telegram service, or deployment is part of this work.

## Evidence and review provenance

The supplied handoff records completed Astra/Opus research and reciprocal
criticism in task `ba4d0f92-8e56-4a95-bcbc-83f56066fb91`, run
`7197ce19-17fe-46e0-a1e6-db0ca8413c52`. Individual legacy prose was not persisted;
the repaired report reader cannot recover it. This document records decisions
verified against code and primary sources, not reconstructed missing reports.

Current implementation task: `064ae123-8016-4266-bde6-06f0c900156b`, Duo run
`1cb9a1ed-90ad-467c-9f5d-ab302a4b40ee`. The task configuration reports
`gpt-6-astra`, requested `xhigh`. This is configuration evidence, not independent
native confirmation of effort. The orchestrator will run the independent
`opus` / `claude-opus-5-5` reviewer on the real diff after this coding turn.
No new review receipt or acceptance existed when implementation began.
Publication remains gated on that review and correction rounds; this coding
turn must not commit or push.

## Scope matrix

| Retained finding | Implementation / disposition | Verification and remaining questions |
|---|---|---|
| P0 factorization termination | Add `try_factorize`, reject primes/non-semiprimes, bound all rho loops/restarts; authentication uses checked path. Legacy tuple-returning `factorize` remains with documented panic on failure. | Small/composite/prime/u64 boundary corpus; existing three large PQ vectors; child-process regression killed/reaped after 10 seconds. |
| P0 DH input validation | Bound encrypted answer before slicing, validate hash and padding, safe 2048-bit prime/subgroup before modular arithmetic, bound public values and checked time subtraction. Harden shared `check_p_and_g` against invalid generators and entropy errors. | Existing exact successful handshake vector; short ciphertext, valid-hash invalid-DH fixtures. Unknown valid primes still use existing primality backend. |
| P0 outbound accounting | Reserve all encrypted/header/container/padding bytes under `0xff000`; automatic wrapper included; all requests, ACKs and salt requests use capacity checks. | Exact request limit with compression on/off and update wrapper; retained ACK IDs/deadline across full packets; batches max 8192. Conservative uncompressed limit is now 1,044,372 bytes, minus automatic wrapper. |
| P0 inbound limits | Remove outbound-derived inbound body cap; require constructor-sized aligned bodies within the enclosing slice. | Real TL `rpc_result(upload.file)` with 1 MiB bytes: 1,048,604-byte body. Transport receive cap unchanged. |
| P0 gzip resources | Configurable 64 MiB/member, 128 MiB cumulative/packet, depth 32, work 4096 defaults. Charge failed decode work too; iterative body unwrapping releases owned parents. | Exact-limit CRC/footer, over-limit expansion, trailing data/recovery, deep wrappers, sibling cumulative budget, hidden nested simple containers. These are resource policies, not protocol size guarantees. |
| P0 authenticated envelopes | Check session, padding, positive odd IDs, nonnegative sequence, container count (1024), complete inner envelope layout/order; bounded recent-ID set suppresses replays and re-ACKs duplicates. Gzip bodies reuse their one authenticated envelope. | Cache adds up to 8 KiB of retained IDs/session, allocated on use. Timestamp rejection remains conditional work: a reused key does not prove local clock synchronization; bad-time service messages must remain usable. |
| P0 input-triggered panics | Wrong session, unsupported service replies, malformed plain lengths, raw `Sender` requests, short RPC replies and responses to serialized-but-unsent requests become errors. | Sender permit-release/cancellation tests; malformed container and length tests; protocol errors scoped to the connection or addressed invocation. |
| P0 delivery/retry certainty | Share original connection error through `Arc`; distinguish not-sent from sent/partially-written packets. Default retry accepts only not-sent connection failures. Typed transport inspection preserves cached-key 404 fallback through the new wrappers. Keep custom retry policies explicit. | Partial/complete/no-write scenarios, original OS error inspection and shared identity, bounded definitive protocol rejections; local transport-404 fixture must observe a fresh plaintext ReqPqMulti. No automatic replay after ambiguous delivery. |
| P1 service handling | Plain phase has no automatic keepalive. ACK-only packets get an idle deadline on the retained sender timer; one salt-prefetch does not block later user RPCs. At most one outstanding ping, 65,536 pending ACK IDs, finite salt/protocol rejection retries. Optional valid-packet liveness deadline complements initialization timeout. Config persistence is outside the network initialization deadline. | Paused-time service/keepalive/liveness tests; timer cancellation, ping retirement, ACK backlog/wire compatibility/deferral; silent connections. Salt expiry and the storage/deadline boundary remain review targets. |
| P1 pool admission/fairness | Per-DC single-flight setup tasks outside control runner; failures shared with waiters; cancellation/shutdown owns tasks; generation identity rejects stale completions. Serialize config/key merges under a cold-path async mutex. Optional request-byte admission travels with SDK permits. Cooperative runner work budgets. | Same-DC shared setup failure while another DC responds; caller cancellation and quit regressions; byte limit and release tests. Cross-pool fairness still depends on caller/runtime scheduling. |
| P1 updates and client input | Out-of-band home failure generation wakes `UpdatesReceiver` even when updates queue is full. Catch-up alternates account/channels and rotates channel pages. Deduplicate saved channels, preserve older position, widen counters. Checked loading/account/channel recovery rejects invalid state without mutation; legacy loading repairs missing common state. Invalid mentions remain ordinary text links; empty/incomplete albums fail locally; download offsets/upload part counts checked. | Queue-overflow notification, saved-state/counter and catch-up tests, malformed HTML/Markdown, media input regressions. Legacy low-level message-box methods retain preconditions; the normal updates receiver uses the checked paths. |
| P2 micro-optimizations | No runtime replacement, lookup rewrite, compression-policy or buffer rewrite without independent evidence. Existing AES-IGE, shared bodies, gzip reuse, queue and routing optimizations retained. | Release local/mock baseline and final comparison described below. Correctness checks can cost CPU/memory; no universal speedup claim. |
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

## Reproducible local measurements

Host: Linux x86_64, AMD Ryzen 9 7950X3D; rustc 1.99.1
(`58cf8f9c3`, 2026-10-13), cargo 1.99.1, locked workspace dependencies, default
release optimization (no `target-cpu=native`). Cargo was not on the initial PATH;
commands use `PATH=/root/.cargo/bin:$PATH`.

Before production edits, built the existing loopback harness with only the new
1/64 latency sampling instrumentation. Preserved that executable as
`/tmp/grammers-064ae123/baseline-loopback`. Workload: 64 pools, 16 callers,
4-second windows, real framing/AES-IGE/serialization, plain and gzip replies,
alternating allocator instrumentation; responder uses four threads. No Telegram
endpoint or credential. Linux's reported thread "user" field is actually total
thread CPU; its kernel/cycle fields are unavailable, not measured zero cost.

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
or substantiate a runtime rewrite. No runtime change is justified by the
retained evidence.

The first paired candidate is retained under
`benchmarks/2026-10-30/first-candidate/`, including unfavorable measurements.
It used a transient additional service timer. Three alternating pairs measured
plain throughput 60,346 -> 49,015 RPC/s and gzip 62,640 -> 58,580 RPC/s
(medians of nine untracked windows per reply type). The final candidate reuses
one pinned timer for service, keepalive and liveness deadlines, adds the ACK
backlog bound and single outstanding ping, and keeps config writes outside the
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

## Final validation

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
this document. Resolve blocking findings and re-run affected checks. No commit
or push belongs to the first implementation turn.

Exact handoff: branch `duo/1cb9a1ed/impl`, worktree
`/var/lib/helos/workspaces/duo/1cb9a1ed-90ad-467c-9f5d-ab302a4b40ee/impl`,
base/HEAD `7774ed666317d981e1dfea0d12311cbd38acb43e`; implementation and artifacts
are uncommitted. No PR, remote branch, commit, push or deployment was created.

Remaining-work ledger for the review/completion continuation:

1. Run the configured independent Opus review against this real diff and raw
   measurements. Persist its complete report and both native runner receipts
   with the Duo run; requested `xhigh` must not be described as independently
   native-confirmed effort. Current implementation acceptance is **pending**.
2. Prioritize delivery-aware error/fresh-key compatibility, per-DC setup
   cancellation/generation races and storage timing, ACK/salt semantics,
   envelope/replay validation, gzip budgets, and checked message-box recovery.
   Timestamp rejection with an uncertain clock is explicitly deferred; legacy
   low-level message-box APIs still require their documented valid states.
3. Challenge the default resource budgets and measured CPU/heap cost. P2 runtime,
   packing, lookup, serialization, buffer and lock rewrites remain unadopted
   without isolated evidence. No claim of all possible optimizations is made.
4. Resolve blocking review findings, run affected verification and receive a
   substantive reciprocal review. If two rounds leave blockers, record them
   for correction/review continuation; do not mark accepted or publish.
5. Any later publication must follow the owner's applicable authorization and
   review gate, verify the intended fork/branch, use a normal push, and compare
   the remote SHA. This first coding turn performs neither commit nor push.
