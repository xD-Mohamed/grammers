# Second-review correction evidence

Review checkpoint: `5b16ac41ae9d6b0cdd0bf42df50d6064561bc19e`. The correction
source and binaries are identified by source.json; no future integration commit
is invented. [The ledger](../../../REVIEW-ROUND2.md) records all twelve findings,
the full-budget factorization fixture's limits and the remaining review gate.

The only non-local traffic was one execution of each of the two existing
read-only public test-DC probes. See live-tests.json and the preserved
live-source.patch for that exact build. Both passed. No account was logged in,
no credentials were used, and no stress or consumer traffic was generated.
The other tests and benchmarks use local/in-process fixtures only.

## Reproduce overload comparisons

Use Rust/Cargo 1.99.1 and locked dependencies, default release profile/features
without target-CPU overrides. The Linux host exposes CPUs 0–3 (Ryzen 7950X3D
model); it is shared, so affinity is not exclusive ownership. The complete
loopback harness is unchanged from round one, SHA-256
`b840c64ac397e50117e58faae89dadeba61e85e3c61bdc7eb09cc125878f7071`.

1. Build the historical `7774ed6` baseline using the identical-harness overlay
   in [round1/README.md](../round1/README.md). Its preserved executable is reused;
   its hash must match source.json. No baseline production edit is applied.
2. Build the corrected source with
   `cargo test --locked --release -p grammers-mtsender --test loopback --no-run --message-format=json`.
   Copy Cargo's loopback compiler-artifact executable to a separate location
   before subsequent builds can replace it.
3. Create a separate copy of the corrected source and apply
   [scan-control.patch](scan-control.patch). This restores only the old full
   pre-scan; the rest of the production code and harness are identical. Build
   with the same command and copy this executable separately.
4. With no concurrent builds/benchmarks, run:

   ```sh
   python3 scripts/compare-overload.py --baseline artifacts/baseline-loopback \
     --candidate artifacts/final-loopback --output artifacts/overload --pairs 12
   python3 scripts/compare-overload.py --baseline artifacts/scan-control-loopback \
     --candidate artifacts/final-loopback --output artifacts/scan-effect --pairs 12
   ```

Each comparison alternates A/B and B/A, with 12 pairs. Each execution schedules
180,000 arrivals independently of completion at 120,000/s over 1500 ms, using
64 pools, two DCs, and at most 1024 in-flight tasks. Half the arrivals use a busy
shared pool; the remainder rotate through quiet pools. Client CPU 0, responder
CPUs 2,3. Latency starts at intended arrival; capacity rejection is explicitly
counted as harness drops. Errors/timeouts are excluded from successful latency
and counted separately. Per-pair raw logs, samples, binary hashes and exact
configuration are preserved.

Intervals resample paired execution ratios, not individual RPC observations:
fixed random seed, 10,000 bootstrap draws, 95% percentile interval of the median.
They do not correct for multiple comparisons or guarantee host stationarity.
A median paired percentage differs from a ratio of separate medians. This is a
short overload diagnostic, not sustainable production capacity. Whole-process
RSS includes the responders; no new CPU-per-RPC or allocation saving is claimed.

## Component controls

Build release unit tests and run only these exact offline tests with
`--exact NAME --ignored --nocapture --test-threads=1` under `taskset -c 0` and
`/usr/bin/time -v`:

- `grammers-crypto`: `factorize::tests::benchmark_factorization_budget`.
  Five executions of the actual inner rho loop's entire 1,250,000-step budget.
  Its prime input is normally rejected by the public entry point, intentionally
  bypassing that early check to measure the work ceiling. It does not establish
  a worst wall-clock bound or a hostile public PQ that exhausts the budget.
- `grammers-mtsender`: `sender_pool::optimization_tests::benchmark_cold_waiter_scan`.
  Three alternating pairs each at 1024 and 20,000 open waiters, old poll-all
  expression versus the new closure-wake/suffix path. Same RPC/oneshot allocation
  and setup grouping. Invalid DC 999 performs no socket work. This isolates
  arrival bookkeeping, not full handshakes or a storm of concurrent cancellations.

The exact commands, binary hashes and complete validation logs are retained.
The evidence check exports the Git index to a fresh directory and resolves
document links there; it also compares the historical source-input hashes.
JSON files must be tracked, not merely present in an ignored working directory.

## Historical source identity

Round-one JSON files were mistakenly ignored by the global `*.json` rule and
are included by this correction. Their numbers are unchanged. The round-one
source manifest now names its actual checkpoint and uses explicit commit IDs
in its diff command. Its source hash still matches the original measurement.
The full review and both provider receipts are retained without private runner
IDs/host paths; requested xhigh effort is not native-effort confirmation.
