# Audit follow-up measurements (2026-10-31)

Later measurements of bot workloads are in [2026-10-31-bot](../2026-10-31-bot/README.md).
The `.gitignore` of that commit excluded `*.json` here, so the `build.json` and `summary.json`
linked below were never committed and no longer exist. The raw logs were.

Paired comparisons of this tree with the published commit `2ffd991` and the historical
baseline `7774ed6`. [Build identities](final/build.json) bind the three executables and
the measured Rust inputs. All three use the same loopback harness. Changes are medians
of paired percentage changes; brackets are unadjusted paired bootstrap 95% intervals.
This is local mock traffic on a shared 4-core host: no Telegram traffic, production
capacity or hosted run is involved.

## Throughput and tails (12 alternating pairs, 1.5 s windows)

| Metric | This tree vs `2ffd991` | This tree vs `7774ed6` | `2ffd991` vs `7774ed6` |
|---|---:|---:|---:|
| Plain RPC/s | +2.59% [-0.89, +9.42] | -4.23% [-8.23, +4.81] | +2.16% [-0.63, +4.53] |
| Plain client CPU/RPC | -2.64% [-8.59, +0.95] | +4.51% [-4.48, +8.44] | -2.04% [-4.26, +0.51] |
| Plain sampled p99 | -1.75% [-8.55, +3.42] | +4.53% [-4.49, +13.32] | -3.95% [-7.74, +0.74] |
| Gzip RPC/s | -0.97% [-7.16, +6.98] | -2.46% [-7.45, +4.01] | -2.54% [-7.97, +3.25] |
| Gzip client CPU/RPC | +0.96% [-6.55, +7.70] | +2.49% [-3.90, +8.05] | +2.68% [-3.18, +8.68] |
| Gzip sampled p99 | -0.35% [-7.31, +10.95] | -0.08% [-2.09, +10.99] | **+5.00% [+0.91, +11.62]** |
| Overload completed | **+9.27% [+7.63, +10.86]** | **+1.27% [+0.18, +6.88]** | **-6.45% [-8.29, -4.98]** |
| Busy-pool p50 | **-15.02% [-23.47, -10.36]** | **+35.33% [+27.26, +52.20]** | **+71.53% [+62.32, +82.51]** |
| Busy-pool p95 | **-18.84% [-20.05, -15.25]** | **+103.59% [+88.58, +108.16]** | **+142.31% [+131.26, +153.10]** |
| Busy-pool p99 | **-19.07% [-22.30, -14.17]** | **+96.45% [+76.61, +106.94]** | **+142.66% [+122.00, +153.04]** |
| Quiet-pool p50 | **+4.74% [+1.67, +5.61]** | **-42.26% [-44.20, -41.55]** | **-44.62% [-45.69, -43.93]** |
| Quiet-pool p95 | **+5.19% [+3.18, +9.01]** | **-42.65% [-46.44, -40.11]** | **-44.68% [-47.52, -41.86]** |
| Quiet-pool p99 | +2.82% [-5.19, +9.26] | **-43.87% [-46.88, -37.65]** | **-41.98% [-47.49, -38.88]** |
| Overload process RSS | +0.72% [-0.15, +2.17] | **+6.58% [+5.51, +8.00]** | **+7.30% [+3.48, +8.55]** |

Bold intervals exclude zero. **No general speedup is established:** closed-loop
throughput and CPU have no resolved change in any column, and against the historical
baseline the point estimates lean slower. The overload gain comes from the request
batching below, at a small real cost to quiet pools at p50 and p95 (the p99 interval
crosses zero). Busy-pool tails remain far above the historical baseline and quiet-pool
tails far below it; that fairness tradeoff is retained on purpose. Raw logs and samples:
[against 2ffd991](final/against-accepted), [against 7774ed6](final/against-historical),
[2ffd991 against 7774ed6](final/accepted-against-historical).

These figures vary between sessions. `2ffd991` was previously reported at plain -3.15%
and overload -0.91% against an older retained baseline executable, whereas the third
column above measures +2.16% and -6.45% with the current harness.

## Client heap (B/session, one run per binary, deterministic census)

| Stage | `7774ed6` | `2ffd991` | This tree |
|---|---:|---:|---:|
| Pool created, not connected | 2,103 | 2,143 | 2,143 |
| Connected after one RPC | 7,911 | 7,919 | 7,911 |
| 1100 replies in a burst | 7,911 | 16,079 | **14,031** |
| 1100 replies, one per about 1.2 s of server time | not run | 16,079 | **9,423** |
| 1100 replies, one per 10 s of server time | not run | 16,079 | **8,079** |
| One 1 MiB reply, right after | not run | 2,104,038 | **1,063,646** |
| One 1 MiB reply, then a small one after 61 s | not run | 2,104,070 | **7,926** |

`7774ed6` had no replay history, which is why it stays at 7,911. The saturated cost is
reduced, not eliminated: 14,031 B remains 77% above it. Server time is simulated with
`LOOPBACK_MSG_ID_STEP_MS`. Buffers are released on the first transfer after 60 s without
a larger one; idle time alone frees nothing, but keepalive replies provide that transfer,
so an idle session releases within about two minutes. Logs: [final/heap](final/heap).

Whole-process peak RSS in the 1 MiB open-loop scenarios did not improve, because it is
dominated by traffic in flight and includes the local responder. Median of three runs,
`2ffd991` then this tree: 89,092/89,268 KiB at 100 calls/s and 369,244/378,752 KiB at
2,000/s, the latter completing about 7% more calls, all with zero errors
([logs](final/large)). Three runs give no confidence interval. Heap-run process RSS was
11,460, 14,512 and 13,788 KiB for the three revisions, one run each.

## Decisions from measurement

Each row compares a variant with the tree before it; overload scenario only.

| Variant | Completed | Busy p99 | Quiet p99 | Decision |
|---|---:|---:|---:|---|
| [Receive-arm-only budget](budget/overload/summary.json), six pairs | +0.96% [-3.39, +9.56] | -24.21% [-34.87, -18.39] | +38.75% [+29.19, +48.88] | Rejected: quiet tail cost, no resolved throughput |
| [Batch 32, one charge](experiments/batch-32/summary.json) | +12.58% [+9.12, +15.81] | -36.85% [-44.03, -32.90] | +28.42% [+13.56, +44.28] | Rejected: fairness |
| [Batch 16, one charge](experiments/batch-16/summary.json) | +12.77% [+11.41, +14.16] | -38.06% [-41.90, -37.10] | +25.76% [+13.99, +35.24] | Rejected: fairness |
| [Batch 8, one charge](experiments/batch-8/summary.json) | +12.87% [+9.65, +14.47] | -37.65% [-39.37, -35.33] | +21.98% [+12.91, +33.70] | Rejected: up to 1,024 enqueues per poll instead of 128 |
| [Batch 8, charged per request](experiments/batch-8-charged/summary.json) | +8.98% [+5.21, +15.45] | -16.60% [-20.91, -10.17] | +0.31% [-5.11, +10.49] | **Kept** |

Batch variants differ from the kept code only in `READY_RPC_BATCH` and in omitting the
per-request `consume_budget`. The receive-arm-only [source patch](budget-source.patch)
also carries receiver and test hunks that the loopback harness does not exercise, with
[closed-loop results](budget/rpc/summary.json) and raw logs retained.

[Before batching](experiments/before-batching), this tree measured overload completed
+0.31% [-1.84, +2.43] against `2ffd991` and -7.61% [-11.46, -5.62] against `7774ed6`.
One still earlier run showed -7.76% against `2ffd991` while a review process was active on
this host. Its logs were not kept, so this is a disclosure and not evidence.

## Generated TL nesting limit

Worst measured stack per boxed TL level, from [instrumented decoders](tl-depth/stack-probe-tests.rs.txt)
([release](tl-depth/stack-release.txt), [unoptimized](tl-depth/stack-unoptimized.txt)):

| Path | Release | Unoptimized |
|---|---:|---:|
| `RichText` | 1.3 KiB | 6.9 KiB |
| `PageBlockCover` | 2.6 KiB | 14.7 KiB |
| `PageBlockDetails` | 4.1 KiB | 18.6 KiB |
| `MessageMedia`/`StoryItem` | 3.6 KiB | 12.6 KiB |
| `MessageMedia`/`WebPage`/`StoryItem` | 3.1 KiB | 11.1 KiB |
| `MessageMedia`/`Poll` | 3.4 KiB | 11.9 KiB |
| Fixed `Updates` to `Message` prefix (9 levels) | 51 KiB total | 289 KiB total |

So 64 levels cost about 276 KiB in release and 1.3 MiB unoptimized, within a 2 MiB stack;
the regression runs the worst case on a real 2 MiB child thread. A [schema scan](tl-depth/schema-scan.py)
([output](tl-depth/schema-scan.txt)) found 23 levels as the upper bound with each
recursive type met once. An earlier 32-frame candidate allowed only 16 levels, which a
realistic Instant View can reach, so the limit was raised before publication.

## Reproduction

Export each revision, copy this tree's `grammers-mtsender/tests/loopback.rs` over it (and
add the `libc` dev-dependency for `7774ed6`), then build with the command in
[build.json](final/build.json). Run `scripts/compare-loopback-r1.py --runs 12` and
`scripts/compare-overload.py --pairs 12`. For the census use `--exact
loopback_session_footprint` and `--exact loopback_large_reply_retention` with `--ignored
--nocapture --test-threads=1`, `LOOPBACK_CLIENT_CPUS=0`, `LOOPBACK_SERVER_CPUS=2,3`,
`LOOPBACK_RUNTIME=current`, and `LOOPBACK_MSG_ID_STEP_MS=25` or `3` for the quiet stages
(400 sessions). The 1 MiB scenarios use `--exact loopback_open_loop` with
`LOOPBACK_LARGE=1 LOOPBACK_COLD=1` and `LOOPBACK_RATE=100` or `2000`.
