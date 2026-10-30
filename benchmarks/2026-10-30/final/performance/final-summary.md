# Final source versus historical baseline

Final source is checkpoint `5f0cb05` plus the exact correction in
[the source receipt](../source.json). The baseline is `7774ed6` with the identical
benchmark harness, reused from the [round-two build receipt](../../round2/source.json).
[Final build command and executable hashes](final-build.json) identify both
binaries. These default-feature release binaries use Rust/Cargo 1.99.1 and locked
dependencies, without target-cpu overrides. No builds ran alongside measurements.

Twelve alternating execution pairs per scenario use the current-thread runtime,
client CPU 0 and responder CPUs 2,3 on a shared four-visible-CPU Linux host.
Affinity does not reserve CPUs. Intervals are unadjusted paired bootstrap 95%
intervals (10,000 draws, fixed seed), not RPC-level confidence intervals.
Ratios of medians differ from medians of paired ratios.

Rebuild each source with the recorded command and preserve its executable before
building the other version. The historical baseline uses this identical harness
and its test-only dependencies, as documented in the earlier source receipts.
With those executables saved locally, reproduce the paired campaigns using:

```sh
python3 scripts/compare-loopback-r1.py --baseline artifacts/baseline-loopback \
  --candidate artifacts/final-loopback --output artifacts/final-paired \
  --runs 12 --window-ms 1500
python3 scripts/compare-overload.py --baseline artifacts/baseline-loopback \
  --candidate artifacts/final-loopback --output artifacts/final-overload --pairs 12
```

For the heap census, run each executable with `--exact loopback_session_footprint
--ignored --nocapture --test-threads=1`. For allocation windows use `--exact
loopback_rpc_cost --ignored --nocapture --test-threads=1`, unset `LOOPBACK_SHORT`,
and set `LOOPBACK_WINDOW_MS=1500`. Both use `LOOPBACK_CLIENT_CPUS=0`,
`LOOPBACK_SERVER_CPUS=2,3` and `LOOPBACK_RUNTIME=current`; `/usr/bin/time -v`
records process RSS. Execution logs retain the exact command and successful result.

## Established routes

64 pools, 16 callers, 1.5-second measurement windows, plain 1592-byte or gzip
528-byte replies, with sampled latency. Complete
[configuration](paired/configuration.json), [samples](paired/samples.json),
[summary](paired/summary.json) and all 24 execution logs are retained.
CPU is the client thread only; peak RSS includes responder threads. Hardware
cycles are unavailable (the harness prints 0), so no cycle-count claim is made.

| Metric | Baseline median | Final median | Paired change [95% interval] |
|---|---:|---:|---:|
| Plain RPC/s | 68,020.000 | 68,367.000 | +0.79% [-2.51%, +3.08%] |
| Plain client user us/RPC | 6.680 | 6.505 | +2.31% [-4.56%, +11.43%] |
| Plain client kernel us/RPC | 7.920 | 8.100 | -4.03% [-6.68%, +7.80%] |
| Plain client total CPU us/RPC | 14.470 | 14.500 | -0.10% [-2.88%, +2.86%] |
| Plain sampled p99 us | 516.000 | 494.950 | -5.42% [-15.57%, +5.00%] |
| Gzip RPC/s | 72,391.500 | 67,768.000 | -7.24% [-10.55%, +1.57%] |
| Gzip client user us/RPC | 6.080 | 6.535 | +2.90% [-0.53%, +22.25%] |
| Gzip client kernel us/RPC | 7.660 | 7.850 | +1.17% [-1.22%, +11.53%] |
| Gzip client total CPU us/RPC | 13.735 | 14.560 | +7.34% [-1.36%, +10.39%] |
| Gzip sampled p99 us | 479.900 | 550.550 | +13.41% [-4.72%, +39.52%] |
| Whole-process peak RSS KiB | 11,300.000 | 11,300.000 | +0.00% [-0.04%, +0.04%] |

No throughput, CPU or p99 change here has an interval excluding zero. This
neither establishes a speedup nor proves equivalence. Gzip point estimates lean
slower and must not be hidden by the plain result.

## Scheduled overload

64 pools, two DCs, 180,000 scheduled arrivals over 1.5 seconds, half to one busy
pool and half rotating across quiet pools, with a 1024-task cap. Complete
[configuration](overload/configuration.json), [samples](overload/samples.json),
[summary](overload/summary.json) and all 24 raw logs are retained.

| Metric | Baseline median | Final median | Paired change [95% interval] |
|---|---:|---:|---:|
| Completed calls | 98,533.000 | 93,345.500 | -6.59% [-13.49%, +0.14%] |
| Harness drops | 81,467.000 | 86,654.500 | +6.30% [-0.14%, +18.01%] |
| Busy pool p99 ms | 25.188 | 51.493 | +113.28% [+78.52%, +140.34%] |
| Quiet pools p99 ms | 28.537 | 16.321 | -43.38% [-52.72%, -23.11%] |
| Whole-process peak RSS KiB | 11,484.000 | 11,462.000 | -0.37% [-1.15%, +0.59%] |

All executions passed, with zero RPC errors and peak 1024. Latency begins at the
intended arrival; pre-invocation harness drops are excluded from successful-call
latency. Compare latency together with completed and dropped counts.

Completed calls lean lower, but this new interval includes zero narrowly. It
**does not erase** the previous −8.21% [−14.80%, −2.20%] overload regression or
show that it was fixed. Different campaigns on a shared host are not paired
controls for one another. Busy tail is worse and quiet tail better in the final
source. The separate [one-line scheduling control](README.md) attributes a
substantial tail tradeoff to cooperativity; it does not attribute the throughput
loss. No sustainable production capacity or general optimization is claimed.

## Client heap census

Fresh baseline/final runs each measure 400 client sessions with one connection
and 1100 replies per session. The counting allocator excludes responder threads.
[Configuration](heap/configuration.json), [baseline log](heap/baseline.log) and
[final log](heap/candidate.log) retain the allocation-size breakdown.

| Client state | Baseline B/session | Final B/session |
|---|---:|---:|
| Pool created | 2,103 | 2,143 |
| One RPC | 7,911 | 7,791 |
| Nine RPCs | 7,911 | 7,887 |
| 1100 RPCs, replay history saturated | 7,911 | 15,951 |

Saturated heap rises **101.6%**, with an explicit 8192-byte replay-history
allocation per final session. The final census is 64 B/session above the earlier
round-one 15,887 B/session result; this is a fresh measurement, not a reused
nine-reply estimate. One run per executable supplies no sampling interval or
cross-allocator guarantee. Whole-process peak RSS is 11,304 → 14,120 KiB in this
scenario and includes responders/runtime/allocator effects; it is not the client
heap metric or the overload RSS measurement.

## Allocation windows

One separate execution per binary alternates untracked/allocator-tracked windows
(two tracked 1.5-second windows for each payload). [Configuration](allocations/configuration.json),
[parsed samples](allocations/samples.json), [baseline raw log](allocations/baseline.log)
and [final raw log](allocations/candidate.log) are retained. Counting excludes
responder threads. The two-decimal allocation figures include harness amortization;
small byte-count differences are not an isolated allocator optimization result.
Timing from these instrumented runs is not mixed into the paired throughput study.

| Executable / payload / round | Allocations per RPC | Bytes allocated per RPC |
|---|---:|---:|
| baseline Plain 1 | 2.00 | 1801 |
| baseline Plain 3 | 2.00 | 1801 |
| baseline Gzip 1 | 2.00 | 1798 |
| baseline Gzip 3 | 2.00 | 1800 |
| candidate Plain 1 | 2.00 | 1802 |
| candidate Plain 3 | 2.00 | 1805 |
| candidate Gzip 1 | 2.00 | 1813 |
| candidate Gzip 3 | 2.00 | 1800 |

## Reused evidence and measurement boundaries

The earlier runtime/blocking-socket, large-reply, cold-waiter and factorization
controls remain source-relevant: the final corrections do not change those
paths. Their original source and limitations remain in the round-one/two
ledgers. In particular, factorization's full internal-budget fixture is not a
public hostile-PQ fixture or a universal wall-clock bound; the 20,000-waiter
component is not a full handshake/cancellation-storm test. Saturated replay
history remains an intentional 8 KiB per-session security cost. No fresh live
Telegram traffic, account login, consumer deployment or paid service was used.
