# Scoped review-routing continuation

The parent confirmed the original branch `<candidate-branch>`, checkpoint
`5f0cb0511416e666b5476c078c0aaa86d9bce69e`, staged correction and matching live
source. The cumulative Rust/Cargo patch from `7774ed6` is 311182 bytes with
SHA-256 `ffd90ca2c1b6300495b896a4e6abb91320445567af9bae50ecbccda1c32fafb2`.
The additional Rust/Cargo patch from `5f0cb05` retains SHA-256
`d01408c11de42d3febdd40280e3ced8402d200e5da96376fbb2982492f0dc119`.
Only review tooling, documents and continuation receipts changed in this turn.

Two review workers were explicitly requested as `gpt-6-astra`, `xhigh`.
They used the original absolute candidate paths, current source, the complete
cumulative patch and saved evidence. Shell creation was rejected and direct Git
state reads were denied; branch/index identity and Rust test execution were
parent-confirmed. Requested model/effort on a worker launch is not a new native
provider identity receipt. The outer runner must retain its own native receipt;
neither this file nor an implementation verdict substitutes for Claude review.

## Source review

The source worker accepted the inspected corrections and cumulative production
changes, with no blocking defect found. It inspected all twelve round-two
dispositions, concentrating on queued delivery certainty, permit retirement,
serialized-request handling, bounded handshake offload, wake-based cold-waiter
polling, GetState cancellation/login/channel preservation, optional salt failure,
and the additional metadata/PeerId guards. The scope is not an exhaustive audit
of every original component. No worker build, test or benchmark ran.

## Performance and evidence review

The evidence worker independently parsed all **72 raw logs** through read-only
tools and JavaScript. Every saved sample matched. All **38 metric summaries**,
including medians, paired ratios and all fixed-seed 10,000-draw bootstrap
endpoints, reproduced. Both 12-pair overload campaigns had 180,000 scheduled
arrivals per execution, with busy p99 higher and quiet p99 lower in each pair.
The heap logs confirm 7,911 to 15,951 B/session (+101.63%), including 8192 bytes
of replay-ID allocation. The saved layer-232 patch contains exactly nine changed
signatures and 25 additions; accounting, validation and relocation patches
support the stated upstream dispositions.

Verdict: scoped performance/evidence acceptance. The earlier significant
throughput regression remains unresolved; the current interval crossing zero
does not prove a fix. The cooperative-budget control supports the measured
successful-call tail tradeoff, not historical throughput attribution. Per-group
drop counts are unavailable, so broader fairness claims would exceed the data.
P2 and upstream deferrals are accepted compatibility/measurement decisions,
not completed evaluation of those proposals or approval of every optimization.

## Review-tool correction and acceptance

The evidence worker found membership and build-input gaps in the original
checker. The parent corrected them and added ten offline regressions. A second
inspection caught ignored new Cargo configuration and staged-deletion gaps;
the final checker compares checkpoint, index and live inventories for these
inputs. The worker accepted the revised gate within its documented scope,
without running it. The additional Rust/Cargo source-file discovery check is
explicitly limited to nonignored files; the parent separately observed no
untracked Rust/Cargo inputs outside Cargo target output in this candidate.

[Fresh command receipts](validation/routing-commands.json) and
[Python output](validation/routing-python.log) are parent-executed evidence.
The first ten-case fixture attempt had one setup error because Git refused to
remove a newly staged file without a HEAD. The fixture now unlinks and stages
the deletion. Its failed output is retained separately; all ten final tests pass.

These are scoped Astra conclusions only. The last substantive Claude verdict
remains changes until a new Opus review explicitly accepts the original
cumulative candidate. No commit, push, consumer deployment or new live Telegram
traffic occurred in this continuation.
