# Historical measurements after second Opus correction

Measured source: [manifest](source/source.json), SHA-256 `94c015aac9c37cab1adb435deccca824363a29a043fbe529db05cba32d645b23`.
[Build receipt](build.json) records the actual command, Rust/Cargo 1.99.1,
selected build environment and both executable hashes. The exact retained
baseline executable is reused; no fresh baseline build is claimed.

Twelve alternating A/B–B/A pairs use the unchanged local encrypted loopback
harness, a 1,500 ms window, client CPU 0 and responder CPUs 2,3. No concurrent
build or test suite ran during these measurements. The host is shared, so
uncontrolled external load remains a limitation. There is no Telegram traffic.

Changes below are medians of paired percentage changes, not percentages computed
from the separate overall medians. Brackets are the scripts' paired-bootstrap
95% intervals, without adjustment for comparisons across multiple metrics.
[Paired raw logs and samples](paired/samples.json) and
[overload raw logs and samples](overload/samples.json) retain all executions.

| Metric | Paired change [95% interval] |
|---|---|
| Plain RPC/s | -5.80% [-9.36%, -2.32%] |
| Plain client CPU/RPC | +5.94% [+2.50%, +9.30%] |
| Plain sampled p99 | +10.06% [-4.29%, +21.39%] |
| Gzip RPC/s | +0.23% [-8.63%, +2.63%] |
| Gzip client CPU/RPC | +0.10% [-2.03%, +8.55%] |
| Gzip sampled p99 | +2.77% [-5.41%, +14.88%] |
| Overload completed | -3.58% [-7.95%, -0.53%] |
| Busy-pool p99 | +115.73% [+103.19%, +143.51%] |
| Quiet-pool p99 | -45.69% [-50.00%, -35.89%] |
| Overload whole-process max RSS | -0.57% [-1.10%, -0.16%] |

Plain throughput is measurably slower in this run. Gzip does not establish a
speedup. All 24 overload executions account for 180,000 offered calls as
completed + errors + harness drops; errors are zero and peak admission is 1,024.
Overload latency starts at intended arrival and excludes harness-dropped calls.
The busy/quiet tail tradeoff remains. These runs do not attribute the older
significant -8.21% throughput loss, nor establish a general optimization or
production capacity guarantee. Historical percentages must not be subtracted
from current percentages as causal shares.

## Heap, RSS and allocations

The [400-session heap census](heap/candidate.log) is compared with its
[baseline](heap/baseline.log):

| Client heap stage | Baseline B/session | Candidate B/session |
|---|---:|---:|
| Pool created | 2,103 | 2,143 |
| Connected, one RPC | 7,911 | 7,919 |
| Nine RPCs | 7,911 | 8,015 |
| 1,100 RPCs, saturated replay history | 7,911 | 16,079 |

The saturated increase is 8,168 B/session (+103.25%), including the 8,192-byte
replay-ID allocation. This correction is 128 B/session above the prior
15,951-byte candidate in the retained heap census. No more precise attribution
of that layout change is claimed. Container processing also allocates a temporary
bounded ID list; the ordinary single-message path does not allocate that list.

Whole-process maximum RSS in this heap run is **11,312 → 14,496 KiB (+28.15%)**.
That observation includes the mock server and is distinct from client heap and
from the overload RSS estimate above. One heap run per binary supplies no
confidence interval. Earlier heap-run RSS of 11,308 → 14,356 KiB (+27.0%) remains
historical and is not replaced by an “inconclusive” label.

[Allocation baseline](allocations/baseline.log) and
[candidate](allocations/candidate.log) each report 2.00 allocations/RPC in two
tracked windows per payload. Plain bytes/RPC are 1,799/1,796 versus 1,812/1,799;
gzip bytes/RPC are 1,797/1,797 versus 1,792/1,794. These local windows are not a
separate confidence experiment and do not characterize every packet/container.

## Reproduction and source boundaries

Build the current default-feature loopback test with the command in build.json,
retain its executable, then run `scripts/compare-loopback-r1.py --runs 12` and
`scripts/compare-overload.py --pairs 12` with explicit baseline/candidate paths.
Configuration files in paired/, overload/, heap/ and allocations/ carry the
source manifest and binary identities. The evidence gate checks their consistency
and can hash the retained executables supplied through its CLI. Compilation
provenance remains a parent-executed receipt, not an independent reviewer build.

Update-state changes are outside loopback. Their separate
[deadline component receipt](deadlines/receipt.json) and logs compare the previous
candidate plus the identical benchmark with current code, at 1k and 10k channels.
This measures deadline checks only; queue removal and complete catch-up throughput
are not established by that component test.

Three alternating release pairs on CPU 0 measure 32 cached checks and one
expired sweep per size. Median elapsed times are:

| Channels | Previous: 32 cached checks | Current: 32 cached checks | Previous: expired sweep | Current: expired sweep |
|---|---:|---:|---:|---:|
| 1,000 | 7.458 ms | 131 ns | 0.432 ms | 3.287 µs |
| 10,000 | 757.106 ms | 150 ns | 46.756 ms | 24.395 µs |

The current cached path returns a stored deadline; its tiny elapsed totals are
sensitive to timer resolution and loop overhead. No precise nanosecond latency
or universal speedup is inferred. The expired sweep and unchanged benchmark
control support removal of the repeated membership scan. Raw logs, full control
source hashes and executable hashes are retained beside the component receipt.

Warm 20k/s, cold-path and 1 MiB scheduled-arrival scenarios were not rerun for this historical snapshot. Their prior one-pair observations belong to 5b16ac4. See the final evidence index for the later corrected build and scenario runs.
