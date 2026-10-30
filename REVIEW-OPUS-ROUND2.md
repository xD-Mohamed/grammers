# Corrections after the second Opus review (2026-10-30)

Historical candidate: `5f0cb0511416e666b5476c078c0aaa86d9bce69e` plus the correction
identified by [source metadata](benchmarks/2026-10-30/final/performance/post-opus2/source/source.json), cumulative
against `7774ed6`. Historical checkpoint commits and subjects remain unchanged;
any eventual final commit must use a conventional subject.

The [complete second Opus report](benchmarks/2026-10-30/final/opus-round2.md)
returned **changes** after identifying the original candidate. Its
[native receipt](benchmarks/2026-10-30/final/provider-receipts.json) confirms
`claude-opus-5-5`; requested `xhigh` is not a native-confirmed effort. Opus and
its workers had no shell. All execution reported here is parent-run. Acceptance of an exact reviewed tree is recorded externally; see PUBLICATION.md.

| Finding | Correction and evidence |
|---|---|
| 1: missing channel hash | Recovery defers at most 30 times. A further miss warns and discards that channel's saved backlog position, allowing later live updates to resume. One-second retry delays give about 30 seconds of grace; scheduling may make this longer. The unavailable backlog is explicitly lost. Tests cover uncached and minimal peers, siblings, and resumed live delivery through the real receiver. Old ledger row 8 is superseded. |
| 2: replay floor | Track the highest actually evicted ID. Cache membership and floor stay stable throughout each container; commit bounded pending IDs after successful decoding. Delayed RPC results/errors still complete callers, with their own updates suppressed. Direct Pong/FutureSalts and correlated bad-message rejections also complete or retry callers; stale responses never replace salt/clock/sequence state. Other stale leaves are ACKed/dropped. Historical mixed-sibling sender tests cover both orders, success/error and permit release. Historical gzip, real eviction and duplicate suppression coverage was decoder-level; the later acceptance correction adds a sender replay fixture. Old teardown policy in row 9 is withdrawn. |
| 3: liveness | A retained 1 ms timer wake allows reactor readiness to refresh before the final bounded read. Cancellation cannot restart the grace. Return any nonempty decoded update batch. Tests do not prime readiness with peek; they include cancellation, partial packets and actual config-then-key persistence ordering. |
| 4: catch-up cost | Per-entry pending flags and a ready count replace repeated queue membership scans. Cached checks return directly before the next deadline; expired sweeps are linear in entries. The separate 1k/10k-channel release benchmark measures this component, not entire catch-up throughput. |
| 5: evidence gate | Canonical full-index diffs use an isolated Git configuration. Default verification checks saved upstream artifacts; `--verify-upstream` explicitly requires recorded Codeberg objects. Current named checks require exact command scope, real positive test totals, and source/tooling hashes. Current benchmark build/configuration identities must agree; supplied retained executables are hashed. External privacy patterns scan every tracked text file. Independent-clone portability is recorded separately. |
| 6: documentation | Current instructions and scope point here; historical test/benchmark counts stay explicitly historical. Startup repair differs from strict try_load. Load normalization and deferral panic are documented. Historical heap-run process RSS of 11,308 to 14,356 KiB (+27.0%) is disclosed separately from inconclusive overload RSS. Public ledgers describe measurement-time facts and review status. |
| Weak tests | Add actual-eviction mixed containers, three-second clock-boundary cases, production config/key finalization, deterministic DH gate arrival, and maximum 1024-child authenticated container decoding. |
| Negative RPC codes | Conservatively classify negative codes as UncertainDelivery while preserving the original RPC and default no-replay behavior. No claim is made that Telegram was observed sending a particular negative code. |
| Empty future salts | Retain current salts/clock anchor; internal empty answers consume the existing bounded refresh allowance. |
| Near-limit ACK fairness | ACKs normally go first. After an ACK-caused request deferral, the next attempt prioritizes the user request. Repeated incoming ACK demand cannot starve it; deferred ACK IDs/deadline survive. |

The additional [scoped Astra inspection](benchmarks/2026-10-30/final/astra-opus2-source-review.md) caught the direct-service correlation and negative flood-name cases; those are corrected as well.

All listed findings are valid. No review finding is rejected. The prior accepted
TL recursion deferral remains: authenticated deeply nested generated TL values
can overflow the stack despite MTProto/gzip limits. Receive-arm-only budgeting
also remains unevaluated. Neither is presented as completed implementation.

## Validation and measurements

Stable-source [final validation](benchmarks/2026-10-30/final/validation/opus2-commands.json)
passed **651 release workspace tests/doctests, 42 ignored**, all-target/all-feature
check, strict Clippy, rustfmt and warning-free Rustdoc. Optional session builds
passed 50/72/57/92 tests (two ignored each), minimal client 39, debug mention
regressions 2, and evidence-checker Python regressions 19. Toolchain: Rust/Cargo
1.99.1. The earlier preliminary run changed source during execution and is
explicitly excluded from final validation. Earlier 631-test and benchmark receipts retain
[their own source snapshot](benchmarks/2026-10-30/final/performance/post-opus/source/source.json).
No hosted Windows CI or live Telegram traffic is claimed.

[Fresh paired measurements](benchmarks/2026-10-30/final/performance/post-opus2/README.md)
cover replay/liveness changes: plain throughput **-5.80% [-9.36%, -2.32%]**,
gzip **+0.23% [-8.63%, +2.63%]**, overload completion **-3.58%
[-7.95%, -0.53%]**, busy p99 **+115.73%**, quiet p99 **-45.69%**.
Saturated client heap is **7,911 → 16,079 B/session** (128 B above the previous
candidate); heap-run process RSS is **11,312 → 14,496 KiB (+28.15%)**.
Allocation windows report 2.00 allocations/RPC for both binaries. Update-state
changes lie outside loopback and receive a separate deadline component comparison.
At 10,000 channels, three release pairs give median 32-check totals of
757.106 ms before and 150 ns after caching; the expired sweep drops from
46.756 ms to 24.395 µs. Tiny cached timings are resolution-sensitive and do not
establish full catch-up throughput. All component executions passed.
No historical benchmark is relabeled as current execution. No general optimization
or causal explanation of the older significant -8.21% loss is claimed.

## Evidence boundaries

The gate verifies source/artifact membership, hashes and local links. Historical
`*commands.json` receipts are checked throughout the measurement directory; historical final receipts
remain bound to archived manifests. Current checks must include workspace,
format/check/Clippy/Rustdoc, optional session features, minimal client, debug
mention regressions and Python gate tests. Tooling hashes prevent reuse after
checker changes. Build/measurement hash consistency does not itself prove a
binary was compiled from source; the build receipt is a parent execution record.
Retained executable bytes can be independently checked with explicit CLI paths.

The default gate makes no private-pattern or upstream-object claim when those
optional checks are not requested. Privacy patterns are supplied outside the
repository so their local path/branch values are not published. Pattern scans
cannot prove absence of every possible secret. A fresh clone must pass the
default gate without upstream objects and without sharing the source object store.

[Independent-clone portability](benchmarks/2026-10-30/final/validation/opus2-portability.json)
passed on `main` and detached HEAD without Codeberg objects. The clone used
Git upload-pack with local copying/hardlinks disabled and no alternates, then
received the exact staged files without creating a commit. Explicit wrong-branch
and missing-upstream checks failed as intended. Fetching the recorded Codeberg
tip made `--verify-upstream` pass. The receipt identifies its exact index tree
and artifact manifest; it predates inclusion of its own logs and this paragraph.
Final delivery repeats the clone check after all public artifacts are staged,
with that last receipt stored outside the candidate to avoid self-reference.

## Acceptance continuation

The [subsequent acceptance preparation](benchmarks/2026-10-30/final/acceptance-review.md)
records later source/tooling corrections and their own validation and measurements.
Exact-tree acceptance and publication receipts are external under PUBLICATION.md.
