# Local October optimization measurements

See [the scope and review ledger](../../OPTIMIZATION-2026-10.md) for decisions,
compatibility and results. All network traffic in these tests is localhost.

This directory's top-level samples describe the historical implementation
checkpoint `8f109a6`, before review corrections. Use [round1/README.md](round1/README.md)
for the current identical-harness comparison, runtime controls, saturated heap,
source provenance and final validation. Historical figures are retained rather
than silently replaced.

## Reproduction

Use Rust 1.99.1 and the checked-in lockfile, default release optimization, and
the same machine for both versions. This run had four visible CPUs (AMD Ryzen
9 7950X3D model), about 5.8 GiB RAM and Linux 5.15.0-124-generic. The machine is
shared; alternating samples reduce order bias but do not remove host noise.

1. Create a separate detached worktree at
   `7774ed666317d981e1dfea0d12311cbd38acb43e` and apply
   `baseline-latency.patch` there. It adds only latency sampling to the existing
   harness and was verified to apply cleanly. The baseline executable was built
   and measured before production edits.
2. In each checkout, run
   `cargo test --locked --release -p grammers-mtsender --test loopback --no-run`.
   Copy the executable path printed by Cargo to separate baseline/candidate
   paths; do not allow a subsequent build to overwrite either copy.
3. Run the comparison with no concurrent builds:

   ```sh
   python3 scripts/compare-loopback.py \
     --baseline artifacts/baseline-loopback \
     --candidate artifacts/candidate-final-loopback \
     --output /tmp/grammers-comparison --runs 3
   ```

   The script hashes both executables, alternates A/B then B/A, preserves every
   log, and reports medians plus individual samples. Each execution has five
   four-second windows for each reply type: 0/2/4 untracked CPU/throughput and
   1/3 allocator tracking. Latency samples every 64th RPC, with preallocated
   sample storage. Configuration: 64 pools, 16 callers, a single client runtime
   thread and four responder threads, warmed routes, updates disabled, real
   encryption/framing, a roughly 270-byte request and 1592/528-byte reply bodies.

4. Run each executable separately with
   `--exact loopback_session_footprint --ignored --nocapture --test-threads=1`
   under `/usr/bin/time -v` for the 400-session heap census. This measures after
   nine RPCs, before the 1024-entry replay history reaches its maximum 8 KiB.
5. Build release library tests for `grammers-mtproto` and `grammers-mtsender`.
   Their printed executables accept these independent ignored tests:
   `manual_tl::optimization_tests::benchmark_gzip_resource_limit` and
   `queue::tests::benchmark_queue_handoff`. Run separately with
   `--exact NAME --ignored --nocapture --test-threads=1`, under `/usr/bin/time -v`.

## Artifacts and limits

`initial-baseline.log` contains the pre-edit RPC and heap run. `first-candidate/`
preserves the intermediate implementation's slower results. Final paired logs
and `summary.json` are beside this file. `binaries.json` identifies the exact
executables; `resources.json` records whole-process CPU/RSS for each paired run.
The baseline SHA-256 is
`580b9bab4a17b258db0514c76aa23613f275391101a2eabf16d9b43cbc5b4827`;
the first candidate SHA-256 is
`81e795eea20bd58fab8bbca5929bb4453618517fbe2a8fb004a04c93d66a04a6`.

Linux's harness CPU column is total client-thread CPU; the kernel/cycle fields
are unavailable. `/usr/bin/time` includes the responder, setup and teardown;
RSS is a process high-water mark, not per-client retention. Allocation samples
include amortized latency-buffer allocation. Reply sizes are serialized bodies,
not complete TCP bandwidth. The gzip limit fixture includes compressed-input
construction in process RSS, while its printed duration covers decoding alone.

The workload does not reproduce Telegram, a consumer deployment, thousands of
simultaneously pending calls, a particular owner's update mix, or network loss.
The queue test compares two channels on Tokio; it does not compare runtimes or
prove that Tokio is the bottleneck. No runtime or speculative packing/lookup/
compression rewrite is justified by these measurements.

`gzip-budget-filtered.log` preserves an initial invocation using the wrong test
module name: it ran zero tests and is not a measurement. `gzip-budget.log` is the
corrected run, checked to contain one passing test.
