# Scoped Astra review records

Two workers were requested as `gpt-6-astra`, effort `xhigh`. No lower model was
requested. These are their returned review conclusions, not a substitute for
the outer Duo runner's native identity receipts or Claude's final acceptance.
Both workers' shell execution was rejected by the tool boundary. The parent
executed builds/tests/benchmarks; the workers inspected source and saved output.

## Protocol worker: original corrected checkpoint

The worker read both complete Claude reviews and their ledgers. It found the
round-two queue/error, serialized-response, blocking-job, recovery, waker and
salt-prefetch corrections coherent, but requested changes for malformed
GetState counters, invalid channel IDs and negative live date/sequence metadata.
It also identified eager arithmetic in checked PeerId constructors. Those
findings are implemented in the final source diff.

After inspecting the corrections and five regression bodies, its final response:

> Accept by source inspection for the identified gaps.
>
> The final code validates GetState before persistence, safely handles extreme
> peer IDs, rejects invalid saved/live channel IDs—including pts=0—and rejects
> negative live date/sequence metadata before updating state. The checked fallback
> preserves a typed protocol error.
>
> Reviewed all five named regressions, including recovery retry and ready-queue
> yielding. No further blocker found in these corrections.
>
> No tests ran in this worker. Parent-run validation remains separate; this
> verdict does not grant full publication or performance acceptance.

## Performance worker

The worker inspected all 24 raw scheduling-control executions, samples,
configuration, binary hashes and the one-line control patch. Its final response:

> Accept retaining sender cooperativity as an explicit, measured fairness tradeoff.
>
> Reviewed all 24 raw executions, samples, hashes, and the one-line control patch.
> All passed, with zero RPC errors and peak concurrency 1024.
>
> Retained budget versus removed budget: busy p99 +115.77% [80.28%, 147.68%];
> quiet p99 −44.61% [−55.22%, −20.10%]. Both directions held in every pair.
>
> Completed RPCs −6.47% [−16.23%, +6.70%] remains inconclusive. This experiment
> does not attribute the historical throughput regression or replay CPU cost.

## Final performance evidence pass

After the final-source measurements and negative fairness control completed,
the performance worker returned:

> Scoped performance review: ACCEPT. No further evidence changes requested.
>
> I checked all 48 final paired/overload execution logs against their saved
> samples and independently recomputed reported medians and paired changes.
> They match. Heap and allocation logs support 7,911 → 15,951 B/session
> (+101.6%), including 8192 B replay history, and 2.00 allocations/RPC.
>
> No established throughput improvement or equivalence; gzip estimates lean
> slower. The fresh overload interval does not erase the earlier significant
> −8.21% loss. The isolated scheduling control supports retaining cooperativity
> as a busy/quiet latency tradeoff. The negative regression demonstrably detects
> its removal.
>
> Throughput attribution, replay CPU cost, optimal scheduling quantum, and
> sustainable production capacity remain unresolved or deferred.

The worker also confirmed consistent source/build identity and explicit
single-run heap/allocation and shared-host limits. It executed no tests or
benchmarks; it inspected parent output. This is performance evidence acceptance,
not external Opus acceptance or publication approval.

The parent validation and fresh-index export receipts bind the logs and
artifacts to the source. Neither Astra conclusion constitutes both-provider
final acceptance. Prior Claude verdicts remain changes, and the latest
correction must receive its own independent Opus review before publication.

## Final protocol and scope pass

After reading the final validation, upstream audit, Git diagnostic and count
clarification, the protocol worker returned:

> ACCEPT for the examined source corrections and documented scope. No further
> source changes requested. This applies to <candidate-branch> at checkpoint
> 5f0cb0511416e666b5476c078c0aaa86d9bce69e plus its reviewed staged corrections.
> It is not publication approval or the outstanding Opus verdict.

It accepted all twelve round-two dispositions: evidence membership; queue
failure certainty/permit ordering; targeted response retirement; shared bounded
factorization/DH workers; cancel-safe validated GetState; diagnostic-only child
sequence ordering; home-generation recovery; cold-waiter wake/cancellation
handling; Common state independent of saved channels; optional-salt disabling;
accurate mutation-corpus scope; and source provenance.

It accepted the added peer/channel/counter guards and examined the five new
regression bodies. It also accepted the recorded dispositions of the original
forty findings, while stating this limit:

> My independent source inspection concentrated on protocol, invocation
> lifetime, recovery, salts and factorization. This is not a claim that I
> separately reimplemented or exhaustively audited every client, importer,
> documentation and benchmark change covered by that ledger.

The worker examined the 17-commit upstream inventory and four saved challenged
patches. It accepted accounting/validation adaptation and explicit compatibility
deferrals for layer 232, sans-I/O, crypto buffer moves, dependencies/releases
and media APIs. The cosmetic spelling fix remains explicitly deferred. It did
not treat these as measured performance rejections.

It independently reconciled the saved workspace result summaries as 604 passed,
zero failed, 41 ignored, and reviewed the optional/debug/lint/build/doc evidence.
The parent counted 604 top-level passing lines separately. The captured deadline
child re-executes six existing functions, adds no six reported tests, and does
not require subtracting one from the top-level total. The worker executed none
of these commands itself.

It accepted the performance qualifications, not a universal improvement claim,
and confirmed that the Git diagnostic distinguishes process-creation rejection
from repository failure. Its remaining gates are the refreshed artifact manifest
and external Opus inspection of the original checkout. Automatic runner routing
must be checked when that review starts. No broad test rerun was requested for
these documentation changes, and no commit/push was performed.
