# Review-correction evidence

All network work is on loopback with fixed mock keys. No Telegram traffic,
credentials, consumer deployment, hosted CI result or publication is involved.
The [correction ledger](../../../REVIEW-ROUND1.md) maps every review finding to
its decision and evidence. The final source is an uncommitted correction of
`8f109a6`; the historical baseline is `7774ed6`.

## Reproduce the three release binaries

Use the Rust/Cargo versions and environment in [source.json](source.json).
Both baseline and candidate use the **entire identical** final loopback harness,
SHA-256 `b840c64ac397e50117e58faae89dadeba61e85e3c61bdc7eb09cc125878f7071`.

1. Archive baseline `7774ed666317d981e1dfea0d12311cbd38acb43e` to a separate
   directory outside the working checkout. Overlay the current
   `grammers-mtsender/tests/loopback.rs`, `grammers-mtsender/Cargo.toml`, and
   `Cargo.lock`. The manifest/lock overlay supplies the Linux-only `libc`
   development dependency for affinity and `getrusage`; production source stays
   at the baseline. This cannot be a pre-change run of the newly added harness:
   it is a reconstructed baseline, complementing the original pre-edit run.
2. Build the corrected checkout as the candidate. [source-inputs.json](source-inputs.json)
   contains hashes of every tracked Rust source, Cargo manifest and lockfile.
   The source manifest records HEAD, the exact code-diff command and its SHA-256,
   not an invented commit for uncommitted work.
3. Copy corrected source to a third separate directory and apply
   [timer-control.patch](timer-control.patch). This changes only the reset
   predicate back to `deadline() != deadline`. It isolates the timer proposal;
   it is not the historical baseline.
4. In each directory, run:

   ```sh
   cargo test --locked --release -p grammers-mtsender --test loopback \
     --no-run --message-format=json
   ```

   Use the Cargo `compiler-artifact.executable` for target `loopback`, copying
   it immediately to `artifacts/baseline-loopback`, `artifacts/final-loopback`
   or `artifacts/timer-control-loopback` before another build can overwrite it.
   Default features/profile, locked dependencies, no target-CPU override.
   Binary hashes are in source.json and each paired configuration.json.

## Executions and analysis

Run with no concurrent builds or other local benchmarks:

```sh
python3 scripts/compare-loopback-r1.py \
  --baseline artifacts/baseline-loopback --candidate artifacts/final-loopback \
  --output artifacts/paired --runs 22
python3 scripts/compare-loopback-r1.py \
  --baseline artifacts/timer-control-loopback --candidate artifacts/final-loopback \
  --output artifacts/timer-paired --runs 22
python3 scripts/run-loopback-scenarios.py \
  --baseline artifacts/baseline-loopback --candidate artifacts/final-loopback \
  --output artifacts/scenarios
```

The first two scripts alternate A/B and B/A. Each execution measures 1500 ms
per reply shape after warming 64 independent pools, with 16 calls in flight.
Client threads use CPU 0; four responder workers use CPUs 2,3. Affinity constrains
placement, not exclusive CPU ownership on this shared machine. Samples include
draining issued work. RPC latency samples every 64th completion. Paired median
ratios and 95% percentile bootstrap intervals use 10,000 resamples and a fixed
seed; execution pairs, not individual RPCs, are the resampling units. Intervals
are descriptive, without multiple-comparison adjustment or host-stationarity
guarantees. A difference of separate medians need not equal the median paired
ratio. Do not cherry-pick a single significant secondary metric as a global win.

Scenario configurations and exit codes are preserved beside their raw logs:

- Three repetitions each: current-thread/one CPU, two-worker Tokio/one CPU,
  two-worker Tokio/two CPUs, blocking sockets/one CPU, blocking sockets/two CPUs.
  All use four responder workers and the same framing, crypto and plain reply.
  Blocking clients bypass SDK routing, admission and async request bookkeeping;
  they are a control, not a compatible alternative runtime implementation.
- Independently scheduled arrivals: 20,000/s warm and cold; 120,000/s overload;
  100/s and 2,000/s cold mixed 1 MiB/plain-file and gzip replies. 64 pools, two
  DCs, half the arrivals on a shared busy pool and half on rotating quiet pools.
  In-flight cap 1024; excess arrivals are counted as **harness admission drops**.
  Latency starts at intended arrival, so scheduler/admission waiting is included.
  Error/timeout work is counted separately and excluded from successful latency.
  These short runs characterize particular conditions, not sustainable capacity.
- Heap census: 400 pools, before connection, after 1, 9 and **1100** RPCs each.
  This fills the bounded replay history that the earlier nine-RPC census missed.

Linux client CPU uses `getrusage(RUSAGE_THREAD)`: user and system separately.
For multi-thread Tokio, main-thread CPU is near zero; use the individual
`CLIENT_WORKER_CPU` totals, which include startup/shutdown and therefore are not
the same measurement window as the current-thread per-RPC field. Blocking
worker totals bracket their measurement windows. Cycle values of zero mean
unavailable. `/usr/bin/time -v` includes responder, setup and teardown; RSS is
whole-process high-water usage, not per-session retained memory.

Component and syscall commands are recorded with the corresponding logs. Exact
test names are checked to run one test, not a zero-match filter. The existing
component A/B implementations predate this patch; rerunning them is not a new
performance gain. Syscall tracing perturbs timing and is used only diagnostically.

## Artifact history and limits

`paired/`, `timer-paired/`, `scenarios/`, and `validation/` describe the final
frozen source. `before-final-validation/` and `scenarios-before-final-validation/`
preserve earlier results, including unfavorable ones; they are **not final**
measurements. [harness-history.json](harness-history.json) and
[warmup-harness.patch](warmup-harness.patch) identify the intervening harness
changes (parallel warmup outside windows and matched blocking responder workers).
Do not combine these stages into one statistical sample.

The full earlier implementation/review artifacts and accepted provider identities
are retained, with private runner IDs and host paths removed. `xhigh` is requested
effort; receipts do not independently report native effort. No independent
acceptance of the subsequent correction diff is asserted here. The legacy
missing report prose could not be restored.
