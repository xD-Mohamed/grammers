# Post-Opus local release comparison

[Build/toolchain receipt](build.json), [paired configuration](paired/configuration.json),
[overload configuration](overload/configuration.json), [raw allocations](allocations/samples.json)
and [heap census](heap/samples.json) bind these results to the corrected source.

Twelve alternating baseline/candidate pairs, CPU-pinned current-thread client,
local mock responder, 1.5-second windows; fixed-seed 10,000-draw paired bootstrap.
Intervals resample executions, not RPCs. Baseline is the exact earlier executable;
candidate was freshly built. No concurrent builds/tests or live Telegram load.

| Metric | Paired median change [95% bootstrap interval] |
|---|---|
| Plain throughput | -2.91% [-8.04%, -0.40%] |
| Plain client user+kernel CPU/RPC | +2.68% [+0.02%, +8.42%] |
| Plain sampled p99 | +10.51% [+3.93%, +27.49%] |
| Gzip throughput | -2.05% [-8.10%, +5.61%] |
| Gzip client user+kernel CPU/RPC | +2.18% [-4.70%, +8.67%] |
| Gzip sampled p99 | -5.13% [-11.01%, +32.70%] |
| Overload completed calls | -8.36% [-22.63%, +0.37%] |
| Busy-pool p99 | +118.80% [+99.63%, +201.04%] |
| Quiet-pool p99 | -28.26% [-45.71%, -10.35%] |
| Overload whole-process maximum RSS | +0.46% [-0.93%, +1.52%] |

All 24 overload executions account for 180,000 offered calls with zero RPC
errors; excess work is rejected by the harness admission boundary. The cap
is 1024 active requests. Paired changes are not ratios of the two medians.

Client heap after 1100 RPCs/session is **7,911 → 15,951 bytes/session**, unchanged
from the previous candidate measurement; the retained replay cache is 8 KiB.
The 400-session census is one run per binary, not a confidence interval.
Each tracked allocation window reports 2.00 allocations/RPC. Allocated byte counts
are retained in the samples; transient buffer growth and scheduling vary them.
RSS is the whole process, including responder/runtime; it is not client-only memory.

These data establish no general optimization. Plain throughput and sampled p99
regressed in this run. Overload completion is -8.36% with an interval crossing
zero; this does not erase or explain the older significant -8.21% loss. Busy/quiet
tail and replay-memory tradeoffs remain explicit. This run does not isolate the
cause of any performance difference. The earlier remove-budget control is
historical evidence about scheduling, not a newly measured placement alternative.
The new candidate still requires independent Opus review of these tradeoffs.

The harness does not exercise MessageBoxes/UpdatesReceiver or real Telegram
behavior. Earlier performance logs remain tied to their saved measured-source
manifest, not silently rebound to this correction.

Reproduce using the exact binaries identified in build.json:

```sh
python3 scripts/compare-loopback-r1.py --baseline <baseline-binary> --candidate <candidate-binary> --output <paired-output> --runs 12 --window-ms 1500
python3 scripts/compare-overload.py --baseline <baseline-binary> --candidate <candidate-binary> --output <overload-output> --pairs 12
```
