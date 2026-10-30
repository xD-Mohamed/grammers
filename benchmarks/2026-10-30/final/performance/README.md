# Isolated sender scheduling control

Fresh-source throughput, split CPU, tail latency, heap/allocation and overload
comparisons against the historical baseline are recorded separately in
[final-summary.md](final-summary.md). The isolated causal experiment below
uses the round-two checkpoint and its unchanged sender policy.

This control changes exactly one line of checkpoint `5f0cb05`:
[no-sender-budget.patch](no-sender-budget.patch) removes
`tokio::task::consume_budget().await` from `run_sender`. Protocol, replay,
admission and response checks, plus the separate pool control-loop yield, remain.
This is a benchmark-only control; the production sender retains cooperativity.

The custom queue returns ready items without consuming Tokio's cooperative
budget. The explicit outer call bounds a ready queue's uninterrupted loop work.
This experiment includes both scheduling and generated-code effects; it does
not separately measure the check's CPU cost and the cost of yielding.

Twelve alternating release pairs use the identical harness: current-thread
runtime on CPU 0, responders on CPUs 2,3, 64 pools/two DCs, half the arrivals to
one busy pool and half rotating through quiet pools, 180,000 independently
scheduled arrivals over 1.5 seconds at 120,000/s, and a 1024-task cap. No builds
ran concurrently. Affinity is not exclusive ownership on this shared host.
[Configuration and binary hashes](sender-budget/configuration.json),
[all samples](sender-budget/samples.json), [summary](sender-budget/summary.json)
and the 24 individual raw logs are retained.

| Metric | No sender budget | Retained budget | Median paired change [95% interval] |
|---|---:|---:|---:|
| Completed RPCs | 90,430.5 | 86,287 | −6.47% [−16.23%, +6.70%] |
| Harness drops | 89,569.5 | 93,713 | +7.36% [−5.34%, +21.78%] |
| Busy pool p99 | 28.825 ms | 61.783 ms | +115.77% [+80.28%, +147.68%] |
| Quiet pools p99 | 30.660 ms | 18.767 ms | −44.61% [−55.22%, −20.10%] |

All 24 executions reached the cap, recorded zero RPC errors and passed the exact
test. Busy p99 rises and quiet p99 falls in every pair. Latency starts at intended
arrival; refused arrivals count as harness drops before invocation and are
excluded from successful latency. Interpret completion counts alongside latency.

The isolated experiment establishes the explicit scheduling policy as a
substantial contributor to this local busy/quiet tail tradeoff. The sender keeps
cooperativity so a ready queue cannot bypass runtime fairness. This is neither
an overall speed improvement nor evidence of an optimal scheduling quantum.
`ready_connection_queue_yields_before_draining_its_backlog` checks this behavior
independently of timing measurements in the final validation suite.

The completed-RPC interval includes zero: this control does **not** establish
the sender budget as the cause of the historical −8.21% throughput change.
It does not attribute replay CPU cost, establish sustainable capacity or bound
wall-clock latency. Historical/current percentages cannot be subtracted to
estimate causal shares. Further throughput attribution remains future work.
The observed process-RSS reduction also cannot be generalized to per-client
heap: process RSS includes responders and different queue/buffer occupancy.

Intervals resample execution pairs with 10,000 fixed-seed bootstrap draws;
they are unadjusted for multiple comparisons or shared-host drift. Median paired
ratios differ from ratios of execution medians. No Telegram traffic was used.

The source/build provenance is in [source.json](source.json). Reconstruct
checkpoint `5f0cb05` in a separate source directory, apply only the saved patch,
and build with the recorded locked default-feature release command. Preserve the
executable and compare it with the hash-verified round-two executable:

```sh
python3 scripts/compare-overload.py --baseline artifacts/no-sender-budget-loopback \
  --candidate artifacts/round2-loopback --output artifacts/sender-budget --pairs 12
```

These causal measurements use the round-two source. The final update-state
validation corrections do not change the sender scheduling policy; fresh-source
baseline comparisons are recorded separately in the final review ledger.


Post-Opus note: pre-existing measurements and validation logs describe the source
preserved in `performance/measured-source` relative to this final evidence root.
The current corrected source is identified by the root source manifest.
`validation/opus-commands.json` and `performance/post-opus/build.json` record its
fresh validation/build identities. Historical source acceptance is superseded;
a new Opus verdict is required. See the root REVIEW-OPUS-CORRECTION.md ledger.
