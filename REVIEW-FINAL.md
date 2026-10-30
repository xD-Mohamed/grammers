# Historical pre-Opus correction and review evidence

This continues local branch `<candidate-branch>` at
`5f0cb0511416e666b5476c078c0aaa86d9bce69e`, preserving its history after
`5b16ac4` and `8f109a6`. The [historical source manifest](benchmarks/2026-10-30/final/performance/measured-source/source.json)
and [historical Rust diff](benchmarks/2026-10-30/final/performance/measured-source/source.patch) identify the
additional correction without inventing a future commit SHA. At measurement
time, this correction was uncommitted and the branch was absent from the
origin's advertised refs. These are historical observations, not publication status.

The complete retained task result and all five saved prior turn reports were
read, including both pages of each Claude review. The prior terminal job status
does not change their verdict: **changes**. The second-round correction had not
received independent acceptance. The first and second Claude reviewers could
not run shell/tests; their execution limitations remain part of the evidence.

## Additional source corrections

- GetState rejects negative pts/qts/date/seq before touching storage or the
  message box. Recovery stays armed on rejection. The regression returns four
  malformed replies, verifies unchanged saved channel state, then recovers from
  a valid reply through the ordinary `UpdatesReceiver::next` path.
- Checked PeerId chat/channel constructors evaluate arithmetic only after range
  validation. This removes debug overflow on extreme invalid integers.
- Checked saved state and incoming update metadata enforce representable channel
  IDs; validation precedes the zero-PTS routing filter. ChannelTooLong with an
  invalid ID cannot slip through using pts=0. Legacy message-box callers receive
  a typed protocol error at channel-recovery conversion instead of panicking.
- Adapted live date/sequence fields are checked before producing or persisting
  update state. Existing valid pts progression and peerless messageEmpty behavior
  were intended to be preserved; Opus found that zero QTS was wrongly rejected.
  This claim is superseded by the post-Opus correction below.
- A deterministic ready-queue regression verifies that a connection yields
  before draining 512 immediately rejected requests. Production sender scheduling
  remains unchanged from `5f0cb05`.

The historical [Astra reviews](benchmarks/2026-10-30/final/astra-review.md) accepted
the then-current corrections (including the subsequently discovered zero-QTS
regression), the documented scope dispositions and the then-final performance
evidence with its explicit tradeoffs. Source inspection concentrated on protocol,
invocation lifetime and recovery; it was not an exhaustive independent re-audit
of every original component. Workers were requested as `gpt-6-astra`/`xhigh`; they could
read source/artifacts but shell was rejected. All execution evidence here belongs
to the parent. Native identity/effort receipts must come from the outer runner;
these records do not assert a new Claude verdict or complete publication approval.

## Previous findings and scope

All forty round-one dispositions remain in [REVIEW-ROUND1.md](REVIEW-ROUND1.md).
The twelve round-two findings were rechecked as follows:

| Findings | Final disposition |
|---|---|
| 1, evidence membership | Recursive JSON exceptions retained; final artifacts have the same explicit exception and a fresh-index export checker. |
| 2, queued delivery certainty | Close/drain preserves the original shared error, definite-not-sent status and permit release before wakeup. Real socket-failure regression retained. |
| 3, response scan and overload | Redundant scan stays removed. The new one-line scheduling control attributes a busy/quiet tail tradeoff; it does not establish the cause of the historical throughput loss. |
| 4, handshake CPU | Step2 and step3 stay in the same bounded four-slot blocking pool. Started CPU work retains its permit after cancellation; the factorization loop is finite. |
| 5, 7, 9, account recovery | Cancellation/storage retry, cached-login guard and missing-Common detection remain, with the additional malformed-state guards above. |
| 6, real-server evidence | Reuse the two unchanged read-only test-DC probe receipts. Do not repeat live traffic or infer representative container compatibility; child-sequence ordering remains warning-only. |
| 8, cold waiters | New arrivals poll only new waiters; closure wakes trigger rescans. Retain caller-only/partial-cancel and 20,000-waiter regressions. |
| 10, salts | Exhausted optional prefetch disables itself; current-salt RPCs and later bad_server_salt recovery remain usable. |
| 11, mutation corpus | Claim remains the actual single-rpc_result seed, with separate container/gzip/service regressions. No coverage-guided fuzzing claim. |
| 12, source metadata | Round-two manifest now names actual checkpoint 5f0cb05 and explicit diff endpoints. Historical task-scoped no-commit text is not an owner prohibition. |

The bounded-handshake, separate packet/expansion limits, 1 MiB reply, gzip work,
delivery-aware retries, service deadlines, per-DC setup/admission/cancellation,
updates and client-input obligations remain covered by the main
[scope matrix](OPTIMIZATION-2026-10.md). Replay-cache saturation still retains
8 KiB/session; the previous reviewer explicitly accepted that cost.

New compression policies, forced packing windows, sent-request indices,
serializer/allocator replacements and lock-free routing remain deferred. They
would change latency, memory or ownership policy without an established gain;
this continuation addresses reproducible correctness failures and the measured
scheduling tradeoff first. They are **not evaluated**, not rejected by invented
measurements. The runtime and blocking-socket controls do not justify replacing
Tokio. Per-packet resource limits are not a global memory bound across pools.

## Canonical upstream

The [official Git source audit](benchmarks/2026-10-30/final/upstream/README.md)
verified Codeberg's tip, all 17 newer commits and exact challenged diffs. It
explains adaptation of accounting/validation and each deferred item, including
the public crypto-buffer API chain and layer 232's nine changed signatures plus
25 added constructors/methods. No schema-only layer bump or automatic merge was
performed. Existing layer 230 and public fork APIs remain the compatibility base.

## Causal scheduling measurement

Twelve isolated release pairs remove only `consume_budget` from `run_sender`
in a control build. Restoring the production budget changes busy p99 by
**+115.77% [80.28%, 147.68%]** and quiet p99 by
**−44.61% [−55.22%, −20.10%]**. Both directions occur in every pair.
Completed RPCs change **−6.47% [−16.23%, +6.70%]**, which is inconclusive.
[Raw samples, exact patch, hashes and limits](benchmarks/2026-10-30/final/performance/README.md)
support retaining cooperativity as an explicit tradeoff.

This identifies a scheduling contribution to the tail change; it cannot assign
the historical −8.21% throughput regression to Tokio or replay handling, and
historical/current percentages cannot be subtracted as causal shares. No general
optimization, optimal quantum or production capacity guarantee is claimed.

Historical pre-Opus source comparisons against `7774ed6` are separate from that causal
control. Twelve pairs give plain throughput +0.79% [−2.51%, +3.08%] and gzip
−7.24% [−10.55%, +1.57%], establishing no speedup. Twelve overload pairs complete
−6.59% [−13.49%, +0.14%] calls, with busy p99 +113.28% and quiet p99 −43.38%.
The narrowly zero-crossing completion interval does not negate the older
significant regression. Saturated client heap is freshly measured at
7,911 → 15,951 B/session (+101.6%), including 8192 B replay history. Full CPU,
latency, RSS, allocation, source identity and measurement limits are in the
[final comparison](benchmarks/2026-10-30/final/performance/final-summary.md).

## Review and publication gate

Historical pre-Opus validation passed **604 workspace tests/doctests**, with
**41 ignored**. All-target/all-feature check, strict Clippy, warning-free Rustdoc
and rustfmt passed. Minimal client: 39 tests; memory/import/string/file session
configurations: 42/64/49/84 passed (one ignored each). The extreme PeerId test
also passed in debug mode. [Commands and complete logs](benchmarks/2026-10-30/final/validation/README.md)
bind these results to the final source manifest. Removing the cooperative
budget in a temporary control makes the new fairness test fail with all
512 requests drained, confirming the regression detects the intended behavior.

The initial review runner selected a baseline-only checkout. Its “nothing to
review” result did not review this cumulative candidate. Subsequent Opus reviews
explicitly identified the original source and returned changes. Native receipts
and their execution limits are retained with the corresponding review reports.
Parent-run tests remain distinct from reviewer-executed tests, and requested
effort is distinct from native-confirmed model identity.

## Historical review-routing continuation

The continuation preserved the source and supplied the cumulative candidate to
the reviewer. Its checker behavior and validation below describe that historical
state; current correction scope is in the [acceptance ledger](benchmarks/2026-10-30/final/acceptance-review.md).

The evidence checker now rejects the wrong explicitly selected worktree/branch/
HEAD, differences between live tracked contents and the index (even with
assume-unchanged), incomplete tracked Rust/Cargo manifests and additional
nonignored source files, and any live evidence omitted from the index. TL and
repository build-configuration inputs outside the historical Rust/Cargo manifest
must match the checkpoint, including inventory checks for staged deletions and
ignored configuration additions. Ambient toolchains/configuration remain outside
this repository-content check. Ten offline Python regressions pass; temporary
fixture repositories create trees but no commits.

Fresh parent validation again passed **604 workspace tests/doctests, 41 ignored**,
all-target/all-feature check, strict Clippy, warning-free Rustdoc and fmt.
[Continuation commands](benchmarks/2026-10-30/final/validation/routing-commands.json)
record these executions and the corrected test-fixture attempt. Optional-feature
and performance runs reuse the unchanged, hash-checked source receipts above;
no benchmark or live Telegram probe was repeated. The official Codeberg Git tip
was independently reconfirmed as `1e31e73` and its saved patch hashes still match.

[Scoped continuation reviews](benchmarks/2026-10-30/final/routing-review.md)
record renewed Astra source/evidence inspection and numerical recomputation,
plus the corrected checker review. They retain execution and identity limits.
At the time of that continuation, the original source and staged work were
intact and no commit or push had been performed in that turn. Its next Opus
review returned changes, as recorded below; this paragraph is historical.


## Post-Opus review status

Claude's next review explicitly identified the original candidate and returned
**changes**, not acceptance. Its zero-QTS finding invalidates the earlier source
acceptance for that correction. See [the first Opus correction dispositions and historical
verification](REVIEW-OPUS-CORRECTION.md). Native reviewer identity was
`claude-opus-5-5`; shell execution was unavailable to it and its source workers.
Tests and benchmarks remain parent-executed. The subsequent corrected source still
requires a new Opus review; historical scoped Astra acceptance is not promoted
to acceptance of this newer source.
