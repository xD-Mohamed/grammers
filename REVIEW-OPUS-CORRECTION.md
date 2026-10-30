# Post-Opus correction (2026-10-30)

Historical first Opus correction at checkpoint `5f0cb05`, cumulative against
`7774ed6`. Its exact source is archived under
[post-opus/source](benchmarks/2026-10-30/final/performance/post-opus/source/source.json).
The second Opus review reopened rows 8, 9, 11 and 17. Current dispositions and
validation are in [REVIEW-OPUS-ROUND2.md](REVIEW-OPUS-ROUND2.md).

Claude Opus 5.5 explicitly reviewed the original candidate and returned
**changes**. Native receipt confirmed `claude-opus-5-5`; it and its source
subagents could not execute shell/tests. Its earlier baseline-only verdict is
not used. The historical Astra source acceptance missed finding 1 and is
superseded for that source. Final acceptance is recorded outside this tree under PUBLICATION.md.

| Finding | Disposition |
|---|---|
| 1 | Fixed. Zero PTS/QTS is the no-state sentinel while negative counts and invalid channel IDs remain rejected. A real ChannelParticipant QTS-zero fixture and valid siblings cover socket, account difference and channel difference paths. |
| 2 | Fixed. Connection-reset fixture constructs ErrorKind directly and checks kind; no Linux errno assumption. Windows CI has not been observed here. |
| 3 | Explicit deferral permitted by the finding. Generated TL recursion remains unbounded and can abort on authenticated malicious cached-page content. The scope matrix now states the threat model and required coordinated decoder/cleanup work. Wrapper depth does not cover it. |
| 4 | Fixed. Both mention parsers use checked PeerId::user; oversized IDs and the self sentinel are rejected in their regression vectors. |
| 5 | Fixed. Malformed individual difference updates are warned/skipped; validated state, new messages and valid siblings advance. Invalid top-level state still errors without applying it. Full, slice and channel differences are covered. |
| 6 | Fixed. Stream startup repairs invalid saved counters/rows with warnings, retaining valid primary/channel positions. Strict try_load remains; CHANGELOG records the behavior. |
| 7 | Fixed. QTS without primary state is delivered state-free; a stream awaiting primary state periodically rechecks cached login. Both catch-up modes have a startup-before-login regression. |
| 8 | Reopened by second Opus review: unbounded repetitions lost live updates. Historical implementation: A missing access hash defers a pending channel for a bounded retry interval instead of removing its position. The ready target is stable throughout an in-flight different-channel RPC; backlog and deadline regressions cover this. |
| 9 | Reopened by second Opus review: teardown discarded valid sibling results. Historical implementation: Exact cached duplicates are ACKed. IDs below the replay floor produce an explicit protocol connection error without ACK, retiring callers through teardown instead of leaving Sent indefinitely. Out-of-order, inner duplicate and inner underflow tests cover the distinction. |
| 10 | Fixed. Clock jitter preserves the last emitted ID; the floor resets only when more than three seconds ahead of corrected time. Existing large-clock-correction tests remain. |
| 11 | Reopened by second Opus review: readiness could remain uncached. Historical implementation: Liveness is rearmed after config/key storage. Expiry inspects immediately ready reads up to the receive cap before failing; partial input cannot wait or extend the deadline. Buffered, partial, awaiting-step and slow-store regressions exercise actual paths. |
| 12 | Fixed. Authorization, login migration, file migration/auth recovery and channel RPC500 handling inspect root_cause and retain original errors. Download tests cover plain/shared/nested-shared recovery and shared error identity. |
| 13 | Documented deferral. Receive-arm-only budgeting has not been measured; current budget charges also bound ready service/read/write iterations. The existing measured fairness/tail tradeoff remains, without claiming optimal placement or attributing historical throughput loss. |
| 14 | Strengthened. DH tests isolate generator/prime validation from time checks and exercise actual generate_auth_key step2/step3 through a saturated job gate. Failure-generation regression uses a real closed socket/full update queue; closed-queue assertion verifies rejected ownership; liveness test awaits one step through expiry. |
| 15 | Added tests for future salts/error preservation, plain frame errors, out-of-order/inner replay, real-packet sibling gzip budgets/failed CRC charging, partially written keepalive, four started DH jobs retaining capacity after cancellation, setup-task panic with tracked live waiters, and non-ignored two-worker pool races. |
| 16 | Fixed. Branch identity is enforced only by an explicit option. Main/detached checks are tested. Evidence output does not assert historical commit/publication facts as current state. |
| 17 | Reopened by second Opus review. The historical gate checked final/validation command receipts, including routing receipts; FAILED rows/incorrect totals/nonzero exits reject. The historical scan covered benchmark artifacts only; the current gate scans all tracked text using external patterns. Index bytes and artifact sizes are checked below. |
| 18 | Agreed. Keep historical checkpoint objects/subjects pinned by receipts. Any eventual final commit must use a conventional subject; historical checkpoint subjects remain unchanged. |
| 19 | Fixed for the fresh build receipt: exact rustc/cargo versions and source/binary identities are captured. The old receipt lacked version capture and is explicitly historical. The scope ledger targets this cumulative corrected candidate. |

## Verification

Parent execution on the unchanged 171-file Rust/Cargo input manifest passed:

- Release workspace/all-features: **631 tests/doctests passed, 41 ignored**.
- All-target/all-feature check, strict Clippy, rustfmt and warning-free Rustdoc.
- Optional session builds: memory 48, import 70, native string 55, file import 90
  passed (one ignored in each). Minimal client library: 39 passed.
- Debug mention-ID regressions: 2 passed. Evidence-checker Python regressions:
  14 passed.

[Commands, toolchain and totals](benchmarks/2026-10-30/final/validation/opus-commands.json)
link each result to a raw log. The source manifest SHA-256 is
`6d9ac6eb7f2f94987cea0db51ec7e468905834013d83a1e30303c694778e6471`.
Superseded attempts are retained separately with their actual failures and fixes;
they are not counted as passing final validation. All execution is parent-run.
No hosted Windows CI, live Telegram traffic, authorization or deployment was run.

The full [Opus changes report](benchmarks/2026-10-30/final/opus-round1.md),
[native model receipts](benchmarks/2026-10-30/final/provider-receipts.json) and
[scoped Astra correction inspection](benchmarks/2026-10-30/final/astra-correction-review.md)
retain identity and execution limits. Acceptance is recorded outside this tree under PUBLICATION.md.

## Fresh local performance

[Full measurements, confidence intervals and limits](benchmarks/2026-10-30/final/performance/post-opus/README.md):
12 pairs give plain throughput **-2.91% [-8.04%, -0.40%]**, gzip **-2.05%
[-8.10%, +5.61%]**. Overload completion is **-8.36% [-22.63%, +0.37%]**;
busy p99 **+118.80%**, quiet p99 **-28.26%**. Saturated client heap remains
**7,911 → 15,951 B/session**. Two allocation windows per payload report
2.00 allocations/RPC for both binaries. Overload whole-process RSS is inconclusive. The separate saturated-heap process run measured maximum RSS **11,308 → 14,356 KiB (+27.0%)**; this is a whole-process observation, not per-session heap or a paired confidence estimate.

There is no general optimization claim, no attribution of the old significant
-8.21% throughput loss, and no claim that these runs cover Telegram behavior.
The current sender/protocol changes received fresh measurements. Earlier raw
logs and scheduling controls remain explicitly historical. Exact-tree acceptance and publication receipts are recorded outside the tree
under PUBLICATION.md.


## Historical evidence integrity

The first correction gate checked 171 Rust/Cargo inputs, three schema/build
anchors, artifact hashes and local links. Its receipt scope, privacy coverage,
configuration-dependent diff hashes and shared-object portability exercise were
insufficient; the second review requested corrections. The historical
[blob inventory](benchmarks/2026-10-30/final/validation/opus-index-audit.json) and
[portability attempt](benchmarks/2026-10-30/final/validation/opus-portability.json)
remain as observations of that earlier candidate. Current independent-clone
validation and exact checker scope are documented in REVIEW-OPUS-ROUND2.md.
